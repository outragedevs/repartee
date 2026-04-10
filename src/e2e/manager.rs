//! `E2eManager` — the single entry point the rest of the app uses to
//! drive the RPE2E protocol.
//!
//! Responsibilities:
//!
//! - own the long-term `Identity` and a handle to the `Keyring`
//! - encrypt outgoing plaintext into one or more wire-format lines
//! - decrypt an incoming wire-format line under the strict handle check
//! - build and dispatch KEYREQ (initiator side)
//! - answer KEYREQ with KEYRSP when policy allows (responder side)
//! - consume KEYRSP to install a trusted incoming session (initiator side)
//! - enforce per-peer rate limiting on outgoing KEYREQ
//!
//! The handshake key-agreement is X25519 over *ephemeral* keys carried in
//! both KEYREQ (`eph_x25519`) and KEYRSP (`ephemeral_pub`). The Ed25519
//! long-term identity is used only to sign/verify each handshake message;
//! it is never directly fed into ECDH (which would conflate two curves).
//! See `docs/plans/2026-04-11-rpee2e-implementation.md` §12b for the
//! rationale.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey as XPub, StaticSecret};

use crate::e2e::chunker::split_plaintext;
use crate::e2e::crypto::{
    aead::{self, SessionKey},
    fingerprint::{fingerprint, Fingerprint},
    identity::Identity,
    sig,
};
use crate::e2e::error::{E2eError, Result};
use crate::e2e::handshake::{
    encode_keyreq, encode_keyrsp, signed_keyreq_payload, signed_keyrsp_payload, KeyReq, KeyRsp,
    RateLimiter,
};
use crate::e2e::keyring::{
    ChannelConfig, ChannelMode, IncomingSession, Keyring, PeerRecord, TrustStatus,
};
use crate::e2e::wire::{build_aad, fresh_msgid, WireChunk};
use crate::e2e::TS_TOLERANCE_SECS;

/// In-flight handshake state — the initiator's ephemeral X25519 secret,
/// held until the matching KEYRSP arrives. Stored as raw bytes because
/// `x25519_dalek::StaticSecret` is not `Clone` and we want a boring
/// `HashMap` value.
///
/// Do **not** derive `Debug` on types that hold the raw secret.
struct PendingHandshake {
    #[allow(dead_code, reason = "future diagnostics: pending channel listing")]
    channel: String,
    eph_x25519_secret: [u8; 32],
}

/// Top-level E2E controller. One instance lives in `AppState` for the
/// lifetime of the process.
pub struct E2eManager {
    identity: Identity,
    keyring: Keyring,
    rate_limiter: Mutex<RateLimiter>,
    /// In-flight handshakes keyed by `(channel, keyreq_nonce)`. The
    /// responder doesn't echo the KEYREQ nonce in KEYRSP, so lookup in
    /// `handle_keyrsp` scans entries by channel.
    pending: Mutex<HashMap<(String, [u8; 16]), PendingHandshake>>,
}

/// Outcome of attempting to decrypt an incoming wire-format line.
#[derive(Debug, Clone)]
pub enum DecryptOutcome {
    /// One plaintext fragment (single chunk). Callers concatenate across
    /// chunks at the UI layer; the protocol is stateless per chunk.
    Plaintext(String),
    /// The chunk is well-formed but we don't have a session key for this
    /// sender on this channel yet. Callers should consider sending a
    /// `KEYREQ` (subject to rate limiting).
    MissingKey { handle: String, channel: String },
    /// Security-level rejection: handle mismatch, replay window violation,
    /// AEAD failure, peer not trusted, etc. Callers should surface this as
    /// an e2e warning rather than as a normal message.
    Rejected(String),
}

impl E2eManager {
    /// Load the identity row from the keyring, or generate and persist a
    /// fresh one if none exists.
    pub fn load_or_init(keyring: Keyring) -> Result<Self> {
        let identity = if let Some((_pk, sk, _fp, _ts)) = keyring.load_identity()? {
            Identity::from_secret_bytes(&sk)
        } else {
            let id = Identity::generate()?;
            let pk = id.public_bytes();
            let sk = id.secret_bytes();
            let fp = fingerprint(&pk);
            let now = now_unix();
            keyring.save_identity(&pk, &sk, &fp, now)?;
            id
        };
        Ok(Self {
            identity,
            keyring,
            rate_limiter: Mutex::new(RateLimiter::new()),
            pending: Mutex::new(HashMap::new()),
        })
    }

    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        fingerprint(&self.identity.public_bytes())
    }

    #[must_use]
    pub fn keyring(&self) -> &Keyring {
        &self.keyring
    }

    #[must_use]
    pub fn identity_pub(&self) -> [u8; 32] {
        self.identity.public_bytes()
    }

    // ---------- encrypt outgoing ----------

    /// Encrypt `plaintext` for `channel` and return one wire-format line per
    /// chunk. Callers send these verbatim via PRIVMSG. Honors lazy rotation:
    /// if the outgoing session is flagged `pending_rotation`, a fresh key is
    /// generated first.
    pub fn encrypt_outgoing(
        &self,
        sender_handle: &str,
        channel: &str,
        plaintext: &str,
    ) -> Result<Vec<String>> {
        let sk = self.get_or_generate_outgoing_key(channel)?;
        let chunks = split_plaintext(plaintext)?;
        let total_usize = chunks.len();
        let total = u8::try_from(total_usize).map_err(|_| E2eError::ChunkLimit(u8::MAX))?;
        if total == 0 {
            // split_plaintext always returns at least one chunk, but keep a
            // defensive check — `build_aad` and wire format require part ≥ 1.
            return Err(E2eError::Wire("chunker produced zero chunks".into()));
        }
        let msgid = fresh_msgid();
        let ts = now_unix();
        let mut out = Vec::with_capacity(total_usize);
        for (idx, plain) in chunks.iter().enumerate() {
            // idx is in 0..total, so idx+1 fits in u8 because total ≤ u8::MAX.
            let part = u8::try_from(idx + 1)
                .map_err(|_| E2eError::ChunkLimit(u8::MAX))?;
            let aad = build_aad(sender_handle, channel, &msgid, ts, part, total);
            let (nonce, ct) = aead::encrypt(&sk, &aad, plain)?;
            let wire = WireChunk {
                msgid,
                ts,
                part,
                total,
                nonce,
                ciphertext: ct,
            };
            out.push(wire.encode()?);
        }
        Ok(out)
    }

    fn get_or_generate_outgoing_key(&self, channel: &str) -> Result<SessionKey> {
        if let Some(sess) = self.keyring.get_outgoing_session(channel)?
            && !sess.pending_rotation
        {
            return Ok(sess.sk);
        }
        // Either no session yet, or the current one is flagged
        // `pending_rotation` (lazy rotate). Create a fresh session key;
        // INSERT OR REPLACE in set_outgoing_session also clears the flag.
        let fresh = aead::generate_session_key()?;
        self.keyring
            .set_outgoing_session(channel, &fresh, now_unix())?;
        Ok(fresh)
    }

    // ---------- decrypt incoming ----------

    /// Decrypt an incoming wire-format line. `sender_handle` **must** come
    /// from the raw IRC `user@host` prefix captured by the IRC parser —
    /// never from a field inside the encrypted payload. Strict handle check
    /// is what binds a session key to an on-wire identity.
    pub fn decrypt_incoming(
        &self,
        sender_handle: &str,
        channel: &str,
        wire_line: &str,
    ) -> Result<DecryptOutcome> {
        let Some(wire) = WireChunk::parse(wire_line)? else {
            // Not an RPE2E01 line at all — caller should render it as the
            // plain IRC text.
            return Ok(DecryptOutcome::Plaintext(wire_line.to_string()));
        };

        // Replay window check (application layer, on top of AEAD).
        let now = now_unix();
        let skew = (now - wire.ts).abs();
        if skew > TS_TOLERANCE_SECS {
            return Ok(DecryptOutcome::Rejected(format!(
                "ts outside tolerance window ({skew}s skew)"
            )));
        }

        let Some(sess) = self.keyring.get_incoming_session(sender_handle, channel)? else {
            return Ok(DecryptOutcome::MissingKey {
                handle: sender_handle.to_string(),
                channel: channel.to_string(),
            });
        };
        if sess.status != TrustStatus::Trusted {
            return Ok(DecryptOutcome::Rejected(format!(
                "peer not trusted (status={:?})",
                sess.status
            )));
        }

        let aad = build_aad(
            sender_handle,
            channel,
            &wire.msgid,
            wire.ts,
            wire.part,
            wire.total,
        );
        match aead::decrypt(&sess.sk, &wire.nonce, &aad, &wire.ciphertext) {
            Ok(pt) => match String::from_utf8(pt) {
                Ok(s) => Ok(DecryptOutcome::Plaintext(s)),
                Err(e) => Ok(DecryptOutcome::Rejected(format!("utf8: {e}"))),
            },
            Err(e) => Ok(DecryptOutcome::Rejected(format!("aead failed: {e}"))),
        }
    }

    // ---------- handshake initiator ----------

    /// Build a signed KEYREQ for `channel`, generating and stashing a fresh
    /// ephemeral X25519 secret. The secret is retrieved later in
    /// `handle_keyrsp` to derive the wrap key.
    pub fn build_keyreq(&self, channel: &str) -> Result<KeyReq> {
        let mut nonce = [0u8; 16];
        rand::fill(&mut nonce);

        let mut eph_secret = [0u8; 32];
        rand::fill(&mut eph_secret);
        let eph_pub = {
            let sec = StaticSecret::from(eph_secret);
            XPub::from(&sec).to_bytes()
        };

        let pubkey = self.identity.public_bytes();
        let sig_payload = signed_keyreq_payload(channel, &pubkey, &eph_pub, &nonce);
        let sig_bytes = sig::sign(self.identity.signing_key(), &sig_payload);

        self.pending
            .lock()
            .expect("e2e pending mutex poisoned")
            .insert(
                (channel.to_string(), nonce),
                PendingHandshake {
                    channel: channel.to_string(),
                    eph_x25519_secret: eph_secret,
                },
            );

        Ok(KeyReq {
            channel: channel.to_string(),
            pubkey,
            eph_x25519: eph_pub,
            nonce,
            sig: sig_bytes,
        })
    }

    /// Wrap a KEYREQ in CTCP framing ready for NOTICE dispatch.
    #[must_use]
    #[allow(
        clippy::unused_self,
        reason = "method form is kept for symmetry with the rest of the public API; \
                  future versions may bind per-instance state (e.g. NOTICE ID counters)"
    )]
    pub fn encode_keyreq_ctcp(&self, req: &KeyReq) -> String {
        format!("\x01{}\x01", encode_keyreq(req))
    }

    /// Rate-limit check for outgoing KEYREQ. Returns `true` if a request to
    /// `peer_handle` is allowed right now.
    pub fn allow_keyreq(&self, peer_handle: &str) -> bool {
        self.rate_limiter
            .lock()
            .expect("rate limiter mutex poisoned")
            .allow_outgoing(peer_handle)
    }

    // ---------- handshake responder ----------

    /// Handle an incoming KEYREQ. Returns `Some(KeyRsp)` ready to send back
    /// via NOTICE if policy allows, `None` if the request is silently
    /// ignored (channel disabled / peer not trusted / quiet mode). Returns
    /// `Err` only on protocol-level problems (bad signature, malformed
    /// field).
    pub fn handle_keyreq(&self, sender_handle: &str, req: &KeyReq) -> Result<Option<KeyRsp>> {
        // Verify signature over the full KEYREQ payload, binding `eph_x25519`.
        let sig_payload =
            signed_keyreq_payload(&req.channel, &req.pubkey, &req.eph_x25519, &req.nonce);
        sig::verify(&req.pubkey, &sig_payload, &req.sig)?;

        // Check channel-level config.
        let ch_cfg = self
            .keyring
            .get_channel_config(&req.channel)?
            .unwrap_or_else(|| ChannelConfig {
                channel: req.channel.clone(),
                enabled: false,
                mode: ChannelMode::Normal,
            });
        if !ch_cfg.enabled {
            return Ok(None);
        }

        // TOFU peer record (upsert so last_seen/last_handle stay fresh).
        let fp = fingerprint(&req.pubkey);
        let now = now_unix();
        let peer_rec = PeerRecord {
            fingerprint: fp,
            pubkey: req.pubkey,
            last_handle: Some(sender_handle.to_string()),
            last_nick: None,
            first_seen: now,
            last_seen: now,
            global_status: TrustStatus::Pending,
        };
        self.keyring.upsert_peer(&peer_rec)?;

        // Policy: in auto-accept we always respond; in normal/quiet we only
        // respond if we already have a trusted incoming session from this
        // peer on this channel (i.e. they were /e2e accept-ed earlier).
        let allow = match ch_cfg.mode {
            ChannelMode::AutoAccept => true,
            ChannelMode::Normal | ChannelMode::Quiet => self
                .keyring
                .get_incoming_session(sender_handle, &req.channel)?
                .is_some_and(|s| s.status == TrustStatus::Trusted),
        };
        if !allow {
            return Ok(None);
        }

        // Our outgoing channel key is what we wrap and hand the peer so
        // they can decrypt our future messages.
        let our_sk = self.get_or_generate_outgoing_key(&req.channel)?;

        // Fresh ephemeral X25519 keypair for ECDH with the initiator's
        // ephemeral public.
        let mut our_eph_secret = [0u8; 32];
        rand::fill(&mut our_eph_secret);
        let our_eph_sec = StaticSecret::from(our_eph_secret);
        let our_eph_pub = XPub::from(&our_eph_sec).to_bytes();

        // `info` (and AEAD `aad`) must be computable identically by both
        // sides. We deliberately omit `sender_handle` here because the
        // initiator does not know its own server-assigned handle at the
        // point it calls `handle_keyrsp`. The ephemeral X25519 keypairs
        // themselves bind the exchange to a specific peer.
        let info = wrap_info(&req.channel);
        let wrap_key = derive_wrap_key(&our_eph_sec, &req.eph_x25519, info.as_bytes());
        let (wrap_nonce, wrap_ct) = aead::encrypt(&wrap_key, info.as_bytes(), &our_sk)?;

        // Sign response.
        let mut rsp_nonce = [0u8; 16];
        rand::fill(&mut rsp_nonce);
        let sig_payload =
            signed_keyrsp_payload(&req.channel, &our_eph_pub, &wrap_nonce, &wrap_ct, &rsp_nonce);
        let sig_bytes = sig::sign(self.identity.signing_key(), &sig_payload);

        // We have also just shipped *our own* outgoing session to the peer.
        // Record an incoming-session row on our side pointing at the
        // initiator so that when they start sending encrypted PRIVMSGs we
        // can decrypt them — except we don't know *their* key yet because
        // in this model the two sides each use their own outgoing key. The
        // peer's future KEYREQ→KEYRSP (or the reciprocal of this one
        // initiated by us) installs the other direction.
        //
        // NOTE: this v1 model is "unidirectional per KEYREQ"; a fully
        // symmetric exchange requires either side to send its own KEYREQ.
        // The integration tests only drive one direction.

        Ok(Some(KeyRsp {
            channel: req.channel.clone(),
            ephemeral_pub: our_eph_pub,
            wrap_nonce,
            wrap_ct,
            nonce: rsp_nonce,
            sig: sig_bytes,
        }))
    }

    /// Wrap a KEYRSP in CTCP framing ready for NOTICE dispatch.
    #[must_use]
    #[allow(
        clippy::unused_self,
        reason = "method form is kept for symmetry with the rest of the public API"
    )]
    pub fn encode_keyrsp_ctcp(&self, rsp: &KeyRsp) -> String {
        format!("\x01{}\x01", encode_keyrsp(rsp))
    }

    // ---------- handshake initiator (KEYRSP consumer) ----------

    /// Consume an incoming KEYRSP. Verifies the responder's signature with
    /// `sender_pubkey` (which the caller must have remembered from the
    /// prior KEYREQ round-trip), looks up our pending ephemeral secret,
    /// completes the ECDH, unwraps the session key, and installs it as a
    /// *trusted* incoming session — the initiator already consented by
    /// having sent the KEYREQ in the first place.
    pub fn handle_keyrsp(
        &self,
        sender_handle: &str,
        sender_pubkey: &[u8; 32],
        rsp: &KeyRsp,
    ) -> Result<()> {
        // Verify signature first, before touching any state.
        let sig_payload = signed_keyrsp_payload(
            &rsp.channel,
            &rsp.ephemeral_pub,
            &rsp.wrap_nonce,
            &rsp.wrap_ct,
            &rsp.nonce,
        );
        sig::verify(sender_pubkey, &sig_payload, &rsp.sig)?;

        // Find matching pending handshake by channel. v1 allows at most one
        // in-flight handshake per channel at a time — this is fine in
        // practice given the 30-second per-peer rate limit.
        let mut pending = self.pending.lock().expect("e2e pending mutex poisoned");
        let key_to_remove = pending
            .iter()
            .find(|(k, _)| k.0 == rsp.channel)
            .map(|(k, _)| k.clone());
        let key = key_to_remove
            .ok_or_else(|| E2eError::Handshake("no pending handshake for channel".into()))?;
        let ph = pending
            .remove(&key)
            .expect("just found the key above, cannot be absent");
        drop(pending);

        // Derive wrap key from our stored ephemeral secret + peer's
        // ephemeral public. `info` must match exactly what the responder
        // used — see `handle_keyreq` for the rationale on handle omission.
        let info = wrap_info(&rsp.channel);
        let our_sec = StaticSecret::from(ph.eph_x25519_secret);
        let wrap_key = derive_wrap_key(&our_sec, &rsp.ephemeral_pub, info.as_bytes());

        // Unwrap the session key.
        let sk_bytes = aead::decrypt(&wrap_key, &rsp.wrap_nonce, info.as_bytes(), &rsp.wrap_ct)?;
        if sk_bytes.len() != 32 {
            return Err(E2eError::Crypto(format!(
                "unwrapped sk has unexpected length {}",
                sk_bytes.len()
            )));
        }
        let mut sk = [0u8; 32];
        sk.copy_from_slice(&sk_bytes);

        // Install as trusted incoming session. We also TOFU-record the
        // peer in `e2e_peers`; upsert is safe.
        let fp = fingerprint(sender_pubkey);
        let now = now_unix();
        self.keyring.upsert_peer(&PeerRecord {
            fingerprint: fp,
            pubkey: *sender_pubkey,
            last_handle: Some(sender_handle.to_string()),
            last_nick: None,
            first_seen: now,
            last_seen: now,
            global_status: TrustStatus::Trusted,
        })?;
        let sess = IncomingSession {
            handle: sender_handle.to_string(),
            channel: rsp.channel.clone(),
            fingerprint: fp,
            sk,
            status: TrustStatus::Trusted,
            created_at: now,
        };
        self.keyring.set_incoming_session(&sess)?;
        Ok(())
    }
}

/// Canonical HKDF `info` / AEAD `aad` for the handshake wrap step. Must be
/// computable identically by both initiator and responder from data they
/// already have. We bind only the channel; the ephemeral X25519 keypairs
/// already bind the exchange to a specific peer pair.
fn wrap_info(channel: &str) -> String {
    format!("RPE2E01-WRAP:{channel}")
}

/// Derive a 32-byte wrap key from a pair of ephemeral X25519 keys. Matches
/// the same HKDF construction used by `crypto::ecdh::EphemeralKeypair`.
fn derive_wrap_key(secret: &StaticSecret, peer_pub_bytes: &[u8; 32], info: &[u8]) -> [u8; 32] {
    let peer_pub = XPub::from(*peer_pub_bytes);
    let shared = secret.diffie_hellman(&peer_pub);
    let hk = Hkdf::<Sha256>::new(Some(b"RPE2E01-WRAP"), shared.as_bytes());
    let mut okm = [0u8; 32];
    hk.expand(info, &mut okm)
        .expect("hkdf expand 32 bytes never fails for OKM ≤ 255 * HashLen");
    okm
}

fn now_unix() -> i64 {
    // SystemTime can technically be before UNIX_EPOCH on misconfigured
    // systems; in that case we fall back to 0. The replay-window check
    // works relatively, so a consistent-but-wrong clock still keeps both
    // sides in sync. AEAD is the real authenticity guarantee.
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::e2e::keyring::Keyring;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex as StdMutex};

    const SCHEMA: &str = "
        CREATE TABLE e2e_identity (id INTEGER PRIMARY KEY CHECK (id = 1), pubkey BLOB NOT NULL, privkey BLOB NOT NULL, fingerprint BLOB NOT NULL, created_at INTEGER NOT NULL);
        CREATE TABLE e2e_peers (fingerprint BLOB PRIMARY KEY, pubkey BLOB NOT NULL, last_handle TEXT, last_nick TEXT, first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL, global_status TEXT NOT NULL DEFAULT 'pending');
        CREATE TABLE e2e_outgoing_sessions (channel TEXT PRIMARY KEY, sk BLOB NOT NULL, created_at INTEGER NOT NULL, pending_rotation INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE e2e_incoming_sessions (handle TEXT NOT NULL, channel TEXT NOT NULL, fingerprint BLOB NOT NULL, sk BLOB NOT NULL, status TEXT NOT NULL DEFAULT 'pending', created_at INTEGER NOT NULL, PRIMARY KEY (handle, channel));
        CREATE TABLE e2e_channel_config (channel TEXT PRIMARY KEY, enabled INTEGER NOT NULL DEFAULT 0, mode TEXT NOT NULL DEFAULT 'normal');
        CREATE TABLE e2e_autotrust (id INTEGER PRIMARY KEY AUTOINCREMENT, scope TEXT NOT NULL, handle_pattern TEXT NOT NULL, created_at INTEGER NOT NULL, UNIQUE(scope, handle_pattern));
    ";

    fn make_manager() -> E2eManager {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        let kr = Keyring::new(Arc::new(StdMutex::new(conn)));
        E2eManager::load_or_init(kr).unwrap()
    }

    #[test]
    fn load_or_init_persists_identity() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        let shared = Arc::new(StdMutex::new(conn));

        let kr1 = Keyring::new(shared.clone());
        let m1 = E2eManager::load_or_init(kr1).unwrap();
        let pk1 = m1.identity_pub();

        let kr2 = Keyring::new(shared);
        let m2 = E2eManager::load_or_init(kr2).unwrap();
        let pk2 = m2.identity_pub();

        assert_eq!(pk1, pk2, "identity must persist across loads");
    }

    #[test]
    fn build_keyreq_stores_pending_and_is_signed() {
        let mgr = make_manager();
        let req = mgr.build_keyreq("#x").unwrap();
        // Signature verifies with the same pubkey embedded in the KEYREQ.
        let payload = signed_keyreq_payload("#x", &req.pubkey, &req.eph_x25519, &req.nonce);
        sig::verify(&req.pubkey, &payload, &req.sig).unwrap();
        // Pending map has exactly one entry.
        assert_eq!(
            mgr.pending
                .lock()
                .expect("pending mutex poisoned in test")
                .len(),
            1
        );
    }

    #[test]
    fn encrypt_decrypt_requires_session() {
        let mgr = make_manager();
        // No incoming session installed → decrypt_incoming returns MissingKey.
        let wire = mgr
            .encrypt_outgoing("~alice@host", "#x", "hi")
            .unwrap()
            .remove(0);
        let outcome = mgr.decrypt_incoming("~alice@host", "#x", &wire).unwrap();
        match outcome {
            DecryptOutcome::MissingKey { .. } => {}
            other => panic!("expected MissingKey, got {other:?}"),
        }
    }
}

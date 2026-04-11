//! RPE2E CTCP handshake: KEYREQ / KEYRSP / REKEY encode/parse and rate limiting.
//!
//! Wire form (inside the CTCP `\x01 ... \x01` framing, sent via NOTICE):
//!
//! ```text
//! RPEE2E KEYREQ v=1 chan=#x pub=<hex32> eph=<hex32> nonce=<hex16> sig=<hex64>
//! RPEE2E KEYRSP v=1 chan=#x pub=<hex32> eph=<hex32> wnonce=<hex24> wrap=<b64> nonce=<hex16> sig=<hex64>
//! RPEE2E REKEY  v=1 chan=#x pub=<hex32> eph=<hex32> wnonce=<hex24> wrap=<b64> nonce=<hex16> sig=<hex64>
//! ```
//!
//! `pub` carries the initiator's long-term Ed25519 identity pubkey. `eph` on
//! `KEYREQ` is the initiator's ephemeral X25519 pubkey; it is bound to the
//! signature so a MitM cannot swap it out. `eph` on `KEYRSP` is the
//! responder's ephemeral X25519 pubkey. The wrap key is derived by either
//! side from an X25519 ECDH of their own ephemeral secret with the peer's
//! ephemeral public (HKDF-SHA256, see `crypto::ecdh`).
//!
//! `REKEY` is the unsolicited-distribution variant used for lazy rotate
//! (spec §5.3): after a `/e2e revoke` the sender pushes a freshly generated
//! session key to every remaining trusted peer. Unlike KEYRSP — which is a
//! response to a KEYREQ and derives its wrap key from the initiator's
//! ephemeral X25519 — REKEY is initiator-driven: the sender makes a fresh
//! ephemeral X25519 keypair and encrypts to the peer's **long-term Ed25519
//! identity converted to X25519** via the standard birational map
//! (RFC 7748 Appendix A, matching libsodium's
//! `crypto_sign_ed25519_pk_to_curve25519`). This lets Alice push a new key
//! to peers she has never received a KEYREQ from in the current session.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::e2e::crypto::aead::NONCE_LEN;
use crate::e2e::error::{E2eError, Result};

pub const CTCP_TAG: &str = "RPEE2E";
pub const PROTO_VERSION: u8 = 1;

/// Minimum gap between outgoing KEYREQ to the same peer.
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(30);

/// Sliding-window length for the incoming-KEYREQ rate limiter (spec §5.4).
const INCOMING_WINDOW: Duration = Duration::from_secs(60);

/// Maximum incoming KEYREQs per peer within `INCOMING_WINDOW` before the
/// peer is pushed into backoff.
const INCOMING_MAX_PER_WINDOW: usize = 3;

/// Backoff duration applied after a peer exceeds the incoming window.
const INCOMING_BACKOFF: Duration = Duration::from_secs(5 * 60);

/// KEYREQ message. `pubkey` is the initiator's long-term Ed25519 identity;
/// `eph_x25519` is a fresh ephemeral X25519 public used for ECDH. Both are
/// bound to `sig`.
#[derive(Debug, Clone)]
pub struct KeyReq {
    pub channel: String,
    pub pubkey: [u8; 32],
    pub eph_x25519: [u8; 32],
    pub nonce: [u8; 16],
    pub sig: [u8; 64],
}

/// KEYRSP message. Carries the responder's ephemeral X25519 pub and an AEAD
/// ciphertext containing the channel session key wrapped under the derived
/// ECDH+HKDF wrap key. `pubkey` is the responder's long-term Ed25519
/// identity — the initiator verifies the signature against it and uses it
/// as the TOFU pin, so the pubkey does not need to be known out-of-band.
#[derive(Debug, Clone)]
pub struct KeyRsp {
    pub channel: String,
    pub pubkey: [u8; 32],
    pub ephemeral_pub: [u8; 32],
    pub wrap_nonce: [u8; NONCE_LEN],
    pub wrap_ct: Vec<u8>,
    pub nonce: [u8; 16],
    pub sig: [u8; 64],
}

/// Canonical payload signed by the initiator in KEYREQ. Binding
/// `eph_x25519` into the signature prevents a downgrade or swap attack
/// where a MitM would otherwise be able to substitute its own X25519 key
/// without breaking the Ed25519 signature.
fn sig_payload_keyreq(
    channel: &str,
    pubkey: &[u8; 32],
    eph_x25519: &[u8; 32],
    nonce: &[u8; 16],
) -> Vec<u8> {
    let mut v = Vec::with_capacity(16 + channel.len() + 32 + 32 + 16);
    v.extend_from_slice(b"KEYREQ:");
    v.extend_from_slice(channel.as_bytes());
    v.push(b':');
    v.extend_from_slice(pubkey);
    v.push(b':');
    v.extend_from_slice(eph_x25519);
    v.push(b':');
    v.extend_from_slice(nonce);
    v
}

/// Unsolicited REKEY message. Shape is identical to KEYRSP on the wire
/// except that (a) the ECDH wrap is derived from the peer's long-term
/// Ed25519 identity (via the RFC 7748 birational map to X25519), and
/// (b) the sender is the initiator of the distribution rather than a
/// responder to an incoming KEYREQ.
#[derive(Debug, Clone)]
pub struct KeyRekey {
    pub channel: String,
    pub pubkey: [u8; 32],
    pub eph_pub: [u8; 32],
    pub wrap_nonce: [u8; NONCE_LEN],
    pub wrap_ct: Vec<u8>,
    pub nonce: [u8; 16],
    pub sig: [u8; 64],
}

/// Canonical payload signed by the responder in KEYRSP. Binds the
/// responder's identity `pubkey` so a MitM cannot substitute its own
/// long-term key without breaking the Ed25519 signature, even though the
/// initiator has no prior record of the responder.
fn sig_payload_keyrsp(
    channel: &str,
    pubkey: &[u8; 32],
    eph_pub: &[u8; 32],
    wrap_nonce: &[u8; NONCE_LEN],
    wrap_ct: &[u8],
    nonce: &[u8; 16],
) -> Vec<u8> {
    let mut v =
        Vec::with_capacity(16 + channel.len() + 32 + 32 + NONCE_LEN + wrap_ct.len() + 16);
    v.extend_from_slice(b"KEYRSP:");
    v.extend_from_slice(channel.as_bytes());
    v.push(b':');
    v.extend_from_slice(pubkey);
    v.push(b':');
    v.extend_from_slice(eph_pub);
    v.push(b':');
    v.extend_from_slice(wrap_nonce);
    v.push(b':');
    v.extend_from_slice(wrap_ct);
    v.push(b':');
    v.extend_from_slice(nonce);
    v
}

/// Canonical payload signed by the sender in REKEY. Binds the
/// sender's identity `pubkey`, the channel, the fresh ephemeral X25519
/// public, the wrap nonce+ciphertext, and an anti-replay nonce. The
/// leading `REKEY:` domain separator prevents cross-protocol confusion
/// with KEYREQ / KEYRSP payloads.
fn sig_payload_keyrekey(
    channel: &str,
    pubkey: &[u8; 32],
    eph_pub: &[u8; 32],
    wrap_nonce: &[u8; NONCE_LEN],
    wrap_ct: &[u8],
    nonce: &[u8; 16],
) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + channel.len() + 32 + 32 + NONCE_LEN + wrap_ct.len() + 16);
    v.extend_from_slice(b"REKEY:");
    v.extend_from_slice(channel.as_bytes());
    v.push(b':');
    v.extend_from_slice(pubkey);
    v.push(b':');
    v.extend_from_slice(eph_pub);
    v.push(b':');
    v.extend_from_slice(wrap_nonce);
    v.push(b':');
    v.extend_from_slice(wrap_ct);
    v.push(b':');
    v.extend_from_slice(nonce);
    v
}

#[must_use]
pub fn encode_keyreq(req: &KeyReq) -> String {
    format!(
        "{CTCP_TAG} KEYREQ v={PROTO_VERSION} chan={chan} pub={pub_} eph={eph} nonce={nonce} sig={sig}",
        chan = req.channel,
        pub_ = hex::encode(req.pubkey),
        eph = hex::encode(req.eph_x25519),
        nonce = hex::encode(req.nonce),
        sig = hex::encode(req.sig),
    )
}

#[must_use]
pub fn encode_keyrsp(rsp: &KeyRsp) -> String {
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    format!(
        "{CTCP_TAG} KEYRSP v={PROTO_VERSION} chan={chan} pub={pub_} eph={eph} wnonce={wnonce} wrap={wrap} nonce={nonce} sig={sig}",
        chan = rsp.channel,
        pub_ = hex::encode(rsp.pubkey),
        eph = hex::encode(rsp.ephemeral_pub),
        wnonce = hex::encode(rsp.wrap_nonce),
        wrap = B64.encode(&rsp.wrap_ct),
        nonce = hex::encode(rsp.nonce),
        sig = hex::encode(rsp.sig),
    )
}

#[must_use]
pub fn encode_keyrekey(rk: &KeyRekey) -> String {
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    format!(
        "{CTCP_TAG} REKEY v={PROTO_VERSION} chan={chan} pub={pub_} eph={eph} wnonce={wnonce} wrap={wrap} nonce={nonce} sig={sig}",
        chan = rk.channel,
        pub_ = hex::encode(rk.pubkey),
        eph = hex::encode(rk.eph_pub),
        wnonce = hex::encode(rk.wrap_nonce),
        wrap = B64.encode(&rk.wrap_ct),
        nonce = hex::encode(rk.nonce),
        sig = hex::encode(rk.sig),
    )
}

#[derive(Debug)]
pub enum HandshakeMsg {
    Req(KeyReq),
    Rsp(KeyRsp),
    Rekey(KeyRekey),
}

/// Parse a single RPE2E handshake body (what lives inside the `\x01...\x01`
/// CTCP framing). Returns `Ok(None)` when the body does not start with the
/// `RPEE2E` tag, so callers can fall through to other CTCP handling.
pub fn parse(body: &str) -> Result<Option<HandshakeMsg>> {
    let mut parts = body.split_whitespace();
    if parts.next() != Some(CTCP_TAG) {
        return Ok(None);
    }
    let kind = parts
        .next()
        .ok_or_else(|| E2eError::Handshake("missing type".into()))?;
    let rest: Vec<&str> = parts.collect();

    let kv = parse_kv(&rest);
    let v: u8 = kv
        .get("v")
        .ok_or_else(|| E2eError::Handshake("missing v".into()))?
        .parse()
        .map_err(|e| E2eError::Handshake(format!("bad v: {e}")))?;
    if v != PROTO_VERSION {
        return Err(E2eError::Handshake(format!("unsupported version {v}")));
    }

    match kind {
        "KEYREQ" => parse_keyreq(&kv).map(|r| Some(HandshakeMsg::Req(r))),
        "KEYRSP" => parse_keyrsp(&kv).map(|r| Some(HandshakeMsg::Rsp(r))),
        "REKEY" => parse_keyrekey(&kv).map(|r| Some(HandshakeMsg::Rekey(r))),
        _ => Err(E2eError::Handshake(format!("unknown type {kind}"))),
    }
}

fn kv_get<'a>(kv: &'a HashMap<&'a str, &'a str>, key: &'static str) -> Result<&'a str> {
    kv.get(key)
        .copied()
        .ok_or_else(|| E2eError::Handshake(key.into()))
}

fn parse_wrap_nonce(kv: &HashMap<&str, &str>) -> Result<[u8; NONCE_LEN]> {
    let raw = hex::decode(kv_get(kv, "wnonce")?)?;
    if raw.len() != NONCE_LEN {
        return Err(E2eError::Handshake(format!(
            "wnonce len {} != {NONCE_LEN}",
            raw.len()
        )));
    }
    let mut arr = [0u8; NONCE_LEN];
    arr.copy_from_slice(&raw);
    Ok(arr)
}

fn parse_wrap_ct(kv: &HashMap<&str, &str>) -> Result<Vec<u8>> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    B64.decode(kv_get(kv, "wrap")?)
        .map_err(|e| E2eError::Handshake(format!("bad wrap b64: {e}")))
}

fn parse_keyreq(kv: &HashMap<&str, &str>) -> Result<KeyReq> {
    Ok(KeyReq {
        channel: kv_get(kv, "chan")?.to_string(),
        pubkey: hex32(kv_get(kv, "pub")?)?,
        eph_x25519: hex32(kv_get(kv, "eph")?)?,
        nonce: hex16(kv_get(kv, "nonce")?)?,
        sig: hex64(kv_get(kv, "sig")?)?,
    })
}

fn parse_keyrsp(kv: &HashMap<&str, &str>) -> Result<KeyRsp> {
    Ok(KeyRsp {
        channel: kv_get(kv, "chan")?.to_string(),
        pubkey: hex32(kv_get(kv, "pub")?)?,
        ephemeral_pub: hex32(kv_get(kv, "eph")?)?,
        wrap_nonce: parse_wrap_nonce(kv)?,
        wrap_ct: parse_wrap_ct(kv)?,
        nonce: hex16(kv_get(kv, "nonce")?)?,
        sig: hex64(kv_get(kv, "sig")?)?,
    })
}

fn parse_keyrekey(kv: &HashMap<&str, &str>) -> Result<KeyRekey> {
    Ok(KeyRekey {
        channel: kv_get(kv, "chan")?.to_string(),
        pubkey: hex32(kv_get(kv, "pub")?)?,
        eph_pub: hex32(kv_get(kv, "eph")?)?,
        wrap_nonce: parse_wrap_nonce(kv)?,
        wrap_ct: parse_wrap_ct(kv)?,
        nonce: hex16(kv_get(kv, "nonce")?)?,
        sig: hex64(kv_get(kv, "sig")?)?,
    })
}

fn parse_kv<'a>(fields: &'a [&'a str]) -> HashMap<&'a str, &'a str> {
    let mut out = HashMap::new();
    for f in fields {
        if let Some((k, v)) = f.split_once('=') {
            out.insert(k, v);
        }
    }
    out
}

fn hex32(s: &str) -> Result<[u8; 32]> {
    let raw = hex::decode(s)?;
    if raw.len() != 32 {
        return Err(E2eError::Handshake(format!(
            "expected 32 bytes, got {}",
            raw.len()
        )));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&raw);
    Ok(arr)
}

fn hex16(s: &str) -> Result<[u8; 16]> {
    let raw = hex::decode(s)?;
    if raw.len() != 16 {
        return Err(E2eError::Handshake(format!(
            "expected 16 bytes, got {}",
            raw.len()
        )));
    }
    let mut arr = [0u8; 16];
    arr.copy_from_slice(&raw);
    Ok(arr)
}

fn hex64(s: &str) -> Result<[u8; 64]> {
    let raw = hex::decode(s)?;
    if raw.len() != 64 {
        return Err(E2eError::Handshake(format!(
            "expected 64 bytes, got {}",
            raw.len()
        )));
    }
    let mut arr = [0u8; 64];
    arr.copy_from_slice(&raw);
    Ok(arr)
}

// ---------- rate limiter ----------

/// Per-peer incoming bucket. Sliding 60-second window of recent KEYREQ
/// arrivals; once the window fills with `INCOMING_MAX_PER_WINDOW` hits the
/// peer is pushed into `backoff_until` and every further KEYREQ is dropped
/// until the backoff expires, at which point the window resets.
#[derive(Debug, Default)]
struct IncomingBucket {
    /// Timestamps of recent KEYREQs received from this peer (sliding 60s window).
    recent: VecDeque<Instant>,
    /// When set, the peer is in 5-minute backoff — reject all until `Instant`.
    backoff_until: Option<Instant>,
}

/// Per-peer rate limiter for RPE2E handshake traffic. Enforces:
/// - outgoing: a minimum 30 second gap between KEYREQs to the same peer
///   so we don't flood passive/offline nicks.
/// - incoming: max `INCOMING_MAX_PER_WINDOW` KEYREQs per peer per
///   `INCOMING_WINDOW`; exceeding that puts the peer into
///   `INCOMING_BACKOFF` backoff during which every further KEYREQ is
///   dropped without any crypto work. Cheap to reject a signature flood
///   before the expensive Ed25519 verify runs.
#[derive(Debug, Default)]
pub struct RateLimiter {
    last_sent: HashMap<String, Instant>,
    incoming: HashMap<String, IncomingBucket>,
}

impl RateLimiter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if sending to `peer_handle` is allowed right now and
    /// records the attempt.
    pub fn allow_outgoing(&mut self, peer_handle: &str) -> bool {
        let now = Instant::now();
        if let Some(ts) = self.last_sent.get(peer_handle)
            && now.duration_since(*ts) < RATE_LIMIT_WINDOW
        {
            return false;
        }
        self.last_sent.insert(peer_handle.to_string(), now);
        true
    }

    /// Returns `true` if we should respond to an incoming KEYREQ from
    /// `peer_handle`. Enforces the spec §5.4 limit of
    /// `INCOMING_MAX_PER_WINDOW` per `INCOMING_WINDOW` with an
    /// `INCOMING_BACKOFF` timeout on excess.
    pub fn allow_incoming(&mut self, peer_handle: &str) -> bool {
        let now = Instant::now();
        let bucket = self
            .incoming
            .entry(peer_handle.to_string())
            .or_default();
        if let Some(until) = bucket.backoff_until {
            if now < until {
                return false;
            }
            bucket.backoff_until = None;
            bucket.recent.clear();
        }
        // Evict entries older than the sliding window.
        while let Some(front) = bucket.recent.front() {
            if now.duration_since(*front) > INCOMING_WINDOW {
                bucket.recent.pop_front();
            } else {
                break;
            }
        }
        if bucket.recent.len() >= INCOMING_MAX_PER_WINDOW {
            bucket.backoff_until = Some(now + INCOMING_BACKOFF);
            return false;
        }
        bucket.recent.push_back(now);
        true
    }

    /// Test-only helper: forcibly expire any backoff on `peer_handle` so a
    /// unit test can simulate the wait without sleeping. Clears the recent
    /// window as well, matching the end-of-backoff path in `allow_incoming`.
    #[cfg(test)]
    fn force_expire_backoff(&mut self, peer_handle: &str) {
        if let Some(bucket) = self.incoming.get_mut(peer_handle) {
            bucket.backoff_until = None;
            bucket.recent.clear();
        }
    }
}

/// Public accessor: canonical signed payload for KEYREQ (for use by
/// `E2eManager` when signing / verifying).
#[must_use]
pub fn signed_keyreq_payload(
    channel: &str,
    pubkey: &[u8; 32],
    eph_x25519: &[u8; 32],
    nonce: &[u8; 16],
) -> Vec<u8> {
    sig_payload_keyreq(channel, pubkey, eph_x25519, nonce)
}

/// Public accessor: canonical signed payload for KEYRSP. `pubkey` is the
/// responder's long-term Ed25519 identity — see `sig_payload_keyrsp` for
/// the MitM-resistance rationale.
#[must_use]
pub fn signed_keyrsp_payload(
    channel: &str,
    pubkey: &[u8; 32],
    eph_pub: &[u8; 32],
    wrap_nonce: &[u8; NONCE_LEN],
    wrap_ct: &[u8],
    nonce: &[u8; 16],
) -> Vec<u8> {
    sig_payload_keyrsp(channel, pubkey, eph_pub, wrap_nonce, wrap_ct, nonce)
}

/// Public accessor: canonical signed payload for REKEY.
#[must_use]
pub fn signed_keyrekey_payload(
    channel: &str,
    pubkey: &[u8; 32],
    eph_pub: &[u8; 32],
    wrap_nonce: &[u8; NONCE_LEN],
    wrap_ct: &[u8],
    nonce: &[u8; 16],
) -> Vec<u8> {
    sig_payload_keyrekey(channel, pubkey, eph_pub, wrap_nonce, wrap_ct, nonce)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_req() -> KeyReq {
        KeyReq {
            channel: "#x".into(),
            pubkey: [1; 32],
            eph_x25519: [9; 32],
            nonce: [2; 16],
            sig: [3; 64],
        }
    }

    fn sample_rsp() -> KeyRsp {
        KeyRsp {
            channel: "#x".into(),
            pubkey: [12; 32],
            ephemeral_pub: [4; 32],
            wrap_nonce: [5; NONCE_LEN],
            wrap_ct: vec![6, 7, 8, 9],
            nonce: [10; 16],
            sig: [11; 64],
        }
    }

    fn sample_rekey() -> KeyRekey {
        KeyRekey {
            channel: "#x".into(),
            pubkey: [13; 32],
            eph_pub: [14; 32],
            wrap_nonce: [15; NONCE_LEN],
            wrap_ct: vec![16, 17, 18, 19, 20],
            nonce: [21; 16],
            sig: [22; 64],
        }
    }

    #[test]
    fn keyreq_roundtrip() {
        let req = sample_req();
        let enc = encode_keyreq(&req);
        let parsed = parse(&enc).unwrap().unwrap();
        match parsed {
            HandshakeMsg::Req(r) => {
                assert_eq!(r.channel, req.channel);
                assert_eq!(r.pubkey, req.pubkey);
                assert_eq!(r.eph_x25519, req.eph_x25519);
                assert_eq!(r.nonce, req.nonce);
                assert_eq!(r.sig, req.sig);
            }
            HandshakeMsg::Rsp(_) | HandshakeMsg::Rekey(_) => panic!("expected Req"),
        }
    }

    #[test]
    fn keyrsp_roundtrip() {
        let rsp = sample_rsp();
        let enc = encode_keyrsp(&rsp);
        let parsed = parse(&enc).unwrap().unwrap();
        match parsed {
            HandshakeMsg::Rsp(r) => {
                assert_eq!(r.channel, rsp.channel);
                assert_eq!(r.pubkey, rsp.pubkey);
                assert_eq!(r.ephemeral_pub, rsp.ephemeral_pub);
                assert_eq!(r.wrap_nonce, rsp.wrap_nonce);
                assert_eq!(r.wrap_ct, rsp.wrap_ct);
                assert_eq!(r.nonce, rsp.nonce);
                assert_eq!(r.sig, rsp.sig);
            }
            HandshakeMsg::Req(_) | HandshakeMsg::Rekey(_) => panic!("expected Rsp"),
        }
    }

    #[test]
    fn keyrekey_roundtrip() {
        let rk = sample_rekey();
        let enc = encode_keyrekey(&rk);
        let parsed = parse(&enc).unwrap().unwrap();
        match parsed {
            HandshakeMsg::Rekey(r) => {
                assert_eq!(r.channel, rk.channel);
                assert_eq!(r.pubkey, rk.pubkey);
                assert_eq!(r.eph_pub, rk.eph_pub);
                assert_eq!(r.wrap_nonce, rk.wrap_nonce);
                assert_eq!(r.wrap_ct, rk.wrap_ct);
                assert_eq!(r.nonce, rk.nonce);
                assert_eq!(r.sig, rk.sig);
            }
            HandshakeMsg::Req(_) | HandshakeMsg::Rsp(_) => panic!("expected Rekey"),
        }
    }

    #[test]
    fn keyrekey_sig_payload_binds_eph_and_ct() {
        let p1 = signed_keyrekey_payload("#x", &[1; 32], &[2; 32], &[3; NONCE_LEN], &[4, 5], &[6; 16]);
        let p2 = signed_keyrekey_payload("#x", &[1; 32], &[9; 32], &[3; NONCE_LEN], &[4, 5], &[6; 16]);
        let p3 = signed_keyrekey_payload("#x", &[1; 32], &[2; 32], &[3; NONCE_LEN], &[4, 6], &[6; 16]);
        assert_ne!(p1, p2);
        assert_ne!(p1, p3);
    }

    #[test]
    fn parse_non_rpee2e_returns_none() {
        assert!(parse("SOMETHING ELSE").unwrap().is_none());
        assert!(parse("").unwrap().is_none());
    }

    #[test]
    fn parse_rejects_unknown_version() {
        let line = format!(
            "{CTCP_TAG} KEYREQ v=9 chan=#x pub={p} eph={e} nonce={n} sig={s}",
            p = hex::encode([0u8; 32]),
            e = hex::encode([0u8; 32]),
            n = hex::encode([0u8; 16]),
            s = hex::encode([0u8; 64]),
        );
        assert!(parse(&line).is_err());
    }

    #[test]
    fn rate_limiter_blocks_within_window() {
        let mut rl = RateLimiter::new();
        assert!(rl.allow_outgoing("~bob@host"));
        assert!(!rl.allow_outgoing("~bob@host"));
        assert!(rl.allow_outgoing("~alice@host"));
    }

    #[test]
    fn allow_incoming_permits_first_three_then_backoffs() {
        let mut rl = RateLimiter::new();
        // The first three arrivals inside the 60s window are accepted.
        assert!(rl.allow_incoming("~bob@host"));
        assert!(rl.allow_incoming("~bob@host"));
        assert!(rl.allow_incoming("~bob@host"));
        // The fourth tips the bucket into backoff — rejected AND an
        // `INCOMING_BACKOFF` timeout is installed.
        assert!(!rl.allow_incoming("~bob@host"));
        // Every subsequent arrival while the backoff is live is rejected
        // without consulting the sliding window at all.
        assert!(!rl.allow_incoming("~bob@host"));
        assert!(!rl.allow_incoming("~bob@host"));
        // Sanity: the bucket records the backoff_until timestamp.
        let bucket = rl.incoming.get("~bob@host").expect("bucket present");
        assert!(bucket.backoff_until.is_some());
    }

    #[test]
    fn allow_incoming_backoff_expires_after_window() {
        let mut rl = RateLimiter::new();
        // Fill the window and trip the backoff.
        assert!(rl.allow_incoming("~bob@host"));
        assert!(rl.allow_incoming("~bob@host"));
        assert!(rl.allow_incoming("~bob@host"));
        assert!(!rl.allow_incoming("~bob@host"));
        // Simulate the 5-minute backoff elapsing without actually sleeping.
        rl.force_expire_backoff("~bob@host");
        // Once the backoff has expired the bucket accepts the next three
        // KEYREQs again — we're back to the fresh-window state.
        assert!(rl.allow_incoming("~bob@host"));
        assert!(rl.allow_incoming("~bob@host"));
        assert!(rl.allow_incoming("~bob@host"));
        // And the fourth trips the next backoff cycle.
        assert!(!rl.allow_incoming("~bob@host"));
    }

    #[test]
    fn allow_incoming_independent_per_peer() {
        let mut rl = RateLimiter::new();
        // Bob fills his bucket and lands in backoff.
        assert!(rl.allow_incoming("~bob@host"));
        assert!(rl.allow_incoming("~bob@host"));
        assert!(rl.allow_incoming("~bob@host"));
        assert!(!rl.allow_incoming("~bob@host"));
        // Alice's bucket is independent — she still has her full quota.
        assert!(rl.allow_incoming("~alice@host"));
        assert!(rl.allow_incoming("~alice@host"));
        assert!(rl.allow_incoming("~alice@host"));
        assert!(!rl.allow_incoming("~alice@host"));
        // Bob still blocked.
        assert!(!rl.allow_incoming("~bob@host"));
    }

    #[test]
    fn keyreq_sig_payload_binds_eph_x25519() {
        let p1 = signed_keyreq_payload("#x", &[1; 32], &[9; 32], &[2; 16]);
        let p2 = signed_keyreq_payload("#x", &[1; 32], &[8; 32], &[2; 16]);
        // Changing only the ephemeral X25519 must change the signed payload;
        // otherwise a MitM could swap it without invalidating the Ed25519 sig.
        assert_ne!(p1, p2);
    }
}

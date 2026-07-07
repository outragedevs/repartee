//! SQLite keyring operations for RPE2E.
//!
//! The keyring is a thin CRUD layer over the existing `rusqlite::Connection`
//! owned by the top-level `Storage`. It exposes typed records for each of the
//! six `e2e_*` tables created by `storage::db::create_schema` (identity,
//! peers, outgoing sessions, incoming sessions, channel config, autotrust).
//!
//! The `Keyring` clones the `Arc<Mutex<Connection>>` so the same connection is
//! shared with the rest of the app — there is no second database file.

use std::sync::{Arc, Mutex};

use aes_gcm::{Aes256Gcm, Key};
use rusqlite::{Connection, OptionalExtension, params};

use crate::e2e::crypto::aead::SessionKey;
use crate::e2e::crypto::fingerprint::Fingerprint;
use crate::e2e::error::Result;

/// Trust status of a peer/session. Stored as lowercase text in SQLite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustStatus {
    Pending,
    Trusted,
    Revoked,
}

impl TrustStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Trusted => "trusted",
            Self::Revoked => "revoked",
        }
    }

    /// Parse from the stored text form. Anything unknown falls back to
    /// `Pending` — the safest default for an unknown peer.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s {
            "trusted" => Self::Trusted,
            "revoked" => Self::Revoked,
            _ => Self::Pending,
        }
    }
}

/// Channel-level encryption mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelMode {
    /// Accept any incoming KEYREQ and immediately trust the peer (TOFU).
    AutoAccept,
    /// Store peer as pending until explicit `/e2e accept`.
    Normal,
    /// Like normal but suppresses UI prompts; unknown peers are silently
    /// dropped until explicit `/e2e accept`.
    Quiet,
}

impl ChannelMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AutoAccept => "auto-accept",
            Self::Normal => "normal",
            Self::Quiet => "quiet",
        }
    }

    /// Parse from the stored text form. `"auto"` is accepted as an alias for
    /// `"auto-accept"`. Anything else collapses to `Normal`.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s {
            "auto-accept" | "auto" => Self::AutoAccept,
            "quiet" => Self::Quiet,
            _ => Self::Normal,
        }
    }
}

/// A known remote peer, identified by fingerprint of their Ed25519 pubkey.
#[derive(Debug, Clone)]
pub struct PeerRecord {
    pub fingerprint: Fingerprint,
    pub pubkey: [u8; 32],
    pub last_handle: Option<String>,
    pub last_nick: Option<String>,
    pub first_seen: i64,
    pub last_seen: i64,
    pub global_status: TrustStatus,
}

/// A peer's session key for a specific channel (we decrypt their messages
/// with this key).
#[derive(Debug, Clone)]
pub struct IncomingSession {
    pub handle: String,
    pub channel: String,
    pub fingerprint: Fingerprint,
    pub sk: SessionKey,
    pub status: TrustStatus,
    pub created_at: i64,
}

/// Our own session key for a channel (we encrypt outgoing messages with
/// this). `pending_rotation` triggers a lazy re-keying on the next send.
#[derive(Debug, Clone)]
pub struct OutgoingSession {
    pub channel: String,
    pub sk: SessionKey,
    pub created_at: i64,
    pub pending_rotation: bool,
}

/// Per-channel encryption config.
#[derive(Debug, Clone)]
pub struct ChannelConfig {
    pub channel: String,
    pub enabled: bool,
    pub mode: ChannelMode,
}

/// Keyring handle. Cloning only clones the `Arc`; the underlying
/// `Connection` is shared.
#[derive(Debug, Clone)]
pub struct Keyring {
    db: Arc<Mutex<Connection>>,
    secret_key: Option<Key<Aes256Gcm>>,
    /// Labels of the servers currently configured, shared across clones.
    /// Gates the renamed-label heal in [`Self::get_channel_config`]: a
    /// scoped sibling row is only adopted when its label is NOT configured
    /// anymore (a true rename signal) — never when the other network still
    /// exists (cross-network isolation). Empty (e.g. tests) disables the
    /// heal entirely.
    configured_networks: Arc<std::sync::RwLock<std::collections::HashSet<String>>>,
}

impl Keyring {
    /// Construct a keyring that shares the given SQLite connection.
    #[must_use]
    pub fn new(db: Arc<Mutex<Connection>>) -> Self {
        Self {
            db,
            secret_key: None,
            configured_networks: Arc::default(),
        }
    }

    pub fn new_encrypted(db: Arc<Mutex<Connection>>) -> Result<Self> {
        let key_hex = crate::storage::crypto::load_or_create_keyring_key()
            .map_err(crate::e2e::error::E2eError::Keyring)?;
        let secret_key = crate::storage::crypto::import_key(&key_hex)
            .map_err(crate::e2e::error::E2eError::Keyring)?;
        Ok(Self {
            db,
            secret_key: Some(secret_key),
            configured_networks: Arc::default(),
        })
    }

    fn encode_secret(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        self.secret_key.as_ref().map_or_else(
            || Ok(bytes.to_vec()),
            |key| {
                crate::storage::crypto::encrypt_bytes(bytes, key)
                    .map_err(crate::e2e::error::E2eError::Keyring)
            },
        )
    }

    fn decode_secret<const N: usize>(&self, bytes: &[u8], field: &str) -> Result<[u8; N]> {
        let decoded = match self.secret_key.as_ref() {
            Some(key) if bytes.len() != N => crate::storage::crypto::decrypt_bytes(bytes, key)
                .map_err(crate::e2e::error::E2eError::Keyring)?,
            _ => bytes.to_vec(),
        };
        if decoded.len() != N {
            return Err(crate::e2e::error::E2eError::Keyring(format!(
                "{field} has unexpected length {}",
                decoded.len()
            )));
        }
        let mut out = [0u8; N];
        out.copy_from_slice(&decoded);
        Ok(out)
    }

    pub fn replace_all_for_import(
        &self,
        identity: (&[u8; 32], &[u8; 32], &Fingerprint, i64),
        peers: &[PeerRecord],
        incoming: &[IncomingSession],
        outgoing: &[OutgoingSession],
        channels: &[ChannelConfig],
        autotrust: &[(String, String, i64)],
    ) -> Result<()> {
        let mut conn = self.db.lock().expect("keyring mutex poisoned");
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM e2e_outgoing_recipients", [])?;
        tx.execute("DELETE FROM e2e_autotrust", [])?;
        tx.execute("DELETE FROM e2e_channel_config", [])?;
        tx.execute("DELETE FROM e2e_outgoing_sessions", [])?;
        tx.execute("DELETE FROM e2e_incoming_sessions", [])?;
        tx.execute("DELETE FROM e2e_peers", [])?;
        // The DM handle cache is per-(network, nick) resolution state for the
        // OLD keyring; the snapshot carries no network, so it can't be rebuilt.
        // Clear it so an imported keyring can't key a DM under a stale handle
        // before the peer speaks — it re-populates observationally.
        tx.execute("DELETE FROM e2e_dm_handle_cache", [])?;
        tx.execute("DELETE FROM e2e_identity", [])?;

        let (pubkey, privkey, fingerprint, created_at) = identity;
        let enc_privkey = self.encode_secret(privkey)?;
        tx.execute(
            "INSERT INTO e2e_identity (id, pubkey, privkey, fingerprint, created_at)
             VALUES (1, ?1, ?2, ?3, ?4)",
            params![
                pubkey.as_slice(),
                enc_privkey,
                fingerprint.as_slice(),
                created_at
            ],
        )?;

        for rec in peers {
            tx.execute(
                "INSERT INTO e2e_peers
                    (fingerprint, pubkey, last_handle, last_nick, first_seen, last_seen, global_status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    rec.fingerprint.as_slice(),
                    rec.pubkey.as_slice(),
                    rec.last_handle,
                    rec.last_nick,
                    rec.first_seen,
                    rec.last_seen,
                    rec.global_status.as_str(),
                ],
            )?;
        }

        for sess in incoming {
            let enc_sk = self.encode_secret(&sess.sk)?;
            tx.execute(
                "INSERT INTO e2e_incoming_sessions
                    (handle, channel, fingerprint, sk, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    sess.handle,
                    sess.channel,
                    sess.fingerprint.as_slice(),
                    enc_sk,
                    sess.status.as_str(),
                    sess.created_at,
                ],
            )?;
        }

        for sess in outgoing {
            let enc_sk = self.encode_secret(&sess.sk)?;
            tx.execute(
                "INSERT INTO e2e_outgoing_sessions
                    (channel, sk, created_at, pending_rotation)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    sess.channel,
                    enc_sk,
                    sess.created_at,
                    i64::from(sess.pending_rotation),
                ],
            )?;
        }

        for cfg in channels {
            tx.execute(
                "INSERT INTO e2e_channel_config (channel, enabled, mode)
                 VALUES (?1, ?2, ?3)",
                params![cfg.channel, i64::from(cfg.enabled), cfg.mode.as_str()],
            )?;
        }

        for (scope, handle_pattern, created_at) in autotrust {
            tx.execute(
                "INSERT INTO e2e_autotrust (scope, handle_pattern, created_at)
                 VALUES (?1, ?2, ?3)",
                params![scope, handle_pattern, created_at],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    // ---------- identity ----------

    /// Persist (or replace) the local long-term identity keypair. There is at
    /// most one row in `e2e_identity` (enforced by the CHECK constraint on
    /// `id = 1`).
    pub fn save_identity(
        &self,
        pubkey: &[u8; 32],
        privkey: &[u8; 32],
        fingerprint: &Fingerprint,
        created_at: i64,
    ) -> Result<()> {
        let enc_privkey = self.encode_secret(privkey)?;
        let conn = self.db.lock().expect("keyring mutex poisoned");
        conn.execute(
            "INSERT OR REPLACE INTO e2e_identity (id, pubkey, privkey, fingerprint, created_at)
             VALUES (1, ?1, ?2, ?3, ?4)",
            params![
                pubkey.as_slice(),
                enc_privkey,
                fingerprint.as_slice(),
                created_at
            ],
        )?;
        Ok(())
    }

    /// Return `Ok(None)` if no identity has been generated yet.
    pub fn load_identity(&self) -> Result<Option<([u8; 32], [u8; 32], Fingerprint, i64)>> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let row: Option<(Vec<u8>, Vec<u8>, Vec<u8>, i64)> = conn
            .query_row(
                "SELECT pubkey, privkey, fingerprint, created_at FROM e2e_identity WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((pk, sk, fp, ts)) = row else {
            return Ok(None);
        };
        if pk.len() != 32 || fp.len() != 16 {
            return Err(crate::e2e::error::E2eError::Keyring(format!(
                "e2e_identity row has unexpected blob lengths (pk={}, fp={})",
                pk.len(),
                fp.len()
            )));
        }
        let mut pk_arr = [0u8; 32];
        let mut fp_arr = [0u8; 16];
        pk_arr.copy_from_slice(&pk);
        fp_arr.copy_from_slice(&fp);
        let sk_arr = self.decode_secret::<32>(&sk, "e2e_identity privkey")?;
        Ok(Some((pk_arr, sk_arr, fp_arr, ts)))
    }

    // ---------- peers ----------

    /// Insert or update a peer by fingerprint. Existing rows have their
    /// `last_handle`, `last_nick`, `last_seen`, and `global_status` refreshed;
    /// `first_seen` is preserved.
    pub fn upsert_peer(&self, rec: &PeerRecord) -> Result<()> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        conn.execute(
            "INSERT INTO e2e_peers
                (fingerprint, pubkey, last_handle, last_nick, first_seen, last_seen, global_status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(fingerprint) DO UPDATE SET
                last_handle = excluded.last_handle,
                last_nick = excluded.last_nick,
                last_seen = excluded.last_seen,
                global_status = excluded.global_status",
            params![
                rec.fingerprint.as_slice(),
                rec.pubkey.as_slice(),
                rec.last_handle,
                rec.last_nick,
                rec.first_seen,
                rec.last_seen,
                rec.global_status.as_str(),
            ],
        )?;
        Ok(())
    }

    pub fn get_peer_by_fingerprint(&self, fp: &Fingerprint) -> Result<Option<PeerRecord>> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let row: Option<(Vec<u8>, Option<String>, Option<String>, i64, i64, String)> = conn
            .query_row(
                "SELECT pubkey, last_handle, last_nick, first_seen, last_seen, global_status
                 FROM e2e_peers WHERE fingerprint = ?1",
                params![fp.as_slice()],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((pk, handle, nick, first, last, status)) = row else {
            return Ok(None);
        };
        if pk.len() != 32 {
            return Err(crate::e2e::error::E2eError::Keyring(format!(
                "e2e_peers row pubkey has unexpected length {}",
                pk.len()
            )));
        }
        let mut pk_arr = [0u8; 32];
        pk_arr.copy_from_slice(&pk);
        Ok(Some(PeerRecord {
            fingerprint: *fp,
            pubkey: pk_arr,
            last_handle: handle,
            last_nick: nick,
            first_seen: first,
            last_seen: last,
            global_status: TrustStatus::parse(&status),
        }))
    }

    /// Find a peer by their last known handle (`ident@host`). This is the
    /// reverse lookup used for the `(known fingerprint, new handle)` warning
    /// case in TOFU classification. Returns `None` if no row matches. If
    /// multiple peer rows share the same `last_handle` (theoretically
    /// possible if two different identities lived under the same host at
    /// different times), the most recently seen wins.
    pub fn get_peer_by_handle(&self, handle: &str) -> Result<Option<PeerRecord>> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let row: Option<(Vec<u8>, Vec<u8>, Option<String>, Option<String>, i64, i64, String)> =
            conn.query_row(
                "SELECT fingerprint, pubkey, last_handle, last_nick, first_seen, last_seen, global_status
                 FROM e2e_peers
                 WHERE last_handle = ?1
                 ORDER BY last_seen DESC
                 LIMIT 1",
                params![handle],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((fp, pk, last_handle, last_nick, first, last, status)) = row else {
            return Ok(None);
        };
        if fp.len() != 16 || pk.len() != 32 {
            return Err(crate::e2e::error::E2eError::Keyring(format!(
                "e2e_peers row has unexpected blob lengths (fp={}, pk={})",
                fp.len(),
                pk.len()
            )));
        }
        let mut fp_arr = [0u8; 16];
        let mut pk_arr = [0u8; 32];
        fp_arr.copy_from_slice(&fp);
        pk_arr.copy_from_slice(&pk);
        Ok(Some(PeerRecord {
            fingerprint: fp_arr,
            pubkey: pk_arr,
            last_handle,
            last_nick,
            first_seen: first,
            last_seen: last,
            global_status: TrustStatus::parse(&status),
        }))
    }

    // ---------- outgoing sessions ----------

    pub fn set_outgoing_session(
        &self,
        channel: &str,
        sk: &SessionKey,
        created_at: i64,
    ) -> Result<()> {
        let enc_sk = self.encode_secret(sk)?;
        let conn = self.db.lock().expect("keyring mutex poisoned");
        conn.execute(
            "INSERT OR REPLACE INTO e2e_outgoing_sessions
                (channel, sk, created_at, pending_rotation)
             VALUES (?1, ?2, ?3, 0)",
            params![channel, enc_sk, created_at],
        )?;
        Ok(())
    }

    pub fn get_outgoing_session(&self, channel: &str) -> Result<Option<OutgoingSession>> {
        if let Some(sess) = self.get_outgoing_session_exact(channel)? {
            return Ok(Some(sess));
        }
        // Legacy-row fallback (see get_channel_config). Critical here: a
        // scoped miss that generated a FRESH key would look like a rotation
        // without a REKEY — every pre-upgrade peer would fail AEAD.
        self.legacy_fallback(channel)
            .map_or_else(|| Ok(None), |wire| self.get_outgoing_session_exact(wire))
    }

    fn get_outgoing_session_exact(&self, channel: &str) -> Result<Option<OutgoingSession>> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let row: Option<(Vec<u8>, i64, i64)> = conn
            .query_row(
                "SELECT sk, created_at, pending_rotation
                 FROM e2e_outgoing_sessions WHERE channel = ?1",
                params![channel],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((sk, ts, pr)) = row else {
            return Ok(None);
        };
        let k = self.decode_secret::<32>(&sk, "e2e_outgoing_sessions sk")?;
        Ok(Some(OutgoingSession {
            channel: channel.to_string(),
            sk: k,
            created_at: ts,
            pending_rotation: pr != 0,
        }))
    }

    pub fn mark_outgoing_pending_rotation(&self, channel: &str) -> Result<()> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        // Mutations hit the legacy unscoped row too: before the first scoped
        // write, the read fallback serves the legacy row — a rotate/revoke
        // that only touched the scoped key would silently not apply.
        conn.execute(
            "UPDATE e2e_outgoing_sessions SET pending_rotation = 1 WHERE channel = ?1 OR channel = ?2",
            params![channel, crate::e2e::wire_context(channel)],
        )?;
        Ok(())
    }

    pub fn clear_outgoing_pending_rotation(&self, channel: &str) -> Result<()> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        conn.execute(
            "UPDATE e2e_outgoing_sessions SET pending_rotation = 0 WHERE channel = ?1 OR channel = ?2",
            params![channel, crate::e2e::wire_context(channel)],
        )?;
        Ok(())
    }

    // ---------- incoming sessions ----------

    pub fn set_incoming_session(&self, s: &IncomingSession) -> Result<()> {
        let enc_sk = self.encode_secret(&s.sk)?;
        let conn = self.db.lock().expect("keyring mutex poisoned");
        conn.execute(
            "INSERT OR REPLACE INTO e2e_incoming_sessions
                (handle, channel, fingerprint, sk, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                s.handle,
                s.channel,
                s.fingerprint.as_slice(),
                enc_sk,
                s.status.as_str(),
                s.created_at,
            ],
        )?;
        Ok(())
    }

    /// Install an incoming session under strict TOFU semantics. If a row
    /// already exists for the same `(handle, channel)` with a DIFFERENT
    /// fingerprint, this returns `E2eError::HandleMismatch` and leaves the
    /// existing row untouched — callers surface that as a TOFU warning and
    /// require `/e2e reverify` to accept the new key.
    ///
    /// Idempotent refresh (same fingerprint) is allowed: the row is updated
    /// in place, preserving `(handle, channel)` as the logical identity.
    ///
    /// This is the method the handshake consumer uses. The plain
    /// `set_incoming_session` remains for explicit-override paths
    /// (`/e2e reverify`, import, tests).
    pub fn install_incoming_session_strict(&self, s: &IncomingSession) -> Result<()> {
        let enc_sk = self.encode_secret(&s.sk)?;
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let mut existing: Option<(Vec<u8>, Vec<u8>, String)> = conn
            .query_row(
                "SELECT fingerprint, sk, status FROM e2e_incoming_sessions
                 WHERE handle = ?1 AND channel = ?2",
                params![s.handle, s.channel],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        // Upgraded keyring: the established session may still live under the
        // legacy unscoped row. The FIRST scoped install (post-upgrade REKEY)
        // must consult it too, or that install skips the TOFU
        // fingerprint-continuity check and drops the superseded key that the
        // prev-key grace window exists to retain.
        if existing.is_none()
            && let Some(wire) = self.legacy_fallback(&s.channel)
        {
            existing = conn
                .query_row(
                    "SELECT fingerprint, sk, status FROM e2e_incoming_sessions
                     WHERE handle = ?1 AND channel = ?2",
                    params![s.handle, wire],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
        }
        if let Some((existing_fp, _, _)) = &existing
            && existing_fp.as_slice() != s.fingerprint.as_slice()
        {
            return Err(crate::e2e::error::E2eError::HandleMismatch {
                expected: format!("fp={}", hex::encode(existing_fp)),
                got: format!("fp={}", hex::encode(s.fingerprint)),
            });
        }
        // Retain the key this install supersedes: a REKEY NOTICE can overtake
        // PRIVMSG ciphertext already sent under the old key, and without the
        // previous key those in-flight lines fail AEAD until a manual
        // re-handshake. Only a Trusted, non-placeholder key that actually
        // changed is worth keeping; `prev_created_at` records the replacement
        // time so decrypt can enforce the grace window
        // (`manager::REKEY_PREV_KEY_GRACE_SECS`). The blob is copied as
        // stored — already encrypted when the keyring is.
        let prev: Option<(Vec<u8>, i64)> = existing.and_then(|(_, old_enc, old_status)| {
            if old_status != TrustStatus::Trusted.as_str() {
                return None;
            }
            let old_sk = self
                .decode_secret::<32>(&old_enc, "e2e_incoming_sessions sk")
                .ok()?;
            if old_sk == s.sk || old_sk == [0u8; 32] {
                return None;
            }
            Some((old_enc, now_unix()))
        });
        conn.execute(
            "INSERT OR REPLACE INTO e2e_incoming_sessions
                (handle, channel, fingerprint, sk, status, created_at, prev_sk, prev_created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                s.handle,
                s.channel,
                s.fingerprint.as_slice(),
                enc_sk,
                s.status.as_str(),
                s.created_at,
                prev.as_ref().map(|(blob, _)| blob.as_slice()),
                prev.as_ref().map(|(_, at)| *at),
            ],
        )?;
        Ok(())
    }

    /// The previous session key for `(handle, channel)` plus the unix time it
    /// was superseded, when one is retained. Decrypt-side reorder tolerance
    /// only — see [`Self::install_incoming_session_strict`].
    pub fn get_incoming_prev_key(
        &self,
        handle: &str,
        channel: &str,
    ) -> Result<Option<(SessionKey, i64)>> {
        if let Some(prev) = self.get_incoming_prev_key_exact(handle, channel)? {
            return Ok(Some(prev));
        }
        self.legacy_fallback(channel).map_or_else(
            || Ok(None),
            |wire| self.get_incoming_prev_key_exact(handle, wire),
        )
    }

    fn get_incoming_prev_key_exact(
        &self,
        handle: &str,
        channel: &str,
    ) -> Result<Option<(SessionKey, i64)>> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let row: Option<(Option<Vec<u8>>, Option<i64>)> = conn
            .query_row(
                "SELECT prev_sk, prev_created_at FROM e2e_incoming_sessions
                 WHERE handle = ?1 AND channel = ?2",
                params![handle, channel],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((Some(enc), Some(replaced_at))) = row else {
            return Ok(None);
        };
        let sk = self.decode_secret::<32>(&enc, "e2e_incoming_sessions prev_sk")?;
        Ok(Some((sk, replaced_at)))
    }

    /// Record a REKEY nonce as consumed. Returns `false` when the exact nonce
    /// was already seen for this `(fingerprint, channel)` — a replay. The
    /// `INSERT OR IGNORE` makes check-and-record one atomic statement under
    /// the keyring lock, and the nonce is covered by the REKEY signature, so
    /// single-use enforcement here is complete replay protection without a
    /// wire-format change.
    pub fn record_rekey_nonce(
        &self,
        fingerprint: &Fingerprint,
        channel: &str,
        nonce: &[u8],
    ) -> Result<bool> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO e2e_seen_rekeys (fingerprint, channel, nonce, seen_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![fingerprint.as_slice(), channel, nonce, now_unix()],
        )?;
        Ok(inserted == 1)
    }

    pub fn get_incoming_session(
        &self,
        handle: &str,
        channel: &str,
    ) -> Result<Option<IncomingSession>> {
        if let Some(sess) = self.get_incoming_session_exact(handle, channel)? {
            return Ok(Some(sess));
        }
        // Legacy-row fallback (see get_channel_config): sessions installed
        // before network scoping keep decrypting.
        self.legacy_fallback(channel).map_or_else(
            || Ok(None),
            |wire| self.get_incoming_session_exact(handle, wire),
        )
    }

    fn get_incoming_session_exact(
        &self,
        handle: &str,
        channel: &str,
    ) -> Result<Option<IncomingSession>> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let row: Option<(Vec<u8>, Vec<u8>, String, i64)> = conn
            .query_row(
                "SELECT fingerprint, sk, status, created_at
                 FROM e2e_incoming_sessions WHERE handle = ?1 AND channel = ?2",
                params![handle, channel],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((fp, sk, st, ts)) = row else {
            return Ok(None);
        };
        if fp.len() != 16 {
            return Err(crate::e2e::error::E2eError::Keyring(format!(
                "e2e_incoming_sessions row has unexpected blob lengths (fp={})",
                fp.len(),
            )));
        }
        let mut fp_arr = [0u8; 16];
        fp_arr.copy_from_slice(&fp);
        let sk_arr = self.decode_secret::<32>(&sk, "e2e_incoming_sessions sk")?;
        Ok(Some(IncomingSession {
            handle: handle.to_string(),
            channel: channel.to_string(),
            fingerprint: fp_arr,
            sk: sk_arr,
            status: TrustStatus::parse(&st),
            created_at: ts,
        }))
    }

    pub fn update_incoming_status(
        &self,
        handle: &str,
        channel: &str,
        status: TrustStatus,
    ) -> Result<()> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        // Legacy row included — see mark_outgoing_pending_rotation.
        conn.execute(
            "UPDATE e2e_incoming_sessions SET status = ?1
             WHERE handle = ?2 AND (channel = ?3 OR channel = ?4)",
            params![
                status.as_str(),
                handle,
                channel,
                crate::e2e::wire_context(channel)
            ],
        )?;
        Ok(())
    }

    pub fn delete_incoming_session(&self, handle: &str, channel: &str) -> Result<()> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        // Legacy row included — see mark_outgoing_pending_rotation.
        conn.execute(
            "DELETE FROM e2e_incoming_sessions
             WHERE handle = ?1 AND (channel = ?2 OR channel = ?3)",
            params![handle, channel, crate::e2e::wire_context(channel)],
        )?;
        Ok(())
    }

    /// Delete every incoming session row belonging to `handle` across all
    /// channels. Used by `/e2e reverify` to purge a stale identity's
    /// session footprint before upserting the new key.
    ///
    /// Returns the number of rows removed so callers can surface a
    /// human-readable summary.
    pub fn delete_incoming_sessions_for_handle(&self, handle: &str) -> Result<usize> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let n = conn.execute(
            "DELETE FROM e2e_incoming_sessions WHERE handle = ?1",
            params![handle],
        )?;
        Ok(n)
    }

    /// Delete every outgoing-recipient row referencing `handle`. Mirrors
    /// `delete_incoming_sessions_for_handle` for the reverse direction
    /// (we stop pushing our outgoing session key to this identity until
    /// it re-handshakes under the new pubkey). Returns the number of
    /// rows removed.
    pub fn delete_outgoing_recipients_for_handle(&self, handle: &str) -> Result<usize> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let n = conn.execute(
            "DELETE FROM e2e_outgoing_recipients WHERE handle = ?1",
            params![handle],
        )?;
        Ok(n)
    }

    /// Delete the peer row identified by `fp`. Used by `/e2e reverify`
    /// to evict a stale identity before upserting the newly-consented
    /// pubkey. Intentionally does NOT cascade to incoming-sessions or
    /// outgoing-recipients — the reverify path deletes those explicitly
    /// via `delete_incoming_sessions_for_handle` and
    /// `delete_outgoing_recipients_for_handle` so the two cleanups are
    /// visible side-by-side.
    pub fn delete_peer_by_fingerprint(&self, fp: &Fingerprint) -> Result<()> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        conn.execute(
            "DELETE FROM e2e_peers WHERE fingerprint = ?1",
            params![fp.as_slice()],
        )?;
        Ok(())
    }

    /// List all incoming sessions on `channel` whose status is `trusted`.
    pub fn list_trusted_peers_for_channel(&self, channel: &str) -> Result<Vec<IncomingSession>> {
        let mut peers = self.list_trusted_peers_for_channel_exact(channel)?;
        // UNION with the legacy unscoped rows — same transition rule as
        // list_outgoing_recipients: dedup by handle, the scoped row wins.
        if let Some(wire) = self.legacy_fallback(channel) {
            for sess in self.list_trusted_peers_for_channel_exact(wire)? {
                if !peers.iter().any(|p| p.handle == sess.handle) {
                    peers.push(sess);
                }
            }
        }
        Ok(peers)
    }

    fn list_trusted_peers_for_channel_exact(
        &self,
        channel: &str,
    ) -> Result<Vec<IncomingSession>> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT handle, fingerprint, sk, status, created_at
             FROM e2e_incoming_sessions
             WHERE channel = ?1 AND status = 'trusted'",
        )?;
        let rows = stmt.query_map(params![channel], |r| {
            let handle: String = r.get(0)?;
            let fp: Vec<u8> = r.get(1)?;
            let sk: Vec<u8> = r.get(2)?;
            let st: String = r.get(3)?;
            let ts: i64 = r.get(4)?;
            Ok((handle, fp, sk, st, ts))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (handle, fp, sk, st, ts) = row?;
            if fp.len() != 16 {
                return Err(crate::e2e::error::E2eError::Keyring(format!(
                    "e2e_incoming_sessions row has unexpected blob lengths (fp={})",
                    fp.len(),
                )));
            }
            let mut fp_arr = [0u8; 16];
            fp_arr.copy_from_slice(&fp);
            let sk_arr = self.decode_secret::<32>(&sk, "e2e_incoming_sessions sk")?;
            out.push(IncomingSession {
                handle,
                channel: channel.to_string(),
                fingerprint: fp_arr,
                sk: sk_arr,
                status: TrustStatus::parse(&st),
                created_at: ts,
            });
        }
        Ok(out)
    }

    // ---------- channel config ----------

    pub fn set_channel_config(&self, cfg: &ChannelConfig) -> Result<()> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        conn.execute(
            "INSERT OR REPLACE INTO e2e_channel_config (channel, enabled, mode)
             VALUES (?1, ?2, ?3)",
            params![cfg.channel, i64::from(cfg.enabled), cfg.mode.as_str()],
        )?;
        Ok(())
    }

    pub fn get_channel_config(&self, channel: &str) -> Result<Option<ChannelConfig>> {
        if let Some(cfg) = self.get_channel_config_exact(channel)? {
            return Ok(Some(cfg));
        }
        // Network-scoped read over a pre-scoping database: fall back to the
        // legacy unscoped row so existing setups survive the upgrade. A
        // scoped row always wins when present (all writes are scoped), and
        // the fallback is denied on multi-network configs — see
        // `legacy_fallback` (the renamed-label heal below stays available
        // regardless: it reads scoped siblings, not the legacy row).
        if let Some(wire) = legacy_wire_fallback(channel) {
            if self.legacy_adoption_allowed()
                && let Some(cfg) = self.get_channel_config_exact(wire)?
            {
                return Ok(Some(cfg));
            }
            // Renamed-label heal: a config.toml `label` change orphans every
            // scoped row, and the enabled check is the FAIL-OPEN point — a
            // miss here sends an explicitly-encrypted conversation as
            // plaintext. When exactly ONE other network's row shares the
            // wire part, the rename is unambiguous and its config is used
            // (erring toward encryption); two or more candidates keep the
            // cross-network isolation and return nothing. Sessions do NOT
            // heal — the send generates a fresh key and peers re-handshake,
            // which is recoverable, unlike a plaintext send.
            if let Some(healed) = self.unique_scoped_config_sibling(wire)? {
                // Adopt the sibling ONLY when its label vanished from the
                // config (true rename); a still-configured network keeps its
                // rows to itself — that is the isolation this scoping exists
                // for. An empty configured set (tests, pre-init reads)
                // disables the heal.
                let healed_net = healed
                    .split_once(crate::e2e::CONTEXT_NET_SEPARATOR)
                    .map(|(net, _)| net.to_string())
                    .unwrap_or_default();
                let configured = self
                    .configured_networks
                    .read()
                    .expect("configured networks lock poisoned");
                if !configured.is_empty() && !configured.contains(&healed_net) {
                    tracing::warn!(
                        "e2e: config for {channel} healed from a renamed network's row — \
                         re-run /e2e on to migrate it to the current label"
                    );
                    return self.get_channel_config_exact(&healed);
                }
            }
        }
        Ok(None)
    }

    /// Record the labels of the currently configured servers — see the
    /// `configured_networks` field. Called once at startup.
    pub fn set_configured_networks<I: IntoIterator<Item = String>>(&self, labels: I) {
        let mut guard = self
            .configured_networks
            .write()
            .expect("configured networks lock poisoned");
        *guard = labels.into_iter().collect();
    }

    /// Whether a scoped miss may consult the legacy (unscoped, pre-upgrade)
    /// row at all. With more than one configured network the legacy row has
    /// no determinable owner, and serving it to every scoped lookup would
    /// hand ONE network's keys to ANY network sharing the wire name — the
    /// exact cross-network reuse the scoping exists to prevent. Zero or one
    /// configured network is unambiguous (and startup migration via
    /// [`Self::adopt_legacy_contexts`] usually empties the legacy rows
    /// first; the read fallback then remains as a safety net).
    fn legacy_adoption_allowed(&self) -> bool {
        self.configured_networks
            .read()
            .expect("configured networks lock poisoned")
            .len()
            <= 1
    }

    /// The legacy unscoped row key to consult for a scoped `channel`, or
    /// `None` when `channel` is already unscoped or legacy adoption is
    /// denied — see [`Self::legacy_adoption_allowed`].
    fn legacy_fallback<'a>(&self, channel: &'a str) -> Option<&'a str> {
        let wire = legacy_wire_fallback(channel)?;
        self.legacy_adoption_allowed().then_some(wire)
    }

    /// Migrate pre-scoping (unscoped) keyring rows to network-scoped ones.
    /// Called once at startup, after [`Self::set_configured_networks`].
    ///
    /// Ownership is resolved per context:
    /// - exactly one configured network → it owns everything (unambiguous);
    /// - otherwise a DM context (`@handle`) is attributed via the
    ///   network-keyed `e2e_dm_handle_cache`, and a channel context via the
    ///   message log's `(network, buffer)` pairs — but only when exactly one
    ///   configured network matches.
    ///
    /// Contexts that cannot be attributed are left in place and returned so
    /// the caller can warn; the read-side [`Self::legacy_fallback`] gate
    /// keeps them inert on multi-network configs (fail-closed: fresh
    /// handshakes re-establish sessions instead of reusing another
    /// network's keys). When a scoped row already exists for the same key,
    /// the scoped row wins and the legacy one is dropped.
    pub fn adopt_legacy_contexts(&self) -> Result<Vec<String>> {
        let configured: Vec<String> = {
            let guard = self
                .configured_networks
                .read()
                .expect("configured networks lock poisoned");
            guard.iter().cloned().collect()
        };
        if configured.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let legacy = Self::legacy_context_values(&conn)?;
        let mut unattributed = Vec::new();
        for wire in legacy {
            let owner = if let [only] = configured.as_slice() {
                Some(only.clone())
            } else {
                Self::attribute_legacy_context(&conn, &wire, &configured)?
            };
            match owner {
                Some(network) => Self::migrate_legacy_context(&conn, &wire, &network)?,
                None => unattributed.push(wire),
            }
        }
        Ok(unattributed)
    }

    /// Distinct unscoped context values across every context-keyed table.
    /// (`e2e_seen_rekeys` and `prev_sk` are excluded: both were introduced
    /// together with scoping, so they can never hold legacy rows.)
    fn legacy_context_values(conn: &Connection) -> Result<Vec<String>> {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT ctx FROM (
                 SELECT channel AS ctx FROM e2e_channel_config
                 UNION SELECT channel FROM e2e_outgoing_sessions
                 UNION SELECT channel FROM e2e_incoming_sessions
                 UNION SELECT channel FROM e2e_outgoing_recipients
                 UNION SELECT scope FROM e2e_autotrust WHERE scope <> 'global'
             ) WHERE instr(ctx, char(31)) = 0",
        )?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Legacy contexts still present after [`Self::adopt_legacy_contexts`] —
    /// startup warns about them on multi-network configs, where the read
    /// fallback is denied and the state is effectively dormant.
    pub fn list_legacy_contexts(&self) -> Result<Vec<String>> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        Self::legacy_context_values(&conn)
    }

    /// The single configured network that `wire` can be attributed to, or
    /// `None` when zero or several match (ambiguity keeps the row legacy).
    fn attribute_legacy_context(
        conn: &Connection,
        wire: &str,
        configured: &[String],
    ) -> Result<Option<String>> {
        let networks: Vec<String> = if let Some(handle) = wire.strip_prefix('@') {
            let mut stmt = conn
                .prepare("SELECT DISTINCT network FROM e2e_dm_handle_cache WHERE handle = ?1")?;
            stmt.query_map(params![handle], |r| r.get(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        } else if Self::messages_table_exists(conn)? {
            // The keyring shares its database with message storage in
            // production; the buffer column is stored lowercased.
            let mut stmt =
                conn.prepare("SELECT DISTINCT network FROM messages WHERE buffer = lower(?1)")?;
            stmt.query_map(params![wire], |r| r.get(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        Ok(match networks.as_slice() {
            [only] if configured.contains(only) => Some(only.clone()),
            _ => None,
        })
    }

    /// Standalone keyring databases (tests, tooling) have no message log.
    fn messages_table_exists(conn: &Connection) -> Result<bool> {
        let row: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'messages'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(row.is_some())
    }

    /// Rename every row keyed by the unscoped `wire` context to
    /// `{network}\x1F{wire}`, atomically. A rename that would collide with
    /// an existing scoped row is skipped and the legacy row deleted — every
    /// scoped write is newer than any pre-upgrade row, so scoped wins.
    fn migrate_legacy_context(conn: &Connection, wire: &str, network: &str) -> Result<()> {
        let scoped = crate::e2e::scoped_context(network, wire);
        let tx = conn.unchecked_transaction()?;
        for (table, column) in [
            ("e2e_channel_config", "channel"),
            ("e2e_outgoing_sessions", "channel"),
            ("e2e_incoming_sessions", "channel"),
            ("e2e_outgoing_recipients", "channel"),
            ("e2e_autotrust", "scope"),
        ] {
            tx.execute(
                &format!("UPDATE OR IGNORE {table} SET {column} = ?1 WHERE {column} = ?2"),
                params![scoped, wire],
            )?;
            tx.execute(
                &format!("DELETE FROM {table} WHERE {column} = ?1"),
                params![wire],
            )?;
        }
        tx.commit()?;
        tracing::info!("e2e: migrated legacy context '{wire}' to network '{network}'");
        Ok(())
    }

    /// The single scoped `e2e_channel_config` row whose wire part equals
    /// `wire`, or `None` when zero or several networks have one — see the
    /// renamed-label heal in [`Self::get_channel_config`].
    fn unique_scoped_config_sibling(&self, wire: &str) -> Result<Option<String>> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT DISTINCT channel FROM e2e_channel_config
             WHERE instr(channel, char(31)) > 0
               AND substr(channel, instr(channel, char(31)) + 1) = ?1",
        )?;
        let rows = stmt
            .query_map(params![wire], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(match rows.as_slice() {
            [only] => Some(only.clone()),
            _ => None,
        })
    }

    /// Distinct network labels embedded in scoped keyring rows (config +
    /// sessions). Startup compares them with the configured server labels
    /// and warns about orphans — a renamed `label` silently detaches every
    /// scoped row from its conversations.
    pub fn list_scoped_networks(&self) -> Result<Vec<String>> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT DISTINCT substr(channel, 1, instr(channel, char(31)) - 1) FROM (
                 SELECT channel FROM e2e_channel_config
                 UNION SELECT channel FROM e2e_outgoing_sessions
                 UNION SELECT channel FROM e2e_incoming_sessions
             ) WHERE instr(channel, char(31)) > 0",
        )?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn get_channel_config_exact(&self, channel: &str) -> Result<Option<ChannelConfig>> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let row: Option<(i64, String)> = conn
            .query_row(
                "SELECT enabled, mode FROM e2e_channel_config WHERE channel = ?1",
                params![channel],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row.map(|(en, mo)| ChannelConfig {
            channel: channel.to_string(),
            enabled: en != 0,
            mode: ChannelMode::parse(&mo),
        }))
    }

    // ---------- autotrust ----------

    pub fn add_autotrust(&self, scope: &str, handle_pattern: &str, created_at: i64) -> Result<()> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        conn.execute(
            "INSERT OR IGNORE INTO e2e_autotrust (scope, handle_pattern, created_at)
             VALUES (?1, ?2, ?3)",
            params![scope, handle_pattern, created_at],
        )?;
        Ok(())
    }

    pub fn list_autotrust(&self) -> Result<Vec<(String, String)>> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let mut stmt = conn.prepare("SELECT scope, handle_pattern FROM e2e_autotrust")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Return `true` if any autotrust rule matches `handle` for the given
    /// scope — i.e. any rule with `scope = "global"` OR `scope = channel`
    /// whose `handle_pattern` glob matches `handle` case-insensitively.
    ///
    /// The glob syntax is minimal by design: `*` matches any sequence
    /// (possibly empty), `?` matches exactly one character, everything
    /// else is a literal. No bracket expressions. This mirrors spec §7.
    pub fn autotrust_matches(&self, handle: &str, channel: &str) -> Result<bool> {
        // ?2 is the legacy unscoped scope (see get_channel_config's
        // fallback): autotrust rows written before network scoping keep
        // matching scoped lookups. Denied on multi-network configs like
        // every legacy adoption — a pre-upgrade rule for `#chan` must not
        // auto-trust peers on a DIFFERENT network's `#chan`.
        let legacy_scope = self.legacy_fallback(channel).unwrap_or(channel);
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT handle_pattern FROM e2e_autotrust
             WHERE scope = 'global' OR scope = ?1 OR scope = ?2",
        )?;
        let rows = stmt
            .query_map(params![channel, legacy_scope], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for pat in rows {
            if glob_matches_ci(&pat, handle) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn remove_autotrust(&self, pattern: &str) -> Result<()> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        conn.execute(
            "DELETE FROM e2e_autotrust WHERE handle_pattern = ?1",
            params![pattern],
        )?;
        Ok(())
    }

    // ---------- outgoing recipients (for lazy rotate distribution) ----------

    /// Record that `handle` (with Ed25519 `fingerprint`) is a recipient of
    /// our outgoing session key for `channel`. Idempotent — `PRIMARY KEY
    /// (channel, handle)` de-duplicates, and we keep the earliest
    /// `first_sent_at` so the row shows the age of the relationship.
    pub fn record_outgoing_recipient(
        &self,
        channel: &str,
        handle: &str,
        fingerprint: &Fingerprint,
        first_sent_at: i64,
    ) -> Result<()> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        conn.execute(
            "INSERT INTO e2e_outgoing_recipients
                (channel, handle, fingerprint, first_sent_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(channel, handle) DO UPDATE SET
                fingerprint = excluded.fingerprint",
            params![channel, handle, fingerprint.as_slice(), first_sent_at],
        )?;
        Ok(())
    }

    /// Remove `handle` from the recipient list for `channel`. Called on
    /// `/e2e revoke` so the subsequent lazy rotate does NOT redistribute
    /// the fresh key to the revoked peer.
    pub fn remove_outgoing_recipient(&self, channel: &str, handle: &str) -> Result<()> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        // Legacy row included — see mark_outgoing_pending_rotation.
        conn.execute(
            "DELETE FROM e2e_outgoing_recipients
             WHERE (channel = ?1 OR channel = ?3) AND handle = ?2",
            params![channel, handle, crate::e2e::wire_context(channel)],
        )?;
        Ok(())
    }

    /// Return every recipient of our outgoing session key for `channel`.
    /// The returned tuples are `(handle, fingerprint)`.
    pub fn list_outgoing_recipients(&self, channel: &str) -> Result<Vec<(String, Fingerprint)>> {
        let mut recipients = self.list_outgoing_recipients_exact(channel)?;
        // UNION with the legacy unscoped rows, not a fallback: on an upgraded
        // keyring pre-scoping peers stay recorded under the wire channel
        // while new handshakes record under the scoped one — a rotate must
        // REKEY both generations or every pre-upgrade peer is left on the
        // old key. Dedup by handle; the scoped row wins.
        if let Some(wire) = self.legacy_fallback(channel) {
            for (handle, fp) in self.list_outgoing_recipients_exact(wire)? {
                if !recipients.iter().any(|(h, _)| *h == handle) {
                    recipients.push((handle, fp));
                }
            }
        }
        Ok(recipients)
    }

    fn list_outgoing_recipients_exact(
        &self,
        channel: &str,
    ) -> Result<Vec<(String, Fingerprint)>> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT handle, fingerprint
             FROM e2e_outgoing_recipients
             WHERE channel = ?1
             ORDER BY first_sent_at ASC",
        )?;
        let rows = stmt.query_map(params![channel], |r| {
            let handle: String = r.get(0)?;
            let fp: Vec<u8> = r.get(1)?;
            Ok((handle, fp))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (handle, fp) = row?;
            if fp.len() != 16 {
                return Err(crate::e2e::error::E2eError::Keyring(format!(
                    "e2e_outgoing_recipients row fingerprint has unexpected length {}",
                    fp.len()
                )));
            }
            let mut fp_arr = [0u8; 16];
            fp_arr.copy_from_slice(&fp);
            out.push((handle, fp_arr));
        }
        Ok(out)
    }

    // ---------- full-table dump helpers (used by portable export) ----------

    /// Return every row of `e2e_peers`, regardless of trust status.
    ///
    /// Used by the JSON export path. The existing `get_peer_by_fingerprint`
    /// and `list_trusted_peers_for_channel` APIs are intentionally scoped
    /// narrower for the hot-path lookups — this one is only for the bulk
    /// dump.
    pub fn list_all_peers(&self) -> Result<Vec<PeerRecord>> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT fingerprint, pubkey, last_handle, last_nick, first_seen, last_seen, global_status
             FROM e2e_peers ORDER BY first_seen ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            let fp: Vec<u8> = r.get(0)?;
            let pk: Vec<u8> = r.get(1)?;
            let last_handle: Option<String> = r.get(2)?;
            let last_nick: Option<String> = r.get(3)?;
            let first_seen: i64 = r.get(4)?;
            let last_seen: i64 = r.get(5)?;
            let status: String = r.get(6)?;
            Ok((
                fp,
                pk,
                last_handle,
                last_nick,
                first_seen,
                last_seen,
                status,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (fp, pk, last_handle, last_nick, first_seen, last_seen, status) = row?;
            if fp.len() != 16 || pk.len() != 32 {
                return Err(crate::e2e::error::E2eError::Keyring(format!(
                    "e2e_peers row has unexpected blob lengths (fp={}, pk={})",
                    fp.len(),
                    pk.len()
                )));
            }
            let mut fp_arr = [0u8; 16];
            let mut pk_arr = [0u8; 32];
            fp_arr.copy_from_slice(&fp);
            pk_arr.copy_from_slice(&pk);
            out.push(PeerRecord {
                fingerprint: fp_arr,
                pubkey: pk_arr,
                last_handle,
                last_nick,
                first_seen,
                last_seen,
                global_status: TrustStatus::parse(&status),
            });
        }
        Ok(out)
    }

    /// Most-recent server-stamped handle (`ident@host`) for `nick` on `network`
    /// (case-insensitive nick). Resolves a DM peer's handle when the live
    /// `Buffer.peer_handle` isn't set yet (the peer hasn't spoken this session)
    /// so the `/e2e` command layer AND the encrypt path key the DM under the same
    /// `@<handle>` context — avoiding a plaintext send when `/e2e on` enabled E2E
    /// for that context.
    ///
    /// Resolution order:
    /// 1. The `(network, nick)` cache — authoritative and network-scoped, so the
    ///    same E2E identity seen on two networks keeps independent handles. This
    ///    is the source for all keyrings once a peer has been observed.
    /// 2. Legacy fallback to `e2e_peers.last_handle` by nick, for keyrings created
    ///    BEFORE the cache table existed: on upgrade the cache starts empty while
    ///    `e2e_peers` still holds last-seen handles. Without this an enabled
    ///    `@<handle>` config would be invisible here and the DM would go out in
    ///    PLAINTEXT. This fallback is network-AGNOSTIC (`e2e_peers` has no network
    ///    column), so the only imperfect case is the SAME nick being an E2E peer
    ///    on two networks while the cache hasn't been populated for `network`
    ///    yet — that yields a wrong-context but still-ENCRYPTED (never plaintext)
    ///    send that self-heals the instant the peer speaks (which writes the
    ///    network-scoped cache via `cache_dm_handle`).
    pub fn last_handle_for_nick(&self, nick: &str, network: &str) -> Result<Option<String>> {
        let cached = self.cached_dm_handle(nick, network)?;
        if cached.is_some() {
            return Ok(cached);
        }
        self.legacy_handle_for_nick(nick)
    }

    /// The network-scoped `(network, nick)` cache row ONLY — no legacy
    /// fallback. This is the source [`crate::irc::events`]' DM handle-change
    /// tracking uses to pick a MIGRATION source: the legacy fallback is
    /// network-agnostic, so treating its result as "the peer's previous
    /// context" would copy an enabled config from a same-nick peer on another
    /// network onto a stranger (see [`Self::legacy_handle_for_nick`]).
    pub fn cached_dm_handle(&self, nick: &str, network: &str) -> Result<Option<String>> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        Ok(conn
            .query_row(
                "SELECT handle FROM e2e_dm_handle_cache WHERE network = ?1 AND nick = ?2",
                params![network, nick],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Pre-cache keyring (upgrade) fallback: resolve the nick's last-seen
    /// handle from `e2e_peers` so an existing enabled `@<handle>` config is
    /// reachable instead of leaking plaintext. Network-AGNOSTIC (`e2e_peers`
    /// has no network column) — safe for the SEND path (worst case is a
    /// wrong-context but still-encrypted send that self-heals when the peer
    /// speaks), but callers must NOT trust it as a config-migration source.
    pub fn legacy_handle_for_nick(&self, nick: &str) -> Result<Option<String>> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        Ok(conn
            .query_row(
                "SELECT last_handle FROM e2e_peers
                 WHERE last_nick = ?1 COLLATE NOCASE AND last_handle IS NOT NULL
                 ORDER BY last_seen DESC LIMIT 1",
                params![nick],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Record that DM peer `nick` was seen as `handle` (`ident@host`) on
    /// `network`, so [`Self::last_handle_for_nick`] can resolve it later.
    /// Keyed by `(network, nick)` — one row per network for a given nick, so the
    /// same identity on two networks keeps independent handles. Latest write
    /// wins (a CHGHOST / reconnect updates the cached handle).
    pub fn cache_dm_handle(&self, network: &str, nick: &str, handle: &str) -> Result<()> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        conn.execute(
            "INSERT INTO e2e_dm_handle_cache (network, nick, handle) VALUES (?1, ?2, ?3)
             ON CONFLICT(network, nick) DO UPDATE SET handle = excluded.handle",
            params![network, nick, handle],
        )?;
        Ok(())
    }

    /// Carry the DM handle cache across an IRC nick change: the row keyed
    /// `(network, old_nick)` is re-keyed to `new_nick`, and the
    /// network-agnostic `e2e_peers.last_nick` hint is refreshed the same
    /// way. Without this a rename orphans the cached `ident@host`: a
    /// `/msg <new_nick>` with no live query buffer resolves no handle,
    /// misses the peer's enabled `@<handle>` config, and falls through to
    /// PLAINTEXT. A row already keyed `new_nick` (a previous holder of that
    /// nick) is replaced — the NICK event is authoritative for who owns the
    /// nick now.
    pub fn rename_dm_nick(&self, network: &str, old_nick: &str, new_nick: &str) -> Result<()> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        conn.execute(
            "UPDATE OR REPLACE e2e_dm_handle_cache SET nick = ?1
             WHERE network = ?2 AND nick = ?3",
            params![new_nick, network, old_nick],
        )?;
        // Same freshness rule as an observe: `last_nick` means "last seen
        // nick", and the rename is the newest sighting. Send-path use of
        // this hint is safe even cross-network — worst case is a
        // wrong-context but still-ENCRYPTED send (see
        // `legacy_handle_for_nick`).
        conn.execute(
            "UPDATE e2e_peers SET last_nick = ?1 WHERE last_nick = ?2 COLLATE NOCASE",
            params![new_nick, old_nick],
        )?;
        Ok(())
    }

    /// Return every row of `e2e_incoming_sessions`, across every channel.
    pub fn list_all_incoming_sessions(&self) -> Result<Vec<IncomingSession>> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT handle, channel, fingerprint, sk, status, created_at
             FROM e2e_incoming_sessions ORDER BY channel ASC, handle ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            let handle: String = r.get(0)?;
            let channel: String = r.get(1)?;
            let fp: Vec<u8> = r.get(2)?;
            let sk: Vec<u8> = r.get(3)?;
            let st: String = r.get(4)?;
            let ts: i64 = r.get(5)?;
            Ok((handle, channel, fp, sk, st, ts))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (handle, channel, fp, sk, st, ts) = row?;
            if fp.len() != 16 {
                return Err(crate::e2e::error::E2eError::Keyring(format!(
                    "e2e_incoming_sessions row has unexpected blob lengths (fp={})",
                    fp.len(),
                )));
            }
            let mut fp_arr = [0u8; 16];
            fp_arr.copy_from_slice(&fp);
            let sk_arr = self.decode_secret::<32>(&sk, "e2e_incoming_sessions sk")?;
            out.push(IncomingSession {
                handle,
                channel,
                fingerprint: fp_arr,
                sk: sk_arr,
                status: TrustStatus::parse(&st),
                created_at: ts,
            });
        }
        Ok(out)
    }

    /// Return every row of `e2e_outgoing_sessions`.
    pub fn list_all_outgoing_sessions(&self) -> Result<Vec<OutgoingSession>> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT channel, sk, created_at, pending_rotation
             FROM e2e_outgoing_sessions ORDER BY channel ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            let channel: String = r.get(0)?;
            let sk: Vec<u8> = r.get(1)?;
            let ts: i64 = r.get(2)?;
            let pr: i64 = r.get(3)?;
            Ok((channel, sk, ts, pr))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (channel, sk, ts, pr) = row?;
            let sk_arr = self.decode_secret::<32>(&sk, "e2e_outgoing_sessions sk")?;
            out.push(OutgoingSession {
                channel,
                sk: sk_arr,
                created_at: ts,
                pending_rotation: pr != 0,
            });
        }
        Ok(out)
    }

    /// Return every row of `e2e_channel_config`. Used by `/e2e autotrust`
    /// enforcement test helpers and by the portable export.
    #[allow(dead_code, reason = "hook for a future /e2e config list command")]
    pub(crate) fn get_autotrust_rules_for_scope(&self, channel: &str) -> Result<Vec<String>> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT handle_pattern FROM e2e_autotrust
             WHERE scope = 'global' OR scope = ?1
             ORDER BY id ASC",
        )?;
        let rows = stmt
            .query_map(params![channel], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Return every row of `e2e_channel_config`.
    pub fn list_all_channel_configs(&self) -> Result<Vec<ChannelConfig>> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT channel, enabled, mode
             FROM e2e_channel_config ORDER BY channel ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            let channel: String = r.get(0)?;
            let enabled: i64 = r.get(1)?;
            let mode: String = r.get(2)?;
            Ok((channel, enabled, mode))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (channel, enabled, mode) = row?;
            out.push(ChannelConfig {
                channel,
                enabled: enabled != 0,
                mode: ChannelMode::parse(&mode),
            });
        }
        Ok(out)
    }
}

/// For a network-scoped context, the legacy (wire) key to retry a read
/// under; `None` for already-unscoped contexts. See `get_channel_config`
/// for the fallback rule (scoped row wins, legacy fills the gap).
fn legacy_wire_fallback(channel: &str) -> Option<&str> {
    let wire = crate::e2e::wire_context(channel);
    (wire != channel).then_some(wire)
}

/// Unix time for keyring bookkeeping rows (`prev_created_at`, `seen_at`).
/// Same clock policy as `manager::now_unix`: a pre-epoch clock degrades to 0
/// rather than panicking — the values gate relative windows, not authenticity.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Minimal case-insensitive glob matcher used by `autotrust_matches`.
///
/// Supports:
/// - `*` — any sequence (possibly empty) of characters
/// - `?` — exactly one character
/// - everything else — literal, case-insensitive
///
/// Lowercasing is ASCII (IRC `ident@host` is always ASCII). No bracket
/// expressions, no escaping — keep the semantics sharp and the test
/// surface small. Iterative backtracking at `*` positions handles the
/// general case.
fn glob_matches_ci(pattern: &str, input: &str) -> bool {
    let pat: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    let inp: Vec<char> = input.chars().flat_map(char::to_lowercase).collect();

    // Positions into `pat` and `inp`. When we consume a `*`, remember the
    // positions so we can backtrack and advance `inp` one character.
    let mut pi = 0usize;
    let mut ii = 0usize;
    let mut star_pat: Option<usize> = None;
    let mut star_inp: usize = 0;

    while ii < inp.len() {
        if pi < pat.len() && (pat[pi] == '?' || pat[pi] == inp[ii]) {
            pi += 1;
            ii += 1;
        } else if pi < pat.len() && pat[pi] == '*' {
            star_pat = Some(pi);
            star_inp = ii;
            pi += 1;
        } else if let Some(sp) = star_pat {
            pi = sp + 1;
            star_inp += 1;
            ii = star_inp;
        } else {
            return false;
        }
    }
    // Trailing `*`s still allowed.
    while pi < pat.len() && pat[pi] == '*' {
        pi += 1;
    }
    pi == pat.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inline the CREATE statements so the test does not depend on
    /// `storage::db::open` (which also applies PRAGMAs we don't need here).
    const SCHEMA: &str = "
        CREATE TABLE e2e_identity (
            id            INTEGER PRIMARY KEY CHECK (id = 1),
            pubkey        BLOB NOT NULL,
            privkey       BLOB NOT NULL,
            fingerprint   BLOB NOT NULL,
            created_at    INTEGER NOT NULL
        );
        CREATE TABLE e2e_peers (
            fingerprint   BLOB PRIMARY KEY,
            pubkey        BLOB NOT NULL,
            last_handle   TEXT,
            last_nick     TEXT,
            first_seen    INTEGER NOT NULL,
            last_seen     INTEGER NOT NULL,
            global_status TEXT NOT NULL DEFAULT 'pending'
        );
        CREATE TABLE e2e_dm_handle_cache (
            network TEXT NOT NULL,
            nick    TEXT NOT NULL COLLATE NOCASE,
            handle  TEXT NOT NULL,
            PRIMARY KEY (network, nick)
        );
        CREATE TABLE e2e_outgoing_sessions (
            channel           TEXT PRIMARY KEY,
            sk                BLOB NOT NULL,
            created_at        INTEGER NOT NULL,
            pending_rotation  INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE e2e_incoming_sessions (
            handle           TEXT NOT NULL,
            channel          TEXT NOT NULL,
            fingerprint      BLOB NOT NULL,
            sk               BLOB NOT NULL,
            status           TEXT NOT NULL DEFAULT 'pending',
            created_at       INTEGER NOT NULL,
            prev_sk          BLOB,
            prev_created_at  INTEGER,
            PRIMARY KEY (handle, channel)
        );
        CREATE TABLE e2e_seen_rekeys (
            fingerprint  BLOB NOT NULL,
            channel      TEXT NOT NULL,
            nonce        BLOB NOT NULL,
            seen_at      INTEGER NOT NULL,
            PRIMARY KEY (fingerprint, channel, nonce)
        );
        CREATE TABLE e2e_channel_config (
            channel  TEXT PRIMARY KEY,
            enabled  INTEGER NOT NULL DEFAULT 0,
            mode     TEXT NOT NULL DEFAULT 'normal'
        );
        CREATE TABLE e2e_autotrust (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            scope           TEXT NOT NULL,
            handle_pattern  TEXT NOT NULL,
            created_at      INTEGER NOT NULL,
            UNIQUE(scope, handle_pattern)
        );
        CREATE TABLE e2e_outgoing_recipients (
            channel        TEXT NOT NULL,
            handle         TEXT NOT NULL,
            fingerprint    BLOB NOT NULL,
            first_sent_at  INTEGER NOT NULL,
            PRIMARY KEY (channel, handle)
        );
    ";

    fn open_mem() -> Keyring {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        Keyring::new(Arc::new(Mutex::new(conn)))
    }

    fn open_mem_encrypted() -> Keyring {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        let key_hex = crate::storage::crypto::generate_key_hex();
        let secret_key = crate::storage::crypto::import_key(&key_hex).unwrap();
        Keyring {
            db: Arc::new(Mutex::new(conn)),
            secret_key: Some(secret_key),
            configured_networks: Arc::default(),
        }
    }

    #[test]
    fn identity_roundtrip() {
        let kr = open_mem();
        let pk = [1u8; 32];
        let sk = [2u8; 32];
        let fp = [3u8; 16];
        kr.save_identity(&pk, &sk, &fp, 1000).unwrap();
        let (lpk, lsk, lfp, lts) = kr.load_identity().unwrap().unwrap();
        assert_eq!(lpk, pk);
        assert_eq!(lsk, sk);
        assert_eq!(lfp, fp);
        assert_eq!(lts, 1000);
    }

    #[test]
    fn identity_roundtrip_encrypted_at_rest() {
        let kr = open_mem_encrypted();
        let pk = [1u8; 32];
        let sk = [2u8; 32];
        let fp = [3u8; 16];
        kr.save_identity(&pk, &sk, &fp, 1000).unwrap();

        let stored_privkey: Vec<u8> = kr
            .db
            .lock()
            .unwrap()
            .query_row("SELECT privkey FROM e2e_identity WHERE id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_ne!(stored_privkey.len(), 32);

        let (lpk, lsk, lfp, lts) = kr.load_identity().unwrap().unwrap();
        assert_eq!(lpk, pk);
        assert_eq!(lsk, sk);
        assert_eq!(lfp, fp);
        assert_eq!(lts, 1000);
    }

    #[test]
    fn identity_none_when_empty() {
        let kr = open_mem();
        assert!(kr.load_identity().unwrap().is_none());
    }

    #[test]
    fn peer_upsert_updates_last_handle() {
        let kr = open_mem();
        let fp = [9u8; 16];
        let rec1 = PeerRecord {
            fingerprint: fp,
            pubkey: [1; 32],
            last_handle: Some("old@host".into()),
            last_nick: Some("alice".into()),
            first_seen: 100,
            last_seen: 100,
            global_status: TrustStatus::Pending,
        };
        kr.upsert_peer(&rec1).unwrap();
        let rec2 = PeerRecord {
            last_handle: Some("new@host".into()),
            last_seen: 200,
            ..rec1
        };
        kr.upsert_peer(&rec2).unwrap();
        let loaded = kr.get_peer_by_fingerprint(&fp).unwrap().unwrap();
        assert_eq!(loaded.last_handle.as_deref(), Some("new@host"));
        assert_eq!(loaded.last_seen, 200);
        // first_seen is preserved
        assert_eq!(loaded.first_seen, 100);
    }

    #[test]
    fn outgoing_session_pending_rotation_flag() {
        let kr = open_mem();
        kr.set_outgoing_session("#x", &[7u8; 32], 100).unwrap();
        let loaded = kr.get_outgoing_session("#x").unwrap().unwrap();
        assert!(!loaded.pending_rotation);
        kr.mark_outgoing_pending_rotation("#x").unwrap();
        let loaded = kr.get_outgoing_session("#x").unwrap().unwrap();
        assert!(loaded.pending_rotation);
        kr.clear_outgoing_pending_rotation("#x").unwrap();
        let loaded = kr.get_outgoing_session("#x").unwrap().unwrap();
        assert!(!loaded.pending_rotation);
    }

    #[test]
    fn incoming_session_status_transitions() {
        let kr = open_mem();
        let s = IncomingSession {
            handle: "~alice@host".into(),
            channel: "#x".into(),
            fingerprint: [5; 16],
            sk: [8; 32],
            status: TrustStatus::Pending,
            created_at: 100,
        };
        kr.set_incoming_session(&s).unwrap();
        kr.update_incoming_status("~alice@host", "#x", TrustStatus::Trusted)
            .unwrap();
        let loaded = kr
            .get_incoming_session("~alice@host", "#x")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.status, TrustStatus::Trusted);

        // list_trusted_peers_for_channel surfaces the row.
        let trusted = kr.list_trusted_peers_for_channel("#x").unwrap();
        assert_eq!(trusted.len(), 1);
        assert_eq!(trusted[0].handle, "~alice@host");

        kr.delete_incoming_session("~alice@host", "#x").unwrap();
        assert!(
            kr.get_incoming_session("~alice@host", "#x")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn channel_config_roundtrip() {
        let kr = open_mem();
        let cfg = ChannelConfig {
            channel: "#x".into(),
            enabled: true,
            mode: ChannelMode::AutoAccept,
        };
        kr.set_channel_config(&cfg).unwrap();
        let loaded = kr.get_channel_config("#x").unwrap().unwrap();
        assert!(loaded.enabled);
        assert_eq!(loaded.mode, ChannelMode::AutoAccept);
    }

    #[test]
    fn autotrust_add_list_remove() {
        let kr = open_mem();
        kr.add_autotrust("global", "~bob@*", 100).unwrap();
        kr.add_autotrust("#x", "*@trusted.org", 100).unwrap();
        let list = kr.list_autotrust().unwrap();
        assert_eq!(list.len(), 2);
        kr.remove_autotrust("~bob@*").unwrap();
        let list = kr.list_autotrust().unwrap();
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn glob_matches_ci_literal_and_wildcards() {
        // Literal, case-insensitive.
        assert!(glob_matches_ci("~bob@b.host", "~bob@b.host"));
        assert!(glob_matches_ci("~BOB@B.HOST", "~bob@b.host"));
        assert!(!glob_matches_ci("~alice@host", "~bob@host"));
        // `*` matches any sequence including empty.
        assert!(glob_matches_ci("*", "anything"));
        assert!(glob_matches_ci("*bob*", "~bob@host"));
        assert!(glob_matches_ci("~*", "~bob@b.host"));
        assert!(glob_matches_ci("*@trusted.org", "~anyone@trusted.org"));
        assert!(!glob_matches_ci("*@trusted.org", "~bob@evil.host"));
        // `?` matches exactly one char.
        assert!(glob_matches_ci("~b?b@host", "~bob@host"));
        assert!(!glob_matches_ci("~b?b@host", "~bb@host"));
        // Multiple stars — backtracking.
        assert!(glob_matches_ci(
            "*@*.trusted.org",
            "~alice@shell.trusted.org"
        ));
        assert!(glob_matches_ci("*@*", "a@b"));
        // Trailing stars.
        assert!(glob_matches_ci("~bob*", "~bob"));
        // Empty pattern only matches empty input.
        assert!(glob_matches_ci("", ""));
        assert!(!glob_matches_ci("", "bob"));
    }

    #[test]
    fn autotrust_matches_global_and_scoped() {
        let kr = open_mem();
        kr.add_autotrust("global", "*@*.trusted.org", 100).unwrap();
        kr.add_autotrust("#x", "~bob@*", 100).unwrap();

        // Global rule hits regardless of channel.
        assert!(
            kr.autotrust_matches("~alice@shell.trusted.org", "#anything")
                .unwrap()
        );
        // Scoped rule hits only on matching channel.
        assert!(kr.autotrust_matches("~bob@b.host", "#x").unwrap());
        assert!(!kr.autotrust_matches("~bob@b.host", "#y").unwrap());
        // No match at all.
        assert!(!kr.autotrust_matches("~stranger@nowhere", "#x").unwrap());
    }

    #[test]
    fn outgoing_recipients_record_list_remove() {
        let kr = open_mem();
        let fp_a = [1u8; 16];
        let fp_b = [2u8; 16];
        kr.record_outgoing_recipient("#x", "~alice@a.host", &fp_a, 100)
            .unwrap();
        kr.record_outgoing_recipient("#x", "~bob@b.host", &fp_b, 110)
            .unwrap();
        // Different channel → not seen when listing #x.
        kr.record_outgoing_recipient("#y", "~carol@c.host", &[3u8; 16], 120)
            .unwrap();

        let list_x = kr.list_outgoing_recipients("#x").unwrap();
        assert_eq!(list_x.len(), 2);
        assert!(list_x.iter().any(|(h, _)| h == "~alice@a.host"));
        assert!(list_x.iter().any(|(h, _)| h == "~bob@b.host"));

        kr.remove_outgoing_recipient("#x", "~bob@b.host").unwrap();
        let list_x2 = kr.list_outgoing_recipients("#x").unwrap();
        assert_eq!(list_x2.len(), 1);
        assert_eq!(list_x2[0].0, "~alice@a.host");
    }

    // ---------- list_all_* dump helpers (used by portable export) ----------

    #[test]
    fn list_all_peers_returns_every_row() {
        let kr = open_mem();
        let rec_a = PeerRecord {
            fingerprint: [0xaa; 16],
            pubkey: [1; 32],
            last_handle: Some("~alice@a.host".into()),
            last_nick: Some("alice".into()),
            first_seen: 100,
            last_seen: 110,
            global_status: TrustStatus::Trusted,
        };
        let rec_b = PeerRecord {
            fingerprint: [0xbb; 16],
            pubkey: [2; 32],
            last_handle: None,
            last_nick: None,
            first_seen: 200,
            last_seen: 220,
            global_status: TrustStatus::Pending,
        };
        let rec_c = PeerRecord {
            fingerprint: [0xcc; 16],
            pubkey: [3; 32],
            last_handle: Some("~carol@c.host".into()),
            last_nick: Some("carol".into()),
            first_seen: 300,
            last_seen: 330,
            global_status: TrustStatus::Revoked,
        };
        kr.upsert_peer(&rec_a).unwrap();
        kr.upsert_peer(&rec_b).unwrap();
        kr.upsert_peer(&rec_c).unwrap();

        let all = kr.list_all_peers().unwrap();
        assert_eq!(all.len(), 3);
        // Ordered by first_seen ASC.
        assert_eq!(all[0].fingerprint, [0xaa; 16]);
        assert_eq!(all[0].global_status, TrustStatus::Trusted);
        assert_eq!(all[1].fingerprint, [0xbb; 16]);
        assert_eq!(all[1].global_status, TrustStatus::Pending);
        assert_eq!(all[1].last_handle, None);
        assert_eq!(all[2].fingerprint, [0xcc; 16]);
        assert_eq!(all[2].global_status, TrustStatus::Revoked);
    }

    #[test]
    fn last_handle_for_nick_scopes_by_network() {
        let kr = open_mem();
        // Same nick (same E2E identity) observed on two networks. The cache is
        // keyed by (network, nick), so caching the NetB handle must NOT clobber
        // the NetA handle — the exact failure of a single fingerprint-keyed row.
        kr.cache_dm_handle("NetA", "bob", "~bob@a.host").unwrap();
        kr.cache_dm_handle("NetB", "bob", "~bob@b.host").unwrap();
        assert_eq!(
            kr.last_handle_for_nick("bob", "NetA").unwrap().as_deref(),
            Some("~bob@a.host"),
            "NetA must still resolve its own handle after NetB was cached"
        );
        assert_eq!(
            kr.last_handle_for_nick("BOB", "NetB").unwrap().as_deref(),
            Some("~bob@b.host"),
            "nick lookup is case-insensitive"
        );
        // A third network with no cache row → None, not a cross-network leak.
        assert_eq!(kr.last_handle_for_nick("bob", "NetC").unwrap(), None);
        // Latest write wins per (network, nick) — a CHGHOST updates the handle.
        kr.cache_dm_handle("NetA", "bob", "~bob@cloak").unwrap();
        assert_eq!(
            kr.last_handle_for_nick("bob", "NetA").unwrap().as_deref(),
            Some("~bob@cloak")
        );
    }

    #[test]
    fn last_handle_for_nick_falls_back_to_e2e_peers_when_cache_empty() {
        // Upgrade scenario: a pre-cache keyring has e2e_peers rows (last-seen
        // handles) but an empty e2e_dm_handle_cache. Without a fallback, a DM to
        // a peer who hasn't spoken this session would resolve None and go out in
        // PLAINTEXT despite an enabled @<handle> config. The fallback resolves
        // the handle from e2e_peers so the config is reachable.
        let kr = open_mem();
        kr.upsert_peer(&PeerRecord {
            fingerprint: [7u8; 16],
            pubkey: [1; 32],
            last_handle: Some("~bob@legacy.host".into()),
            last_nick: Some("bob".into()),
            first_seen: 100,
            last_seen: 200,
            global_status: TrustStatus::Trusted,
        })
        .unwrap();
        // Cache is empty → fall back to e2e_peers (case-insensitive nick).
        assert_eq!(
            kr.last_handle_for_nick("BOB", "AnyNet").unwrap().as_deref(),
            Some("~bob@legacy.host"),
            "must resolve the legacy e2e_peers handle when the cache is empty"
        );
        // Once the cache is populated it is authoritative and wins over e2e_peers.
        kr.cache_dm_handle("AnyNet", "bob", "~bob@fresh.host").unwrap();
        assert_eq!(
            kr.last_handle_for_nick("bob", "AnyNet").unwrap().as_deref(),
            Some("~bob@fresh.host"),
            "network-scoped cache must take precedence over the legacy fallback"
        );
    }

    #[test]
    fn list_all_incoming_sessions_returns_every_row() {
        let kr = open_mem();
        for (handle, channel, fp_byte, sk_byte, status) in [
            ("~alice@a.host", "#rust", 0x11, 0x21, TrustStatus::Trusted),
            ("~bob@b.host", "#rust", 0x12, 0x22, TrustStatus::Pending),
            ("~alice@a.host", "#go", 0x13, 0x23, TrustStatus::Revoked),
        ] {
            kr.set_incoming_session(&IncomingSession {
                handle: handle.into(),
                channel: channel.into(),
                fingerprint: [fp_byte; 16],
                sk: [sk_byte; 32],
                status,
                created_at: 1_000,
            })
            .unwrap();
        }
        let all = kr.list_all_incoming_sessions().unwrap();
        assert_eq!(all.len(), 3);
        // Check each row round-tripped intact; the set is unordered by row
        // content but we can confirm every (handle,channel) pair is present.
        let pairs: Vec<(String, String)> = all
            .iter()
            .map(|s| (s.handle.clone(), s.channel.clone()))
            .collect();
        assert!(pairs.contains(&("~alice@a.host".into(), "#rust".into())));
        assert!(pairs.contains(&("~bob@b.host".into(), "#rust".into())));
        assert!(pairs.contains(&("~alice@a.host".into(), "#go".into())));
    }

    #[test]
    fn list_all_outgoing_sessions_returns_every_row() {
        let kr = open_mem();
        kr.set_outgoing_session("#rust", &[1u8; 32], 1_000).unwrap();
        kr.set_outgoing_session("#go", &[2u8; 32], 2_000).unwrap();
        kr.mark_outgoing_pending_rotation("#go").unwrap();
        let all = kr.list_all_outgoing_sessions().unwrap();
        assert_eq!(all.len(), 2);
        // Ordered by channel ASC — '#' < alphanumerics but both start with
        // '#', so it's literal channel-name order: "#go" < "#rust".
        assert_eq!(all[0].channel, "#go");
        assert!(all[0].pending_rotation);
        assert_eq!(all[1].channel, "#rust");
        assert!(!all[1].pending_rotation);
    }

    #[test]
    fn install_incoming_session_strict_rejects_fingerprint_change() {
        let kr = open_mem();
        let first = IncomingSession {
            handle: "~alice@host".into(),
            channel: "#x".into(),
            fingerprint: [0xaa; 16],
            sk: [1u8; 32],
            status: TrustStatus::Trusted,
            created_at: 100,
        };
        kr.install_incoming_session_strict(&first).unwrap();

        // A second KEYRSP under the same (handle, channel) signed by a
        // different Ed25519 identity — must be rejected.
        let imposter = IncomingSession {
            fingerprint: [0xbb; 16],
            sk: [2u8; 32],
            created_at: 200,
            ..first
        };
        let err = kr
            .install_incoming_session_strict(&imposter)
            .expect_err("imposter must be rejected");
        match err {
            crate::e2e::error::E2eError::HandleMismatch { .. } => {}
            other => panic!("expected HandleMismatch, got {other:?}"),
        }

        // Existing row is untouched: fingerprint and sk are still the first.
        let loaded = kr
            .get_incoming_session("~alice@host", "#x")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.fingerprint, [0xaa; 16]);
        assert_eq!(loaded.sk, [1u8; 32]);
    }

    #[test]
    fn install_incoming_session_strict_accepts_same_fingerprint() {
        let kr = open_mem();
        let first = IncomingSession {
            handle: "~alice@host".into(),
            channel: "#x".into(),
            fingerprint: [0xaa; 16],
            sk: [1u8; 32],
            status: TrustStatus::Trusted,
            created_at: 100,
        };
        kr.install_incoming_session_strict(&first).unwrap();

        // Same fingerprint, rotated session key, later timestamp → allowed.
        let refresh = IncomingSession {
            sk: [2u8; 32],
            created_at: 200,
            ..first
        };
        kr.install_incoming_session_strict(&refresh).unwrap();

        let loaded = kr
            .get_incoming_session("~alice@host", "#x")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.fingerprint, [0xaa; 16]);
        assert_eq!(loaded.sk, [2u8; 32]);
        assert_eq!(loaded.created_at, 200);
    }

    #[test]
    fn get_peer_by_handle_returns_most_recent() {
        let kr = open_mem();
        // Two different identities that both lived under ~alice@host at
        // different times. get_peer_by_handle must return the most recent
        // one so TrustChange classification surfaces the freshest row.
        let older = PeerRecord {
            fingerprint: [0x11; 16],
            pubkey: [1; 32],
            last_handle: Some("~alice@host".into()),
            last_nick: Some("alice-old".into()),
            first_seen: 100,
            last_seen: 150,
            global_status: TrustStatus::Trusted,
        };
        let newer = PeerRecord {
            fingerprint: [0x22; 16],
            pubkey: [2; 32],
            last_handle: Some("~alice@host".into()),
            last_nick: Some("alice-new".into()),
            first_seen: 200,
            last_seen: 250,
            global_status: TrustStatus::Trusted,
        };
        kr.upsert_peer(&older).unwrap();
        kr.upsert_peer(&newer).unwrap();

        let loaded = kr
            .get_peer_by_handle("~alice@host")
            .unwrap()
            .expect("reverse lookup must find a row");
        assert_eq!(loaded.fingerprint, [0x22; 16]);
        assert_eq!(loaded.last_nick.as_deref(), Some("alice-new"));

        // Unknown handle → None.
        assert!(kr.get_peer_by_handle("~ghost@nowhere").unwrap().is_none());
    }

    #[test]
    fn list_all_channel_configs_returns_every_row() {
        let kr = open_mem();
        kr.set_channel_config(&ChannelConfig {
            channel: "#rust".into(),
            enabled: true,
            mode: ChannelMode::AutoAccept,
        })
        .unwrap();
        kr.set_channel_config(&ChannelConfig {
            channel: "#go".into(),
            enabled: false,
            mode: ChannelMode::Quiet,
        })
        .unwrap();
        let all = kr.list_all_channel_configs().unwrap();
        assert_eq!(all.len(), 2);
        // '#go' < '#rust' alphabetically.
        assert_eq!(all[0].channel, "#go");
        assert!(!all[0].enabled);
        assert_eq!(all[0].mode, ChannelMode::Quiet);
        assert_eq!(all[1].channel, "#rust");
        assert!(all[1].enabled);
        assert_eq!(all[1].mode, ChannelMode::AutoAccept);
    }
}

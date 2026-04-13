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
}

impl Keyring {
    /// Construct a keyring that shares the given SQLite connection.
    #[must_use]
    pub fn new(db: Arc<Mutex<Connection>>) -> Self {
        Self {
            db,
            secret_key: None,
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
        conn.execute(
            "UPDATE e2e_outgoing_sessions SET pending_rotation = 1 WHERE channel = ?1",
            params![channel],
        )?;
        Ok(())
    }

    pub fn clear_outgoing_pending_rotation(&self, channel: &str) -> Result<()> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        conn.execute(
            "UPDATE e2e_outgoing_sessions SET pending_rotation = 0 WHERE channel = ?1",
            params![channel],
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
        let existing: Option<Vec<u8>> = conn
            .query_row(
                "SELECT fingerprint FROM e2e_incoming_sessions
                 WHERE handle = ?1 AND channel = ?2",
                params![s.handle, s.channel],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(existing_fp) = existing
            && existing_fp.as_slice() != s.fingerprint.as_slice()
        {
            return Err(crate::e2e::error::E2eError::HandleMismatch {
                expected: format!("fp={}", hex::encode(&existing_fp)),
                got: format!("fp={}", hex::encode(s.fingerprint)),
            });
        }
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

    pub fn get_incoming_session(
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
        conn.execute(
            "UPDATE e2e_incoming_sessions SET status = ?1 WHERE handle = ?2 AND channel = ?3",
            params![status.as_str(), handle, channel],
        )?;
        Ok(())
    }

    pub fn delete_incoming_session(&self, handle: &str, channel: &str) -> Result<()> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        conn.execute(
            "DELETE FROM e2e_incoming_sessions WHERE handle = ?1 AND channel = ?2",
            params![handle, channel],
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
        let conn = self.db.lock().expect("keyring mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT handle_pattern FROM e2e_autotrust
             WHERE scope = 'global' OR scope = ?1",
        )?;
        let rows = stmt
            .query_map(params![channel], |r| r.get::<_, String>(0))?
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
        conn.execute(
            "DELETE FROM e2e_outgoing_recipients
             WHERE channel = ?1 AND handle = ?2",
            params![channel, handle],
        )?;
        Ok(())
    }

    /// Return every recipient of our outgoing session key for `channel`.
    /// The returned tuples are `(handle, fingerprint)`.
    pub fn list_outgoing_recipients(&self, channel: &str) -> Result<Vec<(String, Fingerprint)>> {
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
        CREATE TABLE e2e_outgoing_sessions (
            channel           TEXT PRIMARY KEY,
            sk                BLOB NOT NULL,
            created_at        INTEGER NOT NULL,
            pending_rotation  INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE e2e_incoming_sessions (
            handle       TEXT NOT NULL,
            channel      TEXT NOT NULL,
            fingerprint  BLOB NOT NULL,
            sk           BLOB NOT NULL,
            status       TEXT NOT NULL DEFAULT 'pending',
            created_at   INTEGER NOT NULL,
            PRIMARY KEY (handle, channel)
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

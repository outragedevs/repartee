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

use rusqlite::{params, Connection, OptionalExtension};

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
}

impl Keyring {
    /// Construct a keyring that shares the given SQLite connection.
    #[must_use]
    pub fn new(db: Arc<Mutex<Connection>>) -> Self {
        Self { db }
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
        let conn = self.db.lock().expect("keyring mutex poisoned");
        conn.execute(
            "INSERT OR REPLACE INTO e2e_identity (id, pubkey, privkey, fingerprint, created_at)
             VALUES (1, ?1, ?2, ?3, ?4)",
            params![
                pubkey.as_slice(),
                privkey.as_slice(),
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
        // We stored fixed-length blobs — length-mismatch here means the DB
        // was hand-tampered. Treat as a hard error to avoid silent corruption.
        if pk.len() != 32 || sk.len() != 32 || fp.len() != 16 {
            return Err(crate::e2e::error::E2eError::Keyring(format!(
                "e2e_identity row has unexpected blob lengths (pk={}, sk={}, fp={})",
                pk.len(),
                sk.len(),
                fp.len()
            )));
        }
        let mut pk_arr = [0u8; 32];
        let mut sk_arr = [0u8; 32];
        let mut fp_arr = [0u8; 16];
        pk_arr.copy_from_slice(&pk);
        sk_arr.copy_from_slice(&sk);
        fp_arr.copy_from_slice(&fp);
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

    // ---------- outgoing sessions ----------

    pub fn set_outgoing_session(
        &self,
        channel: &str,
        sk: &SessionKey,
        created_at: i64,
    ) -> Result<()> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        conn.execute(
            "INSERT OR REPLACE INTO e2e_outgoing_sessions
                (channel, sk, created_at, pending_rotation)
             VALUES (?1, ?2, ?3, 0)",
            params![channel, sk.as_slice(), created_at],
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
        if sk.len() != 32 {
            return Err(crate::e2e::error::E2eError::Keyring(format!(
                "e2e_outgoing_sessions row sk has unexpected length {}",
                sk.len()
            )));
        }
        let mut k = [0u8; 32];
        k.copy_from_slice(&sk);
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
        let conn = self.db.lock().expect("keyring mutex poisoned");
        conn.execute(
            "INSERT OR REPLACE INTO e2e_incoming_sessions
                (handle, channel, fingerprint, sk, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                s.handle,
                s.channel,
                s.fingerprint.as_slice(),
                s.sk.as_slice(),
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
        if fp.len() != 16 || sk.len() != 32 {
            return Err(crate::e2e::error::E2eError::Keyring(format!(
                "e2e_incoming_sessions row has unexpected blob lengths (fp={}, sk={})",
                fp.len(),
                sk.len()
            )));
        }
        let mut fp_arr = [0u8; 16];
        let mut sk_arr = [0u8; 32];
        fp_arr.copy_from_slice(&fp);
        sk_arr.copy_from_slice(&sk);
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
            if fp.len() != 16 || sk.len() != 32 {
                return Err(crate::e2e::error::E2eError::Keyring(format!(
                    "e2e_incoming_sessions row has unexpected blob lengths (fp={}, sk={})",
                    fp.len(),
                    sk.len()
                )));
            }
            let mut fp_arr = [0u8; 16];
            let mut sk_arr = [0u8; 32];
            fp_arr.copy_from_slice(&fp);
            sk_arr.copy_from_slice(&sk);
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

    pub fn add_autotrust(
        &self,
        scope: &str,
        handle_pattern: &str,
        created_at: i64,
    ) -> Result<()> {
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

    pub fn remove_autotrust(&self, pattern: &str) -> Result<()> {
        let conn = self.db.lock().expect("keyring mutex poisoned");
        conn.execute(
            "DELETE FROM e2e_autotrust WHERE handle_pattern = ?1",
            params![pattern],
        )?;
        Ok(())
    }
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
    ";

    fn open_mem() -> Keyring {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        Keyring::new(Arc::new(Mutex::new(conn)))
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
        assert!(kr
            .get_incoming_session("~alice@host", "#x")
            .unwrap()
            .is_none());
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
}

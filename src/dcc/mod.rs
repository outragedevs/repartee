pub mod chat;
pub mod protocol;
pub mod types;

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::irc::ignore::wildcard_to_regex;

use types::{DccRecord, DccState};

// ─── DccEvent ─────────────────────────────────────────────────────────────────

/// Events emitted by DCC async tasks back to the main event loop.
#[derive(Debug)]
pub enum DccEvent {
    /// A DCC CHAT request arrived from the network; user must accept or decline.
    IncomingRequest {
        nick: String,
        conn_id: String,
        addr: IpAddr,
        port: u16,
        passive_token: Option<u32>,
        ident: String,
        host: String,
    },
    /// TCP handshake completed; DCC CHAT session is now live.
    ChatConnected { id: String },
    /// A chat line was received from the remote peer.
    ChatMessage { id: String, text: String },
    /// A /me action was received from the remote peer.
    ChatAction { id: String, text: String },
    /// The remote peer closed the connection or we initiated a close.
    ChatClosed { id: String, reason: Option<String> },
    /// An I/O or protocol error occurred on the DCC session.
    ChatError { id: String, error: String },
}

// ─── DccManager ───────────────────────────────────────────────────────────────

/// Owns all DCC connection state and provides the management API.
///
/// Create via [`DccManager::new`], which also returns the event receiver
/// that the main loop should drain.
pub struct DccManager {
    /// All active DCC records, keyed by their unique ID.
    pub records: HashMap<String, DccRecord>,
    /// Sender half — async tasks post [`DccEvent`]s through this.
    pub dcc_tx: mpsc::UnboundedSender<DccEvent>,
    /// Per-session senders for outgoing chat lines (keyed by record ID).
    pub chat_senders: HashMap<String, mpsc::UnboundedSender<String>>,

    // ── Configuration ────────────────────────────────────────────────────────
    /// How long a pending/listening session waits before timing out (seconds).
    pub timeout_secs: u64,
    /// Preferred TCP port range for our listener; `(0, 0)` = OS-assigned.
    pub port_range: (u16, u16),
    /// Our externally-visible IP to advertise in DCC offers, if known.
    pub own_ip: Option<IpAddr>,
    /// Allow auto-accept of offers that come from ports ≤ 1023.
    pub autoaccept_lowports: bool,
    /// `nick!ident@host` wildcard masks whose offers are accepted automatically.
    pub autochat_masks: Vec<String>,
    /// Maximum number of simultaneous DCC connections permitted.
    pub max_connections: usize,
}

impl DccManager {
    /// Create a new manager and the corresponding event receiver.
    ///
    /// The caller must poll the receiver inside the main `tokio::select!` loop.
    pub fn new() -> (Self, mpsc::UnboundedReceiver<DccEvent>) {
        let (dcc_tx, dcc_rx) = mpsc::unbounded_channel();
        let manager = Self {
            records: HashMap::new(),
            dcc_tx,
            chat_senders: HashMap::new(),
            timeout_secs: 300,
            port_range: (0, 0),
            own_ip: None,
            autoaccept_lowports: false,
            autochat_masks: Vec::new(),
            max_connections: 10,
        };
        (manager, dcc_rx)
    }

    /// Generate a unique record ID for the given nick.
    ///
    /// Returns the nick lowercased. If that ID is already taken, appends a
    /// numeric suffix (`2`, `3`, …) until an unused one is found.
    pub fn generate_id(&self, nick: &str) -> String {
        let base = nick.to_lowercase();
        if !self.records.contains_key(&base) {
            return base;
        }
        let mut n = 2u32;
        loop {
            let candidate = format!("{base}{n}");
            if !self.records.contains_key(&candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Find the first record in `WaitingUser` or `Listening` state for `nick`
    /// (case-insensitive).
    pub fn find_pending(&self, nick: &str) -> Option<&DccRecord> {
        let nick_lower = nick.to_lowercase();
        self.records.values().find(|r| {
            r.nick.to_lowercase() == nick_lower
                && matches!(r.state, DccState::WaitingUser | DccState::Listening)
        })
    }

    /// Return the most recently created record in `WaitingUser` state.
    pub fn find_latest_pending(&self) -> Option<&DccRecord> {
        self.records
            .values()
            .filter(|r| matches!(r.state, DccState::WaitingUser))
            // Most recently created = largest Instant value (latest point in time)
            .max_by_key(|r| r.created)
    }

    /// Find the first record in `Connected` state for `nick` (case-insensitive).
    pub fn find_connected(&self, nick: &str) -> Option<&DccRecord> {
        let nick_lower = nick.to_lowercase();
        self.records.values().find(|r| {
            r.nick.to_lowercase() == nick_lower && matches!(r.state, DccState::Connected)
        })
    }

    /// Remove all `WaitingUser`/`Listening` records older than `timeout_secs`.
    ///
    /// Returns `(id, nick)` pairs for every record that was removed so the
    /// caller can notify the user or clean up associated buffers.
    pub fn purge_expired(&mut self) -> Vec<(String, String)> {
        let timeout = Duration::from_secs(self.timeout_secs);
        // Collect IDs first — we cannot hold an immutable borrow on `records`
        // while calling `records.remove()` in the same expression.
        #[allow(clippy::needless_collect)]
        let expired: Vec<String> = self
            .records
            .iter()
            .filter(|(_, r)| {
                matches!(r.state, DccState::WaitingUser | DccState::Listening)
                    && r.created.elapsed() > timeout
            })
            .map(|(id, _)| id.clone())
            .collect();

        expired
            .into_iter()
            .filter_map(|id| {
                self.chat_senders.remove(&id);
                self.records.remove(&id).map(|r| (id, r.nick))
            })
            .collect()
    }

    /// Rename the nick on every matching record (case-insensitive), re-keying
    /// both `records` and `chat_senders` so lookups by the new nick work.
    ///
    /// Returns `(old_id, new_id, old_buf_suffix, new_buf_suffix)` tuples so the
    /// caller can rename DCC chat buffers in the UI.
    pub fn update_nick(
        &mut self,
        old_nick: &str,
        new_nick: &str,
    ) -> Vec<(String, String, String, String)> {
        let old_lower = old_nick.to_lowercase();

        // Collect IDs that need re-keying — cannot mutate maps while iterating.
        let old_ids: Vec<String> = self
            .records
            .iter()
            .filter(|(_, r)| r.nick.to_lowercase() == old_lower)
            .map(|(id, _)| id.clone())
            .collect();

        let mut renamed = Vec::new();

        for old_id in old_ids {
            let Some(mut record) = self.records.remove(&old_id) else {
                continue;
            };
            new_nick.clone_into(&mut record.nick);
            let new_id = self.generate_id(new_nick);
            let old_buf_suffix = format!("={old_id}");
            let new_buf_suffix = format!("={new_id}");
            record.id.clone_from(&new_id);
            self.records.insert(new_id.clone(), record);

            // Re-key the chat sender channel if one exists.
            if let Some(sender) = self.chat_senders.remove(&old_id) {
                self.chat_senders.insert(new_id.clone(), sender);
            }

            renamed.push((old_id, new_id, old_buf_suffix, new_buf_suffix));
        }

        renamed
    }

    /// Remove and return the first record matching `nick` (case-insensitive).
    pub fn close_by_nick(&mut self, nick: &str) -> Option<DccRecord> {
        let nick_lower = nick.to_lowercase();
        let id = self
            .records
            .iter()
            .find(|(_, r)| r.nick.to_lowercase() == nick_lower)
            .map(|(id, _)| id.clone())?;
        self.records.remove(&id)
    }

    /// Remove and return the record with the given ID.
    pub fn close_by_id(&mut self, id: &str) -> Option<DccRecord> {
        self.records.remove(id)
    }

    /// Send a chat line to the async task managing the session with `id`.
    ///
    /// Returns `Err(String)` when there is no live sender for that ID (the
    /// session does not exist or has not connected yet).
    pub fn send_chat_line(&self, id: &str, text: &str) -> Result<(), String> {
        let sender = self
            .chat_senders
            .get(id)
            .ok_or_else(|| format!("no active DCC CHAT session for id {id:?}"))?;
        sender
            .send(text.to_owned())
            .map_err(|_| format!("DCC CHAT session {id:?} channel is closed"))
    }

    /// Return `true` if this incoming offer should be auto-accepted.
    ///
    /// Checks:
    /// 1. Rejects low ports (≤ 1023) unless `autoaccept_lowports` is set.
    /// 2. Matches `nick!ident@host` against each mask in `autochat_masks`.
    pub fn should_auto_accept(&self, nick: &str, ident: &str, host: &str, port: u16) -> bool {
        // Low-port guard: ports ≤ 1023 can be used to hijack trusted services.
        if port != 0 && port <= 1023 && !self.autoaccept_lowports {
            return false;
        }

        let full_mask = format!("{nick}!{ident}@{host}");
        for pattern in &self.autochat_masks {
            let re = wildcard_to_regex(pattern);
            if re.is_match(&full_mask) {
                return true;
            }
        }
        false
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Instant;
    use types::{DccRecord, DccState, DccType};

    fn make_record(id: &str, nick: &str, state: DccState) -> DccRecord {
        DccRecord {
            id: id.to_owned(),
            dcc_type: DccType::Chat,
            nick: nick.to_owned(),
            conn_id: "test_conn".to_owned(),
            addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 12345,
            state,
            passive_token: None,
            created: Instant::now(),
            started: None,
            bytes_transferred: 0,
            mirc_ctcp: true,
            ident: "user".to_owned(),
            host: "example.com".to_owned(),
        }
    }

    fn make_manager() -> DccManager {
        let (mgr, _rx) = DccManager::new();
        mgr
    }

    // ── generate_id ──────────────────────────────────────────────────────────

    #[test]
    fn generate_id_unique() {
        let mut mgr = make_manager();

        // First ID for alice → "alice"
        let id1 = mgr.generate_id("Alice");
        assert_eq!(id1, "alice");
        mgr.records.insert(id1.clone(), make_record(&id1, "Alice", DccState::Connected));

        // Second ID for alice → "alice2"
        let id2 = mgr.generate_id("Alice");
        assert_eq!(id2, "alice2");
        mgr.records.insert(id2.clone(), make_record(&id2, "Alice", DccState::Connected));

        // Third ID for alice → "alice3"
        let id3 = mgr.generate_id("Alice");
        assert_eq!(id3, "alice3");
    }

    // ── find_pending ─────────────────────────────────────────────────────────

    #[test]
    fn find_pending_by_nick() {
        let mut mgr = make_manager();
        mgr.records.insert("bob".to_owned(), make_record("bob", "Bob", DccState::WaitingUser));

        let found = mgr.find_pending("BOB");
        assert!(found.is_some());
    }

    #[test]
    fn find_pending_not_connected() {
        let mut mgr = make_manager();
        mgr.records.insert("bob".to_owned(), make_record("bob", "Bob", DccState::Connected));

        let found = mgr.find_pending("bob");
        assert!(found.is_none());
    }

    // ── find_latest_pending ──────────────────────────────────────────────────

    #[test]
    fn find_latest_pending() {
        let mut mgr = make_manager();

        // Insert an older record first, then a newer one.
        let mut old_rec = make_record("alice", "Alice", DccState::WaitingUser);
        // Backdate the older record so it has a larger elapsed time.
        old_rec.created = Instant::now() - Duration::from_secs(10);
        mgr.records.insert("alice".to_owned(), old_rec);

        let new_rec = make_record("bob", "Bob", DccState::WaitingUser);
        mgr.records.insert("bob".to_owned(), new_rec);

        let latest = mgr.find_latest_pending().expect("should find a pending record");
        assert_eq!(latest.nick, "Bob");
    }

    // ── find_connected ───────────────────────────────────────────────────────

    #[test]
    fn find_connected_by_nick() {
        let mut mgr = make_manager();
        mgr.records.insert("carol".to_owned(), make_record("carol", "Carol", DccState::Connected));

        let found = mgr.find_connected("carol");
        assert!(found.is_some());
    }

    // ── update_nick ──────────────────────────────────────────────────────────

    #[test]
    fn update_nick_rekeys_records_and_senders() {
        let mut mgr = make_manager();
        mgr.records.insert("dave".to_owned(), make_record("dave", "Dave", DccState::Connected));
        // Simulate an active chat sender for the old ID.
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        mgr.chat_senders.insert("dave".to_owned(), tx);

        let tuples = mgr.update_nick("Dave", "Dave_");
        assert_eq!(tuples.len(), 1);
        let (old_id, new_id, old_buf, new_buf) = &tuples[0];
        assert_eq!(old_id, "dave");
        assert_eq!(new_id, "dave_");
        assert_eq!(old_buf, "=dave");
        assert_eq!(new_buf, "=dave_");
        // Old key removed, new key present.
        assert!(!mgr.records.contains_key("dave"));
        assert_eq!(mgr.records["dave_"].nick, "Dave_");
        assert!(!mgr.chat_senders.contains_key("dave"));
        assert!(mgr.chat_senders.contains_key("dave_"));
    }

    // ── close_by_nick ────────────────────────────────────────────────────────

    #[test]
    fn close_by_nick_removes() {
        let mut mgr = make_manager();
        mgr.records.insert("eve".to_owned(), make_record("eve", "Eve", DccState::Connected));

        let removed = mgr.close_by_nick("Eve");
        assert!(removed.is_some());
        assert!(mgr.records.is_empty());
    }

    // ── close_by_id ──────────────────────────────────────────────────────────

    #[test]
    fn close_by_id_removes() {
        let mut mgr = make_manager();
        mgr.records.insert("frank".to_owned(), make_record("frank", "Frank", DccState::Listening));

        let removed = mgr.close_by_id("frank");
        assert!(removed.is_some());
        assert!(mgr.records.is_empty());
    }

    // ── purge_expired ────────────────────────────────────────────────────────

    #[test]
    fn purge_expired_removes_old() {
        let mut mgr = make_manager();
        // Very short timeout so the backdated record is definitely expired.
        mgr.timeout_secs = 5;

        let mut old_rec = make_record("grace", "Grace", DccState::WaitingUser);
        old_rec.created = Instant::now() - Duration::from_secs(10);
        mgr.records.insert("grace".to_owned(), old_rec);

        // Connected records must not be purged regardless of age.
        let mut conn_rec = make_record("hank", "Hank", DccState::Connected);
        conn_rec.created = Instant::now() - Duration::from_secs(10);
        mgr.records.insert("hank".to_owned(), conn_rec);

        let purged = mgr.purge_expired();
        assert_eq!(purged.len(), 1);
        assert_eq!(purged[0].1, "Grace");
        assert!(mgr.records.contains_key("hank"));
    }

    #[test]
    fn purge_keeps_fresh() {
        let mut mgr = make_manager();
        mgr.timeout_secs = 300;

        // Fresh WaitingUser record should not be purged.
        mgr.records.insert("iris".to_owned(), make_record("iris", "Iris", DccState::WaitingUser));

        let purged = mgr.purge_expired();
        assert!(purged.is_empty());
    }
}

// IRCv3 `batch` extension — collects messages within a BATCH and processes them as a group.
//
// Server sends:
//   BATCH +ref_tag batch_type [params...]  — start a batch
//   messages with @batch=ref_tag tag       — messages within the batch
//   BATCH -ref_tag                         — end the batch
//
// NETSPLIT/NETJOIN batch types produce summary messages instead of individual
// QUIT/JOIN events, providing server-authoritative netsplit information.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use irc::proto::{Command, Message as IrcMessage};

use crate::irc::formatting::{extract_nick, extract_nick_userhost};
use crate::state::AppState;
use crate::state::buffer::{Message, MessageType, NickEntry, make_buffer_id};

/// Maximum number of nicks to show in a netsplit/netjoin summary line.
const MAX_NICKS_DISPLAY: usize = 15;

/// Maximum time a batch can remain open before being discarded (60 seconds).
const BATCH_TIMEOUT_SECS: u64 = 60;

const MAX_BATCH_MESSAGES: usize = 4096;

/// Information about an in-progress batch.
#[derive(Debug, Clone)]
pub struct BatchInfo {
    /// The batch type (e.g. "NETSPLIT", "NETJOIN", or a vendor extension).
    pub batch_type: String,
    /// Additional parameters from the BATCH start line.
    pub params: Vec<String>,
    /// Messages collected while the batch was open.
    pub messages: Vec<IrcMessage>,
    pub dropped_messages: usize,
    /// When this batch was opened.
    pub started_at: Instant,
}

/// Tracks open `IRCv3` batches for a single connection.
#[derive(Debug, Default)]
pub struct BatchTracker {
    /// Open batches keyed by reference tag.
    open: HashMap<String, BatchInfo>,
}

impl BatchTracker {
    /// Start a new batch with the given reference tag, type, and parameters.
    pub fn start_batch(&mut self, ref_tag: &str, batch_type: &str, params: Vec<String>) {
        self.open.insert(
            ref_tag.to_string(),
            BatchInfo {
                batch_type: batch_type.to_uppercase(),
                params,
                messages: Vec::new(),
                dropped_messages: 0,
                started_at: Instant::now(),
            },
        );
    }

    /// Remove batches that have been open longer than `BATCH_TIMEOUT_SECS`
    /// and return them so the caller can replay their collected messages.
    ///
    /// A timed-out batch usually means the server crashed mid-batch or its
    /// `BATCH -tag` line never arrived. Replaying the buffered messages
    /// through the normal handler keeps `Buffer.users` and other state in
    /// sync — silently dropping them would leave stale nicks behind for QUIT
    /// batches and miss new nicks for JOIN batches. Should be called
    /// periodically (e.g. once per second from the main tick).
    pub fn purge_expired(&mut self) -> Vec<BatchInfo> {
        let timeout = std::time::Duration::from_secs(BATCH_TIMEOUT_SECS);
        self.open
            .extract_if(|_, info| info.started_at.elapsed() >= timeout)
            .map(|(tag, info)| {
                tracing::warn!(
                    "expired batch tag={tag} type={} msgs={} — replaying through normal handler",
                    info.batch_type,
                    info.messages.len()
                );
                info
            })
            .collect()
    }

    /// Check whether a message belongs to an open batch via its `@batch` tag.
    #[must_use]
    pub fn is_batched(&self, msg: &IrcMessage) -> bool {
        Self::get_batch_tag(msg).is_some_and(|tag| self.open.contains_key(tag))
    }

    /// Add a message to its batch (identified by the `@batch` tag).
    ///
    /// Returns `true` if the message was added to a batch, `false` if no
    /// matching open batch was found.
    pub fn add_message(&mut self, msg: IrcMessage) -> bool {
        let Some(tag) = Self::get_batch_tag_owned(&msg) else {
            return false;
        };
        if let Some(info) = self.open.get_mut(&tag) {
            if info.messages.len() >= MAX_BATCH_MESSAGES {
                info.dropped_messages += 1;
                if info.dropped_messages == 1 {
                    tracing::warn!(
                        tag,
                        batch_type = %info.batch_type,
                        max = MAX_BATCH_MESSAGES,
                        "discarding excess IRCv3 batch messages"
                    );
                }
                return true;
            }
            info.messages.push(msg);
            true
        } else {
            false
        }
    }

    /// End a batch and return its collected information.
    ///
    /// Returns `None` if no batch with the given tag exists.
    pub fn end_batch(&mut self, ref_tag: &str) -> Option<BatchInfo> {
        self.open.remove(ref_tag)
    }

    /// Extract the `@batch` tag value from a message, returning a reference
    /// to the tag string within the message's tag list.
    fn get_batch_tag(msg: &IrcMessage) -> Option<&str> {
        msg.tags.as_ref().and_then(|tags| {
            tags.iter()
                .find(|t| t.0 == "batch")
                .and_then(|t| t.1.as_deref())
        })
    }

    /// Same as `get_batch_tag` but returns an owned `String`.
    fn get_batch_tag_owned(msg: &IrcMessage) -> Option<String> {
        Self::get_batch_tag(msg).map(str::to_string)
    }
}

/// Process a completed batch, generating appropriate state changes and messages.
///
/// - NETSPLIT: Produces a single summary line instead of individual QUIT messages.
/// - NETJOIN: Produces a single summary line instead of individual JOIN messages.
/// - Other batch types: Messages are replayed through the normal handler.
///
/// `clean_end` is `true` for a batch closed by its `BATCH -tag` line and
/// `false` for one force-completed by the timeout purge. It only matters for
/// CHATHISTORY: a timed-out short `BEFORE` batch must not be mistaken for
/// genuine end-of-history (see [`crate::irc::chathistory::HistoryState::complete_target`]).
pub fn process_completed_batch(
    state: &mut AppState,
    conn_id: &str,
    batch: &BatchInfo,
    clean_end: bool,
) {
    match batch.batch_type.as_str() {
        "NETSPLIT" => process_netsplit_batch(state, conn_id, batch),
        "NETJOIN" => process_netjoin_batch(state, conn_id, batch),
        "CHATHISTORY" => {
            // draft/chathistory: store-only backlog fill. Conversational lines
            // are persisted to SQLite without live display/state mutation; the
            // UI surfaces them via normal pagination. Bookkeeping (clear
            // in-flight, advance the BEFORE anchor watermark, mark exhaustion)
            // runs here so it covers BOTH the normal `BATCH -tag` end path and
            // the timed-out-batch purge path (app/maintenance.rs).
            //
            // A reconnect AFTER/LATEST gap-fill is the exception: its rows fall
            // between the pre-disconnect tail and post-reconnect live messages,
            // which scroll-up pagination (OLDER-only) never reaches. For those
            // directions we also collect the conversational rows and splice
            // them into the live buffer.
            use crate::irc::chathistory::Direction;
            let direction = batch.params.first().and_then(|target| {
                state
                    .connections
                    .get(conn_id)
                    .and_then(|c| c.chathistory.in_flight_direction(target))
            });
            let is_gapfill = matches!(direction, Some(Direction::After | Direction::Latest));
            let outcome =
                crate::irc::events::ingest_chathistory_batch(state, conn_id, batch, is_gapfill);
            if let Some(target) = batch.params.first() {
                if let Some(conn) = state.connections.get_mut(conn_id) {
                    conn.chathistory.complete_target(
                        target,
                        batch.messages.len(),
                        outcome.oldest,
                        clean_end,
                    );
                }
                // Older rows just landed in SQLite. A buffer that marked itself
                // history-exhausted from a short startup log must re-open so a
                // later scroll-up paginates the freshly ingested rows. Gate on the
                // ingested row count, NOT the raw batch size: a batch of only
                // skipped lines (event-playback, undecryptable, non-ACTION CTCP)
                // stores nothing, so re-opening would leave scrollback re-querying
                // unchanged local history every tick with no new rows to surface
                // (the server is also marked BEFORE-exhausted). Leave the flag set.
                if outcome.ingested > 0 {
                    let buf_id = make_buffer_id(conn_id, target);
                    if let Some(buf) = state.buffers.get_mut(&buf_id) {
                        buf.history_exhausted = false;
                    }
                }
            }
            // Splice gap-fill rows into their live buffers, grouped by the
            // buffer_id resolved during ingest (channel messages and PM echoes
            // may route to different buffers).
            if !outcome.display_rows.is_empty() {
                let mut by_buffer: HashMap<String, Vec<Message>> = HashMap::new();
                for (buf_id, msg) in outcome.display_rows {
                    by_buffer.entry(buf_id).or_default().push(msg);
                }
                for (buf_id, msgs) in by_buffer {
                    state.surface_history_rows(&buf_id, msgs);
                }
            }
        }
        _ => {
            // Unknown batch type — replay messages through the normal handler.
            for msg in &batch.messages {
                crate::irc::events::handle_irc_message(state, conn_id, msg);
            }
        }
    }
}

/// Process a NETSPLIT batch: remove nicks from channels and produce a summary.
///
/// NETSPLIT batch params: `[server1, server2]`
/// Batch contains QUIT messages from users affected by the split.
fn process_netsplit_batch(state: &mut AppState, conn_id: &str, batch: &BatchInfo) {
    let server1 = batch.params.first().map_or("???", String::as_str);
    let server2 = batch.params.get(1).map_or("???", String::as_str);

    let mut nicks: Vec<String> = Vec::new();
    let mut nick_seen: HashSet<String> = HashSet::new();
    let mut affected_buffers: HashMap<String, Vec<String>> = HashMap::new();

    for msg in &batch.messages {
        if let Command::QUIT(_) = &msg.command {
            let Some(nick) = extract_nick(msg.prefix.as_ref()) else {
                continue;
            };

            // Find all buffers this nick is in on this connection.
            // Nick HashMap keys are always lowercase (case-insensitive IRC nicks).
            let nick_lower = nick.to_lowercase();
            let shared: Vec<String> = state
                .buffers
                .iter()
                .filter(|(_, buf)| {
                    buf.connection_id == conn_id && buf.users.contains_key(&nick_lower)
                })
                .map(|(id, _)| id.clone())
                .collect();

            // Remove nick from all buffers
            for buf_id in &shared {
                state.remove_nick(buf_id, &nick_lower);
                affected_buffers
                    .entry(buf_id.clone())
                    .or_default()
                    .push(nick.clone());
            }

            if nick_seen.insert(nick.clone()) {
                nicks.push(nick);
            }
        }
    }

    if nicks.is_empty() {
        return;
    }

    let nick_str = format_nick_list(&nicks);
    let text = format!("Netsplit {server1} \u{21C4} {server2} quits: {nick_str}");

    // Post the summary message to each affected buffer
    let ts = chrono::Utc::now();
    for buf_id in affected_buffers.keys() {
        let id = state.next_message_id();
        state.add_message(
            buf_id,
            Message {
                id,
                timestamp: ts,
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: text.clone(),
                highlight: false,
                event_key: Some("netsplit".to_string()),
                event_params: Some(vec![server1.to_string(), server2.to_string()]),
                log_msg_id: None,
                log_ref_id: None,
                tags: None,
            },
        );
    }
}

/// Process a NETJOIN batch: add nicks back to channels and produce a summary.
///
/// NETJOIN batch params: `[server1, server2]`
/// Batch contains JOIN messages from users rejoining after a split.
fn process_netjoin_batch(state: &mut AppState, conn_id: &str, batch: &BatchInfo) {
    let server1 = batch.params.first().map_or("???", String::as_str);
    let server2 = batch.params.get(1).map_or("???", String::as_str);

    let mut nicks: Vec<String> = Vec::new();
    let mut nick_seen: HashSet<String> = HashSet::new();
    let mut affected_buffers: HashMap<String, bool> = HashMap::new();

    // Directly update nick lists without replaying through handle_irc_message,
    // which would generate individual join display messages we don't want.
    for msg in &batch.messages {
        if let Command::JOIN(channel, account, _) = &msg.command {
            let (nick, _ident, _host) = extract_nick_userhost(msg.prefix.as_ref());
            let buffer_id = make_buffer_id(conn_id, channel);

            // Parse account from extended-join parameter
            let account = match account.as_deref() {
                Some("*") | None => None,
                Some(a) => Some(a.to_string()),
            };

            // Add nick directly to buffer's user list (state mutation only, no message)
            state.add_nick(
                &buffer_id,
                NickEntry {
                    nick: nick.clone(),
                    prefix: String::new(),
                    modes: String::new(),
                    away: false,
                    account,
                    ident: None,
                    host: None,
                },
            );

            affected_buffers.insert(buffer_id, true);
            if nick_seen.insert(nick.clone()) {
                nicks.push(nick);
            }
        }
    }

    if nicks.is_empty() {
        return;
    }

    let nick_str = format_nick_list(&nicks);
    let text = format!("Netsplit over {server1} \u{21C4} {server2} joins: {nick_str}");

    let ts = chrono::Utc::now();
    for buf_id in affected_buffers.keys() {
        let id = state.next_message_id();
        state.add_message(
            buf_id,
            Message {
                id,
                timestamp: ts,
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: text.clone(),
                highlight: false,
                event_key: Some("netjoin".to_string()),
                event_params: Some(vec![server1.to_string(), server2.to_string()]),
                log_msg_id: None,
                log_ref_id: None,
                tags: None,
            },
        );
    }
}

/// Format a list of nicks for display, truncating with "(+N more)" if needed.
fn format_nick_list(nicks: &[String]) -> String {
    if nicks.len() > MAX_NICKS_DISPLAY {
        let shown: Vec<&str> = nicks[..MAX_NICKS_DISPLAY]
            .iter()
            .map(String::as_str)
            .collect();
        let more = nicks.len() - MAX_NICKS_DISPLAY;
        format!("{} (+{more} more)", shown.join(", "))
    } else {
        let refs: Vec<&str> = nicks.iter().map(String::as_str).collect();
        refs.join(", ")
    }
}

/// Check whether the `batch` capability is enabled for a connection.
#[must_use]
#[allow(dead_code)] // Will be used when netsplit heuristic bypass is wired
pub fn has_batch_cap(state: &AppState, conn_id: &str) -> bool {
    state
        .connections
        .get(conn_id)
        .is_some_and(|c| c.enabled_caps.contains("batch"))
}

// === Tests ===

#[cfg(test)]
mod tests {
    use super::*;
    use irc::proto::message::Tag;

    /// Helper to create an `IrcMessage` with a `@batch` tag.
    fn make_batched_message(batch_tag: &str, command: Command) -> IrcMessage {
        IrcMessage {
            tags: Some(vec![Tag("batch".to_string(), Some(batch_tag.to_string()))]),
            prefix: None,
            command,
        }
    }

    /// Helper to create an `IrcMessage` without tags.
    fn make_plain_message(command: Command) -> IrcMessage {
        IrcMessage {
            tags: None,
            prefix: None,
            command,
        }
    }

    /// Helper to create a QUIT message with a prefix and `@batch` tag.
    fn make_quit_msg(nick: &str, reason: &str, batch_tag: &str) -> IrcMessage {
        IrcMessage {
            tags: Some(vec![Tag("batch".to_string(), Some(batch_tag.to_string()))]),
            prefix: Some(irc::proto::Prefix::Nickname(
                nick.to_string(),
                "user".to_string(),
                "host.net".to_string(),
            )),
            command: Command::QUIT(Some(reason.to_string())),
        }
    }

    /// Helper to create a JOIN message with a prefix and `@batch` tag.
    #[allow(dead_code)]
    fn make_join_msg(nick: &str, channel: &str, batch_tag: &str) -> IrcMessage {
        IrcMessage {
            tags: Some(vec![Tag("batch".to_string(), Some(batch_tag.to_string()))]),
            prefix: Some(irc::proto::Prefix::Nickname(
                nick.to_string(),
                "user".to_string(),
                "host.net".to_string(),
            )),
            command: Command::JOIN(channel.to_string(), None, None),
        }
    }

    fn make_test_server_config() -> crate::config::ServerConfig {
        crate::config::ServerConfig {
            label: "Test".to_string(),
            address: "irc.test.net".to_string(),
            port: 6697,
            tls: true,
            tls_verify: true,
            nick: None,
            username: None,
            realname: None,
            password: None,
            sasl_user: None,
            sasl_pass: None,
            bind_ip: None,
            channels: vec!["#test".to_string()],
            encoding: None,
            autoconnect: false,
            auto_reconnect: None,
            reconnect_delay: None,
            reconnect_max_retries: None,
            autosendcmd: None,
            sasl_mechanism: None,
            client_cert_path: None,
        }
    }

    #[test]
    fn start_and_end_batch_collects_messages() {
        let mut tracker = BatchTracker::default();
        tracker.start_batch("abc", "NETSPLIT", vec!["s1.net".into(), "s2.net".into()]);

        let msg1 = make_batched_message("abc", Command::QUIT(Some("split".to_string())));
        let msg2 = make_batched_message("abc", Command::QUIT(Some("split".to_string())));

        assert!(tracker.add_message(msg1));
        assert!(tracker.add_message(msg2));

        let batch = tracker.end_batch("abc").expect("batch should exist");
        assert_eq!(batch.batch_type, "NETSPLIT");
        assert_eq!(batch.params, vec!["s1.net", "s2.net"]);
        assert_eq!(batch.messages.len(), 2);
    }

    #[test]
    fn batch_message_cap_discards_excess() {
        let mut tracker = BatchTracker::default();
        tracker.start_batch("abc", "NETSPLIT", vec![]);

        for _ in 0..MAX_BATCH_MESSAGES + 2 {
            let msg = make_batched_message("abc", Command::QUIT(Some("split".to_string())));
            assert!(tracker.add_message(msg));
        }

        let batch = tracker.end_batch("abc").expect("batch should exist");
        assert_eq!(batch.messages.len(), MAX_BATCH_MESSAGES);
        assert_eq!(batch.dropped_messages, 2);
    }

    #[test]
    fn is_batched_detects_batch_tag() {
        let mut tracker = BatchTracker::default();
        tracker.start_batch("ref1", "NETSPLIT", vec![]);

        let batched = make_batched_message("ref1", Command::QUIT(None));
        let unbatched = make_plain_message(Command::QUIT(None));
        let wrong_tag = make_batched_message("ref2", Command::QUIT(None));

        assert!(tracker.is_batched(&batched));
        assert!(!tracker.is_batched(&unbatched));
        assert!(!tracker.is_batched(&wrong_tag));
    }

    #[test]
    fn end_nonexistent_batch_returns_none() {
        let mut tracker = BatchTracker::default();
        assert!(tracker.end_batch("nonexistent").is_none());
    }

    #[test]
    fn add_message_returns_false_for_unbatched() {
        let mut tracker = BatchTracker::default();
        let msg = make_plain_message(Command::PRIVMSG("#test".into(), "hello".into()));
        assert!(!tracker.add_message(msg));
    }

    #[test]
    fn add_message_returns_false_for_unknown_batch() {
        let mut tracker = BatchTracker::default();
        let msg = make_batched_message("unknown", Command::QUIT(None));
        assert!(!tracker.add_message(msg));
    }

    #[test]
    fn netsplit_batch_produces_summary() {
        let mut state = AppState::new();
        let conn_id = "test";

        // Set up connection and channel buffer with users
        state.add_connection(crate::state::connection::Connection {
            id: conn_id.to_string(),
            label: "Test".to_string(),
            status: crate::state::connection::ConnectionStatus::Connected,
            nick: "me".to_string(),
            user_modes: String::new(),
            isupport: HashMap::new(),
            isupport_parsed: crate::irc::isupport::Isupport::new(),
            error: None,
            lag: None,
            lag_pending: false,
            reconnect_attempts: 0,
            reconnect_delay_secs: 30,
            next_reconnect: None,
            should_reconnect: true,
            joined_channels: vec!["#test".to_string()],
            origin_config: make_test_server_config(),
            enabled_caps: std::collections::HashSet::new(),
            chathistory: crate::irc::chathistory::HistoryState::new(),
            who_token_counter: 0,
            local_ip: None,
            silent_who_channels: std::collections::HashSet::new(),
            silent_banlist_channels: std::collections::HashSet::new(),
        });

        let buf_id = make_buffer_id(conn_id, "#test");
        state.add_buffer(crate::state::buffer::Buffer {
            id: buf_id.clone(),
            connection_id: conn_id.to_string(),
            buffer_type: crate::state::buffer::BufferType::Channel,
            name: "#test".to_string(),
            messages: std::collections::VecDeque::new(),
            activity: crate::state::buffer::ActivityLevel::None,
            unread_count: 0,
            last_read: chrono::Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
            last_speakers: Vec::new(),
            peer_handle: None,
            log_total_lines: None,
            log_oldest_ts: None,
            log_newest_ts: None,
            history_exhausted: false,
            log_initial_loaded: false,
            pin_backlog: false,
        });

        // Add users to the channel
        state.add_nick(
            &buf_id,
            crate::state::buffer::NickEntry {
                nick: "alice".to_string(),
                prefix: String::new(),
                modes: String::new(),
                away: false,
                account: None,
                ident: None,
                host: None,
            },
        );
        state.add_nick(
            &buf_id,
            crate::state::buffer::NickEntry {
                nick: "bob".to_string(),
                prefix: String::new(),
                modes: String::new(),
                away: false,
                account: None,
                ident: None,
                host: None,
            },
        );

        // Create NETSPLIT batch
        let batch = BatchInfo {
            batch_type: "NETSPLIT".to_string(),
            params: vec!["hub.net".to_string(), "leaf.net".to_string()],
            started_at: Instant::now(),
            dropped_messages: 0,
            messages: vec![
                make_quit_msg("alice", "hub.net leaf.net", "ref1"),
                make_quit_msg("bob", "hub.net leaf.net", "ref1"),
            ],
        };

        process_completed_batch(&mut state, conn_id, &batch, true);

        // Nicks should be removed from the buffer
        let buf = state.buffers.get(&buf_id).expect("buffer should exist");
        assert!(!buf.users.contains_key("alice"));
        assert!(!buf.users.contains_key("bob"));

        // A summary message should be added
        assert!(!buf.messages.is_empty());
        let last_msg = buf.messages.back().unwrap();
        assert!(last_msg.text.contains("Netsplit"));
        assert!(last_msg.text.contains("hub.net"));
        assert!(last_msg.text.contains("leaf.net"));
        assert!(last_msg.text.contains("alice"));
        assert!(last_msg.text.contains("bob"));
        assert!(last_msg.text.contains("quits:"));
    }

    #[test]
    fn messages_without_batch_tag_are_not_batched() {
        let mut tracker = BatchTracker::default();
        tracker.start_batch("ref1", "NETSPLIT", vec![]);

        let msg = make_plain_message(Command::PRIVMSG("#test".into(), "hello".into()));
        assert!(!tracker.is_batched(&msg));
        assert!(!tracker.add_message(msg));
    }

    #[test]
    fn multiple_batches_tracked_independently() {
        let mut tracker = BatchTracker::default();
        tracker.start_batch("aaa", "NETSPLIT", vec![]);
        tracker.start_batch("bbb", "NETJOIN", vec![]);

        let msg_a = make_batched_message("aaa", Command::QUIT(None));
        let msg_b = make_batched_message("bbb", Command::JOIN("#test".into(), None, None));

        assert!(tracker.add_message(msg_a));
        assert!(tracker.add_message(msg_b));

        let batch_a = tracker.end_batch("aaa").expect("batch aaa");
        assert_eq!(batch_a.batch_type, "NETSPLIT");
        assert_eq!(batch_a.messages.len(), 1);

        let batch_b = tracker.end_batch("bbb").expect("batch bbb");
        assert_eq!(batch_b.batch_type, "NETJOIN");
        assert_eq!(batch_b.messages.len(), 1);
    }

    #[test]
    fn format_nick_list_under_limit() {
        let nicks: Vec<String> = vec!["alice", "bob", "charlie"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(format_nick_list(&nicks), "alice, bob, charlie");
    }

    #[test]
    fn format_nick_list_over_limit() {
        let nicks: Vec<String> = (0..20).map(|i| format!("nick{i}")).collect();
        let result = format_nick_list(&nicks);
        assert!(result.contains("(+5 more)"));
        assert!(result.contains("nick0"));
        assert!(result.contains("nick14"));
        assert!(!result.contains("nick15"));
    }

    #[test]
    fn batch_type_case_normalized() {
        let mut tracker = BatchTracker::default();
        tracker.start_batch("ref1", "netsplit", vec![]);

        let batch = tracker.end_batch("ref1").expect("batch should exist");
        assert_eq!(batch.batch_type, "NETSPLIT");
    }

    #[test]
    fn purge_expired_removes_old_batches() {
        let mut tracker = BatchTracker::default();
        // Manually insert a batch with an old timestamp
        tracker.open.insert(
            "old".to_string(),
            BatchInfo {
                batch_type: "NETSPLIT".to_string(),
                params: vec![],
                messages: vec![],
                dropped_messages: 0,
                started_at: Instant::now()
                    .checked_sub(std::time::Duration::from_mins(2))
                    .unwrap(),
            },
        );
        // Fresh batch should survive
        tracker.start_batch("fresh", "NETJOIN", vec![]);

        let purged = tracker.purge_expired();
        assert_eq!(purged.len(), 1);
        assert_eq!(purged[0].batch_type, "NETSPLIT");
        assert!(tracker.end_batch("old").is_none());
        assert!(tracker.end_batch("fresh").is_some());
    }

    #[test]
    fn purge_expired_keeps_fresh_batches() {
        let mut tracker = BatchTracker::default();
        tracker.start_batch("a", "NETSPLIT", vec![]);
        tracker.start_batch("b", "NETJOIN", vec![]);

        let purged = tracker.purge_expired();
        assert!(purged.is_empty());
        assert_eq!(tracker.open.len(), 2);
    }

    #[test]
    fn purge_expired_returns_messages_for_replay() {
        // An expired batch must surface its buffered messages so the caller
        // can replay them through the normal handler — otherwise QUITs hidden
        // inside an unterminated netsplit batch leak as stale nicks.
        let mut tracker = BatchTracker::default();
        tracker.open.insert(
            "old".to_string(),
            BatchInfo {
                batch_type: "NETSPLIT".to_string(),
                params: vec!["hub.example".to_string(), "leaf.example".to_string()],
                messages: vec![IrcMessage {
                    tags: None,
                    prefix: Some(irc::proto::Prefix::Nickname(
                        "alice".to_string(),
                        "ali".to_string(),
                        "h.example".to_string(),
                    )),
                    command: Command::QUIT(Some("hub.example leaf.example".to_string())),
                }],
                dropped_messages: 0,
                started_at: Instant::now()
                    .checked_sub(std::time::Duration::from_mins(2))
                    .unwrap(),
            },
        );

        let purged = tracker.purge_expired();
        assert_eq!(purged.len(), 1);
        assert_eq!(purged[0].messages.len(), 1);
        assert_eq!(purged[0].params.len(), 2);
    }

    /// Build a connection + one channel buffer with `alice` present, wired to
    /// a log channel. Returns `(state, rx, buf_id)`.
    fn setup_ingest_state(
        conn_id: &str,
    ) -> (
        AppState,
        tokio::sync::mpsc::Receiver<crate::storage::types::LogRow>,
        String,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let mut state = AppState::new();
        state.log_tx = Some(tx);
        state.add_connection(crate::state::connection::Connection {
            id: conn_id.to_string(),
            label: "libera".to_string(),
            status: crate::state::connection::ConnectionStatus::Connected,
            nick: "me".to_string(),
            user_modes: String::new(),
            isupport: HashMap::new(),
            isupport_parsed: crate::irc::isupport::Isupport::new(),
            error: None,
            lag: None,
            lag_pending: false,
            reconnect_attempts: 0,
            reconnect_delay_secs: 30,
            next_reconnect: None,
            should_reconnect: true,
            joined_channels: vec!["#test".to_string()],
            origin_config: make_test_server_config(),
            enabled_caps: std::collections::HashSet::new(),
            chathistory: crate::irc::chathistory::HistoryState::new(),
            who_token_counter: 0,
            local_ip: None,
            silent_who_channels: std::collections::HashSet::new(),
            silent_banlist_channels: std::collections::HashSet::new(),
        });
        let buf_id = make_buffer_id(conn_id, "#test");
        state.add_buffer(crate::state::buffer::Buffer {
            id: buf_id.clone(),
            connection_id: conn_id.to_string(),
            buffer_type: crate::state::buffer::BufferType::Channel,
            name: "#test".to_string(),
            messages: std::collections::VecDeque::new(),
            activity: crate::state::buffer::ActivityLevel::None,
            unread_count: 0,
            last_read: chrono::Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
            last_speakers: Vec::new(),
            peer_handle: None,
            log_total_lines: None,
            log_oldest_ts: None,
            log_newest_ts: None,
            history_exhausted: false,
            log_initial_loaded: false,
            pin_backlog: false,
        });
        state.add_nick(
            &buf_id,
            NickEntry {
                nick: "alice".to_string(),
                prefix: String::new(),
                modes: String::new(),
                away: false,
                account: None,
                ident: None,
                host: None,
            },
        );
        (state, rx, buf_id)
    }

    fn make_history_privmsg(nick: &str, target: &str, text: &str, msgid: &str) -> IrcMessage {
        make_history_privmsg_at(nick, target, text, msgid, "2024-01-01T00:00:00.000Z")
    }

    fn make_history_privmsg_at(
        nick: &str,
        target: &str,
        text: &str,
        msgid: &str,
        time: &str,
    ) -> IrcMessage {
        IrcMessage {
            tags: Some(vec![
                Tag("batch".to_string(), Some("h1".to_string())),
                Tag("time".to_string(), Some(time.to_string())),
                Tag("msgid".to_string(), Some(msgid.to_string())),
            ]),
            prefix: Some(irc::proto::Prefix::Nickname(
                nick.to_string(),
                "u".to_string(),
                "h.net".to_string(),
            )),
            command: Command::PRIVMSG(target.to_string(), text.to_string()),
        }
    }

    #[test]
    fn chathistory_batch_ingests_conversational_store_only() {
        let conn_id = "test";
        let (mut state, mut rx, buf_id) = setup_ingest_state(conn_id);

        let batch = BatchInfo {
            batch_type: "CHATHISTORY".to_string(),
            params: vec!["#test".to_string()],
            started_at: Instant::now(),
            dropped_messages: 0,
            messages: vec![
                make_history_privmsg("bob", "#test", "hello", "m1"),
                make_history_privmsg("carol", "#test", "hi there", "m2"),
                // Event-playback line — must be skipped in v1 (no live mutation).
                make_join_msg("dave", "#test", "h1"),
            ],
        };

        process_completed_batch(&mut state, conn_id, &batch, true);

        // Two conversational rows persisted, in order; JOIN not persisted.
        let r1 = rx.try_recv().expect("first history row");
        assert_eq!(r1.msg_id, "m1");
        assert_eq!(r1.text, "hello");
        assert_eq!(r1.msg_type, MessageType::Message);
        assert_eq!(r1.timestamp, 1_704_067_200); // 2024-01-01T00:00:00Z
        let r2 = rx.try_recv().expect("second history row");
        assert_eq!(r2.msg_id, "m2");
        assert!(rx.try_recv().is_err(), "JOIN must not be persisted in v1");

        // Store-only: no live display, no nicklist mutation.
        let buf = state.buffers.get(&buf_id).expect("buffer exists");
        assert!(buf.messages.is_empty(), "history must not display live");
        assert!(buf.users.contains_key("alice"), "live nicklist unchanged");
        assert!(
            !buf.users.contains_key("dave"),
            "history JOIN must not mutate live nicklist"
        );
    }

    #[test]
    fn chathistory_stores_same_second_rows_in_chronological_order() {
        // The DB keeps only whole-second timestamps and breaks ties by insertion
        // id, so a same-second page returned newest-first must be persisted
        // oldest-first or it reloads in reverse order within that second.
        let conn_id = "test";
        let (mut state, mut rx, _buf_id) = setup_ingest_state(conn_id);

        let batch = BatchInfo {
            batch_type: "CHATHISTORY".to_string(),
            params: vec!["#test".to_string()],
            started_at: Instant::now(),
            dropped_messages: 0,
            messages: vec![
                make_history_privmsg_at("c", "#test", "third", "m3", "2024-01-01T00:00:05.800Z"),
                make_history_privmsg_at("b", "#test", "second", "m2", "2024-01-01T00:00:05.500Z"),
                make_history_privmsg_at("a", "#test", "first", "m1", "2024-01-01T00:00:05.200Z"),
            ],
        };

        process_completed_batch(&mut state, conn_id, &batch, true);

        let mut texts = Vec::new();
        while let Ok(row) = rx.try_recv() {
            texts.push(row.text);
        }
        assert_eq!(texts, vec!["first", "second", "third"]);
    }

    #[test]
    fn chathistory_watermark_keeps_millis_and_msgid() {
        let conn_id = "test";
        let (mut state, _rx, _buf_id) = setup_ingest_state(conn_id);
        // A BEFORE request is in flight — only a BEFORE completion advances the
        // scroll-back watermark.
        state
            .connections
            .get_mut(conn_id)
            .unwrap()
            .chathistory
            .mark_in_flight("#test", crate::irc::chathistory::Direction::Before, 200);

        // Two lines in the same second (batch order newest-first); the oldest
        // is at .200 with msgid "old". The next-BEFORE watermark must record
        // its full millisecond time and its msgid — not a floored second.
        let batch = BatchInfo {
            batch_type: "CHATHISTORY".to_string(),
            params: vec!["#test".to_string()],
            started_at: Instant::now(),
            dropped_messages: 0,
            messages: vec![
                make_history_privmsg_at(
                    "carol",
                    "#test",
                    "second",
                    "new",
                    "2024-01-01T00:00:00.800Z",
                ),
                make_history_privmsg_at(
                    "bob",
                    "#test",
                    "first",
                    "old",
                    "2024-01-01T00:00:00.200Z",
                ),
            ],
        };

        process_completed_batch(&mut state, conn_id, &batch, true);

        let conn = state.connections.get(conn_id).expect("connection exists");
        let (ms, msgid) = conn
            .chathistory
            .oldest_fetched("#test")
            .expect("watermark recorded");
        assert_eq!(ms, 1_704_067_200_200, "subsecond precision preserved");
        assert_eq!(msgid.as_deref(), Some("old"), "oldest line's msgid kept");
    }

    #[test]
    fn chathistory_skips_undecryptable_e2e_lines() {
        // An E2E ciphertext line we can't decrypt (no session) must NOT be
        // stored as raw +RPE2E01 wire — that would show ciphertext on scroll-up
        // and poison @msgid dedup against a later live plaintext row.
        let conn_id = "test";
        let (mut state, mut rx, _buf_id) = setup_ingest_state(conn_id);

        let batch = BatchInfo {
            batch_type: "CHATHISTORY".to_string(),
            params: vec!["#test".to_string()],
            started_at: Instant::now(),
            dropped_messages: 0,
            messages: vec![
                make_history_privmsg("bob", "#test", "+RPE2E01ciphertext", "e1"),
                make_history_privmsg("carol", "#test", "plain text", "p1"),
            ],
        };

        process_completed_batch(&mut state, conn_id, &batch, true);

        let mut texts = Vec::new();
        while let Ok(row) = rx.try_recv() {
            texts.push(row.text);
        }
        assert_eq!(texts, vec!["plain text"], "undecryptable E2E line is skipped");
    }

    #[test]
    fn chathistory_batch_strips_ctcp_action() {
        let conn_id = "test";
        let (mut state, mut rx, _buf_id) = setup_ingest_state(conn_id);

        let action = make_history_privmsg("bob", "#test", "\u{1}ACTION waves\u{1}", "a1");
        let other_ctcp = make_history_privmsg("bob", "#test", "\u{1}VERSION\u{1}", "c1");
        let batch = BatchInfo {
            batch_type: "CHATHISTORY".to_string(),
            params: vec!["#test".to_string()],
            started_at: Instant::now(),
            dropped_messages: 0,
            messages: vec![action, other_ctcp],
        };

        process_completed_batch(&mut state, conn_id, &batch, true);

        let r1 = rx.try_recv().expect("action row");
        assert_eq!(r1.msg_type, MessageType::Action);
        assert_eq!(r1.text, "waves");
        assert!(
            rx.try_recv().is_err(),
            "non-ACTION CTCP must be skipped in v1"
        );
    }

    #[test]
    fn gapfill_after_batch_splices_rows_into_live_buffer() {
        use crate::state::buffer::Message;
        let conn_id = "test";
        let (mut state, _rx, buf_id) = setup_ingest_state(conn_id);

        // An existing live message sits in the buffer at 00:00:02, tagged with
        // its server @msgid.
        let live_ts = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:02.000Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let mut live_tags = HashMap::new();
        live_tags.insert("msgid".to_string(), "live1".to_string());
        state
            .buffers
            .get_mut(&buf_id)
            .unwrap()
            .messages
            .push_back(Message {
                id: 1,
                timestamp: live_ts,
                message_type: MessageType::Message,
                nick: Some("bob".to_string()),
                nick_mode: None,
                text: "live line".to_string(),
                highlight: false,
                event_key: None,
                event_params: None,
                log_msg_id: None,
                log_ref_id: None,
                tags: Some(live_tags),
            });

        // A reconnect AFTER gap-fill is in flight for this target.
        state
            .connections
            .get_mut(conn_id)
            .unwrap()
            .chathistory
            .mark_in_flight("#test", crate::irc::chathistory::Direction::After, 200);

        // The batch carries one older missed line, one newer one, and a replay
        // of the message already shown live (same @msgid → must dedup).
        let batch = BatchInfo {
            batch_type: "CHATHISTORY".to_string(),
            params: vec!["#test".to_string()],
            started_at: Instant::now(),
            dropped_messages: 0,
            messages: vec![
                make_history_privmsg_at("carol", "#test", "earlier", "gap1", "2024-01-01T00:00:01.000Z"),
                make_history_privmsg_at("dave", "#test", "later", "gap2", "2024-01-01T00:00:03.000Z"),
                make_history_privmsg_at("bob", "#test", "live line", "live1", "2024-01-01T00:00:02.000Z"),
            ],
        };

        process_completed_batch(&mut state, conn_id, &batch, true);

        let texts: Vec<&str> = state
            .buffers
            .get(&buf_id)
            .unwrap()
            .messages
            .iter()
            .map(|m| m.text.as_str())
            .collect();
        // Gap rows spliced in timestamp order; the @msgid duplicate skipped.
        assert_eq!(texts, vec!["earlier", "live line", "later"]);
    }

    #[test]
    fn gapfill_after_batch_broadcasts_web_events() {
        // Splicing into buf.messages bypasses add_message's web-event queue, so
        // surface_history_rows must broadcast the row itself — otherwise
        // connected web clients never see the gap-fill rows (they aren't
        // reachable by older-only pagination) until a full resync. It uses
        // InsertMessage (sorted insert), not NewMessage (append).
        let conn_id = "test";
        let (mut state, _rx, _buf_id) = setup_ingest_state(conn_id);
        state
            .connections
            .get_mut(conn_id)
            .unwrap()
            .chathistory
            .mark_in_flight("#test", crate::irc::chathistory::Direction::After, 200);

        let batch = BatchInfo {
            batch_type: "CHATHISTORY".to_string(),
            params: vec!["#test".to_string()],
            started_at: Instant::now(),
            dropped_messages: 0,
            messages: vec![make_history_privmsg("bob", "#test", "gap line", "g1")],
        };

        process_completed_batch(&mut state, conn_id, &batch, true);

        let broadcast = state
            .pending_web_events
            .iter()
            .filter(|e| matches!(e, crate::web::protocol::WebEvent::InsertMessage { .. }))
            .count();
        assert_eq!(broadcast, 1, "spliced gap-fill row must be broadcast to web clients");
    }

    #[test]
    fn before_batch_does_not_splice_into_live_buffer() {
        // A BEFORE scroll-back is store-only — its rows surface via pagination,
        // never spliced live — so the in-memory buffer stays untouched.
        let conn_id = "test";
        let (mut state, _rx, buf_id) = setup_ingest_state(conn_id);
        state
            .connections
            .get_mut(conn_id)
            .unwrap()
            .chathistory
            .mark_in_flight("#test", crate::irc::chathistory::Direction::Before, 200);

        let batch = BatchInfo {
            batch_type: "CHATHISTORY".to_string(),
            params: vec!["#test".to_string()],
            started_at: Instant::now(),
            dropped_messages: 0,
            messages: vec![make_history_privmsg("bob", "#test", "older line", "b1")],
        };

        process_completed_batch(&mut state, conn_id, &batch, true);

        assert!(
            state.buffers.get(&buf_id).unwrap().messages.is_empty(),
            "BEFORE rows are store-only, not spliced into the live buffer"
        );
    }

    #[test]
    fn chathistory_batch_clears_buffer_history_exhausted() {
        // A buffer whose tiny local log was exhausted at startup
        // (history_exhausted = true) must become re-paginable once a
        // CHATHISTORY batch ingests older rows into SQLite — otherwise the
        // freshly fetched rows never get pulled into the buffer on scroll-up.
        let conn_id = "test";
        let (mut state, _rx, buf_id) = setup_ingest_state(conn_id);
        state
            .buffers
            .get_mut(&buf_id)
            .expect("buffer exists")
            .history_exhausted = true;

        let batch = BatchInfo {
            batch_type: "CHATHISTORY".to_string(),
            params: vec!["#test".to_string()],
            started_at: Instant::now(),
            dropped_messages: 0,
            messages: vec![make_history_privmsg("bob", "#test", "older line", "m1")],
        };

        process_completed_batch(&mut state, conn_id, &batch, true);

        assert!(
            !state
                .buffers
                .get(&buf_id)
                .expect("buffer exists")
                .history_exhausted,
            "ingesting older rows must re-open the buffer for pagination"
        );
    }

    #[test]
    fn empty_chathistory_batch_keeps_history_exhausted() {
        // A batch that ingested nothing leaves history_exhausted untouched, so
        // we don't trigger a pointless re-pagination of unchanged local rows.
        let conn_id = "test";
        let (mut state, _rx, buf_id) = setup_ingest_state(conn_id);
        state
            .buffers
            .get_mut(&buf_id)
            .expect("buffer exists")
            .history_exhausted = true;

        let batch = BatchInfo {
            batch_type: "CHATHISTORY".to_string(),
            params: vec!["#test".to_string()],
            started_at: Instant::now(),
            dropped_messages: 0,
            messages: vec![],
        };

        process_completed_batch(&mut state, conn_id, &batch, true);

        assert!(
            state
                .buffers
                .get(&buf_id)
                .expect("buffer exists")
                .history_exhausted,
            "an empty batch must not reset the exhausted flag"
        );
    }

    #[test]
    fn skipped_only_chathistory_batch_keeps_history_exhausted() {
        // A batch whose lines are ALL skipped (event-playback JOINs, etc.)
        // ingests zero SQLite rows, so there is nothing new for pagination to
        // surface. The buffer's `history_exhausted` flag must stay set — basing
        // the re-open on the raw batch message count instead of the ingested
        // row count leaves scrollback re-querying unchanged local history every
        // tick (the server is also marked BEFORE-exhausted, so no new rows ever
        // arrive). [P2 review]
        let conn_id = "test";
        let (mut state, _rx, buf_id) = setup_ingest_state(conn_id);
        state
            .buffers
            .get_mut(&buf_id)
            .expect("buffer exists")
            .history_exhausted = true;

        let batch = BatchInfo {
            batch_type: "CHATHISTORY".to_string(),
            params: vec!["#test".to_string()],
            started_at: Instant::now(),
            dropped_messages: 0,
            messages: vec![
                make_join_msg("dave", "#test", "h1"),
                make_join_msg("erin", "#test", "h1"),
            ],
        };

        process_completed_batch(&mut state, conn_id, &batch, true);

        assert!(
            state
                .buffers
                .get(&buf_id)
                .expect("buffer exists")
                .history_exhausted,
            "a batch that ingested no rows must not reset the exhausted flag"
        );
    }

    #[test]
    fn clean_short_chathistory_batch_marks_before_exhausted() {
        // Baseline: a normally-closed BEFORE batch shorter than the requested
        // page is genuine end-of-history and DOES mark the target exhausted.
        let conn_id = "test";
        let (mut state, _rx, _buf_id) = setup_ingest_state(conn_id);
        state
            .connections
            .get_mut(conn_id)
            .expect("connection exists")
            .chathistory
            .mark_in_flight("#test", crate::irc::chathistory::Direction::Before, 200);

        let batch = BatchInfo {
            batch_type: "CHATHISTORY".to_string(),
            params: vec!["#test".to_string()],
            started_at: Instant::now(),
            dropped_messages: 0,
            messages: vec![make_history_privmsg("bob", "#test", "only line", "m1")],
        };

        process_completed_batch(&mut state, conn_id, &batch, true);

        assert!(
            state
                .connections
                .get(conn_id)
                .expect("connection exists")
                .chathistory
                .is_before_exhausted("#test"),
            "a clean short BEFORE batch is genuine end-of-history"
        );
    }

    #[test]
    fn timed_out_chathistory_batch_does_not_mark_before_exhausted() {
        // A CHATHISTORY batch force-completed by the timeout purge (its
        // `BATCH -tag` never arrived) is a transport failure. It must release
        // the in-flight lock so scroll-up can retry, but must NOT mark BEFORE
        // exhausted — otherwise the target is wedged until reconnect.
        let conn_id = "test";
        let (mut state, _rx, _buf_id) = setup_ingest_state(conn_id);
        state
            .connections
            .get_mut(conn_id)
            .expect("connection exists")
            .chathistory
            .mark_in_flight("#test", crate::irc::chathistory::Direction::Before, 200);

        let batch = BatchInfo {
            batch_type: "CHATHISTORY".to_string(),
            params: vec!["#test".to_string()],
            started_at: Instant::now(),
            dropped_messages: 0,
            messages: vec![make_history_privmsg("bob", "#test", "partial line", "m1")],
        };

        process_completed_batch(&mut state, conn_id, &batch, false);

        let conn = state.connections.get(conn_id).expect("connection exists");
        assert!(
            !conn.chathistory.is_before_exhausted("#test"),
            "a timed-out batch must not be treated as end-of-history"
        );
        assert!(
            conn.chathistory
                .should_request("#test", crate::irc::chathistory::Direction::Before, true),
            "in-flight cleared so a later scroll-up retries the fetch"
        );
    }

    #[test]
    fn netsplit_batch_removes_nicks_case_insensitive() {
        let mut state = AppState::new();
        let conn_id = "test";

        state.add_connection(crate::state::connection::Connection {
            id: conn_id.to_string(),
            label: "Test".to_string(),
            status: crate::state::connection::ConnectionStatus::Connected,
            nick: "me".to_string(),
            user_modes: String::new(),
            isupport: HashMap::new(),
            isupport_parsed: crate::irc::isupport::Isupport::new(),
            error: None,
            lag: None,
            lag_pending: false,
            reconnect_attempts: 0,
            reconnect_delay_secs: 30,
            next_reconnect: None,
            should_reconnect: true,
            joined_channels: vec!["#test".to_string()],
            origin_config: make_test_server_config(),
            enabled_caps: std::collections::HashSet::new(),
            chathistory: crate::irc::chathistory::HistoryState::new(),
            who_token_counter: 0,
            local_ip: None,
            silent_who_channels: std::collections::HashSet::new(),
            silent_banlist_channels: std::collections::HashSet::new(),
        });

        let buf_id = make_buffer_id(conn_id, "#test");
        state.add_buffer(crate::state::buffer::Buffer {
            id: buf_id.clone(),
            connection_id: conn_id.to_string(),
            buffer_type: crate::state::buffer::BufferType::Channel,
            name: "#test".to_string(),
            messages: std::collections::VecDeque::new(),
            activity: crate::state::buffer::ActivityLevel::None,
            unread_count: 0,
            last_read: chrono::Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
            last_speakers: Vec::new(),
            peer_handle: None,
            log_total_lines: None,
            log_oldest_ts: None,
            log_newest_ts: None,
            history_exhausted: false,
            log_initial_loaded: false,
            pin_backlog: false,
        });

        // Add users — add_nick stores keys as lowercase
        state.add_nick(
            &buf_id,
            NickEntry {
                nick: "Alice".to_string(),
                prefix: String::new(),
                modes: String::new(),
                away: false,
                account: None,
                ident: None,
                host: None,
            },
        );
        state.add_nick(
            &buf_id,
            NickEntry {
                nick: "BOB".to_string(),
                prefix: String::new(),
                modes: String::new(),
                away: false,
                account: None,
                ident: None,
                host: None,
            },
        );

        // QUIT messages use mixed-case nicks (as received from IRC)
        let batch = BatchInfo {
            batch_type: "NETSPLIT".to_string(),
            params: vec!["hub.net".to_string(), "leaf.net".to_string()],
            started_at: Instant::now(),
            dropped_messages: 0,
            messages: vec![
                make_quit_msg("Alice", "hub.net leaf.net", "ref1"),
                make_quit_msg("BOB", "hub.net leaf.net", "ref1"),
            ],
        };

        process_completed_batch(&mut state, conn_id, &batch, true);

        // Nicks should be removed despite case mismatch between IRC prefix and HashMap key
        let buf = state.buffers.get(&buf_id).expect("buffer should exist");
        assert!(!buf.users.contains_key("alice"), "alice should be removed");
        assert!(!buf.users.contains_key("bob"), "bob should be removed");
        assert_eq!(buf.users.len(), 0, "all users should be removed");

        // Summary message should still be present
        assert!(!buf.messages.is_empty());
        let last_msg = buf.messages.back().unwrap();
        assert!(last_msg.text.contains("Netsplit"));
        assert!(last_msg.text.contains("Alice"));
        assert!(last_msg.text.contains("BOB"));
    }
}

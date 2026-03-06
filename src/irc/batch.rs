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

use irc::proto::{Command, Message as IrcMessage};

use crate::irc::formatting::{extract_nick, extract_nick_userhost};
use crate::state::AppState;
use crate::state::buffer::{Message, MessageType, NickEntry, make_buffer_id};

/// Maximum number of nicks to show in a netsplit/netjoin summary line.
const MAX_NICKS_DISPLAY: usize = 15;

/// Information about an in-progress batch.
#[derive(Debug, Clone)]
pub struct BatchInfo {
    /// The batch type (e.g. "NETSPLIT", "NETJOIN", or a vendor extension).
    pub batch_type: String,
    /// Additional parameters from the BATCH start line.
    pub params: Vec<String>,
    /// Messages collected while the batch was open.
    pub messages: Vec<IrcMessage>,
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
            },
        );
    }

    /// Check whether a message belongs to an open batch via its `@batch` tag.
    #[must_use]
    pub fn is_batched(&self, msg: &IrcMessage) -> bool {
        Self::get_batch_tag(msg)
            .is_some_and(|tag| self.open.contains_key(tag))
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
pub fn process_completed_batch(
    state: &mut AppState,
    conn_id: &str,
    batch: &BatchInfo,
) {
    match batch.batch_type.as_str() {
        "NETSPLIT" => process_netsplit_batch(state, conn_id, batch),
        "NETJOIN" => process_netjoin_batch(state, conn_id, batch),
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

            // Find all buffers this nick is in on this connection
            let shared: Vec<String> = state
                .buffers
                .iter()
                .filter(|(_, buf)| {
                    buf.connection_id == conn_id
                        && buf.users.contains_key(&nick)
                })
                .map(|(id, _)| id.clone())
                .collect();

            // Remove nick from all buffers
            for buf_id in &shared {
                state.remove_nick(buf_id, &nick);
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
                tags: HashMap::new(),
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
                tags: HashMap::new(),
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
            reconnect_attempts: 0,
            max_reconnect_attempts: 10,
            reconnect_delay_secs: 30,
            next_reconnect: None,
            should_reconnect: true,
            joined_channels: vec!["#test".to_string()],
            origin_config: make_test_server_config(),
            enabled_caps: std::collections::HashSet::new(),
            who_token_counter: 0,
            silent_who_channels: std::collections::HashSet::new(),
        });

        let buf_id = make_buffer_id(conn_id, "#test");
        state.add_buffer(crate::state::buffer::Buffer {
            id: buf_id.clone(),
            connection_id: conn_id.to_string(),
            buffer_type: crate::state::buffer::BufferType::Channel,
            name: "#test".to_string(),
            messages: Vec::new(),
            activity: crate::state::buffer::ActivityLevel::None,
            unread_count: 0,
            last_read: chrono::Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
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
            messages: vec![
                make_quit_msg("alice", "hub.net leaf.net", "ref1"),
                make_quit_msg("bob", "hub.net leaf.net", "ref1"),
            ],
        };

        process_completed_batch(&mut state, conn_id, &batch);

        // Nicks should be removed from the buffer
        let buf = state.buffers.get(&buf_id).expect("buffer should exist");
        assert!(!buf.users.contains_key("alice"));
        assert!(!buf.users.contains_key("bob"));

        // A summary message should be added
        assert!(!buf.messages.is_empty());
        let last_msg = buf.messages.last().unwrap();
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
}

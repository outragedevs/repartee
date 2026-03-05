use chrono::Utc;
use irc::proto::{Command, Message as IrcMessage, Prefix, Response};

use crate::irc::formatting::{extract_nick, is_channel, is_server_prefix, prefix_to_mode, split_nick_prefix};
use crate::state::buffer::*;
use crate::state::connection::ConnectionStatus;
use crate::state::AppState;

/// Route an incoming IRC protocol message to the appropriate handler,
/// mutating AppState as needed.
pub fn handle_irc_message(state: &mut AppState, conn_id: &str, msg: &IrcMessage) {
    let our_nick = state
        .connections
        .get(conn_id)
        .map(|c| c.nick.clone())
        .unwrap_or_default();

    match &msg.command {
        Command::PRIVMSG(target, text) => {
            handle_privmsg(state, conn_id, &our_nick, &msg.prefix, target, text);
        }
        Command::NOTICE(target, text) => {
            handle_notice(state, conn_id, &msg.prefix, target, text);
        }
        Command::JOIN(channel, _, _) => {
            handle_join(state, conn_id, &our_nick, &msg.prefix, channel);
        }
        Command::PART(channel, reason) => {
            handle_part(
                state,
                conn_id,
                &our_nick,
                &msg.prefix,
                channel,
                reason.as_deref(),
            );
        }
        Command::QUIT(reason) => {
            handle_quit(state, conn_id, &our_nick, &msg.prefix, reason.as_deref());
        }
        Command::NICK(new_nick) => {
            handle_nick_change(state, conn_id, &our_nick, &msg.prefix, new_nick);
        }
        Command::KICK(channel, kicked, reason) => {
            handle_kick(
                state,
                conn_id,
                &our_nick,
                &msg.prefix,
                channel,
                kicked,
                reason.as_deref(),
            );
        }
        Command::TOPIC(channel, topic) => {
            handle_topic(state, conn_id, &msg.prefix, channel, topic.as_deref());
        }
        Command::Response(response, args) => {
            handle_response(state, conn_id, *response, args);
        }
        Command::PING(..) => {
            // Handled automatically by the irc crate
        }
        _ => {}
    }
}

/// Update connection status to Connected and log to the status buffer.
pub fn handle_connected(state: &mut AppState, conn_id: &str) {
    state.update_connection_status(conn_id, ConnectionStatus::Connected);

    let label = state
        .connections
        .get(conn_id)
        .map(|c| c.label.clone())
        .unwrap_or_else(|| conn_id.to_string());
    let buffer_id = make_buffer_id(conn_id, &label);

    let id = state.next_message_id();
    state.add_message(
        &buffer_id,
        Message {
            id,
            timestamp: Utc::now(),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text: format!("Connected to {label}"),
            highlight: false,
            event_key: Some("connected".to_string()),
            event_params: None,
        },
    );
}

/// Update connection status to Disconnected and log to the status buffer.
pub fn handle_disconnected(state: &mut AppState, conn_id: &str, error: Option<&str>) {
    if let Some(err) = error {
        if let Some(conn) = state.connections.get_mut(conn_id) {
            conn.status = ConnectionStatus::Error;
            conn.error = Some(err.to_string());
        }
    } else {
        state.update_connection_status(conn_id, ConnectionStatus::Disconnected);
    }

    let label = state
        .connections
        .get(conn_id)
        .map(|c| c.label.clone())
        .unwrap_or_else(|| conn_id.to_string());
    let buffer_id = make_buffer_id(conn_id, &label);

    let msg_text = match error {
        Some(e) => format!("Disconnected from {label}: {e}"),
        None => format!("Disconnected from {label}"),
    };

    let id = state.next_message_id();
    state.add_message(
        &buffer_id,
        Message {
            id,
            timestamp: Utc::now(),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text: msg_text,
            highlight: false,
            event_key: Some("disconnected".to_string()),
            event_params: None,
        },
    );
}

// === Private handlers ===

fn handle_privmsg(
    state: &mut AppState,
    conn_id: &str,
    our_nick: &str,
    prefix: &Option<Prefix>,
    target: &str,
    text: &str,
) {
    let nick = extract_nick(prefix).unwrap_or_default();
    let target_is_channel = is_channel(target);
    let buffer_name = if target_is_channel { target } else { &nick };
    let buffer_id = make_buffer_id(conn_id, buffer_name);

    // Create query buffer if it doesn't exist for PMs
    if !target_is_channel && !state.buffers.contains_key(&buffer_id) {
        state.add_buffer(Buffer {
            id: buffer_id.clone(),
            connection_id: conn_id.to_string(),
            buffer_type: BufferType::Query,
            name: nick.clone(),
            messages: Vec::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: Utc::now(),
            topic: None,
            topic_set_by: None,
            users: std::collections::HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: std::collections::HashMap::new(),
        });
    }

    // Check if this is a CTCP ACTION
    if text.starts_with('\x01') && text.ends_with('\x01') {
        let inner = &text[1..text.len() - 1];
        if let Some(action_text) = inner.strip_prefix("ACTION ") {
            let is_own = nick == our_nick;
            let activity = if !is_own && !target_is_channel {
                ActivityLevel::Mention
            } else if !is_own {
                ActivityLevel::Activity
            } else {
                ActivityLevel::None
            };
            let id = state.next_message_id();
            state.add_message_with_activity(
                &buffer_id,
                Message {
                    id,
                    timestamp: Utc::now(),
                    message_type: MessageType::Action,
                    nick: Some(nick),
                    nick_mode: None,
                    text: action_text.to_string(),
                    highlight: false,
                    event_key: None,
                    event_params: None,
                },
                activity,
            );
            return;
        }
        // Other CTCP, ignore for now
        return;
    }

    let is_own = nick == our_nick;
    let is_mention = !is_own && text.to_lowercase().contains(&our_nick.to_lowercase());

    let activity = if is_own {
        ActivityLevel::None
    } else if !target_is_channel || is_mention {
        ActivityLevel::Mention // PMs and mentions are mention-level
    } else {
        ActivityLevel::Activity
    };

    let id = state.next_message_id();
    state.add_message_with_activity(
        &buffer_id,
        Message {
            id,
            timestamp: Utc::now(),
            message_type: MessageType::Message,
            nick: Some(nick),
            nick_mode: None,
            text: text.to_string(),
            highlight: is_mention,
            event_key: None,
            event_params: None,
        },
        activity,
    );
}

fn handle_notice(
    state: &mut AppState,
    conn_id: &str,
    prefix: &Option<Prefix>,
    target: &str,
    text: &str,
) {
    let nick = extract_nick(prefix);
    // Server notices or pre-registration notices go to status buffer
    let is_server_notice = nick.is_none() || is_server_prefix(prefix);

    let buffer_name = if is_server_notice {
        state
            .connections
            .get(conn_id)
            .map(|c| c.label.as_str())
            .unwrap_or("Status")
    } else if is_channel(target) {
        target
    } else {
        nick.as_deref().unwrap_or("Status")
    };

    let buffer_id = make_buffer_id(conn_id, buffer_name);
    // Fallback to server buffer if target buffer doesn't exist
    let buffer_id = if state.buffers.contains_key(&buffer_id) {
        buffer_id
    } else {
        let label = state
            .connections
            .get(conn_id)
            .map(|c| c.label.as_str())
            .unwrap_or("Status");
        make_buffer_id(conn_id, label)
    };

    let id = state.next_message_id();
    state.add_message(
        &buffer_id,
        Message {
            id,
            timestamp: Utc::now(),
            message_type: MessageType::Notice,
            nick,
            nick_mode: None,
            text: text.to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
        },
    );
}

fn handle_join(
    state: &mut AppState,
    conn_id: &str,
    our_nick: &str,
    prefix: &Option<Prefix>,
    channel: &str,
) {
    let nick = extract_nick(prefix).unwrap_or_default();
    let buffer_id = make_buffer_id(conn_id, channel);

    if nick == our_nick {
        // We joined — create buffer if not exists
        if !state.buffers.contains_key(&buffer_id) {
            state.add_buffer(Buffer {
                id: buffer_id.clone(),
                connection_id: conn_id.to_string(),
                buffer_type: BufferType::Channel,
                name: channel.to_string(),
                messages: Vec::new(),
                activity: ActivityLevel::None,
                unread_count: 0,
                last_read: Utc::now(),
                topic: None,
                topic_set_by: None,
                users: std::collections::HashMap::new(),
                modes: None,
                mode_params: None,
                list_modes: std::collections::HashMap::new(),
            });
        }
        state.set_active_buffer(&buffer_id);
    } else {
        // Someone else joined — add to nick list
        state.add_nick(
            &buffer_id,
            NickEntry {
                nick: nick.clone(),
                prefix: String::new(),
                modes: String::new(),
                away: false,
                account: None,
            },
        );
    }

    let id = state.next_message_id();
    state.add_message(
        &buffer_id,
        Message {
            id,
            timestamp: Utc::now(),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text: format!("{nick} has joined {channel}"),
            highlight: false,
            event_key: Some("join".to_string()),
            event_params: Some(vec![
                nick,
                String::new(),
                String::new(),
                channel.to_string(),
            ]),
        },
    );
}

fn handle_part(
    state: &mut AppState,
    conn_id: &str,
    our_nick: &str,
    prefix: &Option<Prefix>,
    channel: &str,
    reason: Option<&str>,
) {
    let nick = extract_nick(prefix).unwrap_or_default();
    let buffer_id = make_buffer_id(conn_id, channel);

    if nick == our_nick {
        state.remove_buffer(&buffer_id);
    } else {
        state.remove_nick(&buffer_id, &nick);
        let reason_str = reason.unwrap_or("");
        let id = state.next_message_id();
        state.add_message(
            &buffer_id,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: format!("{nick} has left {channel} ({reason_str})"),
                highlight: false,
                event_key: Some("part".to_string()),
                event_params: Some(vec![
                    nick,
                    String::new(),
                    String::new(),
                    channel.to_string(),
                    reason_str.to_string(),
                ]),
            },
        );
    }
}

fn handle_quit(
    state: &mut AppState,
    conn_id: &str,
    _our_nick: &str,
    prefix: &Option<Prefix>,
    reason: Option<&str>,
) {
    let nick = extract_nick(prefix).unwrap_or_default();
    let reason_str = reason.unwrap_or("");

    // Remove from all buffers on this connection
    let affected: Vec<String> = state
        .buffers
        .iter()
        .filter(|(_, buf)| buf.connection_id == conn_id && buf.users.contains_key(&nick))
        .map(|(id, _)| id.clone())
        .collect();

    for buf_id in &affected {
        state.remove_nick(buf_id, &nick);
        let id = state.next_message_id();
        state.add_message(
            buf_id,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: format!("{nick} has quit ({reason_str})"),
                highlight: false,
                event_key: Some("quit".to_string()),
                event_params: Some(vec![
                    nick.clone(),
                    String::new(),
                    String::new(),
                    reason_str.to_string(),
                ]),
            },
        );
    }
}

fn handle_nick_change(
    state: &mut AppState,
    conn_id: &str,
    our_nick: &str,
    prefix: &Option<Prefix>,
    new_nick: &str,
) {
    let old_nick = extract_nick(prefix).unwrap_or_default();

    // Update our own nick if it's us
    if old_nick == our_nick
        && let Some(conn) = state.connections.get_mut(conn_id)
    {
        conn.nick = new_nick.to_string();
    }

    // Update in all buffers on this connection
    let affected: Vec<String> = state
        .buffers
        .iter()
        .filter(|(_, buf)| buf.connection_id == conn_id && buf.users.contains_key(&old_nick))
        .map(|(id, _)| id.clone())
        .collect();

    for buf_id in &affected {
        state.update_nick(buf_id, &old_nick, new_nick.to_string());
        let id = state.next_message_id();
        state.add_message(
            buf_id,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: format!("{old_nick} is now known as {new_nick}"),
                highlight: false,
                event_key: Some("nick_change".to_string()),
                event_params: Some(vec![old_nick.clone(), new_nick.to_string()]),
            },
        );
    }
}

fn handle_kick(
    state: &mut AppState,
    conn_id: &str,
    our_nick: &str,
    prefix: &Option<Prefix>,
    channel: &str,
    kicked: &str,
    reason: Option<&str>,
) {
    let kicker = extract_nick(prefix).unwrap_or_default();
    let buffer_id = make_buffer_id(conn_id, channel);
    let reason_str = reason.unwrap_or("");

    if kicked == our_nick {
        let id = state.next_message_id();
        state.add_message(
            &buffer_id,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: format!("You were kicked from {channel} by {kicker} ({reason_str})"),
                highlight: false,
                event_key: None,
                event_params: None,
            },
        );
        state.remove_buffer(&buffer_id);
    } else {
        state.remove_nick(&buffer_id, kicked);
        let id = state.next_message_id();
        state.add_message(
            &buffer_id,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: format!("{kicked} was kicked by {kicker} ({reason_str})"),
                highlight: false,
                event_key: Some("kick".to_string()),
                event_params: Some(vec![
                    kicked.to_string(),
                    kicker,
                    channel.to_string(),
                    reason_str.to_string(),
                ]),
            },
        );
    }
}

fn handle_topic(
    state: &mut AppState,
    conn_id: &str,
    prefix: &Option<Prefix>,
    channel: &str,
    topic: Option<&str>,
) {
    let nick = extract_nick(prefix);
    let buffer_id = make_buffer_id(conn_id, channel);

    if let Some(topic_text) = topic {
        state.set_topic(&buffer_id, topic_text.to_string(), nick.clone());
        let setter = nick.unwrap_or_default();
        let id = state.next_message_id();
        state.add_message(
            &buffer_id,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: format!("{setter} changed the topic to: {topic_text}"),
                highlight: false,
                event_key: Some("topic_changed".to_string()),
                event_params: Some(vec![setter, topic_text.to_string()]),
            },
        );
    }
}

fn handle_response(
    state: &mut AppState,
    conn_id: &str,
    response: Response,
    args: &[String],
) {
    match response {
        // RPL_NAMREPLY: args = [our_nick, "=" | "*" | "@", channel, "nick1 nick2 ..."]
        Response::RPL_NAMREPLY => {
            if args.len() >= 4 {
                let channel = &args[2];
                let buffer_id = make_buffer_id(conn_id, channel);
                let nicks_str = &args[3];
                for nick_with_prefix in nicks_str.split_whitespace() {
                    let (prefix, nick) = split_nick_prefix(nick_with_prefix);
                    let modes = prefix_to_mode(&prefix);
                    state.add_nick(
                        &buffer_id,
                        NickEntry {
                            nick: nick.to_string(),
                            prefix,
                            modes,
                            away: false,
                            account: None,
                        },
                    );
                }
            }
        }
        // RPL_TOPIC: args = [our_nick, channel, topic]
        Response::RPL_TOPIC => {
            if args.len() >= 3 {
                let channel = &args[1];
                let topic = &args[2];
                let buffer_id = make_buffer_id(conn_id, channel);
                state.set_topic(&buffer_id, topic.clone(), None);
            }
        }
        // RPL_TOPICWHOTIME: args = [our_nick, channel, set_by, timestamp]
        Response::RPL_TOPICWHOTIME => {
            if args.len() >= 3 {
                let channel = &args[1];
                let set_by = &args[2];
                let buffer_id = make_buffer_id(conn_id, channel);
                if let Some(buf) = state.buffers.get_mut(&buffer_id) {
                    buf.topic_set_by = Some(set_by.clone());
                }
            }
        }
        // RPL_CHANNELMODEIS: args = [our_nick, channel, modes, ...]
        Response::RPL_CHANNELMODEIS => {
            if args.len() >= 3 {
                let channel = &args[1];
                let modes = &args[2];
                let buffer_id = make_buffer_id(conn_id, channel);
                if let Some(buf) = state.buffers.get_mut(&buffer_id) {
                    buf.modes = Some(modes.clone());
                }
            }
        }
        _ => {
            // Log unknown numerics to status buffer
            let label = state
                .connections
                .get(conn_id)
                .map(|c| c.label.as_str())
                .unwrap_or("Status");
            let buffer_id = make_buffer_id(conn_id, label);
            let text = args.join(" ");
            let id = state.next_message_id();
            state.add_message(
                &buffer_id,
                Message {
                    id,
                    timestamp: Utc::now(),
                    message_type: MessageType::Event,
                    nick: None,
                    nick_mode: None,
                    text,
                    highlight: false,
                    event_key: None,
                    event_params: None,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::connection::Connection;
    use irc::proto::Prefix;
    use std::collections::HashMap;

    fn make_test_state() -> AppState {
        let mut state = AppState::new();
        state.add_connection(Connection {
            id: "test".to_string(),
            label: "TestServer".to_string(),
            status: ConnectionStatus::Connected,
            nick: "me".to_string(),
            user_modes: String::new(),
            isupport: HashMap::new(),
            error: None,
            lag: None,
        });
        // Server buffer
        state.add_buffer(Buffer {
            id: make_buffer_id("test", "TestServer"),
            connection_id: "test".to_string(),
            buffer_type: BufferType::Server,
            name: "TestServer".to_string(),
            messages: Vec::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
        });
        // Channel buffer
        let chan_id = make_buffer_id("test", "#test");
        state.add_buffer(Buffer {
            id: chan_id.clone(),
            connection_id: "test".to_string(),
            buffer_type: BufferType::Channel,
            name: "#test".to_string(),
            messages: Vec::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
        });
        // Add ourselves to the channel
        state.add_nick(
            &chan_id,
            NickEntry {
                nick: "me".to_string(),
                prefix: String::new(),
                modes: String::new(),
                away: false,
                account: None,
            },
        );
        state
    }

    fn make_irc_msg(prefix: Option<&str>, command: Command) -> IrcMessage {
        IrcMessage {
            tags: None,
            prefix: prefix.map(|s| Prefix::new_from_str(s)),
            command,
        }
    }

    // === handle_privmsg tests ===

    #[test]
    fn privmsg_to_channel() {
        let mut state = make_test_state();
        let msg = make_irc_msg(
            Some("alice!user@host"),
            Command::PRIVMSG("#test".into(), "hello".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert_eq!(buf.messages.len(), 1);
        assert_eq!(buf.messages[0].text, "hello");
        assert_eq!(buf.messages[0].nick.as_deref(), Some("alice"));
        assert_eq!(buf.messages[0].message_type, MessageType::Message);
    }

    #[test]
    fn privmsg_pm_creates_query_buffer() {
        let mut state = make_test_state();
        let msg = make_irc_msg(
            Some("bob!user@host"),
            Command::PRIVMSG("me".into(), "hi there".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/bob").unwrap();
        assert_eq!(buf.buffer_type, BufferType::Query);
        assert_eq!(buf.messages.len(), 1);
        assert_eq!(buf.messages[0].text, "hi there");
    }

    #[test]
    fn privmsg_mention_sets_highlight() {
        let mut state = make_test_state();
        // Set active buffer to something else so activity is tracked
        state.set_active_buffer("test/testserver");
        let msg = make_irc_msg(
            Some("alice!user@host"),
            Command::PRIVMSG("#test".into(), "hey me, how are you?".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(buf.messages[0].highlight);
        assert_eq!(buf.activity, ActivityLevel::Mention);
    }

    #[test]
    fn privmsg_own_message_no_activity() {
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver"); // switch away
        let msg = make_irc_msg(
            Some("me!user@host"),
            Command::PRIVMSG("#test".into(), "my own message".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert_eq!(buf.activity, ActivityLevel::None);
    }

    #[test]
    fn privmsg_ctcp_action() {
        let mut state = make_test_state();
        let msg = make_irc_msg(
            Some("alice!user@host"),
            Command::PRIVMSG("#test".into(), "\x01ACTION waves\x01".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert_eq!(buf.messages[0].message_type, MessageType::Action);
        assert_eq!(buf.messages[0].text, "waves");
    }

    // === handle_join tests ===

    #[test]
    fn join_our_own_creates_buffer() {
        let mut state = make_test_state();
        let msg = make_irc_msg(
            Some("me!user@host"),
            Command::JOIN("#newchan".into(), None, None),
        );
        handle_irc_message(&mut state, "test", &msg);

        assert!(state.buffers.contains_key("test/#newchan"));
        let buf = state.buffers.get("test/#newchan").unwrap();
        assert_eq!(buf.buffer_type, BufferType::Channel);
    }

    #[test]
    fn join_other_user_adds_nick() {
        let mut state = make_test_state();
        let msg = make_irc_msg(
            Some("carol!user@host"),
            Command::JOIN("#test".into(), None, None),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(buf.users.contains_key("carol"));
        // Should also have a join event message
        assert!(buf.messages.last().unwrap().text.contains("carol has joined"));
    }

    // === handle_part tests ===

    #[test]
    fn part_our_own_removes_buffer() {
        let mut state = make_test_state();
        let msg = make_irc_msg(
            Some("me!user@host"),
            Command::PART("#test".into(), Some("bye".into())),
        );
        handle_irc_message(&mut state, "test", &msg);

        assert!(!state.buffers.contains_key("test/#test"));
    }

    #[test]
    fn part_other_user_removes_nick() {
        let mut state = make_test_state();
        // First add another user
        state.add_nick(
            "test/#test",
            NickEntry {
                nick: "dave".to_string(),
                prefix: String::new(),
                modes: String::new(),
                away: false,
                account: None,
            },
        );
        let msg = make_irc_msg(
            Some("dave!user@host"),
            Command::PART("#test".into(), Some("leaving".into())),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(!buf.users.contains_key("dave"));
        assert!(buf.messages.last().unwrap().text.contains("dave has left"));
    }

    // === handle_quit tests ===

    #[test]
    fn quit_removes_from_all_buffers() {
        let mut state = make_test_state();
        // Add user to channel
        state.add_nick(
            "test/#test",
            NickEntry {
                nick: "eve".to_string(),
                prefix: String::new(),
                modes: String::new(),
                away: false,
                account: None,
            },
        );
        let msg = make_irc_msg(
            Some("eve!user@host"),
            Command::QUIT(Some("gone".into())),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(!buf.users.contains_key("eve"));
        assert!(buf.messages.last().unwrap().text.contains("eve has quit"));
    }

    // === handle_nick_change tests ===

    #[test]
    fn nick_change_updates_our_nick() {
        let mut state = make_test_state();
        let msg = make_irc_msg(
            Some("me!user@host"),
            Command::NICK("me_".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        assert_eq!(state.connections.get("test").unwrap().nick, "me_");
    }

    #[test]
    fn nick_change_other_user() {
        let mut state = make_test_state();
        state.add_nick(
            "test/#test",
            NickEntry {
                nick: "frank".to_string(),
                prefix: "@".to_string(),
                modes: "o".to_string(),
                away: false,
                account: None,
            },
        );
        let msg = make_irc_msg(
            Some("frank!user@host"),
            Command::NICK("frankie".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(!buf.users.contains_key("frank"));
        assert!(buf.users.contains_key("frankie"));
        assert!(buf
            .messages
            .last()
            .unwrap()
            .text
            .contains("frank is now known as frankie"));
    }

    // === handle_kick tests ===

    #[test]
    fn kick_our_own_removes_buffer() {
        let mut state = make_test_state();
        let msg = make_irc_msg(
            Some("op!user@host"),
            Command::KICK("#test".into(), "me".into(), Some("behave".into())),
        );
        handle_irc_message(&mut state, "test", &msg);

        assert!(!state.buffers.contains_key("test/#test"));
    }

    #[test]
    fn kick_other_user_removes_nick() {
        let mut state = make_test_state();
        state.add_nick(
            "test/#test",
            NickEntry {
                nick: "troll".to_string(),
                prefix: String::new(),
                modes: String::new(),
                away: false,
                account: None,
            },
        );
        let msg = make_irc_msg(
            Some("op!user@host"),
            Command::KICK("#test".into(), "troll".into(), Some("bye".into())),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(!buf.users.contains_key("troll"));
        assert!(buf
            .messages
            .last()
            .unwrap()
            .text
            .contains("troll was kicked by op"));
    }

    // === handle_topic tests ===

    #[test]
    fn topic_change_updates_buffer() {
        let mut state = make_test_state();
        let msg = make_irc_msg(
            Some("alice!user@host"),
            Command::TOPIC("#test".into(), Some("new topic".into())),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert_eq!(buf.topic.as_deref(), Some("new topic"));
        assert_eq!(buf.topic_set_by.as_deref(), Some("alice"));
    }

    // === handle_response (numerics) tests ===

    #[test]
    fn rpl_namreply_adds_nicks() {
        let mut state = make_test_state();
        let msg = make_irc_msg(
            None,
            Command::Response(
                Response::RPL_NAMREPLY,
                vec![
                    "me".into(),
                    "=".into(),
                    "#test".into(),
                    "@op +voice regular".into(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(buf.users.contains_key("op"));
        assert_eq!(buf.users.get("op").unwrap().prefix, "@");
        assert_eq!(buf.users.get("op").unwrap().modes, "o");
        assert!(buf.users.contains_key("voice"));
        assert_eq!(buf.users.get("voice").unwrap().prefix, "+");
        assert_eq!(buf.users.get("voice").unwrap().modes, "v");
        assert!(buf.users.contains_key("regular"));
        assert_eq!(buf.users.get("regular").unwrap().prefix, "");
    }

    #[test]
    fn rpl_topic_sets_topic() {
        let mut state = make_test_state();
        let msg = make_irc_msg(
            None,
            Command::Response(
                Response::RPL_TOPIC,
                vec!["me".into(), "#test".into(), "Welcome!".into()],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert_eq!(buf.topic.as_deref(), Some("Welcome!"));
    }

    // === handle_connected / handle_disconnected tests ===

    #[test]
    fn connected_updates_status() {
        let mut state = make_test_state();
        state.update_connection_status("test", ConnectionStatus::Connecting);
        handle_connected(&mut state, "test");

        assert_eq!(
            state.connections.get("test").unwrap().status,
            ConnectionStatus::Connected
        );
    }

    #[test]
    fn disconnected_with_error() {
        let mut state = make_test_state();
        handle_disconnected(&mut state, "test", Some("timeout"));

        let conn = state.connections.get("test").unwrap();
        assert_eq!(conn.status, ConnectionStatus::Error);
        assert_eq!(conn.error.as_deref(), Some("timeout"));
    }

    #[test]
    fn disconnected_clean() {
        let mut state = make_test_state();
        handle_disconnected(&mut state, "test", None);

        assert_eq!(
            state.connections.get("test").unwrap().status,
            ConnectionStatus::Disconnected
        );
    }

    // === handle_notice tests ===

    #[test]
    fn notice_from_server_goes_to_status() {
        let mut state = make_test_state();
        let msg = make_irc_msg(
            Some("irc.server.com"),
            Command::NOTICE("*".into(), "*** Looking up your hostname".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/testserver").unwrap();
        assert!(buf.messages.last().unwrap().text.contains("Looking up"));
        assert_eq!(buf.messages.last().unwrap().message_type, MessageType::Notice);
    }
}

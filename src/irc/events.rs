use std::collections::HashMap;
use std::fmt::Write as _;
use std::time::Instant;

use chrono::Utc;
use irc::proto::{Command, Message as IrcMessage, Prefix, Response};

use crate::config::IgnoreLevel;
use crate::irc::formatting::{extract_nick, extract_nick_userhost, get_highest_prefix, is_channel, is_server_prefix, strip_irc_formatting};
use crate::irc::ignore::should_ignore;
use crate::state::buffer::{make_buffer_id, ActivityLevel, Buffer, BufferType, Message, MessageType, NickEntry};
use crate::state::connection::ConnectionStatus;
use crate::state::AppState;

/// Route an incoming IRC protocol message to the appropriate handler,
/// mutating `AppState` as needed.
pub fn handle_irc_message(state: &mut AppState, conn_id: &str, msg: &IrcMessage) {
    let our_nick = state
        .connections
        .get(conn_id)
        .map(|c| c.nick.clone())
        .unwrap_or_default();

    let tags = extract_tags(msg);

    match &msg.command {
        Command::PRIVMSG(target, text) => {
            handle_privmsg(state, conn_id, &our_nick, msg.prefix.as_ref(), target, text, tags);
        }
        Command::NOTICE(target, text) => {
            handle_notice(state, conn_id, msg.prefix.as_ref(), target, text, tags);
        }
        Command::JOIN(channel, account, _) => {
            handle_join(state, conn_id, &our_nick, msg.prefix.as_ref(), channel, account.as_deref(), tags);
        }
        Command::PART(channel, reason) => {
            handle_part(
                state,
                conn_id,
                &our_nick,
                msg.prefix.as_ref(),
                channel,
                reason.as_deref(),
                tags,
            );
        }
        Command::QUIT(reason) => {
            handle_quit(state, conn_id, &our_nick, msg.prefix.as_ref(), reason.as_deref(), tags);
        }
        Command::NICK(new_nick) => {
            handle_nick_change(state, conn_id, &our_nick, msg.prefix.as_ref(), new_nick, tags);
        }
        Command::KICK(channel, kicked_user, reason) => {
            handle_kick(
                state,
                conn_id,
                &our_nick,
                msg.prefix.as_ref(),
                channel,
                kicked_user,
                reason.as_deref(),
                tags,
            );
        }
        Command::TOPIC(channel, topic) => {
            handle_topic(state, conn_id, msg.prefix.as_ref(), channel, topic.as_deref(), tags);
        }
        Command::ChannelMODE(target, _) | Command::UserMODE(target, _) => {
            handle_mode(state, conn_id, msg.prefix.as_ref(), target, msg, tags);
        }
        Command::INVITE(nick, channel) => {
            handle_invite(state, conn_id, msg.prefix.as_ref(), nick, channel, tags);
        }
        Command::Response(response, args) => {
            handle_response(state, conn_id, *response, args);
        }
        Command::WALLOPS(text) => {
            handle_wallops(state, conn_id, msg.prefix.as_ref(), text);
        }
        Command::ACCOUNT(account) => {
            handle_account(state, conn_id, msg.prefix.as_ref(), account, tags);
        }
        // PING handled automatically by the irc crate
        _ => {}
    }
}

/// Update connection status to Connected and log to the status buffer.
pub fn handle_connected(state: &mut AppState, conn_id: &str) {
    state.update_connection_status(conn_id, ConnectionStatus::Connected);

    // Reset reconnect state on successful connection
    if let Some(conn) = state.connections.get_mut(conn_id) {
        conn.reconnect_attempts = 0;
        conn.next_reconnect = None;
        conn.error = None;
    }

    let label = state
        .connections
        .get(conn_id)
        .map_or_else(|| conn_id.to_string(), |c| c.label.clone());
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
            event_params: None, log_msg_id: None, log_ref_id: None,
            tags: HashMap::new(),
        },
    );
}

/// Get the list of channels to auto-rejoin after reconnecting.
pub fn channels_to_rejoin(state: &AppState, conn_id: &str) -> Vec<String> {
    // Collect channels from existing channel buffers for this connection
    let mut channels: Vec<String> = state
        .buffers
        .values()
        .filter(|b| {
            b.connection_id == conn_id
                && b.buffer_type == crate::state::buffer::BufferType::Channel
        })
        .map(|b| b.name.clone())
        .collect();

    // Also include joined_channels from Connection state (in case buffers were cleaned up)
    if let Some(conn) = state.connections.get(conn_id) {
        for ch in &conn.joined_channels {
            if !channels.contains(ch) {
                channels.push(ch.clone());
            }
        }
    }

    channels
}

/// Update connection status to Disconnected and log to the status buffer.
/// Also sets up reconnect timing if `should_reconnect` is true.
pub fn handle_disconnected(state: &mut AppState, conn_id: &str, error: Option<&str>) {
    // Save list of joined channels before we update state
    let current_channels: Vec<String> = state
        .buffers
        .values()
        .filter(|b| {
            b.connection_id == conn_id
                && b.buffer_type == crate::state::buffer::BufferType::Channel
        })
        .map(|b| b.name.clone())
        .collect();

    if let Some(err) = error {
        if let Some(conn) = state.connections.get_mut(conn_id) {
            conn.status = ConnectionStatus::Error;
            conn.error = Some(err.to_string());
        }
    } else {
        state.update_connection_status(conn_id, ConnectionStatus::Disconnected);
    }

    // Store joined channels and set up reconnect schedule
    if let Some(conn) = state.connections.get_mut(conn_id) {
        if !current_channels.is_empty() {
            conn.joined_channels = current_channels;
        }
        if conn.should_reconnect && conn.reconnect_attempts < conn.max_reconnect_attempts {
            let delay = calculate_reconnect_delay(conn.reconnect_delay_secs, conn.reconnect_attempts);
            conn.next_reconnect = Some(std::time::Instant::now() + std::time::Duration::from_secs(delay));
        }
    }

    let label = state
        .connections
        .get(conn_id)
        .map_or_else(|| conn_id.to_string(), |c| c.label.clone());
    let buffer_id = make_buffer_id(conn_id, &label);

    let mut msg_text = error.map_or_else(
        || format!("Disconnected from {label}"),
        |e| format!("Disconnected from {label}: {e}"),
    );

    // Append reconnect info if applicable
    if let Some(conn) = state.connections.get(conn_id)
        && conn.should_reconnect && conn.reconnect_attempts < conn.max_reconnect_attempts
    {
        let delay = calculate_reconnect_delay(conn.reconnect_delay_secs, conn.reconnect_attempts);
        write!(msg_text, " — reconnecting in {delay}s").unwrap();
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
            text: msg_text,
            highlight: false,
            event_key: Some("disconnected".to_string()),
            event_params: None, log_msg_id: None, log_ref_id: None,
            tags: HashMap::new(),
        },
    );
}

/// Calculate reconnect delay with exponential backoff, capped at 300 seconds.
fn calculate_reconnect_delay(base_delay: u64, attempts: u32) -> u64 {
    let delay = base_delay.saturating_mul(2u64.saturating_pow(attempts));
    delay.min(300)
}

/// Look up a nick's mode prefix (e.g. "@", "+") from the buffer's user list.
fn nick_prefix(state: &AppState, buffer_id: &str, nick: &str) -> Option<String> {
    let buf = state.buffers.get(buffer_id)?;
    let entry = buf.users.get(&nick.to_lowercase())?;
    if entry.prefix.is_empty() {
        None
    } else {
        Some(entry.prefix.clone())
    }
}

/// Extract `IRCv3` message tags from an `irc::proto::Message`.
///
/// Tags with no value are omitted — only `key=value` pairs are returned.
fn extract_tags(msg: &IrcMessage) -> HashMap<String, String> {
    msg.tags.as_ref().map_or_else(HashMap::new, |tags| {
        tags.iter()
            .filter_map(|tag| Some((tag.0.clone(), tag.1.as_ref()?.clone())))
            .collect()
    })
}

// === Private handlers ===

#[expect(clippy::too_many_lines, reason = "linear message handler")]
fn handle_privmsg(
    state: &mut AppState,
    conn_id: &str,
    our_nick: &str,
    prefix: Option<&Prefix>,
    target: &str,
    text: &str,
    tags: HashMap<String, String>,
) {
    let (nick, ident, host) = extract_nick_userhost(prefix);
    let target_is_channel = is_channel(target);
    let buffer_name = if target_is_channel { target } else { &nick };
    let buffer_id = make_buffer_id(conn_id, buffer_name);

    // Check if this is a CTCP (ACTION or other)
    let is_ctcp = text.starts_with('\x01') && text.ends_with('\x01');
    let is_action = is_ctcp
        && text.len() > 2
        && text[1..text.len() - 1].starts_with("ACTION ");

    // --- Ignore check ---
    {
        let ignore_level = if is_action {
            IgnoreLevel::Actions
        } else if is_ctcp {
            IgnoreLevel::Ctcps
        } else if target_is_channel {
            IgnoreLevel::Public
        } else {
            IgnoreLevel::Msgs
        };
        let channel = if target_is_channel { Some(target) } else { None };
        if should_ignore(
            &state.ignores,
            &nick,
            Some(&ident),
            Some(&host),
            &ignore_level,
            channel,
        ) {
            return;
        }
    }

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

    // account-tag: update NickEntry.account from message tags (supplementary)
    if let Some(tag_account) = tags.get("account") {
        let account = if tag_account == "*" {
            None
        } else {
            Some(tag_account.clone())
        };
        if target_is_channel
            && let Some(buf) = state.buffers.get_mut(&buffer_id)
            && let Some(entry) = buf.users.get_mut(&nick.to_lowercase())
        {
            entry.account.clone_from(&account);
        }
    }

    // Check if this is a CTCP ACTION
    if is_ctcp {
        let inner = &text[1..text.len() - 1];
        if let Some(action_text) = inner.strip_prefix("ACTION ") {
            let is_own = nick == our_nick;
            let is_mention = !is_own
                && strip_irc_formatting(action_text)
                    .to_lowercase()
                    .contains(&our_nick.to_lowercase());
            let activity = if is_own {
                ActivityLevel::None
            } else if !target_is_channel || is_mention {
                ActivityLevel::Mention
            } else {
                ActivityLevel::Activity
            };
            let mode_prefix = nick_prefix(state, &buffer_id, &nick);
            let id = state.next_message_id();
            state.add_message_with_activity(
                &buffer_id,
                Message {
                    id,
                    timestamp: Utc::now(),
                    message_type: MessageType::Action,
                    nick: Some(nick),
                    nick_mode: mode_prefix,
                    text: action_text.to_string(),
                    highlight: is_mention,
                    event_key: None,
                    event_params: None, log_msg_id: None, log_ref_id: None,
                    tags,
                },
                activity,
            );
            return;
        }

        // Other CTCP — flood check
        if state.flood_protection {
            let now = Instant::now();
            if state.flood_state.check_ctcp_flood(now) {
                emit(state, &buffer_id, "CTCP flood detected — suppressing");
                return;
            }
        }
        // Non-ACTION CTCP, ignore for now
        return;
    }

    // --- Flood checks for regular messages ---
    if state.flood_protection && nick != our_nick {
        let now = Instant::now();

        // Tilde (~ident) flood check
        if ident.starts_with('~') && state.flood_state.check_tilde_flood(now) {
            emit(
                state,
                &buffer_id,
                "Tilde-ident flood detected — suppressing",
            );
            return;
        }

        // Duplicate text flood check (channel messages only)
        if state
            .flood_state
            .check_duplicate_flood(text, target_is_channel, now)
        {
            emit(
                state,
                &buffer_id,
                "Duplicate text flood detected — suppressing",
            );
            return;
        }
    }

    let is_own = nick == our_nick;
    let is_mention = !is_own
        && strip_irc_formatting(text)
            .to_lowercase()
            .contains(&our_nick.to_lowercase());

    let activity = if is_own {
        ActivityLevel::None
    } else if !target_is_channel || is_mention {
        ActivityLevel::Mention // PMs and mentions are mention-level
    } else {
        ActivityLevel::Activity
    };

    let mode_prefix = nick_prefix(state, &buffer_id, &nick);
    let id = state.next_message_id();
    state.add_message_with_activity(
        &buffer_id,
        Message {
            id,
            timestamp: Utc::now(),
            message_type: MessageType::Message,
            nick: Some(nick),
            nick_mode: mode_prefix,
            text: text.to_string(),
            highlight: is_mention,
            event_key: None,
            event_params: None, log_msg_id: None, log_ref_id: None,
            tags,
        },
        activity,
    );
}

fn handle_notice(
    state: &mut AppState,
    conn_id: &str,
    prefix: Option<&Prefix>,
    target: &str,
    text: &str,
    tags: HashMap<String, String>,
) {
    let nick = extract_nick(prefix);
    // Server notices or pre-registration notices go to status buffer
    let is_server_notice = nick.is_none() || is_server_prefix(prefix);

    // --- Ignore check (skip for server notices) ---
    if !is_server_notice {
        let (n, ident, host) = extract_nick_userhost(prefix);
        let channel = if is_channel(target) { Some(target) } else { None };
        if should_ignore(
            &state.ignores,
            &n,
            Some(&ident),
            Some(&host),
            &IgnoreLevel::Notices,
            channel,
        ) {
            return;
        }
    }

    let buffer_name = if is_server_notice {
        state
            .connections
            .get(conn_id)
            .map_or("Status", |c| c.label.as_str())
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
            .map_or("Status", |c| c.label.as_str());
        make_buffer_id(conn_id, label)
    };

    let mode_prefix = nick.as_deref().and_then(|n| nick_prefix(state, &buffer_id, n));
    let id = state.next_message_id();
    state.add_message(
        &buffer_id,
        Message {
            id,
            timestamp: Utc::now(),
            message_type: MessageType::Notice,
            nick,
            nick_mode: mode_prefix,
            text: text.to_string(),
            highlight: false,
            event_key: None,
            event_params: None, log_msg_id: None, log_ref_id: None,
            tags,
        },
    );
}

fn handle_join(
    state: &mut AppState,
    conn_id: &str,
    our_nick: &str,
    prefix: Option<&Prefix>,
    channel: &str,
    extended_account: Option<&str>,
    tags: HashMap<String, String>,
) {
    let (nick, ident, host) = extract_nick_userhost(prefix);
    let buffer_id = make_buffer_id(conn_id, channel);

    // extended-join: account from second JOIN arg ("*" means not logged in)
    let account = match extended_account {
        Some("*") | None => None,
        Some(a) => Some(a.to_string()),
    };

    // account-tag: supplementary source (only if extended-join didn't provide one)
    let account = account.or_else(|| {
        tags.get("account").and_then(|a| {
            if a == "*" { None } else { Some(a.clone()) }
        })
    });

    // --- Ignore check (never ignore our own joins) ---
    if nick != our_nick
        && should_ignore(
            &state.ignores,
            &nick,
            Some(&ident),
            Some(&host),
            &IgnoreLevel::Joins,
            Some(channel),
        )
    {
        // Still add to nick list so channel state is correct, but suppress the message
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
        return;
    }

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
                account,
                ident: None,
                host: None,
            },
        );

        // --- Netsplit: check if this is a netjoin ---
        if state.netsplit_state.handle_join(&nick, &buffer_id) {
            // Suppress normal join message — netsplit module will batch it
            return;
        }
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
            text: format!("{nick} ({ident}@{host}) has joined {channel}"),
            highlight: false,
            event_key: Some("join".to_string()),
            event_params: Some(vec![nick, ident, host, channel.to_string()]), log_msg_id: None, log_ref_id: None,
            tags,
        },
    );
}

/// Update a nick's `account` field in every buffer on a given connection that
/// contains that nick.  Used by `account-notify` and `account-tag`.
fn update_nick_account_in_buffers(
    state: &mut AppState,
    conn_id: &str,
    nick: &str,
    account: Option<&str>,
) {
    let nick_lower = nick.to_lowercase();
    for buf in state.buffers.values_mut() {
        if buf.connection_id != conn_id {
            continue;
        }
        if let Some(entry) = buf.users.get_mut(&nick_lower) {
            entry.account = account.map(str::to_string);
        }
    }
}

/// Handle `IRCv3` `account-notify`: `:nick!user@host ACCOUNT account_name`
///
/// When a user logs in or out of their NickServ/services account the server
/// sends this command to every channel we share with them.
///   - `account == "*"` → logged out (clear account)
///   - otherwise → logged in as `account`
#[expect(clippy::needless_pass_by_value, reason = "tags follows the convention of all other event handlers")]
fn handle_account(
    state: &mut AppState,
    conn_id: &str,
    prefix: Option<&Prefix>,
    account: &str,
    tags: HashMap<String, String>,
) {
    let Some(nick) = extract_nick(prefix) else {
        return;
    };

    let resolved: Option<&str> = if account == "*" {
        None
    } else {
        Some(account)
    };

    update_nick_account_in_buffers(state, conn_id, &nick, resolved);

    // Log a subtle event in every shared channel
    let shared_buffers: Vec<String> = state
        .buffers
        .values()
        .filter(|b| {
            b.connection_id == conn_id
                && b.buffer_type == BufferType::Channel
                && b.users.contains_key(&nick.to_lowercase())
        })
        .map(|b| b.id.clone())
        .collect();

    let text = resolved.map_or_else(
        || format!("{nick} has logged out"),
        |acct| format!("{nick} is now logged in as {acct}"),
    );

    for buf_id in shared_buffers {
        let id = state.next_message_id();
        state.add_message(
            &buf_id,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: text.clone(),
                highlight: false,
                event_key: Some("account".to_string()),
                event_params: Some(vec![nick.clone(), account.to_string()]),
                log_msg_id: None,
                log_ref_id: None,
                tags: tags.clone(),
            },
        );
    }
}

fn handle_part(
    state: &mut AppState,
    conn_id: &str,
    our_nick: &str,
    prefix: Option<&Prefix>,
    channel: &str,
    reason: Option<&str>,
    tags: HashMap<String, String>,
) {
    let (nick, ident, host) = extract_nick_userhost(prefix);
    let buffer_id = make_buffer_id(conn_id, channel);

    if nick == our_nick {
        state.remove_buffer(&buffer_id);
    } else {
        // Always update nick list regardless of ignore
        state.remove_nick(&buffer_id, &nick);

        // --- Ignore check ---
        if should_ignore(
            &state.ignores,
            &nick,
            Some(&ident),
            Some(&host),
            &IgnoreLevel::Parts,
            Some(channel),
        ) {
            return;
        }

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
                text: format!("{nick} ({ident}@{host}) has left {channel} ({reason_str})"),
                highlight: false,
                event_key: Some("part".to_string()),
                event_params: Some(vec![
                    nick,
                    ident,
                    host,
                    channel.to_string(),
                    reason_str.to_string(),
                ]),
                log_msg_id: None, log_ref_id: None,
                tags,
            },
        );
    }
}

#[expect(clippy::needless_pass_by_value, reason = "tags are dropped when ignored/netsplit, cloned into fan-out Messages otherwise")]
fn handle_quit(
    state: &mut AppState,
    conn_id: &str,
    _our_nick: &str,
    prefix: Option<&Prefix>,
    reason: Option<&str>,
    tags: HashMap<String, String>,
) {
    let (nick, ident, host) = extract_nick_userhost(prefix);
    let reason_str = reason.unwrap_or("");

    // Remove from all buffers on this connection
    let affected: Vec<String> = state
        .buffers
        .iter()
        .filter(|(_, buf)| buf.connection_id == conn_id && buf.users.contains_key(&nick))
        .map(|(id, _)| id.clone())
        .collect();

    // Always remove from nick lists regardless of ignore/netsplit
    for buf_id in &affected {
        state.remove_nick(buf_id, &nick);
    }

    // --- Ignore check ---
    if should_ignore(
        &state.ignores,
        &nick,
        Some(&ident),
        Some(&host),
        &IgnoreLevel::Quits,
        None,
    ) {
        return;
    }

    // --- Netsplit check ---
    if state
        .netsplit_state
        .handle_quit(&nick, reason_str, &affected)
    {
        // Suppress normal quit messages — netsplit module will batch them
        return;
    }

    // First channel gets the full log row; remaining channels get reference rows.
    let primary_msg_id = uuid::Uuid::new_v4().to_string();
    let text = format!("{nick} ({ident}@{host}) has quit ({reason_str})");

    for (i, buf_id) in affected.iter().enumerate() {
        let id = state.next_message_id();
        state.add_message(
            buf_id,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: text.clone(),
                highlight: false,
                event_key: Some("quit".to_string()),
                event_params: Some(vec![
                    nick.clone(),
                    ident.clone(),
                    host.clone(),
                    reason_str.to_string(),
                ]),
                log_msg_id: if i == 0 { Some(primary_msg_id.clone()) } else { None },
                log_ref_id: if i == 0 { None } else { Some(primary_msg_id.clone()) },
                tags: tags.clone(),
            },
        );
    }
}

#[expect(clippy::needless_pass_by_value, reason = "tags are cloned into each fan-out Message")]
fn handle_nick_change(
    state: &mut AppState,
    conn_id: &str,
    our_nick: &str,
    prefix: Option<&Prefix>,
    new_nick: &str,
    tags: HashMap<String, String>,
) {
    let old_nick = extract_nick(prefix).unwrap_or_default();

    // Update our own nick if it's us
    if old_nick == our_nick
        && let Some(conn) = state.connections.get_mut(conn_id)
    {
        conn.nick = new_nick.to_string();
    }

    // --- Ignore check (never ignore our own nick changes) ---
    if old_nick != our_nick {
        let (_, ident, host) = extract_nick_userhost(prefix);
        if should_ignore(
            &state.ignores,
            &old_nick,
            Some(&ident),
            Some(&host),
            &IgnoreLevel::Nicks,
            None,
        ) {
            // Still update nick list so state is correct, but suppress messages
            let affected: Vec<String> = state
                .buffers
                .iter()
                .filter(|(_, buf)| {
                    buf.connection_id == conn_id && buf.users.contains_key(&old_nick)
                })
                .map(|(id, _)| id.clone())
                .collect();
            for buf_id in &affected {
                state.update_nick(buf_id, &old_nick, new_nick.to_string());
            }
            return;
        }
    }

    // Update in all buffers on this connection
    let affected: Vec<String> = state
        .buffers
        .iter()
        .filter(|(_, buf)| buf.connection_id == conn_id && buf.users.contains_key(&old_nick))
        .map(|(id, _)| id.clone())
        .collect();

    // First non-suppressed channel gets the full log row; others get reference rows.
    let primary_msg_id = uuid::Uuid::new_v4().to_string();
    let text = format!("{old_nick} is now known as {new_nick}");
    let mut primary_assigned = false;

    for buf_id in &affected {
        state.update_nick(buf_id, &old_nick, new_nick.to_string());

        // --- Nick flood check ---
        if state.flood_protection
            && old_nick != our_nick
            && state
                .flood_state
                .should_suppress_nick_flood(buf_id, Instant::now())
        {
            // Suppress the message display but nick was already updated above
            continue;
        }

        let is_primary = !primary_assigned;
        if is_primary {
            primary_assigned = true;
        }

        let id = state.next_message_id();
        state.add_message(
            buf_id,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: text.clone(),
                highlight: false,
                event_key: Some("nick_change".to_string()),
                event_params: Some(vec![old_nick.clone(), new_nick.to_string()]),
                log_msg_id: if is_primary { Some(primary_msg_id.clone()) } else { None },
                log_ref_id: if is_primary { None } else { Some(primary_msg_id.clone()) },
                tags: tags.clone(),
            },
        );
    }
}

#[expect(clippy::too_many_arguments, reason = "IRC KICK has many parameters")]
fn handle_kick(
    state: &mut AppState,
    conn_id: &str,
    our_nick: &str,
    prefix: Option<&Prefix>,
    channel: &str,
    kicked_user: &str,
    reason: Option<&str>,
    tags: HashMap<String, String>,
) {
    let (kicker, kicker_ident, kicker_host) = extract_nick_userhost(prefix);
    let buffer_id = make_buffer_id(conn_id, channel);
    let reason_str = reason.unwrap_or("");

    // --- Ignore check (never ignore kicks against us) ---
    if kicked_user != our_nick
        && should_ignore(
            &state.ignores,
            &kicker,
            Some(&kicker_ident),
            Some(&kicker_host),
            &IgnoreLevel::Kicks,
            Some(channel),
        )
    {
        // Still remove kicked user from nick list
        state.remove_nick(&buffer_id, kicked_user);
        return;
    }

    if kicked_user == our_nick {
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
                event_params: None, log_msg_id: None, log_ref_id: None,
                tags,
            },
        );
        state.remove_buffer(&buffer_id);
    } else {
        state.remove_nick(&buffer_id, kicked_user);
        let id = state.next_message_id();
        state.add_message(
            &buffer_id,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: format!("{kicked_user} was kicked by {kicker} ({reason_str})"),
                highlight: false,
                event_key: Some("kick".to_string()),
                event_params: Some(vec![
                    kicked_user.to_string(),
                    kicker,
                    channel.to_string(),
                    reason_str.to_string(),
                ]),
                log_msg_id: None, log_ref_id: None,
                tags,
            },
        );
    }
}

fn handle_topic(
    state: &mut AppState,
    conn_id: &str,
    prefix: Option<&Prefix>,
    channel: &str,
    topic: Option<&str>,
    tags: HashMap<String, String>,
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
                event_params: Some(vec![setter, topic_text.to_string()]), log_msg_id: None, log_ref_id: None,
                tags,
            },
        );
    }
}

fn handle_mode(
    state: &mut AppState,
    conn_id: &str,
    prefix: Option<&Prefix>,
    target: &str,
    raw_msg: &IrcMessage,
    tags: HashMap<String, String>,
) {
    let nick = extract_nick(prefix).unwrap_or_else(|| "server".to_string());

    // Build mode display string and apply changes based on command type
    let mode_display = match &raw_msg.command {
        Command::ChannelMODE(_, modes) => {
            let buffer_id = make_buffer_id(conn_id, target);
            // Apply nick prefix changes
            for mode in modes {
                apply_channel_mode(state, &buffer_id, mode);
            }
            build_channel_mode_string(modes)
        }
        Command::UserMODE(_, modes) => {
            // Update user modes on connection
            if let Some(conn) = state.connections.get_mut(conn_id) {
                for mode in modes {
                    let (adding, m) = match mode {
                        irc::proto::Mode::Plus(m, _)
                        | irc::proto::Mode::NoPrefix(m) => (true, m),
                        irc::proto::Mode::Minus(m, _) => (false, m),
                    };
                    let c = user_mode_letter(m);
                    if adding {
                        if !conn.user_modes.contains(c) {
                            conn.user_modes.push(c);
                        }
                    } else {
                        conn.user_modes = conn.user_modes.replace(c, "");
                    }
                }
            }
            build_user_mode_string(modes)
        }
        _ => String::new(),
    };

    if is_channel(target) {
        let buffer_id = make_buffer_id(conn_id, target);
        let id = state.next_message_id();
        state.add_message(
            &buffer_id,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: format!("{nick} sets mode {mode_display} on {target}"),
                highlight: false,
                event_key: Some("mode".to_string()),
                event_params: Some(vec![nick, mode_display, target.to_string()]), log_msg_id: None, log_ref_id: None,
                tags,
            },
        );
    } else {
        let label = state
            .connections
            .get(conn_id)
            .map_or("Status", |c| c.label.as_str());
        let server_buf = make_buffer_id(conn_id, label);
        let id = state.next_message_id();
        state.add_message(
            &server_buf,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: format!("{nick} sets mode {mode_display} on {target}"),
                highlight: false,
                event_key: Some("mode".to_string()),
                event_params: Some(vec![nick, mode_display, target.to_string()]), log_msg_id: None, log_ref_id: None,
                tags,
            },
        );
    }
}

/// Apply a single channel mode change to nick entries.
fn apply_channel_mode(
    state: &mut AppState,
    buffer_id: &str,
    mode: &irc::proto::Mode<irc::proto::ChannelMode>,
) {
    use irc::proto::ChannelMode;

    let (adding, mode_enum, param) = match mode {
        irc::proto::Mode::Plus(m, p) => (true, m, p.as_deref()),
        irc::proto::Mode::Minus(m, p) => (false, m, p.as_deref()),
        irc::proto::Mode::NoPrefix(_) => return,
    };

    // Map channel modes to prefix mode chars
    let mode_char = match mode_enum {
        ChannelMode::Founder => Some('q'),
        ChannelMode::Admin => Some('a'),
        ChannelMode::Oper => Some('o'),
        ChannelMode::Halfop => Some('h'),
        ChannelMode::Voice => Some('v'),
        _ => None,
    };

    if let Some(mc) = mode_char
        && let Some(target_nick) = param
        && let Some(buf) = state.buffers.get_mut(buffer_id)
        && let Some(entry) = buf.users.get_mut(target_nick)
    {
        if adding && !entry.modes.contains(mc) {
            entry.modes.push(mc);
        } else if !adding {
            entry.modes = entry.modes.replace(mc, "");
        }
        entry.prefix = get_highest_prefix(&entry.modes, "~&@%+");
    }
}

/// Build a displayable mode string from channel modes.
fn build_channel_mode_string(modes: &[irc::proto::Mode<irc::proto::ChannelMode>]) -> String {
    let mut result = String::new();
    let mut params = Vec::new();
    let mut last_sign = ' ';

    for mode in modes {
        let (sign, m, param) = match mode {
            irc::proto::Mode::Plus(m, p) => ('+', m, p.as_deref()),
            irc::proto::Mode::Minus(m, p) => ('-', m, p.as_deref()),
            irc::proto::Mode::NoPrefix(m) => (' ', m, None),
        };
        if sign != last_sign && sign != ' ' {
            result.push(sign);
            last_sign = sign;
        }
        result.push(channel_mode_letter(m));
        if let Some(p) = param {
            params.push(p);
        }
    }

    if !params.is_empty() {
        result.push(' ');
        result.push_str(&params.join(" "));
    }
    result
}

/// Build a displayable mode string from user modes.
fn build_user_mode_string(modes: &[irc::proto::Mode<irc::proto::UserMode>]) -> String {
    let mut result = String::new();
    let mut last_sign = ' ';

    for mode in modes {
        let (sign, m) = match mode {
            irc::proto::Mode::Plus(m, _) => ('+', m),
            irc::proto::Mode::Minus(m, _) => ('-', m),
            irc::proto::Mode::NoPrefix(m) => (' ', m),
        };
        if sign != last_sign && sign != ' ' {
            result.push(sign);
            last_sign = sign;
        }
        result.push(user_mode_letter(m));
    }
    result
}

const fn channel_mode_letter(m: &irc::proto::ChannelMode) -> char {
    use irc::proto::ChannelMode;
    match m {
        ChannelMode::Ban => 'b',
        ChannelMode::Exception => 'e',
        ChannelMode::Limit => 'l',
        ChannelMode::InviteOnly => 'i',
        ChannelMode::InviteException => 'I',
        ChannelMode::Key => 'k',
        ChannelMode::Moderated => 'm',
        ChannelMode::RegisteredOnly => 'R',
        ChannelMode::Secret => 's',
        ChannelMode::ProtectedTopic => 't',
        ChannelMode::NoExternalMessages => 'n',
        ChannelMode::Founder => 'q',
        ChannelMode::Admin => 'a',
        ChannelMode::Oper => 'o',
        ChannelMode::Halfop => 'h',
        ChannelMode::Voice => 'v',
        ChannelMode::Unknown(c) => *c,
    }
}

const fn user_mode_letter(m: &irc::proto::UserMode) -> char {
    use irc::proto::UserMode;
    match m {
        UserMode::Away => 'a',
        UserMode::Invisible => 'i',
        UserMode::Wallops => 'w',
        UserMode::Restricted => 'r',
        UserMode::Oper => 'o',
        UserMode::LocalOper => 'O',
        UserMode::ServerNotices => 's',
        UserMode::MaskedHost => 'x',
        UserMode::Unknown(c) => *c,
    }
}

fn handle_invite(
    state: &mut AppState,
    conn_id: &str,
    prefix: Option<&Prefix>,
    nick: &str,
    channel: &str,
    tags: HashMap<String, String>,
) {
    let inviter = extract_nick(prefix).unwrap_or_default();

    // Show invite in active buffer or server buffer
    let label = state
        .connections
        .get(conn_id)
        .map_or("Status", |c| c.label.as_str());
    let _ = nick; // nick is us (the invited user)
    let buffer_id = state
        .active_buffer_id
        .clone()
        .unwrap_or_else(|| make_buffer_id(conn_id, label));

    let id = state.next_message_id();
    state.add_message(
        &buffer_id,
        Message {
            id,
            timestamp: Utc::now(),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text: format!("{inviter} invites you to {channel}"),
            highlight: true,
            event_key: None,
            event_params: None, log_msg_id: None, log_ref_id: None,
            tags,
        },
    );
}

fn handle_wallops(
    state: &mut AppState,
    conn_id: &str,
    prefix: Option<&Prefix>,
    text: &str,
) {
    let from = extract_nick(prefix).unwrap_or_else(|| "server".to_string());
    let label = state
        .connections
        .get(conn_id)
        .map_or("Status", |c| c.label.as_str());
    let buffer_id = make_buffer_id(conn_id, label);
    emit(state, &buffer_id, &format!("%Ze0af68[Wallops/{from}]%N {text}"));
}

#[expect(clippy::too_many_lines, reason = "dispatcher pattern")]
fn handle_response(
    state: &mut AppState,
    conn_id: &str,
    response: Response,
    args: &[String],
) {
    match response {
        // RPL_MYINFO: args = [our_nick, server_name, version, user_modes, channel_modes, ...]
        Response::RPL_MYINFO => {
            if args.len() >= 3 {
                let server_name = &args[1];
                // Store in isupport for reference
                if let Some(conn) = state.connections.get_mut(conn_id) {
                    conn.isupport.insert("SERVER_NAME".to_string(), server_name.clone());
                }
            }
        }

        // RPL_ISUPPORT: args = [our_nick, TOKEN=VALUE, TOKEN=VALUE, ..., "are supported by this server"]
        Response::RPL_ISUPPORT => {
            if args.len() >= 2 {
                // Parse KEY=VALUE tokens (skip first arg = our nick, skip last = trailing text)
                let tokens = &args[1..args.len().saturating_sub(1)];
                // Store raw tokens in the HashMap (legacy)
                for token in tokens {
                    if let Some((key, value)) = token.split_once('=') {
                        if let Some(conn) = state.connections.get_mut(conn_id) {
                            conn.isupport.insert(key.to_string(), value.to_string());
                        }
                    } else if let Some(conn) = state.connections.get_mut(conn_id) {
                        conn.isupport.insert(token.clone(), String::new());
                    }
                }
                // Structured parsing
                let token_strs: Vec<&str> = tokens.iter().map(String::as_str).collect();
                if let Some(conn) = state.connections.get_mut(conn_id) {
                    conn.isupport_parsed.parse_tokens(&token_strs);
                }
                // Update label from NETWORK for ad-hoc connections
                if let Some(network) = state
                    .connections
                    .get(conn_id)
                    .and_then(|c| c.isupport_parsed.network().map(str::to_owned))
                {
                    update_label_from_network(state, conn_id, &network);
                }
            }
        }

        // RPL_NAMREPLY: args = [our_nick, "=" | "*" | "@", channel, "nick1 nick2 ..."]
        //
        // Supports:
        // - multi-prefix: server sends ALL mode prefixes per nick (e.g. `@+nick`)
        // - userhost-in-names: server sends `nick!user@host` format
        Response::RPL_NAMREPLY => {
            if args.len() >= 4 {
                let channel = &args[2];
                let buffer_id = make_buffer_id(conn_id, channel);
                let nicks_str = &args[3];

                // Get prefix map and userhost-in-names state from connection
                let (prefix_map, has_userhost) = state
                    .connections
                    .get(conn_id)
                    .map_or_else(
                        || (vec![('o', '@'), ('v', '+')], false),
                        |c| (c.isupport_parsed.prefix_map(), c.enabled_caps.contains("userhost-in-names")),
                    );

                for nick_with_prefix in nicks_str.split_whitespace() {
                    let entry = parse_names_entry(nick_with_prefix, &prefix_map, has_userhost);
                    state.add_nick(&buffer_id, entry);
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

        // === WHOIS responses — show in active buffer ===

        // RPL_WHOISUSER: args = [our_nick, nick, user, host, *, realname]
        Response::RPL_WHOISUSER => {
            if args.len() >= 6 {
                let target_buf = whois_buffer(state, conn_id);
                emit(state, &target_buf, &format!(
                    "%Z7aa2f7───── WHOIS {} ──────────────────────────%N", args[1]
                ));
                emit(state, &target_buf, &format!(
                    "%Zc0caf5{}%Z565f89 ({}@{})%N", args[1], args[2], args[3]
                ));
                if args.len() >= 6 && !args[5].is_empty() {
                    emit(state, &target_buf, &format!("  %Za9b1d6{}%N", args[5]));
                }
            }
        }
        // RPL_WHOISSERVER: args = [our_nick, nick, server, server_info]
        Response::RPL_WHOISSERVER => {
            if args.len() >= 4 {
                let target_buf = whois_buffer(state, conn_id);
                let info = if args[3].is_empty() {
                    String::new()
                } else {
                    format!(" ({})", args[3])
                };
                emit(state, &target_buf, &format!(
                    "%Z565f89  server: %Za9b1d6{}{info}%N", args[2]
                ));
            }
        }
        // RPL_WHOISOPERATOR: args = [our_nick, nick, "is an IRC operator"]
        Response::RPL_WHOISOPERATOR => {
            if args.len() >= 3 {
                let target_buf = whois_buffer(state, conn_id);
                emit(state, &target_buf, &format!("  %Zbb9af7{}%N", args[2]));
            }
        }
        // RPL_WHOISIDLE: args = [our_nick, nick, idle_secs, signon_time, ...]
        Response::RPL_WHOISIDLE => {
            if args.len() >= 3 {
                let target_buf = whois_buffer(state, conn_id);
                let idle = args[2].parse::<u64>().unwrap_or(0);
                let mut line = format!("%Z565f89  idle: %Za9b1d6{}", format_duration(idle));
                if args.len() >= 4
                    && let Ok(ts) = args[3].parse::<i64>()
                {
                    let dt = chrono::DateTime::from_timestamp(ts, 0)
                        .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                        .unwrap_or_default();
                    write!(line, "%Z565f89, signon: %Za9b1d6{dt}").unwrap();
                }
                line.push_str("%N");
                emit(state, &target_buf, &line);
            }
        }
        // RPL_WHOISCHANNELS: args = [our_nick, nick, channels]
        Response::RPL_WHOISCHANNELS => {
            if args.len() >= 3 {
                let target_buf = whois_buffer(state, conn_id);
                emit(state, &target_buf, &format!(
                    "%Z565f89  channels: %Za9b1d6{}%N", args[2]
                ));
            }
        }
        // RPL_ENDOFWHOIS: args = [our_nick, nick, "End of WHOIS list"]
        Response::RPL_ENDOFWHOIS => {
            let target_buf = whois_buffer(state, conn_id);
            emit(state, &target_buf,
                "%Z7aa2f7─────────────────────────────────────────────%N");
        }

        // RPL_AWAY: args = [our_nick, nick, away_message]
        Response::RPL_AWAY => {
            if args.len() >= 3 {
                let target_buf = whois_buffer(state, conn_id);
                emit(state, &target_buf, &format!(
                    "%Z565f89  away: %Ze0af68{}%N", args[2]
                ));
            }
        }

        // === Ban list responses ===

        // RPL_BANLIST: args = [our_nick, channel, banmask, set_by, timestamp]
        Response::RPL_BANLIST => {
            if args.len() >= 3 {
                let target_buf = active_or_server_buffer(state, conn_id);
                let set_info = if args.len() >= 5 {
                    format!(" (set by {} {})", args[3], format_timestamp(&args[4]))
                } else {
                    String::new()
                };
                emit(state, &target_buf, &format!(
                    "%Z565f89  ban: %Za9b1d6{}{set_info}%N", args[2]
                ));
            }
        }
        // RPL_ENDOFBANLIST
        Response::RPL_ENDOFBANLIST => {
            let target_buf = active_or_server_buffer(state, conn_id);
            emit(state, &target_buf, "%Z565f89  End of ban list%N");
        }

        // === Exception list responses (+e) ===
        Response::RPL_EXCEPTLIST => {
            if args.len() >= 3 {
                let target_buf = active_or_server_buffer(state, conn_id);
                let set_info = if args.len() >= 5 {
                    format!(" (set by {} {})", args[3], format_timestamp(&args[4]))
                } else {
                    String::new()
                };
                emit(state, &target_buf, &format!(
                    "%Z565f89  except: %Za9b1d6{}{set_info}%N", args[2]
                ));
            }
        }
        Response::RPL_ENDOFEXCEPTLIST => {
            let target_buf = active_or_server_buffer(state, conn_id);
            emit(state, &target_buf, "%Z565f89  End of exception list%N");
        }

        // === Invite exception list responses (+I) ===
        Response::RPL_INVITELIST => {
            if args.len() >= 3 {
                let target_buf = active_or_server_buffer(state, conn_id);
                let set_info = if args.len() >= 5 {
                    format!(" (set by {} {})", args[3], format_timestamp(&args[4]))
                } else {
                    String::new()
                };
                emit(state, &target_buf, &format!(
                    "%Z565f89  invex: %Za9b1d6{}{set_info}%N", args[2]
                ));
            }
        }
        Response::RPL_ENDOFINVITELIST => {
            let target_buf = active_or_server_buffer(state, conn_id);
            emit(state, &target_buf, "%Z565f89  End of invite exception list%N");
        }

        // === MOTD responses ===

        Response::RPL_MOTDSTART => {
            let target_buf = server_buffer(state, conn_id);
            emit(state, &target_buf, "%Z56b6c2── MOTD ──────────────────────────────────────%N");
        }
        Response::RPL_MOTD => {
            if args.len() >= 2 {
                let target_buf = server_buffer(state, conn_id);
                let line = &args[args.len() - 1];
                emit(state, &target_buf, &format!("%Z7aa2f7{line}%N"));
            }
        }
        Response::RPL_ENDOFMOTD => {
            let target_buf = server_buffer(state, conn_id);
            emit(state, &target_buf, "%Z56b6c2── End of MOTD ─────────────────────────────%N");
        }

        // === Nick collision / erroneous nick ===

        Response::ERR_NICKNAMEINUSE => {
            // params: [current, attempted_nick, "Nickname is already in use"]
            let attempted = if args.len() >= 2 { &args[1] } else { "unknown" };
            let new_nick = format!("{attempted}_");
            let target_buf = server_buffer(state, conn_id);
            emit(
                state,
                &target_buf,
                &format!("%Ze0af68Nick {attempted} is in use, trying {new_nick}...%N"),
            );
            // Update the connection's nick so the next attempt uses it.
            // NOTE: The actual NICK command must be sent by the caller (app.rs)
            // since events.rs only has access to AppState, not the IRC sender.
            if let Some(conn) = state.connections.get_mut(conn_id) {
                conn.nick = new_nick;
            }
        }
        Response::ERR_ERRONEOUSNICKNAME => {
            let attempted = if args.len() >= 2 { &args[1] } else { "unknown" };
            let reason = if args.len() >= 3 { &args[2] } else { "Erroneous nickname" };
            let target_buf = server_buffer(state, conn_id);
            emit(
                state,
                &target_buf,
                &format!("%Zff6b6bErroneous nick {attempted}: {reason}%N"),
            );
        }

        // === Away responses ===

        Response::RPL_NOWAWAY => {
            let target_buf = active_or_server_buffer(state, conn_id);
            emit(state, &target_buf, "%Z56b6c2You are now marked as away%N");
        }
        Response::RPL_UNAWAY => {
            let target_buf = active_or_server_buffer(state, conn_id);
            emit(state, &target_buf, "%Z56b6c2You are no longer marked as away%N");
        }

        // === LIST responses ===

        Response::RPL_LIST => {
            // params: [our_nick, channel, user_count, topic]
            if args.len() >= 3 {
                let channel = &args[1];
                let user_count = &args[2];
                let topic = if args.len() >= 4 { &args[3] } else { "" };
                let target_buf = active_or_server_buffer(state, conn_id);
                if topic.is_empty() {
                    emit(state, &target_buf, &format!(
                        "%Zc0caf5{channel}%Z565f89 [{user_count} users]%N"
                    ));
                } else {
                    emit(state, &target_buf, &format!(
                        "%Zc0caf5{channel}%Z565f89 [{user_count} users]%N: {topic}"
                    ));
                }
            }
        }
        Response::RPL_LISTEND => {
            let target_buf = active_or_server_buffer(state, conn_id);
            emit(state, &target_buf, "%Z565f89End of channel list%N");
        }

        // === WHO responses ===

        Response::RPL_WHOREPLY => {
            // params: [our_nick, channel, user, host, server, nick, flags, hopcount_realname]
            if args.len() >= 8 {
                let channel = &args[1];
                let user = &args[2];
                let host = &args[3];
                let nick = &args[5];
                let flags = &args[6];
                let realname = &args[7];
                let target_buf = active_or_server_buffer(state, conn_id);
                emit(state, &target_buf, &format!(
                    "%Zc0caf5{nick}%Z565f89 ({user}@{host}) [{flags}] {channel}%Za9b1d6 {realname}%N"
                ));
            }
        }
        Response::RPL_ENDOFWHO => {
            let target_buf = active_or_server_buffer(state, conn_id);
            emit(state, &target_buf, "%Z565f89End of WHO list%N");
        }

        // === WHOWAS responses ===

        Response::RPL_WHOWASUSER => {
            // params: [our_nick, nick, user, host, *, realname]
            if args.len() >= 6 {
                let nick = &args[1];
                let user = &args[2];
                let host = &args[3];
                let realname = &args[5];
                let target_buf = active_or_server_buffer(state, conn_id);
                emit(state, &target_buf, &format!(
                    "%Zc0caf5{nick}%Z565f89 was ({user}@{host})%Za9b1d6 {realname}%N"
                ));
            }
        }
        Response::RPL_ENDOFWHOWAS => {
            let target_buf = active_or_server_buffer(state, conn_id);
            emit(state, &target_buf, "%Z565f89End of WHOWAS%N");
        }

        // Silently consume RPL_ENDOFNAMES — we already have the nick list
        Response::RPL_ENDOFNAMES => {}

        _ => {
            // Show unknown numerics in server buffer — skip our own nick arg
            let label = state
                .connections
                .get(conn_id)
                .map_or("Status", |c| c.label.as_str());
            let buffer_id = make_buffer_id(conn_id, label);
            // Skip args[0] which is our nick
            let text = if args.len() > 1 {
                args[1..].join(" ")
            } else {
                args.join(" ")
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
                    text,
                    highlight: false,
                    event_key: None,
                    event_params: None, log_msg_id: None, log_ref_id: None,
                    tags: HashMap::new(),
                },
            );
        }
    }
}

/// Update connection label and server buffer name from NETWORK token.
/// Only applies to ad-hoc connections where the label still matches the address.
fn update_label_from_network(state: &mut AppState, conn_id: &str, network_name: &str) {
    let current_label = match state.connections.get(conn_id) {
        Some(conn) => conn.label.clone(),
        None => return,
    };

    // Only update if label looks like a raw address (contains a dot = ad-hoc)
    // Configured servers already have a human-friendly label from config.
    if !current_label.contains('.') {
        return;
    }

    // Update connection label
    if let Some(conn) = state.connections.get_mut(conn_id) {
        conn.label = network_name.to_string();
    }

    // Rename the server buffer: change id and name
    let old_buf_id = make_buffer_id(conn_id, &current_label);
    let new_buf_id = make_buffer_id(conn_id, network_name);
    if let Some(mut buf) = state.buffers.shift_remove(&old_buf_id) {
        buf.id.clone_from(&new_buf_id);
        buf.name = network_name.to_string();
        state.buffers.insert(new_buf_id.clone(), buf);

        // Update active buffer reference if it pointed to the old id
        if state.active_buffer_id.as_deref() == Some(&old_buf_id) {
            state.active_buffer_id = Some(new_buf_id);
        }
    }
}

/// Helper: emit a formatted event message to a buffer.
fn emit(state: &mut AppState, buffer_id: &str, text: &str) {
    let id = state.next_message_id();
    state.add_message(
        buffer_id,
        Message {
            id,
            timestamp: Utc::now(),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text: text.to_string(),
            highlight: false,
            event_key: None,
            event_params: None, log_msg_id: None, log_ref_id: None,
            tags: HashMap::new(),
        },
    );
}

/// Get the server's status buffer ID.
fn server_buffer(state: &AppState, conn_id: &str) -> String {
    let label = state
        .connections
        .get(conn_id)
        .map_or("Status", |c| c.label.as_str());
    make_buffer_id(conn_id, label)
}

/// Get the active buffer, or fall back to the server buffer.
fn active_or_server_buffer(state: &AppState, conn_id: &str) -> String {
    state.active_buffer_id.clone().unwrap_or_else(|| {
        let label = state
            .connections
            .get(conn_id)
            .map_or("Status", |c| c.label.as_str());
        make_buffer_id(conn_id, label)
    })
}

/// Get the buffer where WHOIS output should go.
fn whois_buffer(state: &AppState, conn_id: &str) -> String {
    active_or_server_buffer(state, conn_id)
}

/// Format a duration in seconds to a human-readable string.
fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else if secs < 86400 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d {}h", secs / 86400, (secs % 86400) / 3600)
    }
}

/// Format a unix timestamp string.
fn format_timestamp(ts_str: &str) -> String {
    ts_str
        .parse::<i64>()
        .ok()
        .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

/// Parse a single entry from a NAMES reply, handling multi-prefix and
/// userhost-in-names capabilities.
///
/// `prefix_map` is the server's `(mode_char, prefix_char)` list from ISUPPORT
/// PREFIX (e.g. `[('o', '@'), ('v', '+')]`).
///
/// When `has_userhost` is true, the nick portion is expected in `nick!user@host`
/// format.
///
/// # Examples
///
/// Standard:   `@nick`         → prefix="@", modes="o", nick="nick"
/// Multi:      `@+nick`        → prefix="@+", modes="ov", nick="nick"
/// Userhost:   `@+nick!u@host` → prefix="@+", modes="ov", nick="nick", ident="u", host="host"
fn parse_names_entry(
    raw: &str,
    prefix_map: &[(char, char)],
    has_userhost: bool,
) -> NickEntry {
    // Strip all leading prefix characters, using the server's PREFIX map
    // to determine which characters are valid prefixes and their modes.
    let mut prefix = String::new();
    let mut modes = String::new();
    let mut rest = raw;
    while let Some(c) = rest.chars().next() {
        if let Some(&(mode, _)) = prefix_map.iter().find(|&&(_, p)| p == c) {
            prefix.push(c);
            modes.push(mode);
            rest = &rest[c.len_utf8()..];
        } else {
            break;
        }
    }

    // Parse nick!user@host if userhost-in-names is enabled
    let (nick, ident, host) = if has_userhost {
        parse_userhost(rest)
    } else {
        (rest.to_string(), None, None)
    };

    NickEntry {
        nick,
        prefix,
        modes,
        away: false,
        account: None,
        ident,
        host,
    }
}

/// Parse `nick!user@host` into `(nick, Some(user), Some(host))`.
/// If the format doesn't match, returns `(input, None, None)`.
fn parse_userhost(input: &str) -> (String, Option<String>, Option<String>) {
    if let Some(bang_pos) = input.find('!') {
        let nick = &input[..bang_pos];
        let rest = &input[bang_pos + 1..];
        if let Some(at_pos) = rest.find('@') {
            let ident = &rest[..at_pos];
            let host = &rest[at_pos + 1..];
            return (nick.to_string(), Some(ident.to_string()), Some(host.to_string()));
        }
    }
    (input.to_string(), None, None)
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
            isupport_parsed: crate::irc::isupport::Isupport::new(),
            error: None,
            lag: None,
            reconnect_attempts: 0,
            max_reconnect_attempts: 10,
            reconnect_delay_secs: 30,
            next_reconnect: None,
            should_reconnect: true,
            joined_channels: Vec::new(),
            origin_config: crate::config::ServerConfig {
                label: "TestServer".to_string(),
                address: "irc.test.net".to_string(),
                port: 6697,
                tls: true,
                tls_verify: true,
                autoconnect: false,
                channels: vec![],
                nick: None,
                username: None,
                realname: None,
                password: None,
                sasl_user: None,
                sasl_pass: None,
                bind_ip: None,
                encoding: None,
                auto_reconnect: Some(true),
                reconnect_delay: None,
                reconnect_max_retries: None,
                autosendcmd: None,
            },
            enabled_caps: std::collections::HashSet::new(),
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
                ident: None,
                host: None,
            },
        );
        state
    }

    fn make_irc_msg(prefix: Option<&str>, command: Command) -> IrcMessage {
        IrcMessage {
            tags: None,
            prefix: prefix.map(Prefix::new_from_str),
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

    #[test]
    fn privmsg_ctcp_action_mention() {
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");
        let msg = make_irc_msg(
            Some("alice!user@host"),
            Command::PRIVMSG("#test".into(), "\x01ACTION pokes me\x01".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert_eq!(buf.messages[0].message_type, MessageType::Action);
        assert!(buf.messages[0].highlight);
        assert_eq!(buf.activity, ActivityLevel::Mention);
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
        assert!(buf.messages.last().unwrap().text.contains("carol (user@host) has joined"));
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
                ident: None,
                host: None,
            },
        );
        let msg = make_irc_msg(
            Some("dave!user@host"),
            Command::PART("#test".into(), Some("leaving".into())),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(!buf.users.contains_key("dave"));
        assert!(buf.messages.last().unwrap().text.contains("dave (user@host) has left"));
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
                ident: None,
                host: None,
            },
        );
        let msg = make_irc_msg(
            Some("eve!user@host"),
            Command::QUIT(Some("gone".into())),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(!buf.users.contains_key("eve"));
        assert!(buf.messages.last().unwrap().text.contains("eve (user@host) has quit"));
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
                ident: None,
                host: None,
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
                ident: None,
                host: None,
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

    // === parse_names_entry unit tests (multi-prefix + userhost-in-names) ===

    #[test]
    fn parse_names_standard_single_prefix() {
        let prefix_map = vec![('o', '@'), ('v', '+')];
        let entry = parse_names_entry("@nick", &prefix_map, false);
        assert_eq!(entry.nick, "nick");
        assert_eq!(entry.prefix, "@");
        assert_eq!(entry.modes, "o");
        assert!(entry.ident.is_none());
        assert!(entry.host.is_none());
    }

    #[test]
    fn parse_names_no_prefix() {
        let prefix_map = vec![('o', '@'), ('v', '+')];
        let entry = parse_names_entry("regular", &prefix_map, false);
        assert_eq!(entry.nick, "regular");
        assert_eq!(entry.prefix, "");
        assert_eq!(entry.modes, "");
    }

    #[test]
    fn parse_names_multi_prefix_two_modes() {
        let prefix_map = vec![('o', '@'), ('v', '+')];
        let entry = parse_names_entry("@+nick", &prefix_map, false);
        assert_eq!(entry.nick, "nick");
        assert_eq!(entry.prefix, "@+");
        assert_eq!(entry.modes, "ov");
        assert!(entry.ident.is_none());
        assert!(entry.host.is_none());
    }

    #[test]
    fn parse_names_multi_prefix_five_modes() {
        let prefix_map = vec![
            ('q', '~'),
            ('a', '&'),
            ('o', '@'),
            ('h', '%'),
            ('v', '+'),
        ];
        let entry = parse_names_entry("~&@%+nick", &prefix_map, false);
        assert_eq!(entry.nick, "nick");
        assert_eq!(entry.prefix, "~&@%+");
        assert_eq!(entry.modes, "qaohv");
    }

    #[test]
    fn parse_names_userhost_in_names() {
        let prefix_map = vec![('o', '@'), ('v', '+')];
        let entry = parse_names_entry("@+nick!user@host.com", &prefix_map, true);
        assert_eq!(entry.nick, "nick");
        assert_eq!(entry.prefix, "@+");
        assert_eq!(entry.modes, "ov");
        assert_eq!(entry.ident.as_deref(), Some("user"));
        assert_eq!(entry.host.as_deref(), Some("host.com"));
    }

    #[test]
    fn parse_names_userhost_no_prefix() {
        let prefix_map = vec![('o', '@'), ('v', '+')];
        let entry = parse_names_entry("nick!user@host.com", &prefix_map, true);
        assert_eq!(entry.nick, "nick");
        assert_eq!(entry.prefix, "");
        assert_eq!(entry.modes, "");
        assert_eq!(entry.ident.as_deref(), Some("user"));
        assert_eq!(entry.host.as_deref(), Some("host.com"));
    }

    #[test]
    fn parse_names_userhost_not_enabled_preserves_raw_nick() {
        // Without userhost-in-names, nick!user@host is treated as the nick
        let prefix_map = vec![('o', '@'), ('v', '+')];
        let entry = parse_names_entry("@nick!user@host.com", &prefix_map, false);
        assert_eq!(entry.nick, "nick!user@host.com");
        assert_eq!(entry.prefix, "@");
        assert_eq!(entry.modes, "o");
        assert!(entry.ident.is_none());
        assert!(entry.host.is_none());
    }

    // === parse_names_entry integration via RPL_NAMREPLY ===

    #[test]
    fn rpl_namreply_multi_prefix() {
        let mut state = make_test_state();
        // Set PREFIX=(ov)@+ on the connection's isupport
        if let Some(conn) = state.connections.get_mut("test") {
            conn.isupport_parsed.parse_tokens(&["PREFIX=(ov)@+"]);
        }
        let msg = make_irc_msg(
            None,
            Command::Response(
                Response::RPL_NAMREPLY,
                vec![
                    "me".into(),
                    "=".into(),
                    "#test".into(),
                    "@+alice @bob +carol regular".into(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        let alice = buf.users.get("alice").unwrap();
        assert_eq!(alice.prefix, "@+");
        assert_eq!(alice.modes, "ov");
        let bob = buf.users.get("bob").unwrap();
        assert_eq!(bob.prefix, "@");
        assert_eq!(bob.modes, "o");
        let carol = buf.users.get("carol").unwrap();
        assert_eq!(carol.prefix, "+");
        assert_eq!(carol.modes, "v");
        let regular = buf.users.get("regular").unwrap();
        assert_eq!(regular.prefix, "");
        assert_eq!(regular.modes, "");
    }

    #[test]
    fn rpl_namreply_userhost_in_names() {
        let mut state = make_test_state();
        if let Some(conn) = state.connections.get_mut("test") {
            conn.isupport_parsed.parse_tokens(&["PREFIX=(ov)@+"]);
            conn.enabled_caps.insert("userhost-in-names".to_string());
        }
        let msg = make_irc_msg(
            None,
            Command::Response(
                Response::RPL_NAMREPLY,
                vec![
                    "me".into(),
                    "=".into(),
                    "#test".into(),
                    "@+alice!auser@ahost.net bob!buser@bhost.org".into(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        let alice = buf.users.get("alice").unwrap();
        assert_eq!(alice.prefix, "@+");
        assert_eq!(alice.modes, "ov");
        assert_eq!(alice.ident.as_deref(), Some("auser"));
        assert_eq!(alice.host.as_deref(), Some("ahost.net"));
        let bob = buf.users.get("bob").unwrap();
        assert_eq!(bob.prefix, "");
        assert_eq!(bob.modes, "");
        assert_eq!(bob.ident.as_deref(), Some("buser"));
        assert_eq!(bob.host.as_deref(), Some("bhost.org"));
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

    // === extended-join tests ===

    #[test]
    fn extended_join_with_account() {
        let mut state = make_test_state();
        // extended-join: JOIN #channel account :Real Name
        let msg = make_irc_msg(
            Some("carol!user@host"),
            Command::JOIN("#test".into(), Some("patrick".into()), Some("Real Name".into())),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(buf.users.contains_key("carol"));
        let entry = buf.users.get("carol").unwrap();
        assert_eq!(entry.account.as_deref(), Some("patrick"));
    }

    #[test]
    fn extended_join_without_account() {
        let mut state = make_test_state();
        // extended-join with "*" means not logged in
        let msg = make_irc_msg(
            Some("carol!user@host"),
            Command::JOIN("#test".into(), Some("*".into()), Some("Real Name".into())),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(buf.users.contains_key("carol"));
        let entry = buf.users.get("carol").unwrap();
        assert_eq!(entry.account, None);
    }

    #[test]
    fn standard_join_no_account() {
        let mut state = make_test_state();
        // Standard JOIN (1 arg) — no account info
        let msg = make_irc_msg(
            Some("carol!user@host"),
            Command::JOIN("#test".into(), None, None),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(buf.users.contains_key("carol"));
        let entry = buf.users.get("carol").unwrap();
        assert_eq!(entry.account, None);
    }

    // === account-notify tests ===

    #[test]
    fn account_notify_login() {
        let mut state = make_test_state();
        // Add user to channel first
        state.add_nick(
            "test/#test",
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

        let msg = make_irc_msg(
            Some("alice!user@host"),
            Command::ACCOUNT("alice_account".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        let entry = buf.users.get("alice").unwrap();
        assert_eq!(entry.account.as_deref(), Some("alice_account"));
        // Should have an event message
        assert!(buf.messages.last().unwrap().text.contains("alice is now logged in as alice_account"));
    }

    #[test]
    fn account_notify_logout() {
        let mut state = make_test_state();
        // Add user with an account
        state.add_nick(
            "test/#test",
            NickEntry {
                nick: "alice".to_string(),
                prefix: String::new(),
                modes: String::new(),
                away: false,
                account: Some("alice_account".to_string()),
                ident: None,
                host: None,
            },
        );

        let msg = make_irc_msg(
            Some("alice!user@host"),
            Command::ACCOUNT("*".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        let entry = buf.users.get("alice").unwrap();
        assert_eq!(entry.account, None);
        assert!(buf.messages.last().unwrap().text.contains("alice has logged out"));
    }

    #[test]
    fn account_notify_updates_all_shared_buffers() {
        let mut state = make_test_state();
        // Create a second channel buffer
        let chan2_id = make_buffer_id("test", "#other");
        state.add_buffer(Buffer {
            id: chan2_id.clone(),
            connection_id: "test".to_string(),
            buffer_type: BufferType::Channel,
            name: "#other".to_string(),
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

        // Add alice to both channels
        for buf_id in &["test/#test", "test/#other"] {
            state.add_nick(
                buf_id,
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
        }

        let msg = make_irc_msg(
            Some("alice!user@host"),
            Command::ACCOUNT("alice_acct".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        // Both buffers should have the account updated
        let entry1 = state.buffers.get("test/#test").unwrap().users.get("alice").unwrap();
        assert_eq!(entry1.account.as_deref(), Some("alice_acct"));
        let entry2 = state.buffers.get("test/#other").unwrap().users.get("alice").unwrap();
        assert_eq!(entry2.account.as_deref(), Some("alice_acct"));
    }

    // === account-tag tests ===

    #[test]
    fn account_tag_updates_nick_entry_on_privmsg() {
        let mut state = make_test_state();
        // Add alice to channel without an account
        state.add_nick(
            "test/#test",
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

        // PRIVMSG with account tag
        let mut msg = make_irc_msg(
            Some("alice!user@host"),
            Command::PRIVMSG("#test".into(), "hello".into()),
        );
        msg.tags = Some(vec![
            irc::proto::message::Tag("account".to_string(), Some("alice_acct".to_string())),
        ]);
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        let entry = buf.users.get("alice").unwrap();
        assert_eq!(entry.account.as_deref(), Some("alice_acct"));
    }

    #[test]
    fn extended_join_account_on_own_join() {
        let mut state = make_test_state();
        // Our own extended-join — should create buffer and not crash
        // (account tracking for self is less critical but shouldn't break)
        let msg = make_irc_msg(
            Some("me!user@host"),
            Command::JOIN("#newchan".into(), Some("my_account".into()), Some("My Real Name".into())),
        );
        handle_irc_message(&mut state, "test", &msg);

        assert!(state.buffers.contains_key("test/#newchan"));
        let buf = state.buffers.get("test/#newchan").unwrap();
        assert_eq!(buf.buffer_type, BufferType::Channel);
    }
}

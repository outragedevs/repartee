use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::time::Instant;

use chrono::{DateTime, Utc};
use irc::proto::{Command, Message as IrcMessage, Prefix, Response};

use crate::config::IgnoreLevel;
use crate::irc::formatting::{
    extract_nick, extract_nick_userhost, is_channel, is_server_prefix, modes_to_prefix,
    strip_irc_formatting,
};
use crate::irc::ignore::should_ignore;
use crate::state::AppState;
use crate::state::buffer::{
    ActivityLevel, Buffer, BufferType, Message, MessageType, NickEntry, make_buffer_id,
};
use crate::state::connection::ConnectionStatus;

/// Route an incoming IRC protocol message to the appropriate handler,
/// mutating `AppState` as needed.
#[expect(
    clippy::too_many_lines,
    reason = "IRC command dispatcher — one arm per message type"
)]
pub fn handle_irc_message(state: &mut AppState, conn_id: &str, msg: &IrcMessage) {
    let our_nick = state
        .connections
        .get(conn_id)
        .map(|c| c.nick.clone())
        .unwrap_or_default();

    let tags = extract_tags(msg);

    match &msg.command {
        Command::PRIVMSG(target, text) => {
            handle_privmsg(
                state,
                conn_id,
                &our_nick,
                msg.prefix.as_ref(),
                target,
                text,
                tags,
            );
        }
        Command::NOTICE(target, text) => {
            handle_notice(state, conn_id, msg.prefix.as_ref(), target, text, tags);
        }
        Command::JOIN(channel, account, realname) => {
            handle_join(
                state,
                conn_id,
                &our_nick,
                msg.prefix.as_ref(),
                channel,
                account.as_deref(),
                realname.as_deref(),
                tags,
            );
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
            handle_quit(
                state,
                conn_id,
                &our_nick,
                msg.prefix.as_ref(),
                reason.as_deref(),
                tags,
            );
        }
        Command::NICK(new_nick) => {
            handle_nick_change(
                state,
                conn_id,
                &our_nick,
                msg.prefix.as_ref(),
                new_nick,
                tags,
            );
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
            handle_topic(
                state,
                conn_id,
                msg.prefix.as_ref(),
                channel,
                topic.as_deref(),
                tags,
            );
        }
        Command::ChannelMODE(target, _) | Command::UserMODE(target, _) => {
            handle_mode(state, conn_id, msg.prefix.as_ref(), target, msg, tags);
        }
        Command::INVITE(nick, channel) => {
            handle_invite(
                state,
                conn_id,
                &our_nick,
                msg.prefix.as_ref(),
                nick,
                channel,
                tags,
            );
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
        Command::AWAY(reason) => {
            handle_away(state, conn_id, msg.prefix.as_ref(), reason.as_deref());
        }
        Command::CHGHOST(new_user, new_host) => {
            handle_chghost(
                state,
                conn_id,
                msg.prefix.as_ref(),
                new_user,
                new_host,
                tags,
            );
        }
        Command::ERROR(message) => {
            handle_error(state, conn_id, message);
        }
        // WHOX response (354) comes as Command::Raw because the irc crate
        // doesn't recognize this non-standard numeric.
        Command::Raw(cmd, args) if cmd == "354" => {
            handle_whox_reply(state, conn_id, args);
        }
        // Catch-all for unknown numerics that irc-proto doesn't define
        // (e.g. IRCnet's 344/345 for reop list). Display them like the
        // Response catch-all does — errors to active window, info to server.
        Command::Raw(cmd, args) if cmd.len() == 3 && cmd.chars().all(|c| c.is_ascii_digit()) => {
            // Unknown numerics are typically responses to user commands,
            // so always route to the active window.
            let buffer_id = active_or_server_buffer(state, conn_id);
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
                    event_params: None,
                    log_msg_id: None,
                    log_ref_id: None,
                    tags: None,
                },
            );
        }
        // PING handled automatically by the irc crate
        _ => {}
    }
}

/// Update connection status to Connected and log to the status buffer.
pub fn handle_connected(state: &mut AppState, conn_id: &str) {
    state.update_connection_status(conn_id, ConnectionStatus::Connected);

    // Reset reconnect state on successful connection.
    // Reset ISUPPORT (server sends fresh 005 lines) and silent WHO state.
    // Do NOT clear enabled_caps — the caller sets them from the CAP negotiation
    // result (IrcEvent::Connected carries the negotiated caps). On reconnect,
    // `conn.enabled_caps = enabled_caps` at the call site replaces the old set
    // entirely, so stale caps from a previous session are already gone.
    if let Some(conn) = state.connections.get_mut(conn_id) {
        conn.reconnect_attempts = 0;
        conn.next_reconnect = None;
        conn.error = None;
        conn.isupport_parsed = crate::irc::isupport::Isupport::default();
        conn.silent_who_channels.clear();
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
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
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
            b.connection_id == conn_id && b.buffer_type == crate::state::buffer::BufferType::Channel
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
            b.connection_id == conn_id && b.buffer_type == crate::state::buffer::BufferType::Channel
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
        if conn.should_reconnect {
            let delay =
                calculate_reconnect_delay(conn.reconnect_delay_secs, conn.reconnect_attempts);
            conn.next_reconnect =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(delay));
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
        && conn.should_reconnect
    {
        let delay = calculate_reconnect_delay(conn.reconnect_delay_secs, conn.reconnect_attempts);
        let _ = write!(msg_text, " — reconnecting in {delay}s");
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
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
        },
    );
}

/// Calculate reconnect delay with exponential backoff.
///
/// For the first 10 attempts, uses exponential backoff capped at 300s.
/// After 10 attempts, switches to a fixed 600s (10min) interval.
fn calculate_reconnect_delay(base_delay: u64, attempts: u32) -> u64 {
    if attempts >= 10 {
        return 600;
    }
    let delay = base_delay.saturating_mul(2u64.saturating_pow(attempts));
    delay.min(300)
}

/// Extract a capabilities string from a `CAP` command's field3/field4.
///
/// The IRC protocol sends `CAP * <subcommand> :caps` which the irc crate parses
/// as `CAP(Some("*"), subcmd, Some("caps"), None)`.  In some cases the caps may
/// land in field4 instead (e.g. multiline continuation).  This helper checks both
/// fields, skipping the `*` continuation marker.
fn extract_cap_string(field3: Option<&str>, field4: Option<&str>) -> String {
    // If field3 is "*" (continuation marker), caps are in field4
    if field3 == Some("*") {
        return field4.unwrap_or("").to_string();
    }
    // Otherwise try field4 first (some servers put caps there), then field3
    if let Some(s) = field4
        && !s.is_empty()
    {
        return s.to_string();
    }
    field3.unwrap_or("").to_string()
}

/// Handle `CAP NEW` — new capabilities became available at runtime.
///
/// Parses the caps string, filters to those in [`DESIRED_CAPS`] that are not
/// already enabled, and returns the list of caps that should be requested via
/// `CAP REQ`.  The caller is responsible for sending the actual `CAP REQ`
/// command (since this function has no access to the IRC sender).
///
/// Also logs the event to the server status buffer.
pub fn handle_cap_new(
    state: &mut AppState,
    conn_id: &str,
    field3: Option<&str>,
    field4: Option<&str>,
) -> Vec<String> {
    use crate::irc::cap::DESIRED_CAPS;

    let caps_str = extract_cap_string(field3, field4);
    let new_caps: Vec<String> = caps_str
        .split_whitespace()
        .map(|s| s.split_once('=').map_or(s, |(name, _)| name))
        .map(str::to_ascii_lowercase)
        .collect();

    tracing::info!("CAP NEW from {conn_id}: {}", new_caps.join(" "));

    let enabled = state.connections.get(conn_id).map(|c| &c.enabled_caps);

    let to_request: Vec<String> = new_caps
        .iter()
        .filter(|cap| {
            DESIRED_CAPS.iter().any(|d| d.eq_ignore_ascii_case(cap))
                && enabled.is_none_or(|set| !set.contains(cap.as_str()))
        })
        .cloned()
        .collect();

    // Log to server status buffer
    let label = state
        .connections
        .get(conn_id)
        .map_or_else(|| conn_id.to_string(), |c| c.label.clone());
    let buffer_id = make_buffer_id(conn_id, &label);

    let text = if to_request.is_empty() {
        format!(
            "New capabilities available: {} (none requested)",
            new_caps.join(", ")
        )
    } else {
        format!(
            "New capabilities available: {} — requesting: {}",
            new_caps.join(", "),
            to_request.join(", ")
        )
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
            event_key: Some("cap_new".to_string()),
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
        },
    );

    to_request
}

/// Handle `CAP DEL` — capabilities removed by the server at runtime.
///
/// Parses the caps string and removes each from `conn.enabled_caps`.
/// Logs the event to the server status buffer.
pub fn handle_cap_del(
    state: &mut AppState,
    conn_id: &str,
    field3: Option<&str>,
    field4: Option<&str>,
) {
    let caps_str = extract_cap_string(field3, field4);
    let removed_caps: Vec<String> = caps_str
        .split_whitespace()
        .map(|s| s.split_once('=').map_or(s, |(name, _)| name))
        .map(str::to_ascii_lowercase)
        .collect();

    tracing::info!("CAP DEL from {conn_id}: {}", removed_caps.join(" "));

    let mut actually_removed = Vec::new();
    if let Some(conn) = state.connections.get_mut(conn_id) {
        for cap in &removed_caps {
            if conn.enabled_caps.remove(cap) {
                actually_removed.push(cap.clone());
            }
        }
    }

    // Log to server status buffer
    let label = state
        .connections
        .get(conn_id)
        .map_or_else(|| conn_id.to_string(), |c| c.label.clone());
    let buffer_id = make_buffer_id(conn_id, &label);

    let text = if actually_removed.is_empty() {
        format!(
            "Capabilities removed: {} (none were enabled)",
            removed_caps.join(", ")
        )
    } else {
        format!("Capabilities removed: {}", actually_removed.join(", "))
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
            event_key: Some("cap_del".to_string()),
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
        },
    );
}

/// Handle `CAP ACK` received at runtime (in response to a `CAP REQ` triggered
/// by `CAP NEW`).
///
/// Adds the acknowledged capabilities to `conn.enabled_caps` and logs the event.
pub fn handle_cap_ack(
    state: &mut AppState,
    conn_id: &str,
    field3: Option<&str>,
    field4: Option<&str>,
) {
    let caps_str = extract_cap_string(field3, field4);
    let acked_caps: Vec<String> = caps_str
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();

    tracing::info!("CAP ACK from {conn_id}: {}", acked_caps.join(" "));

    if let Some(conn) = state.connections.get_mut(conn_id) {
        for cap in &acked_caps {
            conn.enabled_caps.insert(cap.clone());
        }
    }

    // Log to server status buffer
    let label = state
        .connections
        .get(conn_id)
        .map_or_else(|| conn_id.to_string(), |c| c.label.clone());
    let buffer_id = make_buffer_id(conn_id, &label);

    let text = format!("Capabilities acknowledged: {}", acked_caps.join(", "));

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
            event_key: Some("cap_ack".to_string()),
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
        },
    );
}

/// Handle `CAP NAK` received at runtime (server refused our `CAP REQ`).
///
/// Logs the rejection to the server status buffer.
pub fn handle_cap_nak(
    state: &mut AppState,
    conn_id: &str,
    field3: Option<&str>,
    field4: Option<&str>,
) {
    let caps_str = extract_cap_string(field3, field4);
    let naked_caps: Vec<String> = caps_str
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();

    tracing::warn!("CAP NAK from {conn_id}: {}", naked_caps.join(" "));

    let label = state
        .connections
        .get(conn_id)
        .map_or_else(|| conn_id.to_string(), |c| c.label.clone());
    let buffer_id = make_buffer_id(conn_id, &label);

    let text = format!("Capabilities rejected: {}", naked_caps.join(", "));

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
            event_key: Some("cap_nak".to_string()),
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
        },
    );
}

/// Look up a nick's highest mode prefix (e.g. `'@'`, `'+'`) from the buffer's user list.
///
/// Thin wrapper around [`AppState::nick_prefix`] for internal callers
/// that use `.map(String::from)` when constructing `Message` structs.
fn nick_prefix(state: &AppState, buffer_id: &str, nick: &str) -> Option<char> {
    state.nick_prefix(buffer_id, nick)
}

/// Extract `IRCv3` message tags from an `irc::proto::Message`.
///
/// Tags with no value are omitted — only `key=value` pairs are returned.
fn extract_tags(msg: &IrcMessage) -> Option<HashMap<String, String>> {
    let tags = msg.tags.as_ref()?;
    let map: HashMap<String, String> = tags
        .iter()
        .filter_map(|tag| Some((tag.0.clone(), tag.1.as_ref()?.clone())))
        .collect();
    if map.is_empty() { None } else { Some(map) }
}

/// Extract the timestamp from `IRCv3` `server-time` tag (`@time=...`).
///
/// If a valid RFC 3339 timestamp is present, use it; otherwise fall back to
/// `Utc::now()`.  This is critical for bouncer/relay playback where messages
/// arrive with historical timestamps.
fn message_timestamp(tags: Option<&HashMap<String, String>>) -> DateTime<Utc> {
    tags.and_then(|t| t.get("time"))
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map_or_else(Utc::now, |dt| dt.with_timezone(&Utc))
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
    tags: Option<HashMap<String, String>>,
) {
    let (nick, ident, host) = extract_nick_userhost(prefix);
    let target_is_channel = is_channel(target);
    let is_own = nick == our_nick;

    // For channels and echo-message echoes (is_own), the buffer is the
    // target.  For incoming PMs the buffer is the sender's nick.  This
    // ensures that when the server echoes our PM to "bob", it routes to the
    // "bob" query buffer instead of creating one named after ourselves.
    let buffer_name = if target_is_channel || is_own {
        target
    } else {
        &nick
    };
    let buffer_id = make_buffer_id(conn_id, buffer_name);

    // Check if this is a CTCP (ACTION or other)
    let is_ctcp = text.starts_with('\x01') && text.ends_with('\x01');
    let is_action = is_ctcp && text.len() > 2 && text[1..text.len() - 1].starts_with("ACTION ");

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
        let channel = if target_is_channel {
            Some(target)
        } else {
            None
        };
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
            messages: VecDeque::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: Utc::now(),
            topic: None,
            topic_set_by: None,
            users: std::collections::HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: std::collections::HashMap::new(),
            last_speakers: Vec::new(),
        });
    }

    // account-tag: update NickEntry.account from message tags (supplementary)
    if let Some(tag_account) = tags.as_ref().and_then(|t| t.get("account")) {
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
            // Save nick before moving into Message — needed for mentions buffer below.
            let nick_saved = if is_mention { Some(nick.clone()) } else { None };
            state.add_message_with_activity(
                &buffer_id,
                Message {
                    id,
                    timestamp: message_timestamp(tags.as_ref()),
                    message_type: MessageType::Action,
                    nick: Some(nick),
                    nick_mode: mode_prefix.map(String::from),
                    text: action_text.to_string(),
                    highlight: is_mention,
                    event_key: None,
                    event_params: None,
                    log_msg_id: None,
                    log_ref_id: None,
                    tags,
                },
                activity,
            );

            // Push to mentions buffer if this is a highlight.
            if let Some(nick) = nick_saved
                && state.buffers.contains_key("_mentions")
            {
                let conn_label = state
                    .connections
                    .get(conn_id)
                    .map_or(conn_id, |c| c.label.as_str())
                    .to_string();
                let mention_text = if target_is_channel {
                    format!("{target} {nick}❯ * {nick} {action_text}")
                } else {
                    format!("{nick}❯ * {nick} {action_text}")
                };
                state.message_counter += 1;
                let mention_msg = Message {
                    id: state.message_counter,
                    timestamp: chrono::Utc::now(),
                    message_type: MessageType::Message,
                    nick: Some(conn_label),
                    nick_mode: None,
                    text: mention_text,
                    highlight: true,
                    event_key: None,
                    event_params: None,
                    log_msg_id: None,
                    log_ref_id: None,
                    tags: None,
                };
                state.add_mention_to_buffer(mention_msg);
            }

            return;
        }

        // Other CTCP — flood check
        if state.flood_protection {
            let now = Instant::now();
            let result = state.flood_state.check_ctcp_flood(now);
            if result.suppressed() {
                if result == crate::irc::flood::FloodResult::Triggered {
                    emit(state, &buffer_id, "CTCP flood detected — suppressing");
                }
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
        if ident.starts_with('~') {
            let result = state.flood_state.check_tilde_flood(now);
            if result.suppressed() {
                if result == crate::irc::flood::FloodResult::Triggered {
                    emit(
                        state,
                        &buffer_id,
                        "Tilde-ident flood detected — suppressing",
                    );
                }
                return;
            }
        }

        // Duplicate text flood check (channel messages only)
        let dup_result = state
            .flood_state
            .check_duplicate_flood(text, target_is_channel, now);
        if dup_result.suppressed() {
            if dup_result == crate::irc::flood::FloodResult::Triggered {
                emit(
                    state,
                    &buffer_id,
                    "Duplicate text flood detected — suppressing",
                );
            }
            return;
        }
    }

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
    // Save nick before moving into Message — needed for mentions buffer below.
    let nick_saved = if is_mention { Some(nick.clone()) } else { None };
    state.add_message_with_activity(
        &buffer_id,
        Message {
            id,
            timestamp: message_timestamp(tags.as_ref()),
            message_type: MessageType::Message,
            nick: Some(nick),
            nick_mode: mode_prefix.map(String::from),
            text: text.to_string(),
            highlight: is_mention,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags,
        },
        activity,
    );

    // Push to mentions buffer if this is a highlight.
    if let Some(nick) = nick_saved
        && state.buffers.contains_key("_mentions")
    {
        let conn_label = state
            .connections
            .get(conn_id)
            .map_or(conn_id, |c| c.label.as_str())
            .to_string();
        let mention_text = if target_is_channel {
            format!("{target} {nick}❯ {text}")
        } else {
            format!("{nick}❯ {text}")
        };
        state.message_counter += 1;
        let mention_msg = Message {
            id: state.message_counter,
            timestamp: chrono::Utc::now(),
            message_type: MessageType::Message,
            nick: Some(conn_label),
            nick_mode: None,
            text: mention_text,
            highlight: true,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
        };
        state.add_mention_to_buffer(mention_msg);
    }
}

fn handle_notice(
    state: &mut AppState,
    conn_id: &str,
    prefix: Option<&Prefix>,
    target: &str,
    text: &str,
    tags: Option<HashMap<String, String>>,
) {
    let nick = extract_nick(prefix);
    // Server notices or pre-registration notices go to status buffer
    let is_server_notice = nick.is_none() || is_server_prefix(prefix);

    // --- Ignore check (skip for server notices) ---
    if !is_server_notice {
        let (n, ident, host) = extract_nick_userhost(prefix);
        let channel = if is_channel(target) {
            Some(target)
        } else {
            None
        };
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

    // echo-message: when the server echoes our own notice to a user, the
    // target is the recipient (e.g. "bob"). Route to that buffer, not ours.
    let our_nick = state
        .connections
        .get(conn_id)
        .map(|c| c.nick.as_str())
        .unwrap_or_default();
    let is_own = nick.as_deref() == Some(our_nick);

    // For channel notices and echo-message echoes (is_own), the buffer is
    // the target.  For incoming user notices the buffer is the sender's nick.
    let buffer_name = if is_server_notice {
        state
            .connections
            .get(conn_id)
            .map_or("Status", |c| c.label.as_str())
    } else if is_channel(target) || is_own {
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

    let mode_prefix = nick
        .as_deref()
        .and_then(|n| nick_prefix(state, &buffer_id, n));
    let id = state.next_message_id();
    state.add_message(
        &buffer_id,
        Message {
            id,
            timestamp: message_timestamp(tags.as_ref()),
            message_type: MessageType::Notice,
            nick,
            nick_mode: mode_prefix.map(String::from),
            text: text.to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags,
        },
    );
}

#[expect(clippy::too_many_arguments, clippy::too_many_lines)]
fn handle_join(
    state: &mut AppState,
    conn_id: &str,
    our_nick: &str,
    prefix: Option<&Prefix>,
    channel: &str,
    extended_account: Option<&str>,
    extended_realname: Option<&str>,
    tags: Option<HashMap<String, String>>,
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
        tags.as_ref()
            .and_then(|t| t.get("account"))
            .and_then(|a| if a == "*" { None } else { Some(a.clone()) })
    });

    // extended-join: realname from third JOIN arg
    let realname = extended_realname.unwrap_or("");

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
                messages: VecDeque::new(),
                activity: ActivityLevel::None,
                unread_count: 0,
                last_read: Utc::now(),
                topic: None,
                topic_set_by: None,
                users: std::collections::HashMap::new(),
                modes: None,
                mode_params: None,
                list_modes: std::collections::HashMap::new(),
                last_speakers: Vec::new(),
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
                account: account.clone(),
                ident: None,
                host: None,
            },
        );
        state.pending_web_events.push(crate::web::protocol::WebEvent::NickEvent {
            buffer_id: buffer_id.clone(),
            kind: crate::web::protocol::NickEventKind::Join,
            nick: nick.clone(),
            new_nick: None,
            prefix: Some(String::new()),
            modes: Some(String::new()),
            away: Some(false),
            message: None,
        });

        // --- Netsplit: check if this is a netjoin ---
        if state.netsplit_state.handle_join(&nick, &buffer_id) {
            // Suppress normal join message — netsplit module will batch it
            return;
        }
    }

    // extended-join: show account and realname in join message when available
    let account_display = account
        .as_deref()
        .map_or(String::new(), |a| format!("[{a}]"));
    let realname_display = if realname.is_empty() {
        String::new()
    } else {
        realname.to_string()
    };

    let id = state.next_message_id();
    state.add_message(
        &buffer_id,
        Message {
            id,
            timestamp: message_timestamp(tags.as_ref()),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text: format!(
                "{nick} ({ident}@{host}) has joined {channel} {account_display} {realname_display}"
            ),
            highlight: false,
            event_key: Some("join".to_string()),
            // $0=nick, $1=ident, $2=host, $3=channel, $4=account, $5=realname
            event_params: Some(vec![
                nick,
                ident,
                host,
                channel.to_string(),
                account_display,
                realname_display,
            ]),
            log_msg_id: None,
            log_ref_id: None,
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
#[expect(
    clippy::needless_pass_by_value,
    reason = "tags follows the convention of all other event handlers"
)]
fn handle_account(
    state: &mut AppState,
    conn_id: &str,
    prefix: Option<&Prefix>,
    account: &str,
    tags: Option<HashMap<String, String>>,
) {
    let Some(nick) = extract_nick(prefix) else {
        return;
    };

    let resolved: Option<&str> = if account == "*" { None } else { Some(account) };

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

    let (text, description) = resolved.map_or_else(
        || {
            (
                format!("{nick} has logged out"),
                "has logged out".to_string(),
            )
        },
        |acct| {
            (
                format!("{nick} is now logged in as {acct}"),
                format!("is now logged in as {acct}"),
            )
        },
    );

    for buf_id in shared_buffers {
        let id = state.next_message_id();
        state.add_message(
            &buf_id,
            Message {
                id,
                timestamp: message_timestamp(tags.as_ref()),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: text.clone(),
                highlight: false,
                event_key: Some("account".to_string()),
                event_params: Some(vec![nick.clone(), description.clone()]),
                log_msg_id: None,
                log_ref_id: None,
                tags: tags.clone(),
            },
        );
    }
}

/// Handle `IRCv3` `away-notify`: `:nick!user@host AWAY :reason` or `:nick!user@host AWAY`
///
/// When a user changes their away status, the server sends AWAY to every
/// channel we share with them.
///   - `reason == Some(text)` → user is away
///   - `reason == None` → user is back
///
/// We silently update `NickEntry.away` without adding event messages (too noisy).
fn handle_away(state: &mut AppState, conn_id: &str, prefix: Option<&Prefix>, reason: Option<&str>) {
    let Some(nick) = extract_nick(prefix) else {
        return;
    };

    let is_away = reason.is_some();
    let nick_lower = nick.to_lowercase();

    let affected_bufs: Vec<String> = state
        .buffers
        .iter()
        .filter(|(_, buf)| {
            buf.connection_id == conn_id && buf.users.contains_key(&nick_lower)
        })
        .map(|(id, _)| id.clone())
        .collect();

    for buf in state.buffers.values_mut() {
        if buf.connection_id != conn_id {
            continue;
        }
        if let Some(entry) = buf.users.get_mut(&nick_lower) {
            entry.away = is_away;
        }
    }

    for buf_id in affected_bufs {
        state.pending_web_events.push(crate::web::protocol::WebEvent::NickEvent {
            buffer_id: buf_id,
            kind: crate::web::protocol::NickEventKind::AwayChange,
            nick: nick.clone(),
            new_nick: None,
            prefix: None,
            modes: None,
            away: Some(is_away),
            message: reason.map(ToString::to_string),
        });
    }
}

/// Handle `IRCv3` `chghost`: `:nick!olduser@oldhost CHGHOST newuser newhost`
///
/// When a user's ident or hostname changes, the server sends CHGHOST to every
/// channel we share with them. We update the `NickEntry` and add a subtle event
/// message.
#[expect(
    clippy::needless_pass_by_value,
    reason = "tags follows the convention of all other event handlers"
)]
fn handle_chghost(
    state: &mut AppState,
    conn_id: &str,
    prefix: Option<&Prefix>,
    new_user: &str,
    new_host: &str,
    tags: Option<HashMap<String, String>>,
) {
    let Some(nick) = extract_nick(prefix) else {
        return;
    };

    let nick_lower = nick.to_lowercase();

    // Update ident/host in all shared buffers
    for buf in state.buffers.values_mut() {
        if buf.connection_id != conn_id {
            continue;
        }
        if let Some(entry) = buf.users.get_mut(&nick_lower) {
            entry.ident = Some(new_user.to_string());
            entry.host = Some(new_host.to_string());
        }
    }

    // Log a subtle event in every shared channel
    let shared_buffers: Vec<String> = state
        .buffers
        .values()
        .filter(|b| {
            b.connection_id == conn_id
                && b.buffer_type == BufferType::Channel
                && b.users.contains_key(&nick_lower)
        })
        .map(|b| b.id.clone())
        .collect();

    let text = format!("{nick} changed host to {new_user}@{new_host}");

    for buf_id in shared_buffers {
        let id = state.next_message_id();
        state.add_message(
            &buf_id,
            Message {
                id,
                timestamp: message_timestamp(tags.as_ref()),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: text.clone(),
                highlight: false,
                event_key: Some("chghost".to_string()),
                event_params: Some(vec![
                    nick.clone(),
                    new_user.to_string(),
                    new_host.to_string(),
                ]),
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
    tags: Option<HashMap<String, String>>,
) {
    let (nick, ident, host) = extract_nick_userhost(prefix);
    let buffer_id = make_buffer_id(conn_id, channel);

    if nick == our_nick {
        state.remove_buffer(&buffer_id);
        // Clean up any pending silent WHO for this channel.
        if let Some(conn) = state.connections.get_mut(conn_id) {
            conn.silent_who_channels.remove(channel);
        }
    } else {
        // Always update nick list regardless of ignore
        state.remove_nick(&buffer_id, &nick);
        state.pending_web_events.push(crate::web::protocol::WebEvent::NickEvent {
            buffer_id: buffer_id.clone(),
            kind: crate::web::protocol::NickEventKind::Part,
            nick: nick.clone(),
            new_nick: None,
            prefix: None,
            modes: None,
            away: None,
            message: reason.map(ToString::to_string),
        });

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
                timestamp: message_timestamp(tags.as_ref()),
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
                log_msg_id: None,
                log_ref_id: None,
                tags,
            },
        );
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "tags are dropped when ignored/netsplit, cloned into fan-out Messages otherwise"
)]
fn handle_quit(
    state: &mut AppState,
    conn_id: &str,
    _our_nick: &str,
    prefix: Option<&Prefix>,
    reason: Option<&str>,
    tags: Option<HashMap<String, String>>,
) {
    let (nick, ident, host) = extract_nick_userhost(prefix);
    let reason_str = reason.unwrap_or("");

    // Remove from all buffers on this connection
    let affected: Vec<String> = state
        .buffers
        .iter()
        .filter(|(_, buf)| {
            buf.connection_id == conn_id && buf.users.contains_key(&nick.to_lowercase())
        })
        .map(|(id, _)| id.clone())
        .collect();

    // Always remove from nick lists regardless of ignore/netsplit
    for buf_id in &affected {
        state.remove_nick(buf_id, &nick);
        state.pending_web_events.push(crate::web::protocol::WebEvent::NickEvent {
            buffer_id: buf_id.clone(),
            kind: crate::web::protocol::NickEventKind::Quit,
            nick: nick.clone(),
            new_nick: None,
            prefix: None,
            modes: None,
            away: None,
            message: reason.map(ToString::to_string),
        });
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

    let ts = message_timestamp(tags.as_ref());
    for (i, buf_id) in affected.iter().enumerate() {
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
                event_key: Some("quit".to_string()),
                event_params: Some(vec![
                    nick.clone(),
                    ident.clone(),
                    host.clone(),
                    reason_str.to_string(),
                ]),
                log_msg_id: if i == 0 {
                    Some(primary_msg_id.clone())
                } else {
                    None
                },
                log_ref_id: if i == 0 {
                    None
                } else {
                    Some(primary_msg_id.clone())
                },
                tags: tags.clone(),
            },
        );
    }
}

/// Rename query buffers in `affected` to `new_nick`.
/// Re-keys the buffer in the `IndexMap` and updates `active_buffer_id`.
fn rename_query_buffers(state: &mut AppState, conn_id: &str, new_nick: &str, affected: &[String]) {
    for buf_id in affected {
        let is_query = state
            .buffers
            .get(buf_id)
            .is_some_and(|b| b.buffer_type == BufferType::Query);
        if !is_query {
            continue;
        }
        let new_buf_id = make_buffer_id(conn_id, new_nick);
        if let Some(mut buf) = state.buffers.shift_remove(buf_id) {
            buf.name = new_nick.to_string();
            buf.id.clone_from(&new_buf_id);
            state.buffers.insert(new_buf_id.clone(), buf);
            if state.active_buffer_id.as_deref() == Some(buf_id.as_str()) {
                state.active_buffer_id = Some(new_buf_id);
            }
        }
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "tags are cloned into each fan-out Message"
)]
#[expect(clippy::too_many_lines, reason = "nick change fan-out + web NickEvent broadcasting")]
fn handle_nick_change(
    state: &mut AppState,
    conn_id: &str,
    our_nick: &str,
    prefix: Option<&Prefix>,
    new_nick: &str,
    tags: Option<HashMap<String, String>>,
) {
    let old_nick = extract_nick(prefix).unwrap_or_default();

    // Update our own nick if it's us
    if old_nick == our_nick
        && let Some(conn) = state.connections.get_mut(conn_id)
    {
        conn.nick = new_nick.to_string();
        // Broadcast to web so status bar updates.
        state.pending_web_events.push(crate::web::protocol::WebEvent::ConnectionStatus {
            conn_id: conn_id.to_string(),
            label: conn.label.clone(),
            connected: conn.status == crate::state::connection::ConnectionStatus::Connected,
            nick: new_nick.to_string(),
        });
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
            // Still update nick list and rename query buffers so state is
            // correct, but suppress the notification message.
            let old_nick_lower = old_nick.to_lowercase();
            let affected: Vec<String> = state
                .buffers
                .iter()
                .filter(|(_, buf)| {
                    buf.connection_id == conn_id
                        && (buf.users.contains_key(&old_nick_lower)
                            || (buf.buffer_type == BufferType::Query
                                && buf.name.to_lowercase() == old_nick_lower))
                })
                .map(|(id, _)| id.clone())
                .collect();
            for buf_id in &affected {
                state.update_nick(buf_id, &old_nick, new_nick);
            }
            rename_query_buffers(state, conn_id, new_nick, &affected);
            return;
        }
    }

    // Update in all buffers on this connection — channels (have user in nick list)
    // AND query buffers (named after the nick, no users list).
    let old_nick_lower = old_nick.to_lowercase();
    let affected: Vec<String> = state
        .buffers
        .iter()
        .filter(|(_, buf)| {
            buf.connection_id == conn_id
                && (buf.users.contains_key(&old_nick_lower)
                    || (buf.buffer_type == BufferType::Query
                        && buf.name.to_lowercase() == old_nick_lower))
        })
        .map(|(id, _)| id.clone())
        .collect();

    // First non-suppressed channel gets the full log row; others get reference rows.
    let primary_msg_id = uuid::Uuid::new_v4().to_string();
    let text = format!("{old_nick} is now known as {new_nick}");
    let mut primary_assigned = false;
    let ts = message_timestamp(tags.as_ref());
    let now = Instant::now();

    for buf_id in &affected {
        state.update_nick(buf_id, &old_nick, new_nick);
        state.pending_web_events.push(crate::web::protocol::WebEvent::NickEvent {
            buffer_id: buf_id.clone(),
            kind: crate::web::protocol::NickEventKind::NickChange,
            nick: old_nick.clone(),
            new_nick: Some(new_nick.to_string()),
            prefix: None,
            modes: None,
            away: None,
            message: None,
        });

        // --- Nick flood check ---
        if state.flood_protection
            && old_nick != our_nick
            && state.flood_state.should_suppress_nick_flood(buf_id, now)
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
                timestamp: ts,
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: text.clone(),
                highlight: false,
                event_key: Some("nick_change".to_string()),
                event_params: Some(vec![old_nick.clone(), new_nick.to_string()]),
                log_msg_id: if is_primary {
                    Some(primary_msg_id.clone())
                } else {
                    None
                },
                log_ref_id: if is_primary {
                    None
                } else {
                    Some(primary_msg_id.clone())
                },
                tags: tags.clone(),
            },
        );
    }

    rename_query_buffers(state, conn_id, new_nick, &affected);
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
    tags: Option<HashMap<String, String>>,
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

    let ts = message_timestamp(tags.as_ref());
    if kicked_user == our_nick {
        let id = state.next_message_id();
        state.add_message(
            &buffer_id,
            Message {
                id,
                timestamp: ts,
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: format!("You were kicked from {channel} by {kicker} ({reason_str})"),
                highlight: false,
                event_key: None,
                event_params: None,
                log_msg_id: None,
                log_ref_id: None,
                tags,
            },
        );
        state.remove_buffer(&buffer_id);
        // Clean up any pending silent WHO for this channel.
        if let Some(conn) = state.connections.get_mut(conn_id) {
            conn.silent_who_channels.remove(channel);
        }
    } else {
        state.remove_nick(&buffer_id, kicked_user);
        let id = state.next_message_id();
        state.add_message(
            &buffer_id,
            Message {
                id,
                timestamp: ts,
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
                log_msg_id: None,
                log_ref_id: None,
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
    tags: Option<HashMap<String, String>>,
) {
    let nick = extract_nick(prefix);
    let buffer_id = make_buffer_id(conn_id, channel);

    if let Some(topic_text) = topic {
        state.set_topic(&buffer_id, topic_text.to_string(), nick.clone());
        state.pending_web_events.push(crate::web::protocol::WebEvent::TopicChanged {
            buffer_id: buffer_id.clone(),
            topic: Some(topic_text.to_string()),
            set_by: nick.clone(),
        });
        let setter = nick.unwrap_or_default();
        let id = state.next_message_id();
        state.add_message(
            &buffer_id,
            Message {
                id,
                timestamp: message_timestamp(tags.as_ref()),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: format!("{setter} changed the topic to: {topic_text}"),
                highlight: false,
                event_key: Some("topic_changed".to_string()),
                event_params: Some(vec![setter, topic_text.to_string()]),
                log_msg_id: None,
                log_ref_id: None,
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
    tags: Option<HashMap<String, String>>,
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
                        irc::proto::Mode::Plus(m, _) | irc::proto::Mode::NoPrefix(m) => (true, m),
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

    let ts = message_timestamp(tags.as_ref());
    if is_channel(target) {
        let buffer_id = make_buffer_id(conn_id, target);
        let id = state.next_message_id();
        state.add_message(
            &buffer_id,
            Message {
                id,
                timestamp: ts,
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: format!("{nick} sets mode {mode_display} on {target}"),
                highlight: false,
                event_key: Some("mode".to_string()),
                event_params: Some(vec![nick, mode_display, target.to_string()]),
                log_msg_id: None,
                log_ref_id: None,
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
                timestamp: ts,
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: format!("{nick} sets mode {mode_display} on {target}"),
                highlight: false,
                event_key: Some("mode".to_string()),
                event_params: Some(vec![nick, mode_display, target.to_string()]),
                log_msg_id: None,
                log_ref_id: None,
                tags,
            },
        );
    }
}

/// Apply a single channel mode change to nick entries and channel mode tracking.
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

    // Nick prefix modes — update user entries
    let nick_mode_char = match mode_enum {
        ChannelMode::Founder => Some('q'),
        ChannelMode::Admin => Some('a'),
        ChannelMode::Oper => Some('o'),
        ChannelMode::Halfop => Some('h'),
        ChannelMode::Voice => Some('v'),
        _ => None,
    };

    if let Some(mc) = nick_mode_char
        && let Some(target_nick) = param
        && let Some(buf) = state.buffers.get_mut(buffer_id)
        && let Some(entry) = buf.users.get_mut(&target_nick.to_lowercase())
    {
        if adding && !entry.modes.contains(mc) {
            entry.modes.push(mc);
        } else if !adding {
            entry.modes = entry.modes.replace(mc, "");
        }
        entry.prefix = modes_to_prefix(&entry.modes, "~&@%+");
        let new_prefix = entry.prefix.clone();
        let new_modes = entry.modes.clone();
        state.pending_web_events.push(crate::web::protocol::WebEvent::NickEvent {
            buffer_id: buffer_id.to_string(),
            kind: crate::web::protocol::NickEventKind::ModeChange,
            nick: target_nick.to_string(),
            new_nick: None,
            prefix: Some(new_prefix),
            modes: Some(new_modes),
            away: None,
            message: None,
        });
        return;
    }

    // Channel modes (not nick prefix, not list modes) — update buf.modes
    // Skip list modes (b, e, I) and nick prefix modes (already handled above)
    let ch = channel_mode_letter(mode_enum);
    let is_list_mode = matches!(
        mode_enum,
        ChannelMode::Ban | ChannelMode::Exception | ChannelMode::InviteException
    );
    if is_list_mode || nick_mode_char.is_some() {
        return;
    }

    if let Some(buf) = state.buffers.get_mut(buffer_id) {
        let modes = buf.modes.get_or_insert_with(String::new);
        if adding {
            if !modes.contains(ch) {
                modes.push(ch);
            }
            // Store params for modes that carry values (k=key, l=limit)
            if matches!(ch, 'k' | 'l')
                && let Some(val) = param
            {
                buf.mode_params
                    .get_or_insert_with(HashMap::new)
                    .insert(ch.to_string(), val.to_string());
            }
        } else {
            *modes = modes.replace(ch, "");
            if let Some(ref mut mp) = buf.mode_params {
                mp.remove(&ch.to_string());
            }
        }
        // Strip leading '+' if present from RPL_CHANNELMODEIS
        if modes.starts_with('+') {
            *modes = modes[1..].to_string();
        }
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
    our_nick: &str,
    prefix: Option<&Prefix>,
    nick: &str,
    channel: &str,
    tags: Option<HashMap<String, String>>,
) {
    let inviter = extract_nick(prefix).unwrap_or_default();

    if nick.eq_ignore_ascii_case(our_nick) {
        // We are the invited user — show in active buffer or server buffer (highlight)
        let label = state
            .connections
            .get(conn_id)
            .map_or("Status", |c| c.label.as_str());
        let buffer_id = state
            .active_buffer_id
            .clone()
            .unwrap_or_else(|| make_buffer_id(conn_id, label));

        let id = state.next_message_id();
        state.add_message(
            &buffer_id,
            Message {
                id,
                timestamp: message_timestamp(tags.as_ref()),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: format!("{inviter} invites you to {channel}"),
                highlight: true,
                event_key: None,
                event_params: None,
                log_msg_id: None,
                log_ref_id: None,
                tags,
            },
        );
    } else {
        // invite-notify: someone else was invited — show in the channel buffer
        let buffer_id = make_buffer_id(conn_id, channel);
        if state.buffers.contains_key(&buffer_id) {
            let id = state.next_message_id();
            state.add_message(
                &buffer_id,
                Message {
                    id,
                    timestamp: message_timestamp(tags.as_ref()),
                    message_type: MessageType::Event,
                    nick: None,
                    nick_mode: None,
                    text: format!("{inviter} invited {nick} to {channel}"),
                    highlight: false,
                    event_key: None,
                    event_params: None,
                    log_msg_id: None,
                    log_ref_id: None,
                    tags,
                },
            );
        }
    }
}

fn handle_error(state: &mut AppState, conn_id: &str, message: &str) {
    tracing::warn!("ERROR from {conn_id}: {message}");

    // Mark the connection as errored
    if let Some(conn) = state.connections.get_mut(conn_id) {
        conn.status = ConnectionStatus::Error;
        conn.error = Some(message.to_string());
    }

    let buf = server_buffer(state, conn_id);
    emit(state, &buf, &format!("%Zff4444ERROR: {message}%N"));
}

fn handle_wallops(state: &mut AppState, conn_id: &str, prefix: Option<&Prefix>, text: &str) {
    let from = extract_nick(prefix).unwrap_or_else(|| "server".to_string());
    let label = state
        .connections
        .get(conn_id)
        .map_or("Status", |c| c.label.as_str());
    let buffer_id = make_buffer_id(conn_id, label);
    emit(
        state,
        &buffer_id,
        &format!("%Ze0af68[Wallops/{from}]%N {text}"),
    );
}

#[expect(clippy::too_many_lines, reason = "dispatcher pattern")]
fn handle_response(state: &mut AppState, conn_id: &str, response: Response, args: &[String]) {
    match response {
        // RPL_MYINFO: informational only, no state changes needed.

        // RPL_ISUPPORT: args = [our_nick, TOKEN=VALUE, TOKEN=VALUE, ..., "are supported by this server"]
        Response::RPL_ISUPPORT => {
            if args.len() >= 2 {
                // Parse KEY=VALUE tokens (skip first arg = our nick, skip last = trailing text)
                let tokens = &args[1..args.len().saturating_sub(1)];
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
        // RPL_CHANNELMODEIS: args = [our_nick, channel, modes, param1, param2, ...]
        // e.g. [nick, #chan, +ntlk, 50, secret]
        Response::RPL_CHANNELMODEIS => {
            if args.len() >= 3 {
                let channel = &args[1];
                let mode_str = args[2].strip_prefix('+').unwrap_or(&args[2]);
                let buffer_id = make_buffer_id(conn_id, channel);
                if let Some(buf) = state.buffers.get_mut(&buffer_id) {
                    buf.modes = Some(mode_str.to_string());
                    // Parse mode params: modes with params (k, l, etc.) consume
                    // positional args starting from args[3].
                    let mut param_idx = 3;
                    let mut params = HashMap::new();
                    for ch in mode_str.chars() {
                        // Type B (always has param): k
                        // Type C (param when set): l
                        if matches!(ch, 'k' | 'l')
                            && let Some(val) = args.get(param_idx)
                        {
                            params.insert(ch.to_string(), val.clone());
                            param_idx += 1;
                        }
                    }
                    if params.is_empty() {
                        buf.mode_params = None;
                    } else {
                        buf.mode_params = Some(params);
                    }
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
                    let _ = write!(line, "%Z565f89, signon: %Za9b1d6{dt}");
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
                let channel = &args[1];
                let mask = &args[2];
                let set_by = args.get(3).cloned().unwrap_or_default();
                let set_at = args.get(4).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);

                // Store in buffer's list_modes for /unban numeric refs.
                let buf_id = crate::state::buffer::make_buffer_id(conn_id, channel);
                if let Some(buf) = state.buffers.get_mut(&buf_id) {
                    let entries = buf.list_modes.entry("b".to_string()).or_default();
                    entries.push(crate::state::buffer::ListEntry {
                        mask: mask.clone(),
                        set_by: set_by.clone(),
                        set_at,
                    });
                }

                // Display numbered entry
                let index = state.buffers.get(&buf_id)
                    .and_then(|b| b.list_modes.get("b"))
                    .map_or(0, Vec::len);
                let target_buf = active_or_server_buffer(state, conn_id);
                let set_info = if set_by.is_empty() {
                    String::new()
                } else {
                    format!(" (set by {} {})", set_by, format_timestamp(args.get(4).map_or("0", |s| s.as_str())))
                };
                let extban_prefix = state.connections.get(conn_id)
                    .and_then(|c| c.isupport_parsed.extban())
                    .map(|(prefix, _)| prefix);
                let mask_display = crate::irc::extban::format_ban_mask(mask, extban_prefix);
                emit(state, &target_buf, &format!(
                    "%Z565f89  {index}. %Za9b1d6{mask_display}{set_info}%N"
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
                let extban_prefix = state.connections.get(conn_id)
                    .and_then(|c| c.isupport_parsed.extban())
                    .map(|(prefix, _)| prefix);
                let mask_display = crate::irc::extban::format_ban_mask(&args[2], extban_prefix);
                emit(state, &target_buf, &format!(
                    "%Z565f89  except: %Za9b1d6{mask_display}{set_info}%N"
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
                let extban_prefix = state.connections.get(conn_id)
                    .and_then(|c| c.isupport_parsed.extban())
                    .map(|(prefix, _)| prefix);
                let mask_display = crate::irc::extban::format_ban_mask(&args[2], extban_prefix);
                emit(state, &target_buf, &format!(
                    "%Z565f89  invex: %Za9b1d6{mask_display}{set_info}%N"
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
            // Display only — the irc crate handles retry via alt_nicks internally.
            let attempted = if args.len() >= 2 { &args[1] } else { "unknown" };
            let target_buf = server_buffer(state, conn_id);
            emit(
                state,
                &target_buf,
                &format!("%Ze0af68Nick {attempted} is already in use%N"),
            );
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

        // === Channel join failures ===
        // Destroy eagerly-created buffers when the server rejects a JOIN.
        // args: [our_nick, channel, reason]

        Response::ERR_CHANNELISFULL       // 471
        | Response::ERR_INVITEONLYCHAN    // 473
        | Response::ERR_BANNEDFROMCHAN    // 474
        | Response::ERR_BADCHANNELKEY     // 475
        | Response::ERR_TOOMANYCHANNELS   // 405
        => {
            let channel = if args.len() >= 2 { &args[1] } else { "?" };
            let reason = if args.len() >= 3 { &args[2] } else { "Cannot join channel" };
            let buffer_id = make_buffer_id(conn_id, channel);

            // Show the error in the server buffer.
            let target_buf = server_buffer(state, conn_id);
            emit(
                state,
                &target_buf,
                &format!("%Zff6b6bCannot join {channel}: {reason}%N"),
            );

            // Destroy the pre-created buffer if no one has joined it yet
            // (no users means we never received our own JOIN confirmation).
            let should_remove = state
                .buffers
                .get(&buffer_id)
                .is_some_and(|buf| buf.users.is_empty());
            if should_remove {
                state.remove_buffer(&buffer_id);
            }
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
                let silent = state
                    .connections
                    .get(conn_id)
                    .is_some_and(|c| c.silent_who_channels.contains(channel.as_str()));
                if !silent {
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
        }
        Response::RPL_ENDOFWHO => {
            // args: [our_nick, target, "End of WHO list"]
            // Target may be a single channel or comma-separated (batched WHO).
            let target = args.get(1).map_or("", String::as_str);
            let was_silent = if let Some(conn) = state.connections.get_mut(conn_id) {
                if target.contains(',') {
                    // Batched WHO — remove each channel individually.
                    let mut any_silent = false;
                    for ch in target.split(',') {
                        any_silent |= conn.silent_who_channels.remove(ch);
                    }
                    any_silent
                } else {
                    conn.silent_who_channels.remove(target)
                }
            } else {
                false
            };
            if !was_silent {
                let target_buf = active_or_server_buffer(state, conn_id);
                emit(state, &target_buf, "%Z565f89End of WHO list%N");
            }
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
            // Error numerics (4xx) go to the active window — they are responses
            // to user commands (e.g. "No such nick/channel"). Informational
            // numerics still go to the server buffer.
            let buffer_id = if response.is_error() {
                active_or_server_buffer(state, conn_id)
            } else {
                server_buffer(state, conn_id)
            };
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
                    tags: None,
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
pub fn emit(state: &mut AppState, buffer_id: &str, text: &str) {
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
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
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
///
/// Uses `as_deref()` to inspect the active buffer ID without cloning,
/// then clones only when needed (the `Some` branch) or constructs a
/// new ID (the `None` branch).
fn active_or_server_buffer(state: &AppState, conn_id: &str) -> String {
    state.active_buffer_id.as_deref().map_or_else(
        || {
            let label = state
                .connections
                .get(conn_id)
                .map_or("Status", |c| c.label.as_str());
            make_buffer_id(conn_id, label)
        },
        str::to_owned,
    )
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
fn parse_names_entry(raw: &str, prefix_map: &[(char, char)], has_userhost: bool) -> NickEntry {
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
            return (
                nick.to_string(),
                Some(ident.to_string()),
                Some(host.to_string()),
            );
        }
    }
    (input.to_string(), None, None)
}

// === WHOX helpers ===

/// Generate the next WHOX token for a connection and return it as a string.
pub fn next_who_token(state: &mut AppState, conn_id: &str) -> String {
    if let Some(conn) = state.connections.get_mut(conn_id) {
        conn.who_token_counter = conn.who_token_counter.wrapping_add(1);
        conn.who_token_counter.to_string()
    } else {
        "0".to_string()
    }
}

/// Build a WHOX WHO command for the given channel.
/// Returns `Some((target, fields_with_token))` if WHOX is available, `None` otherwise.
///
/// When `silent` is true, the channel is added to
/// `Connection::silent_who_channels` so that reply handlers update
/// nick state without displaying output (used for auto-WHO on join).
pub fn build_whox_who(
    state: &mut AppState,
    conn_id: &str,
    channel: &str,
    silent: bool,
) -> Option<(String, String)> {
    let has_whox = state
        .connections
        .get(conn_id)
        .is_some_and(|c| c.isupport_parsed.has_whox());

    if has_whox {
        let token = next_who_token(state, conn_id);
        if silent && let Some(conn) = state.connections.get_mut(conn_id) {
            conn.silent_who_channels.insert(channel.to_string());
        }
        let fields = format!("{},{token}", crate::constants::WHOX_FIELDS);
        Some((channel.to_string(), fields))
    } else {
        None
    }
}

/// Handle a WHOX reply (numeric 354 / `RPL_WHOSPCRPL`).
///
/// Our field selector `%tcuihnfar` produces responses with fields:
///   `[our_nick, token, channel, user, ip, host, nick, flags, account, realname]`
///
/// Note: The irc crate treats 354 as `Command::Raw("354", args)` since it's non-standard.
/// The `args` vec already has `our_nick` as the first element (the trailing prefix from the Raw parse).
fn handle_whox_reply(state: &mut AppState, conn_id: &str, args: &[String]) {
    tracing::trace!(conn_id, args_len = args.len(), ?args, "handle_whox_reply");
    // Minimum fields: our_nick(0) + token(1) + channel(2) + user(3) + ip(4) + host(5)
    //                + nick(6) + flags(7) + account(8) + realname(9)
    if args.len() < 10 {
        tracing::warn!(
            conn_id,
            args_len = args.len(),
            "WHOX reply too short, skipping"
        );
        return;
    }

    // args[1] is the WHOX token
    let channel = &args[2];
    let user = &args[3];
    // args[4] is IP
    let host = &args[5];
    let nick = &args[6];
    let flags = &args[7];
    let account_raw = &args[8];
    let realname = &args[9];

    // Auto-WHO replies are silent — update state only, no display
    let silent = state
        .connections
        .get(conn_id)
        .is_some_and(|c| c.silent_who_channels.contains(channel.as_str()));

    // Parse away status from flags: H = here, G = gone
    let away = flags.starts_with('G');

    // Parse account: "0" means not logged in
    let account = (account_raw != "0").then(|| account_raw.clone());

    // Update NickEntry in the channel buffer
    let buffer_id = make_buffer_id(conn_id, channel);
    if let Some(buf) = state.buffers.get_mut(&buffer_id)
        && let Some(entry) = buf.users.get_mut(&nick.to_lowercase())
    {
        tracing::trace!(%nick, %channel, %away, ?account, "WHOX: updating nick entry");
        entry.ident = Some(user.clone());
        entry.host = Some(host.clone());
        entry.account.clone_from(&account);
        entry.away = away;
    } else {
        tracing::warn!(%nick, %channel, %buffer_id, "WHOX: buffer or nick not found for update");
    }

    // Only display for manual /who — auto-WHO on join is silent
    if !silent {
        let target_buf = active_or_server_buffer(state, conn_id);
        let account_str = account.as_deref().unwrap_or("");
        emit(
            state,
            &target_buf,
            &format!(
                "%Zc0caf5{nick}%Z565f89 ({user}@{host}) [{flags}] {channel}%Za9b1d6 {realname}%Z565f89 [{account_str}]%N"
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::connection::Connection;
    use chrono::{Datelike, Timelike};
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
            lag_pending: false,
            reconnect_attempts: 0,
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
                sasl_mechanism: None,
                client_cert_path: None,
            },
            local_ip: None,
            enabled_caps: std::collections::HashSet::new(),
            who_token_counter: 0,
            silent_who_channels: std::collections::HashSet::new(),
        });
        // Server buffer
        state.add_buffer(Buffer {
            id: make_buffer_id("test", "TestServer"),
            connection_id: "test".to_string(),
            buffer_type: BufferType::Server,
            name: "TestServer".to_string(),
            messages: VecDeque::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
            last_speakers: Vec::new(),
        });
        // Channel buffer
        let chan_id = make_buffer_id("test", "#test");
        state.add_buffer(Buffer {
            id: chan_id.clone(),
            connection_id: "test".to_string(),
            buffer_type: BufferType::Channel,
            name: "#test".to_string(),
            messages: VecDeque::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
            last_speakers: Vec::new(),
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

    fn make_channel_buffer(conn_id: &str, name: &str) -> Buffer {
        Buffer {
            id: make_buffer_id(conn_id, name),
            connection_id: conn_id.to_string(),
            buffer_type: BufferType::Channel,
            name: name.to_string(),
            messages: VecDeque::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: Utc::now(),
            topic: None,
            topic_set_by: None,
            users: std::collections::HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: std::collections::HashMap::new(),
            last_speakers: Vec::new(),
        }
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
        assert!(
            buf.messages
                .back()
                .unwrap()
                .text
                .contains("carol (user@host) has joined")
        );
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
        assert!(
            buf.messages
                .back()
                .unwrap()
                .text
                .contains("dave (user@host) has left")
        );
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
        let msg = make_irc_msg(Some("eve!user@host"), Command::QUIT(Some("gone".into())));
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(!buf.users.contains_key("eve"));
        assert!(
            buf.messages
                .back()
                .unwrap()
                .text
                .contains("eve (user@host) has quit")
        );
    }

    // === handle_nick_change tests ===

    #[test]
    fn nick_change_updates_our_nick() {
        let mut state = make_test_state();
        let msg = make_irc_msg(Some("me!user@host"), Command::NICK("me_".into()));
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
        let msg = make_irc_msg(Some("frank!user@host"), Command::NICK("frankie".into()));
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(!buf.users.contains_key("frank"));
        assert!(buf.users.contains_key("frankie"));
        assert!(
            buf.messages
                .back()
                .unwrap()
                .text
                .contains("frank is now known as frankie")
        );
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
        assert!(
            buf.messages
                .back()
                .unwrap()
                .text
                .contains("troll was kicked by op")
        );
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
        let prefix_map = vec![('q', '~'), ('a', '&'), ('o', '@'), ('h', '%'), ('v', '+')];
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
        assert!(buf.messages.back().unwrap().text.contains("Looking up"));
        assert_eq!(
            buf.messages.back().unwrap().message_type,
            MessageType::Notice
        );
    }

    // === extended-join tests ===

    #[test]
    fn extended_join_with_account() {
        let mut state = make_test_state();
        // extended-join: JOIN #channel account :Real Name
        let msg = make_irc_msg(
            Some("carol!user@host"),
            Command::JOIN(
                "#test".into(),
                Some("patrick".into()),
                Some("Real Name".into()),
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(buf.users.contains_key("carol"));
        let entry = buf.users.get("carol").unwrap();
        assert_eq!(entry.account.as_deref(), Some("patrick"));

        // Join message should include account and realname
        let join_msg = buf.messages.back().unwrap();
        assert!(join_msg.text.contains("[patrick]"));
        assert!(join_msg.text.contains("Real Name"));
        let params = join_msg.event_params.as_ref().unwrap();
        assert_eq!(params[4], "[patrick]"); // $4 = account
        assert_eq!(params[5], "Real Name"); // $5 = realname
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
        assert!(
            buf.messages
                .back()
                .unwrap()
                .text
                .contains("alice is now logged in as alice_account")
        );
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

        let msg = make_irc_msg(Some("alice!user@host"), Command::ACCOUNT("*".into()));
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        let entry = buf.users.get("alice").unwrap();
        assert_eq!(entry.account, None);
        assert!(
            buf.messages
                .back()
                .unwrap()
                .text
                .contains("alice has logged out")
        );
    }

    #[test]
    fn account_notify_updates_all_shared_buffers() {
        let mut state = make_test_state();
        // Create a second channel buffer
        let chan2_id = make_buffer_id("test", "#other");
        state.add_buffer(Buffer {
            id: chan2_id,
            connection_id: "test".to_string(),
            buffer_type: BufferType::Channel,
            name: "#other".to_string(),
            messages: VecDeque::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
            last_speakers: Vec::new(),
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
        let entry1 = state
            .buffers
            .get("test/#test")
            .unwrap()
            .users
            .get("alice")
            .unwrap();
        assert_eq!(entry1.account.as_deref(), Some("alice_acct"));
        let entry2 = state
            .buffers
            .get("test/#other")
            .unwrap()
            .users
            .get("alice")
            .unwrap();
        assert_eq!(entry2.account.as_deref(), Some("alice_acct"));
    }

    // === away-notify tests ===

    #[test]
    fn away_notify_sets_away() {
        let mut state = make_test_state();
        // Add user to channel
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
            Command::AWAY(Some("Gone fishing".into())),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        let entry = buf.users.get("alice").unwrap();
        assert!(
            entry.away,
            "NickEntry.away should be true after AWAY with reason"
        );
        // Should NOT add event messages (too noisy)
        assert!(
            buf.messages.is_empty(),
            "away-notify should not add event messages"
        );
    }

    #[test]
    fn away_notify_clears_away() {
        let mut state = make_test_state();
        // Add user already marked away
        state.add_nick(
            "test/#test",
            NickEntry {
                nick: "alice".to_string(),
                prefix: String::new(),
                modes: String::new(),
                away: true,
                account: None,
                ident: None,
                host: None,
            },
        );

        let msg = make_irc_msg(Some("alice!user@host"), Command::AWAY(None));
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        let entry = buf.users.get("alice").unwrap();
        assert!(
            !entry.away,
            "NickEntry.away should be false after AWAY without reason"
        );
        assert!(
            buf.messages.is_empty(),
            "away-notify should not add event messages"
        );
    }

    #[test]
    fn away_notify_updates_all_shared_buffers() {
        let mut state = make_test_state();
        // Create a second channel buffer
        let chan2_id = make_buffer_id("test", "#other");
        state.add_buffer(Buffer {
            id: chan2_id,
            connection_id: "test".to_string(),
            buffer_type: BufferType::Channel,
            name: "#other".to_string(),
            messages: VecDeque::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
            last_speakers: Vec::new(),
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

        let msg = make_irc_msg(Some("alice!user@host"), Command::AWAY(Some("BRB".into())));
        handle_irc_message(&mut state, "test", &msg);

        // Both buffers should have away = true
        let entry1 = state
            .buffers
            .get("test/#test")
            .unwrap()
            .users
            .get("alice")
            .unwrap();
        assert!(entry1.away);
        let entry2 = state
            .buffers
            .get("test/#other")
            .unwrap()
            .users
            .get("alice")
            .unwrap();
        assert!(entry2.away);
    }

    // === chghost tests ===

    #[test]
    fn chghost_updates_ident_and_host() {
        let mut state = make_test_state();
        // Add user to channel
        state.add_nick(
            "test/#test",
            NickEntry {
                nick: "alice".to_string(),
                prefix: String::new(),
                modes: String::new(),
                away: false,
                account: None,
                ident: Some("olduser".to_string()),
                host: Some("oldhost.example.com".to_string()),
            },
        );

        let msg = make_irc_msg(
            Some("alice!olduser@oldhost.example.com"),
            Command::CHGHOST("newuser".into(), "newhost.example.com".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        let entry = buf.users.get("alice").unwrap();
        assert_eq!(entry.ident.as_deref(), Some("newuser"));
        assert_eq!(entry.host.as_deref(), Some("newhost.example.com"));
    }

    #[test]
    fn chghost_adds_event_message() {
        let mut state = make_test_state();
        // Add user to channel
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
            Some("alice!olduser@oldhost"),
            Command::CHGHOST("newident".into(), "new.host.net".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert_eq!(buf.messages.len(), 1);
        let event = &buf.messages[0];
        assert_eq!(event.message_type, MessageType::Event);
        assert!(
            event
                .text
                .contains("alice changed host to newident@new.host.net")
        );
        assert_eq!(event.event_key.as_deref(), Some("chghost"));
    }

    #[test]
    fn chghost_updates_all_shared_buffers() {
        let mut state = make_test_state();
        // Create a second channel buffer
        let chan2_id = make_buffer_id("test", "#other");
        state.add_buffer(Buffer {
            id: chan2_id,
            connection_id: "test".to_string(),
            buffer_type: BufferType::Channel,
            name: "#other".to_string(),
            messages: VecDeque::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
            last_speakers: Vec::new(),
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
            Command::CHGHOST("changed".into(), "vhost.net".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        // Both buffers should have updated ident/host
        let entry1 = state
            .buffers
            .get("test/#test")
            .unwrap()
            .users
            .get("alice")
            .unwrap();
        assert_eq!(entry1.ident.as_deref(), Some("changed"));
        assert_eq!(entry1.host.as_deref(), Some("vhost.net"));
        let entry2 = state
            .buffers
            .get("test/#other")
            .unwrap()
            .users
            .get("alice")
            .unwrap();
        assert_eq!(entry2.ident.as_deref(), Some("changed"));
        assert_eq!(entry2.host.as_deref(), Some("vhost.net"));

        // Both buffers should have event messages
        assert_eq!(state.buffers.get("test/#test").unwrap().messages.len(), 1);
        assert_eq!(state.buffers.get("test/#other").unwrap().messages.len(), 1);
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
        msg.tags = Some(vec![irc::proto::message::Tag(
            "account".to_string(),
            Some("alice_acct".to_string()),
        )]);
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
            Command::JOIN(
                "#newchan".into(),
                Some("my_account".into()),
                Some("My Real Name".into()),
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        assert!(state.buffers.contains_key("test/#newchan"));
        let buf = state.buffers.get("test/#newchan").unwrap();
        assert_eq!(buf.buffer_type, BufferType::Channel);
    }

    // === server-time tests ===

    #[test]
    fn server_time_tag_used_as_timestamp() {
        let mut state = make_test_state();
        let mut msg = make_irc_msg(
            Some("alice!user@host"),
            Command::PRIVMSG("#test".into(), "hello from the past".into()),
        );
        msg.tags = Some(vec![irc::proto::message::Tag(
            "time".to_string(),
            Some("2020-06-15T10:30:00.000Z".to_string()),
        )]);
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        let ts = buf.messages[0].timestamp;
        assert_eq!(ts.year(), 2020);
        assert_eq!(ts.month(), 6);
        assert_eq!(ts.day(), 15);
        assert_eq!(ts.hour(), 10);
        assert_eq!(ts.minute(), 30);
    }

    #[test]
    fn missing_time_tag_falls_back_to_now() {
        let mut state = make_test_state();
        let before = Utc::now();
        let msg = make_irc_msg(
            Some("alice!user@host"),
            Command::PRIVMSG("#test".into(), "hello".into()),
        );
        handle_irc_message(&mut state, "test", &msg);
        let after = Utc::now();

        let buf = state.buffers.get("test/#test").unwrap();
        let ts = buf.messages[0].timestamp;
        assert!(
            ts >= before && ts <= after,
            "timestamp should be approximately now"
        );
    }

    #[test]
    fn malformed_time_tag_falls_back_to_now() {
        let mut state = make_test_state();
        let before = Utc::now();
        let mut msg = make_irc_msg(
            Some("alice!user@host"),
            Command::PRIVMSG("#test".into(), "hello".into()),
        );
        msg.tags = Some(vec![irc::proto::message::Tag(
            "time".to_string(),
            Some("not-a-timestamp".to_string()),
        )]);
        handle_irc_message(&mut state, "test", &msg);
        let after = Utc::now();

        let buf = state.buffers.get("test/#test").unwrap();
        let ts = buf.messages[0].timestamp;
        assert!(
            ts >= before && ts <= after,
            "malformed tag should fall back to now"
        );
    }

    #[test]
    fn server_time_helper_unit() {
        // Valid RFC 3339 timestamp
        let mut tags = HashMap::new();
        tags.insert("time".to_string(), "2023-01-15T08:45:30.123Z".to_string());
        let ts = message_timestamp(Some(&tags));
        assert_eq!(ts.year(), 2023);
        assert_eq!(ts.month(), 1);
        assert_eq!(ts.day(), 15);
        assert_eq!(ts.hour(), 8);
        assert_eq!(ts.minute(), 45);
        assert_eq!(ts.second(), 30);

        // None tags → fallback
        let before = Utc::now();
        let ts = message_timestamp(None);
        let after = Utc::now();
        assert!(ts >= before && ts <= after);

        // Malformed value → fallback
        let mut bad = HashMap::new();
        bad.insert("time".to_string(), "garbage".to_string());
        let before = Utc::now();
        let ts = message_timestamp(Some(&bad));
        let after = Utc::now();
        assert!(ts >= before && ts <= after);
    }

    // ── cap-notify tests ─────────────────────────────────────────────

    #[test]
    fn cap_new_desired_caps_returns_request_list() {
        let mut state = make_test_state();
        // Pre-enable some caps so they are NOT re-requested
        if let Some(conn) = state.connections.get_mut("test") {
            conn.enabled_caps.insert("multi-prefix".to_string());
        }

        // Server advertises new caps: one already enabled, one desired, one unknown
        let to_request = handle_cap_new(
            &mut state,
            "test",
            Some("multi-prefix echo-message unknown-cap"),
            None,
        );

        // Should only request echo-message (multi-prefix already enabled, unknown-cap not desired)
        assert_eq!(to_request, vec!["echo-message"]);

        // Verify status message was logged
        let buf = state
            .buffers
            .get(&make_buffer_id("test", "TestServer"))
            .unwrap();
        let last = buf.messages.back().unwrap();
        assert!(
            last.text.contains("echo-message"),
            "should mention requested cap"
        );
        assert_eq!(last.event_key.as_deref(), Some("cap_new"));
    }

    #[test]
    fn cap_new_non_desired_caps_ignored() {
        let mut state = make_test_state();
        let to_request =
            handle_cap_new(&mut state, "test", Some("unknown-cap fancy-feature"), None);

        assert!(to_request.is_empty(), "no desired caps should be requested");

        let buf = state
            .buffers
            .get(&make_buffer_id("test", "TestServer"))
            .unwrap();
        let last = buf.messages.back().unwrap();
        assert!(
            last.text.contains("none requested"),
            "should note nothing was requested"
        );
    }

    #[test]
    fn cap_new_with_values_strips_value_part() {
        let mut state = make_test_state();
        // Server sends caps with values (e.g. sasl=PLAIN,EXTERNAL)
        let to_request = handle_cap_new(
            &mut state,
            "test",
            Some("sasl=PLAIN,EXTERNAL server-time"),
            None,
        );

        // Both are desired caps, neither enabled yet
        assert!(to_request.contains(&"sasl".to_string()));
        assert!(to_request.contains(&"server-time".to_string()));
    }

    #[test]
    fn cap_del_removes_from_enabled() {
        let mut state = make_test_state();
        // Pre-enable some caps
        if let Some(conn) = state.connections.get_mut("test") {
            conn.enabled_caps.insert("multi-prefix".to_string());
            conn.enabled_caps.insert("server-time".to_string());
            conn.enabled_caps.insert("away-notify".to_string());
        }

        // Server removes multi-prefix and server-time
        handle_cap_del(&mut state, "test", Some("multi-prefix server-time"), None);

        let conn = state.connections.get("test").unwrap();
        assert!(!conn.enabled_caps.contains("multi-prefix"));
        assert!(!conn.enabled_caps.contains("server-time"));
        assert!(
            conn.enabled_caps.contains("away-notify"),
            "untouched cap should remain"
        );

        let buf = state
            .buffers
            .get(&make_buffer_id("test", "TestServer"))
            .unwrap();
        let last = buf.messages.back().unwrap();
        assert_eq!(last.event_key.as_deref(), Some("cap_del"));
        assert!(last.text.contains("multi-prefix"));
    }

    #[test]
    fn cap_del_for_non_enabled_caps_is_noop() {
        let mut state = make_test_state();
        // No caps enabled
        handle_cap_del(&mut state, "test", Some("fancy-feature unknown-cap"), None);

        let conn = state.connections.get("test").unwrap();
        assert!(conn.enabled_caps.is_empty());

        let buf = state
            .buffers
            .get(&make_buffer_id("test", "TestServer"))
            .unwrap();
        let last = buf.messages.back().unwrap();
        assert!(last.text.contains("none were enabled"));
    }

    #[test]
    fn cap_ack_adds_to_enabled() {
        let mut state = make_test_state();
        handle_cap_ack(&mut state, "test", Some("echo-message invite-notify"), None);

        let conn = state.connections.get("test").unwrap();
        assert!(conn.enabled_caps.contains("echo-message"));
        assert!(conn.enabled_caps.contains("invite-notify"));

        let buf = state
            .buffers
            .get(&make_buffer_id("test", "TestServer"))
            .unwrap();
        let last = buf.messages.back().unwrap();
        assert_eq!(last.event_key.as_deref(), Some("cap_ack"));
        assert!(last.text.contains("echo-message"));
    }

    #[test]
    fn cap_nak_logs_rejection() {
        let mut state = make_test_state();
        handle_cap_nak(&mut state, "test", Some("echo-message"), None);

        // NAK should NOT add to enabled_caps
        let conn = state.connections.get("test").unwrap();
        assert!(!conn.enabled_caps.contains("echo-message"));

        let buf = state
            .buffers
            .get(&make_buffer_id("test", "TestServer"))
            .unwrap();
        let last = buf.messages.back().unwrap();
        assert_eq!(last.event_key.as_deref(), Some("cap_nak"));
        assert!(last.text.contains("echo-message"));
    }

    #[test]
    fn extract_cap_string_field3_primary() {
        // Normal case: caps in field3
        assert_eq!(
            extract_cap_string(Some("multi-prefix server-time"), None),
            "multi-prefix server-time"
        );
    }

    #[test]
    fn extract_cap_string_continuation() {
        // Continuation: field3 = "*", caps in field4
        assert_eq!(
            extract_cap_string(Some("*"), Some("batch echo-message")),
            "batch echo-message"
        );
    }

    #[test]
    fn extract_cap_string_field4_preferred_when_present() {
        // Both present (non-"*" field3): prefer field4 if non-empty
        assert_eq!(
            extract_cap_string(Some("some-prefix"), Some("actual-caps here")),
            "actual-caps here"
        );
    }

    #[test]
    fn cap_new_full_roundtrip_with_ack() {
        // Simulate: CAP NEW → filter → (caller sends REQ) → CAP ACK → enabled
        let mut state = make_test_state();

        // Step 1: CAP NEW announces echo-message and batch
        let to_request = handle_cap_new(&mut state, "test", Some("echo-message batch"), None);
        assert_eq!(to_request.len(), 2);
        assert!(to_request.contains(&"echo-message".to_string()));
        assert!(to_request.contains(&"batch".to_string()));

        // Step 2: Server ACKs the request
        handle_cap_ack(&mut state, "test", Some("echo-message batch"), None);

        let conn = state.connections.get("test").unwrap();
        assert!(conn.enabled_caps.contains("echo-message"));
        assert!(conn.enabled_caps.contains("batch"));

        // Step 3: Server later DELs batch
        handle_cap_del(&mut state, "test", Some("batch"), None);

        let conn = state.connections.get("test").unwrap();
        assert!(
            conn.enabled_caps.contains("echo-message"),
            "echo-message should remain"
        );
        assert!(
            !conn.enabled_caps.contains("batch"),
            "batch should be removed"
        );
    }

    // === echo-message tests ===

    #[test]
    fn echo_message_own_privmsg_displayed_when_cap_enabled() {
        let mut state = make_test_state();
        // Enable echo-message cap
        state
            .connections
            .get_mut("test")
            .unwrap()
            .enabled_caps
            .insert("echo-message".to_string());

        // Server echoes our own PRIVMSG to #test
        let msg = make_irc_msg(
            Some("me!user@host"),
            Command::PRIVMSG("#test".into(), "hello from echo".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert_eq!(buf.messages.len(), 1, "echoed message should be displayed");
        assert_eq!(buf.messages[0].text, "hello from echo");
        assert_eq!(buf.messages[0].nick.as_deref(), Some("me"));
        assert_eq!(buf.messages[0].message_type, MessageType::Message);
    }

    #[test]
    fn echo_message_own_privmsg_no_cap_unchanged() {
        let mut state = make_test_state();
        // echo-message is NOT enabled (default)

        // We receive our own PRIVMSG (unusual without echo-message, but handle gracefully)
        let msg = make_irc_msg(
            Some("me!user@host"),
            Command::PRIVMSG("#test".into(), "my own message".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert_eq!(buf.messages.len(), 1, "message should still be displayed");
        assert_eq!(buf.messages[0].text, "my own message");
        // Own messages should not trigger activity
        assert_eq!(buf.activity, ActivityLevel::None);
    }

    #[test]
    fn echo_message_own_pm_routes_to_recipient_buffer() {
        let mut state = make_test_state();
        state
            .connections
            .get_mut("test")
            .unwrap()
            .enabled_caps
            .insert("echo-message".to_string());

        // Server echoes our PM to "bob" — target is "bob", nick is "me"
        let msg = make_irc_msg(
            Some("me!user@host"),
            Command::PRIVMSG("bob".into(), "hey bob".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        // Should create a query buffer for "bob", not "me"
        assert!(
            state.buffers.contains_key("test/bob"),
            "query buffer should be created for recipient"
        );
        assert!(
            !state.buffers.contains_key("test/me"),
            "should NOT create a buffer named after ourselves"
        );
        let buf = state.buffers.get("test/bob").unwrap();
        assert_eq!(buf.buffer_type, BufferType::Query);
        assert_eq!(buf.messages.len(), 1);
        assert_eq!(buf.messages[0].text, "hey bob");
        assert_eq!(buf.messages[0].nick.as_deref(), Some("me"));
    }

    #[test]
    fn echo_message_own_action_displayed() {
        let mut state = make_test_state();
        state
            .connections
            .get_mut("test")
            .unwrap()
            .enabled_caps
            .insert("echo-message".to_string());

        let msg = make_irc_msg(
            Some("me!user@host"),
            Command::PRIVMSG("#test".into(), "\x01ACTION dances\x01".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert_eq!(buf.messages.len(), 1);
        assert_eq!(buf.messages[0].message_type, MessageType::Action);
        assert_eq!(buf.messages[0].text, "dances");
        assert_eq!(buf.messages[0].nick.as_deref(), Some("me"));
    }

    #[test]
    fn echo_message_own_notice_routes_to_recipient() {
        let mut state = make_test_state();
        state
            .connections
            .get_mut("test")
            .unwrap()
            .enabled_caps
            .insert("echo-message".to_string());

        // Create a query buffer for "bob" so the notice has somewhere to go
        state.add_buffer(Buffer {
            id: make_buffer_id("test", "bob"),
            connection_id: "test".to_string(),
            buffer_type: BufferType::Query,
            name: "bob".to_string(),
            messages: VecDeque::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
            last_speakers: Vec::new(),
        });

        // Server echoes our NOTICE to "bob"
        let msg = make_irc_msg(
            Some("me!user@host"),
            Command::NOTICE("bob".into(), "notice to bob".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/bob").unwrap();
        assert_eq!(buf.messages.len(), 1);
        assert_eq!(buf.messages[0].message_type, MessageType::Notice);
        assert_eq!(buf.messages[0].text, "notice to bob");
    }

    // === invite-notify tests ===

    #[test]
    fn invite_target_is_us_shows_in_active_buffer() {
        let mut state = make_test_state();
        // Set active buffer to the channel so the invite message lands there
        state.set_active_buffer("test/#test");

        let msg = make_irc_msg(
            Some("op!user@host"),
            Command::INVITE("me".into(), "#secret".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        // When we are the target, the message goes to the active buffer
        let buf = state.buffers.get("test/#test").unwrap();
        assert_eq!(buf.messages.len(), 1);
        assert_eq!(buf.messages[0].message_type, MessageType::Event);
        assert_eq!(buf.messages[0].text, "op invites you to #secret");
        assert!(buf.messages[0].highlight);
    }

    #[test]
    fn invite_notify_other_user_shows_in_channel() {
        let mut state = make_test_state();
        // Set active buffer to server so we can verify the message goes to #test, not active
        state.set_active_buffer("test/testserver");

        let msg = make_irc_msg(
            Some("op!user@host"),
            Command::INVITE("alice".into(), "#test".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        // invite-notify: message goes to the channel buffer, not the active buffer
        let buf = state.buffers.get("test/#test").unwrap();
        assert_eq!(buf.messages.len(), 1);
        assert_eq!(buf.messages[0].message_type, MessageType::Event);
        assert_eq!(buf.messages[0].text, "op invited alice to #test");
        assert!(!buf.messages[0].highlight);

        // Server buffer should have no messages from this invite
        let server_buf = state.buffers.get("test/testserver").unwrap();
        assert_eq!(server_buf.messages.len(), 0);
    }

    // === WHOX tests ===

    fn make_whox_state() -> AppState {
        let mut state = make_test_state();
        // Enable WHOX on the connection's ISUPPORT
        if let Some(conn) = state.connections.get_mut("test") {
            conn.isupport_parsed.parse_tokens(&["WHOX"]);
        }
        // Add some users to #test for WHOX updates
        let chan_id = make_buffer_id("test", "#test");
        state.add_nick(
            &chan_id,
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
        state.add_nick(
            &chan_id,
            NickEntry {
                nick: "bob".to_string(),
                prefix: "@".to_string(),
                modes: "o".to_string(),
                away: false,
                account: None,
                ident: None,
                host: None,
            },
        );
        state
    }

    #[test]
    fn whox_reply_updates_nick_entry() {
        let mut state = make_whox_state();
        state.set_active_buffer("test/testserver");

        // WHOX 354 response: our_nick, token, channel, user, ip, host, nick, flags, account, realname
        let msg = make_irc_msg(
            None,
            Command::Raw(
                "354".to_string(),
                vec![
                    "me".to_string(),               // our_nick
                    "1".to_string(),                // token
                    "#test".to_string(),            // channel
                    "~alice".to_string(),           // user
                    "1.2.3.4".to_string(),          // ip
                    "host.example.com".to_string(), // host
                    "alice".to_string(),            // nick
                    "H".to_string(),                // flags (H=here)
                    "patrick".to_string(),          // account
                    "Alice Smith".to_string(),      // realname
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        let entry = buf.users.get("alice").unwrap();
        assert_eq!(entry.ident.as_deref(), Some("~alice"));
        assert_eq!(entry.host.as_deref(), Some("host.example.com"));
        assert_eq!(entry.account.as_deref(), Some("patrick"));
        assert!(!entry.away);
    }

    #[test]
    fn whox_account_zero_means_not_logged_in() {
        let mut state = make_whox_state();
        state.set_active_buffer("test/testserver");

        let msg = make_irc_msg(
            None,
            Command::Raw(
                "354".to_string(),
                vec![
                    "me".to_string(),
                    "1".to_string(),
                    "#test".to_string(),
                    "~bob".to_string(),
                    "5.6.7.8".to_string(),
                    "bob.host.net".to_string(),
                    "bob".to_string(),
                    "H@".to_string(),
                    "0".to_string(), // account="0" → not logged in
                    "Bob Jones".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        let entry = buf.users.get("bob").unwrap();
        assert!(entry.account.is_none());
    }

    #[test]
    fn whox_gone_flag_sets_away() {
        let mut state = make_whox_state();
        state.set_active_buffer("test/testserver");

        let msg = make_irc_msg(
            None,
            Command::Raw(
                "354".to_string(),
                vec![
                    "me".to_string(),
                    "1".to_string(),
                    "#test".to_string(),
                    "~alice".to_string(),
                    "1.2.3.4".to_string(),
                    "host.example.com".to_string(),
                    "alice".to_string(),
                    "G".to_string(), // G = gone/away
                    "alice_acct".to_string(),
                    "Alice".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        let entry = buf.users.get("alice").unwrap();
        assert!(entry.away);
    }

    #[test]
    fn whox_here_flag_clears_away() {
        let mut state = make_whox_state();

        // First set alice as away
        let chan_id = make_buffer_id("test", "#test");
        if let Some(buf) = state.buffers.get_mut(&chan_id)
            && let Some(entry) = buf.users.get_mut("alice")
        {
            entry.away = true;
        }
        state.set_active_buffer("test/testserver");

        let msg = make_irc_msg(
            None,
            Command::Raw(
                "354".to_string(),
                vec![
                    "me".to_string(),
                    "1".to_string(),
                    "#test".to_string(),
                    "~alice".to_string(),
                    "1.2.3.4".to_string(),
                    "host.example.com".to_string(),
                    "alice".to_string(),
                    "H".to_string(), // H = here (not away)
                    "0".to_string(),
                    "Alice".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        let entry = buf.users.get("alice").unwrap();
        assert!(!entry.away);
    }

    #[test]
    fn standard_who_reply_still_works() {
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");

        // Standard RPL_WHOREPLY (352)
        let msg = make_irc_msg(
            None,
            Command::Response(
                Response::RPL_WHOREPLY,
                vec![
                    "me".to_string(),
                    "#test".to_string(),
                    "~user".to_string(),
                    "host.com".to_string(),
                    "irc.net".to_string(),
                    "alice".to_string(),
                    "H@".to_string(),
                    "0 Real Name".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        // Should display in the active/server buffer
        let buf = state.buffers.get("test/testserver").unwrap();
        assert_eq!(buf.messages.len(), 1);
        assert!(buf.messages[0].text.contains("alice"));
    }

    #[test]
    fn next_who_token_increments() {
        let mut state = make_test_state();
        let t1 = next_who_token(&mut state, "test");
        let t2 = next_who_token(&mut state, "test");
        let t3 = next_who_token(&mut state, "test");
        assert_eq!(t1, "1");
        assert_eq!(t2, "2");
        assert_eq!(t3, "3");
    }

    #[test]
    fn build_whox_who_returns_none_without_whox() {
        let mut state = make_test_state();
        // WHOX not enabled by default
        assert!(build_whox_who(&mut state, "test", "#test", false).is_none());
    }

    #[test]
    fn build_whox_who_returns_fields_with_whox() {
        let mut state = make_whox_state();
        let result = build_whox_who(&mut state, "test", "#test", false);
        assert!(result.is_some());
        let (target, fields) = result.unwrap();
        assert_eq!(target, "#test");
        assert!(fields.starts_with("%tcuihnfar,"));
        // Token should be "1" (first call)
        assert!(fields.ends_with(",1"));
    }

    #[test]
    fn build_whox_who_silent_registers_channel() {
        let mut state = make_whox_state();
        let result = build_whox_who(&mut state, "test", "#silent", true);
        assert!(result.is_some());
        let conn = state.connections.get("test").unwrap();
        assert!(conn.silent_who_channels.contains("#silent"));
    }

    #[test]
    fn build_whox_who_non_silent_does_not_register() {
        let mut state = make_whox_state();
        let _result = build_whox_who(&mut state, "test", "#loud", false);
        let conn = state.connections.get("test").unwrap();
        assert!(!conn.silent_who_channels.contains("#loud"));
    }

    #[test]
    fn silent_whox_reply_updates_state_without_display() {
        let mut state = make_whox_state();
        state.set_active_buffer("test/testserver");

        // Register #test as silent auto-WHO
        if let Some(conn) = state.connections.get_mut("test") {
            conn.silent_who_channels.insert("#test".to_string());
        }

        let msg = make_irc_msg(
            None,
            Command::Raw(
                "354".to_string(),
                vec![
                    "me".to_string(),
                    "1".to_string(),
                    "#test".to_string(),
                    "~alice".to_string(),
                    "1.2.3.4".to_string(),
                    "host.example.com".to_string(),
                    "alice".to_string(),
                    "H".to_string(),
                    "alice_acct".to_string(),
                    "Alice Smith".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        // State updated
        let buf = state.buffers.get("test/#test").unwrap();
        let entry = buf.users.get("alice").unwrap();
        assert_eq!(entry.ident.as_deref(), Some("~alice"));
        assert_eq!(entry.account.as_deref(), Some("alice_acct"));

        // No display output — server buffer should be empty
        let server_buf = state.buffers.get("test/testserver").unwrap();
        assert!(server_buf.messages.is_empty());
    }

    #[test]
    fn silent_who_end_cleans_up_and_suppresses_display() {
        let mut state = make_whox_state();
        state.set_active_buffer("test/testserver");

        // Register #test as silent auto-WHO
        if let Some(conn) = state.connections.get_mut("test") {
            conn.silent_who_channels.insert("#test".to_string());
        }

        let msg = make_irc_msg(
            None,
            Command::Response(
                Response::RPL_ENDOFWHO,
                vec![
                    "me".to_string(),
                    "#test".to_string(),
                    "End of WHO list".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        // Silent channel removed
        let conn = state.connections.get("test").unwrap();
        assert!(!conn.silent_who_channels.contains("#test"));

        // No display output
        let server_buf = state.buffers.get("test/testserver").unwrap();
        assert!(server_buf.messages.is_empty());
    }

    #[test]
    fn manual_who_end_displays_message() {
        let mut state = make_whox_state();
        state.set_active_buffer("test/testserver");

        // No silent channels registered — this is a manual /who
        let msg = make_irc_msg(
            None,
            Command::Response(
                Response::RPL_ENDOFWHO,
                vec![
                    "me".to_string(),
                    "#test".to_string(),
                    "End of WHO list".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let server_buf = state.buffers.get("test/testserver").unwrap();
        assert_eq!(server_buf.messages.len(), 1);
        assert!(server_buf.messages[0].text.contains("End of WHO list"));
    }

    // === ERROR handler tests ===

    #[test]
    fn error_command_creates_event_in_status_buffer() {
        let mut state = make_test_state();
        let msg = make_irc_msg(
            Some("irc.server.com"),
            Command::ERROR("Closing Link: timeout".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/testserver").unwrap();
        assert_eq!(buf.messages.len(), 1);
        assert!(buf.messages[0].text.contains("ERROR"));
        assert!(buf.messages[0].text.contains("Closing Link: timeout"));
        assert_eq!(buf.messages[0].message_type, MessageType::Event);
    }

    #[test]
    fn error_command_marks_connection_as_errored() {
        let mut state = make_test_state();
        let msg = make_irc_msg(Some("irc.server.com"), Command::ERROR("Banned".into()));
        handle_irc_message(&mut state, "test", &msg);

        let conn = state.connections.get("test").unwrap();
        assert_eq!(conn.status, ConnectionStatus::Error);
        assert_eq!(conn.error.as_deref(), Some("Banned"));
    }

    // === Join failure: eager buffer cleanup ===

    #[test]
    fn join_failure_removes_empty_buffer() {
        let mut state = make_test_state();
        // Pre-create a channel buffer (erssi-style eager creation).
        state.add_buffer(make_channel_buffer("test", "#locked"));
        assert!(state.buffers.contains_key("test/#locked"));

        // Server responds with 474 ERR_BANNEDFROMCHAN.
        let msg = make_irc_msg(
            None,
            Command::Response(
                Response::ERR_BANNEDFROMCHAN,
                vec![
                    "me".into(),
                    "#locked".into(),
                    "Cannot join channel (+b)".into(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        // Buffer should be destroyed since it had no users.
        assert!(!state.buffers.contains_key("test/#locked"));
    }

    #[test]
    fn join_failure_keeps_active_buffer() {
        let mut state = make_test_state();
        // Pre-create buffer AND add a user (simulating a successful prior join).
        state.add_buffer(make_channel_buffer("test", "#active"));
        state.add_nick(
            "test/#active",
            NickEntry {
                nick: "me".into(),
                prefix: String::new(),
                modes: String::new(),
                away: false,
                account: None,
                ident: None,
                host: None,
            },
        );

        let msg = make_irc_msg(
            None,
            Command::Response(
                Response::ERR_BANNEDFROMCHAN,
                vec![
                    "me".into(),
                    "#active".into(),
                    "Cannot join channel (+b)".into(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        // Buffer should NOT be destroyed — it has users.
        assert!(state.buffers.contains_key("test/#active"));
    }

    #[test]
    fn join_failure_invite_only() {
        let mut state = make_test_state();
        state.add_buffer(make_channel_buffer("test", "#secret"));

        let msg = make_irc_msg(
            None,
            Command::Response(
                Response::ERR_INVITEONLYCHAN,
                vec![
                    "me".into(),
                    "#secret".into(),
                    "Cannot join channel (+i)".into(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        assert!(!state.buffers.contains_key("test/#secret"));
    }

    #[test]
    fn join_failure_channel_full() {
        let mut state = make_test_state();
        state.add_buffer(make_channel_buffer("test", "#crowded"));

        let msg = make_irc_msg(
            None,
            Command::Response(
                Response::ERR_CHANNELISFULL,
                vec![
                    "me".into(),
                    "#crowded".into(),
                    "Cannot join channel (+l)".into(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        assert!(!state.buffers.contains_key("test/#crowded"));
    }

    #[test]
    fn join_failure_bad_key() {
        let mut state = make_test_state();
        state.add_buffer(make_channel_buffer("test", "#keyed"));

        let msg = make_irc_msg(
            None,
            Command::Response(
                Response::ERR_BADCHANNELKEY,
                vec![
                    "me".into(),
                    "#keyed".into(),
                    "Cannot join channel (+k)".into(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        assert!(!state.buffers.contains_key("test/#keyed"));
    }
}

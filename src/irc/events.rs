use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::time::Instant;

use chrono::{DateTime, Utc};
use irc::proto::{Command, Message as IrcMessage, Prefix, Response};

use crate::config::IgnoreLevel;
use crate::irc::formatting::{
    extract_nick, extract_nick_userhost, is_channel, is_server_prefix, modes_to_prefix,
    strip_irc_formatting,
};
use crate::irc::ignore::{matches_mask_patterns, should_ignore};
use crate::state::AppState;
use crate::state::buffer::{
    ActivityLevel, Buffer, BufferType, ListEntry, Message, MessageType, NickEntry, make_buffer_id,
};
use crate::state::connection::ConnectionStatus;

/// Maximum number of entries stored per list mode type (bans, excepts, etc.).
const MAX_LIST_MODE_ENTRIES: usize = 500;
const BAN_MODE_KEY: &str = "b";

fn contains_case_insensitive(set: &HashSet<String>, value: &str) -> bool {
    set.contains(value) || set.iter().any(|entry| entry.eq_ignore_ascii_case(value))
}

fn remove_case_insensitive(set: &mut HashSet<String>, value: &str) -> bool {
    if set.remove(value) {
        return true;
    }
    let Some(existing) = set
        .iter()
        .find(|entry| entry.eq_ignore_ascii_case(value))
        .cloned()
    else {
        return false;
    };
    set.remove(&existing)
}

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
        // IRCv3 `TAGMSG` has no Command variant in the proto crate, so it
        // arrives as Raw. Only `+typing` is understood; the message-tags spec
        // forbids ever displaying TAGMSG in history.
        Command::Raw(verb, args) if verb.eq_ignore_ascii_case("TAGMSG") && !args.is_empty() => {
            handle_tagmsg(state, conn_id, &our_nick, msg, &args[0], tags.as_ref());
        }
        Command::JOIN(channel, account, realname) => {
            let fields = JoinFields {
                channel,
                account: account.as_deref(),
                realname: realname.as_deref(),
                uid: None,
                ip: None,
            };
            handle_join(state, conn_id, &our_nick, msg.prefix.as_ref(), &fields, tags);
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
        // RPL_CREATIONTIME (329): channel creation timestamp.
        // args = [our_nick, #channel, unix_timestamp]
        #[allow(
            clippy::collapsible_match,
            reason = "must NOT collapse into the guard: a malformed 329 (args<3) is swallowed here, and folding the length check into the match guard would let it fall through to the generic-numeric arm and display junk"
        )]
        Command::Raw(cmd, args) if cmd == "329" => {
            // A malformed 329 (fewer than 3 args) is intentionally swallowed
            // here rather than falling through to the generic-numeric arm.
            if args.len() >= 3 {
                let channel = &args[1];
                let silent = state.connections.get(conn_id).is_some_and(|conn| {
                    contains_case_insensitive(&conn.silent_banlist_channels, channel)
                });
                if silent {
                    return;
                }
                let buffer_id = make_buffer_id(conn_id, channel);
                if let Ok(ts) = args[2].parse::<i64>() {
                    let created = chrono::DateTime::from_timestamp(ts, 0).unwrap_or_else(Utc::now);
                    let formatted = created
                        .with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string();
                    let id = state.next_message_id();
                    state.add_message(
                        &buffer_id,
                        Message {
                            id,
                            timestamp: Utc::now(),
                            message_type: MessageType::Event,
                            nick: None,
                            nick_mode: None,
                            text: format!("Channel {channel} created {formatted}"),
                            highlight: false,
                            event_key: Some("channel_created".to_string()),
                            // $0=channel, $1=formatted date
                            event_params: Some(vec![channel.clone(), formatted]),
                            log_msg_id: None,
                            log_ref_id: None,
                            tags: None,
                        },
                    );
                }
            }
        }
        // WHOX response (354) comes as Command::Raw because the irc crate
        // doesn't recognize this non-standard numeric.
        Command::Raw(cmd, args) if cmd == "354" => {
            handle_whox_reply(state, conn_id, args);
        }
        Command::Raw(cmd, args) if cmd == "330" => {
            handle_whois_account(state, conn_id, args);
        }
        Command::Raw(cmd, args) if cmd == "671" => {
            handle_whois_secure(state, conn_id, args);
        }
        // WHOIS numerics with freeform prose that irc-proto has no Response
        // variant for (320 cloak/SSL specials, 307 regnick, 379 modes, ...).
        // Without this arm they'd fall to the generic numeric catch-all and
        // render unthemed outside the WHOIS block. Shorter-than-3-arg forms
        // (no separate nick token) intentionally fall through to that
        // catch-all so the line still displays.
        Command::Raw(cmd, args) if args.len() >= 3 && whois_freeform_key(cmd, args).is_some() => {
            handle_whois_freeform(state, conn_id, cmd, args);
        }
        // ircnet.com/extended-join (IRCnet ircd 2.12.0):
        //   :src JOIN <channel> <uid> <ip> <netjoin> <account> :<realname>
        // Six args exceed irc-proto's JOIN arity, so it arrives as Raw. When
        // both this cap and IRCv3 extended-join are acked, the server sends
        // ONLY this variant — dropping it would lose joins entirely (own
        // joins included, so no channel buffer would ever open). uid/ip are
        // informational; netjoin=1 (burst after relink) rides the same
        // netsplit detection as ordinary joins inside handle_join. Without
        // the cap ack, or with an unexpected arity, the extension fields are
        // untrusted — fall back to a channel-only join (args[0] is the
        // channel in every JOIN form) so the join still lands.
        Command::Raw(cmd, args) if cmd.eq_ignore_ascii_case("JOIN") && !args.is_empty() => {
            let has_cap = state
                .connections
                .get(conn_id)
                .is_some_and(|c| c.enabled_caps.contains("ircnet.com/extended-join"));
            let fields = if has_cap {
                join_fields(&msg.command)
            } else {
                None
            };
            let fields = fields.unwrap_or_else(|| {
                tracing::debug!(
                    args = args.len(),
                    has_cap,
                    "raw JOIN with unexpected shape — treating as channel-only join"
                );
                JoinFields {
                    channel: &args[0],
                    account: None,
                    realname: None,
                    uid: None,
                    ip: None,
                }
            });
            handle_join(state, conn_id, &our_nick, msg.prefix.as_ref(), &fields, tags);
        }
        // IRCv3 standard-reply FAIL for a BATCH command — surface multiline
        // rejections (MULTILINE_MAX_BYTES / MAX_LINES / INVALID_TARGET / INVALID).
        // FAIL arrives as a `Command::Raw` (not a numeric), so it isn't caught by
        // the numeric catch-all below; args = [command, code, [context...], desc].
        Command::Raw(cmd, args)
            if cmd == "FAIL" && args.first().map(String::as_str) == Some("BATCH") =>
        {
            let code = args.get(1).map_or("", String::as_str);
            if code.starts_with("MULTILINE_") {
                let desc = args.last().map_or("multiline error", String::as_str);
                let buffer_id = active_or_server_buffer(state, conn_id);
                emit(state, &buffer_id, &format!("%Zff6b6bmultiline: {code} — {desc}%N"));
            }
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
    // entirely, and `handle_disconnected` already emptied it when the previous
    // session ended, so stale caps are gone either way.
    if let Some(conn) = state.connections.get_mut(conn_id) {
        conn.reconnect_attempts = 0;
        conn.next_reconnect = None;
        conn.error = None;
        conn.isupport_parsed = crate::irc::isupport::Isupport::default();
        conn.silent_who_channels.clear();
        conn.silent_banlist_channels.clear();
        // Fresh connection ⇒ fresh chathistory state. Clears any request left
        // in-flight by a mid-request disconnect (which would otherwise block
        // future CHATHISTORY for that target) and re-evaluates exhaustion.
        conn.chathistory = crate::irc::chathistory::HistoryState::new();
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
///
/// Channel nicklists are wiped here so they don't survive into a reconnect:
/// when the server's auto-JOIN replays after reconnection, we receive a fresh
/// `RPL_NAMREPLY` that rebuilds the list. Without this wipe, departed users
/// from the previous session linger in `Buffer.users` forever (mirrors
/// weechat's `irc_server.c:irc_nick_free_all` per-channel disconnect cleanup).
pub fn handle_disconnected(state: &mut AppState, conn_id: &str, error: Option<&str>) {
    // Save channel names AND wipe nicklists in one pass — channels survive the
    // disconnect (we want to reuse the buffer + history on rejoin), but their
    // user state is no longer authoritative.
    let mut current_channels: Vec<String> = Vec::new();
    for buf in state.buffers.values_mut() {
        if buf.connection_id == conn_id
            && buf.buffer_type == crate::state::buffer::BufferType::Channel
        {
            current_channels.push(buf.name.clone());
            buf.users.clear();
            buf.last_speakers.clear();
        }
    }

    if let Some(err) = error {
        if let Some(conn) = state.connections.get_mut(conn_id) {
            conn.status = ConnectionStatus::Error;
            conn.error = Some(err.to_string());
        }
    } else {
        state.update_connection_status(conn_id, ConnectionStatus::Disconnected);
    }

    // Nobody is typing over a socket that is gone. No `done` can ever arrive, so
    // without this the indicators sit there for the full TTL (30s after a
    // `paused`) and survive into the reconnect.
    for buf_id in state.typing.clear_connection(conn_id) {
        push_typing_web_event(state, &buf_id);
    }

    // Store joined channels and set up reconnect schedule
    if let Some(conn) = state.connections.get_mut(conn_id) {
        if !current_channels.is_empty() {
            conn.joined_channels = current_channels;
        }
        // The negotiated caps and ISUPPORT belong to the session that just
        // ended; both are only ever repopulated at RPL_WELCOME. The IRC handle,
        // however, comes back at `HandleReady` — as soon as the socket is up,
        // before CAP negotiation — so anything consulting them in between (the
        // `+typing` gate, on every tick) would be answering with the previous
        // server's rules. Drop them with the connection.
        conn.enabled_caps.clear();
        conn.isupport_parsed = crate::irc::isupport::Isupport::default();
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

    // Capture the `draft/multiline` cap VALUE (max-bytes/max-lines) before the
    // immutable `enabled` borrow below; the value-stripping `new_caps` map above
    // discards it. Stored after the `to_request` collect (borrow ordering).
    let multiline_limits = caps_str.split_whitespace().find_map(|tok| {
        let (name, value) = tok.split_once('=').map_or((tok, None), |(n, v)| (n, Some(v)));
        if name.eq_ignore_ascii_case("draft/multiline") {
            Some(crate::irc::multiline::parse_limits(value))
        } else {
            None
        }
    });

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

    // Store the multiline limits (the immutable `enabled` borrow has ended).
    // `Some(inner)` = draft/multiline was advertised; `inner` (Option) is the
    // usable limits or None when the advertised value is unusable.
    if let Some(limits) = multiline_limits
        && let Some(conn) = state.connections.get_mut(conn_id)
    {
        conn.multiline = limits;
    }

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
        if removed_caps.iter().any(|c| c == "draft/multiline") {
            conn.multiline = None;
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

    // A NAK for draft/multiline (e.g. requested after a CAP NEW) means the
    // server will not enable it — drop any limits we optimistically stored.
    if naked_caps.iter().any(|c| c == "draft/multiline")
        && let Some(conn) = state.connections.get_mut(conn_id)
    {
        conn.multiline = None;
    }

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

/// Decrypt a CHATHISTORY conversational line's text for storage, mirroring the
/// live PRIVMSG E2E path. Returns the text to store, or `None` to skip the line.
///
/// A non-E2E line passes through unchanged. An `+RPE2E01…` line is decrypted via
/// the same `decrypt_incoming` the live path uses (without firing a KEYREQ — we
/// don't chase sessions for backlog). A ciphertext we can't decrypt (no session,
/// our own echo, or a decrypt error) is skipped rather than stored: persisting
/// it would show gibberish on scroll-up AND let it win the `@msgid` dedup over a
/// later live plaintext row.
fn decrypt_chathistory_text(
    state: &AppState,
    network: &str,
    target: &str,
    sender_handle: &str,
    own_handle: Option<&str>,
    is_own: bool,
    raw_text: &str,
) -> Option<String> {
    if !raw_text.starts_with("+RPE2E01") {
        return Some(raw_text.to_string());
    }
    if is_own {
        return None;
    }
    // A DM whose own handle isn't known yet has no recipient context — skip
    // the line rather than decrypt under the sender (it would fail anyway).
    let context = incoming_e2e_context(network, target, own_handle)?;
    match state
        .e2e_manager
        .as_ref()?
        .decrypt_incoming(sender_handle, &context, raw_text)
    {
        Ok(crate::e2e::manager::DecryptOutcome::Plaintext(plain)) => Some(plain),
        _ => None,
    }
}

/// Result of [`ingest_chathistory_batch`].
pub struct IngestOutcome {
    /// Oldest `(unix_millis, msgid?)` anchor seen across the batch (event lines
    /// included), or `None` if no line carried a `@time` tag. The caller
    /// advances its per-target `BEFORE` anchor to this.
    pub oldest: Option<(i64, Option<String>)>,
    /// `(buffer_id, Message)` rows to splice into a live buffer; empty unless
    /// display collection was requested.
    pub display_rows: Vec<(String, Message)>,
    /// Count of conversational rows actually persisted to `SQLite` (PRIVMSG /
    /// NOTICE / ACTION). Skipped lines (event-playback, undecryptable ciphertext,
    /// non-ACTION CTCP) are excluded. The caller re-opens a buffer's
    /// `history_exhausted` flag only when this is non-zero — a batch that stored
    /// nothing has nothing for pagination to surface.
    pub ingested: usize,
    /// Oldest server-time (unix **millis**) among the rows actually persisted, or
    /// `None` if none were. Unlike `oldest` (all lines, for the next anchor) this
    /// tracks only rows that will surface via pagination, so the caller can settle
    /// scroll-back exhaustion once the buffer has displayed down to it — a
    /// skipped-only batch leaves it `None` and the buffer settles immediately.
    pub oldest_ingested_ms: Option<i64>,
    /// Newest server-time (unix **millis**) across all batch lines plus that
    /// line's `@msgid` (if any), or `None` if none carried a `@time` tag. Used as
    /// the next `AFTER` anchor when a reconnect gap-fill page comes back full (the
    /// gap is larger than one page) — by msgid when the server supports it.
    pub newest: Option<(i64, Option<String>)>,
}

/// Ingest a completed `draft/chathistory` batch into the log store.
///
/// chathistory is a background backlog filler: conversational lines
/// (PRIVMSG / NOTICE, including CTCP ACTION) are persisted **store-only** via
/// [`AppState::ingest_history_message`] — no live display, no nicklist/topic
/// mutation, no highlights or notifications. The UI surfaces these rows later
/// through normal `SQLite` pagination, and the unique `msg_id` index
/// deduplicates against messages already stored from the live stream.
///
/// Returns an [`IngestOutcome`]. `oldest` is the oldest server-time (unix
/// **millis**) seen across **all** batch lines (conversational and
/// event-playback) paired with that line's `@msgid`, or `None` if none carried
/// a `@time` tag; the caller advances its per-target `BEFORE` anchor to this so
/// scroll-up keeps making progress even through windows that contain only
/// (un-ingested) event-playback lines.
///
/// `display_rows` is `(buffer_id, Message)` for each ingested conversational
/// line, populated only when `collect_display` is set. The caller uses it to
/// splice a reconnect `AFTER`/`LATEST` gap-fill into the live buffer (those
/// rows fall between the pre-disconnect tail and post-reconnect live messages,
/// so scroll-up pagination — which only fetches OLDER rows — would never reach
/// them). For `BEFORE` scroll-back the rows surface through normal pagination,
/// so collection is skipped to avoid cloning whole pages.
///
/// `ingested` counts the conversational rows actually persisted (skipped lines
/// excluded), so the caller can tell a productive batch from one that stored
/// nothing.
///
/// v1 scope: `draft/event-playback` lines (JOIN/PART/QUIT/NICK/TOPIC/MODE) and
/// non-ACTION CTCP are skipped rather than rendered into stored event rows.
/// See `docs/superpowers/specs/2026-06-19-draft-chathistory-design.md`.
#[expect(clippy::too_many_lines, reason = "linear chathistory ingest loop")]
pub fn ingest_chathistory_batch(
    state: &AppState,
    conn_id: &str,
    batch: &crate::irc::batch::BatchInfo,
    collect_display: bool,
) -> IngestOutcome {
    let our_nick = state
        .connections
        .get(conn_id)
        .map(|c| c.nick.clone())
        .unwrap_or_default();
    let network = state
        .connections
        .get(conn_id)
        .map(|c| c.label.clone())
        .unwrap_or_default();

    let mut ingested = 0usize;
    let mut oldest_ingested_ms: Option<i64> = None;
    let mut skipped = 0usize;
    let mut display_rows: Vec<(String, Message)> = Vec::new();
    // Oldest line seen across the whole batch (events included): full
    // millisecond server-time plus its IRC @msgid (if any). The caller uses
    // this as the next BEFORE anchor — by msgid when the server prefers it,
    // else by the full-precision timestamp — so scroll-up never floors to the
    // second (skipping same-second messages) or stalls on event-only windows.
    let mut oldest: Option<(i64, Option<String>)> = None;
    // Newest line seen across the whole batch (events included): full
    // millisecond server-time plus its IRC @msgid. The next `AFTER` anchor when a
    // reconnect gap-fill page comes back full and the gap spans more than one
    // page — by msgid (no same-millisecond skips) when the server prefers it.
    let mut newest: Option<(i64, Option<String>)> = None;

    // Persist in chronological order. The DB stores only whole-second
    // timestamps and breaks same-second ties by insertion id, so a page the
    // server returned newest-first would otherwise reload in reverse order
    // within that second. Sort by full-precision `@time` (stable, so lines that
    // share — or lack — a timestamp keep their server order); untimed lines sort
    // last. Ordering only matters for storage/`oldest`; both are order-robust.
    let mut ordered: Vec<&IrcMessage> = batch.messages.iter().collect();
    ordered.sort_by_key(|m| {
        extract_tags(m)
            .as_ref()
            .and_then(|t| t.get("time"))
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map_or(i64::MAX, |dt| dt.timestamp_millis())
    });

    // Our own handle is constant for the batch; resolve it once for the
    // recipient-keyed DM decrypt context.
    let own_handle = state
        .connections
        .get(conn_id)
        .and_then(|c| c.own_handle.clone());

    for msg in ordered {
        let tags = extract_tags(msg);

        if let Some(ts) = tags
            .as_ref()
            .and_then(|t| t.get("time"))
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        {
            let ms = ts.timestamp_millis();
            let msgid = tags.as_ref().and_then(|t| t.get("msgid").cloned());
            if newest.as_ref().is_none_or(|(cur, _)| ms > *cur) {
                newest = Some((ms, msgid.clone()));
            }
            if oldest.as_ref().is_none_or(|(cur, _)| ms < *cur) {
                oldest = Some((ms, msgid));
            }
        }

        let (base_type, target, raw_text) = match &msg.command {
            Command::PRIVMSG(target, text) => (MessageType::Message, target, text),
            Command::NOTICE(target, text) => (MessageType::Notice, target, text),
            // Event-playback and other commands are not ingested in v1.
            _ => {
                skipped += 1;
                continue;
            }
        };

        let (nick, ident, host) = extract_nick_userhost(msg.prefix.as_ref());
        // IRC nicks are case-insensitive: CHATHISTORY playback may echo our own
        // nick in a different case than Connection.nick. A case-sensitive miss here
        // would treat our own E2E ciphertext echo as a peer's (fail decryption →
        // silently drop the row) and route our own PM to a buffer named after
        // ourselves instead of the peer. Match add_message's comparison.
        let is_own = nick.eq_ignore_ascii_case(&our_nick);

        // Decrypt RPE2E ciphertext (same path as live PRIVMSGs) before storing,
        // or skip a line we can't decrypt — see `decrypt_chathistory_text`.
        let Some(text) = decrypt_chathistory_text(
            state,
            &network,
            target,
            &format!("{ident}@{host}"),
            own_handle.as_deref(),
            is_own,
            raw_text,
        ) else {
            skipped += 1;
            continue;
        };
        let text = text.as_str();

        // CTCP ACTION becomes an Action; any other CTCP is skipped in history.
        let is_ctcp = text.starts_with('\u{1}') && text.ends_with('\u{1}');
        let is_action =
            is_ctcp && text.len() > 2 && text[1..text.len() - 1].starts_with("ACTION ");
        if is_ctcp && !is_action {
            skipped += 1;
            continue;
        }
        let (msg_type, display_text) = if is_action {
            let inner = &text[1..text.len() - 1];
            (MessageType::Action, inner["ACTION ".len()..].to_string())
        } else {
            (base_type, text.to_string())
        };

        // Channel messages route to the channel buffer; PMs route to the
        // peer's nick buffer (or our own target for echoed history).
        let buffer_name = if is_channel(target) || is_own {
            target.as_str()
        } else {
            nick.as_str()
        };
        let buffer_id = make_buffer_id(conn_id, buffer_name);

        let timestamp = message_timestamp(tags.as_ref());

        let message = Message {
            id: 0, // store-only: real id assigned if/when spliced into a buffer
            timestamp,
            message_type: msg_type,
            nick: Some(nick),
            nick_mode: None,
            text: display_text,
            highlight: false,
            event_key: None,
            event_params: None,
            // The DB row is keyed by the @msgid carried in `tags` (see
            // `maybe_log`); leaving `log_msg_id` None keeps a spliced display
            // copy from being mistaken for a SQLite-row-id pagination cursor.
            log_msg_id: None,
            log_ref_id: None,
            tags,
        };

        // Only count rows the storage layer actually queued. A row dropped by
        // maybe_log (a `log_exclude_types` type like message/notice/action, or a
        // full log queue) never reaches SQLite, so counting it would advance the
        // `oldest_ingested` watermark and clear `history_exhausted` for rows that
        // never paginate — making BEFORE scroll-up re-request server history while
        // the visible backlog never grows.
        let stored = state.ingest_history_message(&buffer_id, &message);
        if stored {
            ingested += 1;
            let ingested_ms = message.timestamp.timestamp_millis();
            if oldest_ingested_ms.is_none_or(|cur| ingested_ms < cur) {
                oldest_ingested_ms = Some(ingested_ms);
            }
        }
        if collect_display {
            display_rows.push((buffer_id, message));
        }
    }

    if skipped > 0 {
        tracing::debug!(
            conn_id,
            ingested,
            skipped,
            "chathistory: ingested conversational messages (non-conversational skipped in v1)"
        );
    }

    IngestOutcome {
        oldest,
        display_rows,
        ingested,
        oldest_ingested_ms,
        newest,
    }
}

// === Private handlers ===

/// Record our own server-stamped `ident@host` for a connection, de-duplicated.
/// The per-source "is this us?" gate stays at each call site (echo-message,
/// self-USERHOST, own CHGHOST); only the store is centralized so the capture
/// paths stay consistent. Drives the recipient-keyed DM E2E context.
fn set_own_handle(state: &mut AppState, conn_id: &str, handle: String) {
    if let Some(conn) = state.connections.get_mut(conn_id)
        && conn.own_handle.as_deref() != Some(handle.as_str())
    {
        conn.own_handle = Some(handle);
    }
}

/// Recipient-keyed E2E context for an INCOMING message. A channel uses its
/// name verbatim. A DM (target is our own nick) is keyed by OUR own handle —
/// we are the recipient, so this matches the sender's AAD context (the sender
/// encrypted under `@<our_handle>`). Returns `None` for a DM when our own
/// handle isn't known yet: the caller must NOT fall back to the sender handle,
/// because decrypting/KEYREQ-ing under `@<sender>` negotiates the wrong DM
/// direction — it must wait until our handle is learned. Channels never
/// return `None`.
/// The returned context is scoped to `network` for keyring storage (see
/// `e2e::scoped_context`) — the wire/AAD part is recovered inside the
/// manager, so this changes nothing on the wire.
fn incoming_e2e_context(network: &str, target: &str, own_handle: Option<&str>) -> Option<String> {
    if is_channel(target) {
        Some(crate::e2e::scoped_context(network, target))
    } else {
        own_handle.map(|h| crate::e2e::scoped_context(network, &crate::e2e::context_key(target, h)))
    }
}

/// Handle an inbound `TAGMSG`. Only the `+typing` client tag is understood.
///
/// This function must never append a buffer line, bump activity or unread
/// counts, persist anything, or create a buffer. The message-tags spec is
/// explicit: "Clients that receive a `TAGMSG` command MUST NOT display them in
/// the message history by default."
fn handle_tagmsg(
    state: &mut AppState,
    conn_id: &str,
    our_nick: &str,
    msg: &IrcMessage,
    target: &str,
    tags: Option<&HashMap<String, String>>,
) {
    // 1. History replay (`draft/chathistory` / `draft/event-playback`) can hand
    //    us a TAGMSG from hours ago. Typing is a live-only signal (spec §3.3).
    if tags.is_some_and(|t| t.contains_key("batch")) {
        return;
    }
    // (Script suppression is NOT checked here. A TAGMSG arrives as `Command::Raw`,
    //  which is not in `state_mutating` (`src/app/irc.rs`), so a script that
    //  suppresses it returns from the dispatcher before `handle_irc_message` is
    //  ever called — `state.suppress_event_display` is only ever set for the
    //  state-mutating commands, and would always read `false` here.)
    // 2. The user asked not to see typing. This gates INGESTION, not rendering:
    //    tracking it anyway would keep feeding the web clients (spec §5).
    if !state.typing_show {
        return;
    }
    // 3. No typing tag: nothing else is implemented.
    let Some(typing_state) = tags.and_then(crate::irc::typing::parse_typing) else {
        return;
    };

    let (nick, ident, host) = extract_nick_userhost(msg.prefix.as_ref());
    if nick.is_empty() {
        return;
    }
    // 4. echo-message reflects our own TAGMSG back at us (spec §3.2).
    if nick.eq_ignore_ascii_case(our_nick) {
        return;
    }

    // Resolve the target. `&chan` and `+chan` are CHANNELS, so only a character
    // the server actually advertised in STATUSMSG may be stripped, and only when
    // what remains is still a channel.
    let statusmsg = state
        .connections
        .get(conn_id)
        .map(|c| c.isupport_parsed.statusmsg().to_string())
        .unwrap_or_default();
    let target = crate::irc::typing::strip_statusmsg(target, &statusmsg);
    let target_is_channel = is_channel(target);

    // 5. Ignore list.
    let ignore_level = if target_is_channel {
        IgnoreLevel::Public
    } else {
        IgnoreLevel::Msgs
    };
    let channel = target_is_channel.then_some(target);
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

    // A channel TAGMSG belongs to the channel buffer; a TAGMSG aimed at us
    // belongs to the sender's query buffer — same rule as PRIVMSG.
    let buffer_name = if target_is_channel { target } else { &nick };
    let buffer_id = make_buffer_id(conn_id, buffer_name);

    // 6. Typing never creates a buffer: otherwise any stranger could pop a query
    //    window open on your screen without ever sending a message.
    if !state.buffers.contains_key(&buffer_id) {
        return;
    }

    if state.typing.set(&buffer_id, &nick, typing_state, Instant::now()) {
        push_typing_web_event(state, &buffer_id);
    }
}

/// Enqueue the current typing set for a buffer to the web clients.
/// The full set is sent, not a delta — idempotent and self-healing.
pub fn push_typing_web_event(state: &mut AppState, buffer_id: &str) {
    let nicks = state
        .typing
        .nicks(buffer_id)
        .into_iter()
        .map(ToString::to_string)
        .collect();
    state
        .pending_web_events
        .push(crate::web::protocol::WebEvent::Typing {
            buffer_id: buffer_id.to_string(),
            nicks,
        });
}

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
    // IRC nicks are case-insensitive: an echo-message echo may carry our nick
    // in a different case. Match case-insensitively (as ingest_chathistory_batch
    // does) so we still recognise our own echo — capture our handle from it and
    // drop our own ciphertext echo rather than treating it as a peer's.
    let is_own = nick.eq_ignore_ascii_case(our_nick);
    let sender_handle = format!("{ident}@{host}");
    // echo-message: the server echoes our own outgoing PRIVMSG back with our
    // full server-stamped prefix. Capture it as our own handle — it drives
    // the recipient-keyed DM E2E context and tracks vhost changes live.
    if is_own && !ident.is_empty() && !host.is_empty() {
        set_own_handle(state, conn_id, sender_handle.clone());
    }
    let flood_exempt =
        !is_own && matches_mask_patterns(&state.flood_exemptions, &nick, Some(&ident), Some(&host));

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

    // E2E decrypt: if this looks like an RPE2E01 wire-format line, swap
    // `text` for the plaintext before any further processing. Strict handle
    // check uses the raw server-stamped `ident@host`, so attackers cannot
    // decrypt by spoofing a nick.
    //
    // Recipient-keyed (spec §6 + DM addendum): an incoming DM is keyed by OUR
    // own handle (we are the recipient), matching the sender's AAD; a channel
    // passes through unchanged. The auto-KEYREQ fired on a missing session
    // inherits this context, so its `c=` is our own handle too.
    let own_handle = state
        .connections
        .get(conn_id)
        .and_then(|c| c.own_handle.clone());
    // Set for lines that must NOT be logged under the server @msgid (the
    // two E2E placeholders and Err-arm rejections): a later CHATHISTORY
    // replay of the same wire line may decrypt for real, and a logged row
    // would win the unique (network, msg_id) index forever. The flag comes
    // out-of-band from try_decrypt_e2e — classifying by text prefix would
    // misfile a peer's legitimate message that happens to START with the
    // placeholder text.
    let mut e2e_transient_line = false;
    let e2e_network = state
        .connections
        .get(conn_id)
        .map(|c| c.label.clone())
        .unwrap_or_default();
    let decrypted_owned = match incoming_e2e_context(&e2e_network, target, own_handle.as_deref()) {
        Some(decrypt_context) => try_decrypt_e2e(
            state,
            conn_id,
            &nick,
            &sender_handle,
            &decrypt_context,
            text,
            is_own,
        )
        .map(|(decrypted, transient)| {
            e2e_transient_line = transient;
            decrypted
        }),
        // DM whose own handle isn't known yet: do NOT decrypt or fire a KEYREQ
        // under the sender's handle (that negotiates the wrong DM direction).
        // Show a placeholder and wait — the self-USERHOST at RPL_WELCOME, an
        // echo, or CHGHOST will set our handle and the peer re-handshakes on
        // the next message. The handle-learned hook re-fetches this exact line
        // via CHATHISTORY and decrypts it, so the placeholder MUST stay
        // transient (non-logged): persisting it under the server @msgid would
        // block that decryptable replay on the unique (network, msg_id) index.
        // Non-E2E plaintext passes through untouched.
        None if !is_own && text.starts_with("+RPE2E01") => {
            e2e_transient_line = true;
            Some(crate::e2e::AWAITING_OWN_IDENTITY_PLACEHOLDER.to_string())
        }
        // Our OWN ciphertext echo while our handle is still unknown (a
        // nick-only prefix carries no ident@host to seed it): swallow, same
        // as try_decrypt_e2e's is_own branch — the plaintext was already
        // echoed locally at send time. Falling through would render and log
        // the raw +RPE2E01 wire line as a normal message.
        None if is_own && text.starts_with("+RPE2E01") => Some(String::new()),
        None => None,
    };
    // An empty return from `try_decrypt_e2e` means "own echo-message
    // echo of our own encrypted PRIVMSG — already rendered locally by
    // `handle_plain_message`, drop the echo entirely". This path only
    // triggers when `is_own` is true and the wire is `+RPE2E01…`.
    if decrypted_owned.as_deref() == Some("") {
        return;
    }
    let text: &str = decrypted_owned.as_deref().unwrap_or(text);

    // Check if this is a CTCP (ACTION or other)
    let is_ctcp = text.starts_with('\x01') && text.ends_with('\x01');
    let is_action = is_ctcp && text.len() > 2 && text[1..text.len() - 1].starts_with("ACTION ");

    // A message from this nick means they are no longer typing (spec §1.2).
    //
    // BEFORE the ignore check, like PART/KICK/QUIT/NICK: ignore suppresses the
    // notification, not the state change. The levels do not even line up — a
    // TAGMSG is gated at `Public`/`Msgs`, but this handler returns early on
    // `Actions` and `Ctcps` too, so `/ignore alice ACTIONS` would let her typing
    // in and then drop the very `/me` that retracts it, leaving the indicator up
    // for the full TTL.
    if state.typing.clear(&buffer_id, &nick) {
        push_typing_web_event(state, &buffer_id);
    }

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

    // Create query buffer if it doesn't exist for PMs. When we create or
    // find a Query buffer we also stamp `peer_handle` with the raw
    // `ident@host` from the prefix — the E2E layer uses this to key PM
    // session rows under `@<peer_handle>` instead of bare nick (spec §6).
    let buffer_created = !target_is_channel && !state.buffers.contains_key(&buffer_id);
    if buffer_created {
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
            peer_handle: if is_own {
                None
            } else {
                Some(format!("{ident}@{host}"))
            },
            log_total_lines: None,
            log_oldest_ts: None,
            log_newest_ts: None,
            history_exhausted: false,
            log_initial_loaded: false,
            pin_backlog: false,
        });
    }

    // Keep the Query buffer's `peer_handle` in sync with the latest
    // server-stamped userhost. This matters when the peer reconnects from
    // a new host — later messages arrive with a different `ident@host`, and
    // the cached handle must track it so the encrypt path picks up the new
    // pseudochannel key. Never overwrite with `is_own` (echo-message) because
    // that carries our own host. On a CHANGE, migrate the DM E2E config to the
    // new handle (a reconnect/vhost is delivered here, not as CHGHOST) so the
    // next DM does not silently downgrade to plaintext under the new context.
    // An RPE2E handshake delivered as a PRIVMSG (fallback for servers that strip
    // CTCP framing from NOTICEs) is routed to `try_dispatch_rpe2e_ctcp` below,
    // which performs its own DM handle migration. Skip it here so the migration
    // (and its keyring cache write) does not run twice for the same message.
    let is_rpe2e_handshake = {
        let stripped = text.strip_prefix('\x01').unwrap_or(text);
        let stripped = stripped.strip_suffix('\x01').unwrap_or(stripped);
        stripped.starts_with(crate::e2e::handshake::CTCP_TAG)
    };
    if !target_is_channel && !is_own && !is_rpe2e_handshake {
        let new_handle = format!("{ident}@{host}");
        // `Some(old)` only when the handle is new for this buffer (old may be
        // `None`): a freshly-created buffer is already stamped with @<new>, so
        // we still migrate from the cached @<old> (prev = `None`); an existing
        // buffer migrates from its prior handle. `None` = no change → nothing
        // to migrate. The buffer's `peer_handle` moves to @<new> only AFTER
        // the observation succeeds: on a postponed observation (keyring read
        // fault) the buffer must keep — or, for a freshly-stamped buffer,
        // revert to — the previous value, so the encrypt path keeps keying
        // under the still-enabled old context instead of missing the config
        // under `@<new>` and passing plaintext through.
        let changed_from: Option<Option<String>> = if buffer_created {
            Some(None)
        } else if let Some(buf) = state.buffers.get(&buffer_id)
            && buf.peer_handle.as_deref() != Some(new_handle.as_str())
        {
            Some(buf.peer_handle.clone())
        } else {
            None
        };
        if let Some(prev) = changed_from {
            let observed = track_dm_handle_change(state, conn_id, &nick, prev.as_deref(), &new_handle);
            if let Some(buf) = state.buffers.get_mut(&buffer_id) {
                buf.peer_handle = if observed {
                    Some(new_handle.clone())
                } else {
                    prev
                };
            }
        }
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
            let ts = message_timestamp(tags.as_ref());
            // Save nick before moving into Message — needed for mentions buffer below.
            let nick_saved = if is_mention { Some(nick.clone()) } else { None };
            state.add_message_with_activity(
                &buffer_id,
                Message {
                    id,
                    timestamp: ts,
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

            // Push to mentions buffer — channel highlights only.
            if is_mention && target_is_channel && state.buffers.contains_key("_mentions") {
                let nick = nick_saved.unwrap_or_default();
                let conn_label = state
                    .connections
                    .get(conn_id)
                    .map_or(conn_id, |c| c.label.as_str());
                let datetime = ts
                    .with_timezone(&chrono::Local)
                    .format("%Y/%m/%d %H:%M:%S")
                    .to_string();
                let action_body = format!("* {nick} {action_text}");
                let mention_text = crate::ui::format_mention_line(
                    &datetime,
                    conn_label,
                    target,
                    &nick,
                    &action_body,
                    state.nick_color_sat,
                    state.nick_color_lit,
                );
                let mention_msg = Message {
                    id: state.next_message_id(),
                    timestamp: ts,
                    message_type: MessageType::MentionLog,
                    nick: None,
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
        if state.flood_protection && !flood_exempt {
            let now = Instant::now();
            let result = state.flood_state.check_ctcp_flood(now);
            if result.suppressed() {
                if result == crate::irc::flood::FloodResult::Triggered {
                    emit(state, &buffer_id, "CTCP flood detected — suppressing");
                }
                return;
            }
        }
        // RPE2E handshake CTCP dispatch. Some IRC servers strip trailing
        // CTCP framing from NOTICE; accepting RPEE2E in PRIVMSG as well
        // gives us a fallback path that still works on those servers.
        if try_dispatch_rpe2e_ctcp(state, conn_id, prefix, target, text)
            == Some(RpEe2eOutcome::Handled)
        {
            return;
        }
        // Non-ACTION CTCP, ignore for now
        return;
    }

    // RPE2E handshake WITHOUT full CTCP framing: some servers strip the
    // trailing (or even leading) \x01 from relayed CTCPs — the very case the
    // PRIVMSG fallback exists for — but `is_ctcp` above demands BOTH bytes,
    // so the dispatch inside that branch never sees the stripped form. The
    // generic peer-handle tracking earlier was also suppressed for every
    // handshake-looking text; without this branch a half-framed handshake
    // would neither negotiate nor update the DM handle, and would render as
    // a raw RPEE2E blob. The dispatcher itself strips framing leniently.
    if is_rpe2e_handshake
        && try_dispatch_rpe2e_ctcp(state, conn_id, prefix, target, text)
            == Some(RpEe2eOutcome::Handled)
    {
        return;
    }

    // --- Flood checks for regular messages ---
    if state.flood_protection && nick != our_nick && !flood_exempt {
        let now = Instant::now();

        if ident.starts_with('~') {
            // Per-nick tilde rate limit — blocks only the flooding nick
            let result = state.flood_state.check_tilde_nick_flood(&nick, now);
            if result.suppressed() {
                if result == crate::irc::flood::FloodResult::Triggered {
                    emit(
                        state,
                        &buffer_id,
                        &format!("Flood from {nick} detected — suppressing"),
                    );
                }
                return;
            }

            // PM tilde storm — many unique ~ nicks PMing us = botnet
            if !target_is_channel {
                let storm = state.flood_state.check_pm_tilde_storm(&nick, now);
                if storm.suppressed() {
                    if storm == crate::irc::flood::FloodResult::Triggered {
                        emit(
                            state,
                            &buffer_id,
                            "PM flood storm detected — suppressing all ~ PMs",
                        );
                    }
                    return;
                }
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
    let ts = message_timestamp(tags.as_ref());
    // Save nick before moving into Message — needed for mentions buffer below.
    let nick_saved = if is_mention { Some(nick.clone()) } else { None };
    let msg = Message {
        id,
        timestamp: ts,
        message_type: MessageType::Message,
        nick: Some(nick),
        nick_mode: mode_prefix.map(String::from),
        text: text.to_string(),
        highlight: is_mention,
        event_key: None,
        event_params: None,
        log_msg_id: None,
        log_ref_id: None,
        // E2E placeholders (awaiting-own-identity, awaiting-session) must
        // carry NO @msgid: keeping the server tags off means (a) the transient
        // line never occupies the (network, @msgid) storage row, and (b)
        // `buffer_contains_history_row` can't dedup the later decrypted
        // CHATHISTORY replay (same @msgid) against it — which would skip the
        // real message and leave the placeholder showing until restart. Real
        // messages keep their tags.
        tags: if e2e_transient_line { None } else { tags },
    };
    // Placeholders are delivered transiently (never logged) so they don't
    // persist; the decrypted replay is logged + surfaced under the real @msgid.
    if e2e_transient_line {
        state.add_transient_message_with_activity(&buffer_id, msg, activity);
    } else {
        state.add_message_with_activity(&buffer_id, msg, activity);
    }

    // Push to mentions buffer — channel highlights only (not PMs/queries).
    if is_mention && target_is_channel && state.buffers.contains_key("_mentions") {
        let nick = nick_saved.unwrap_or_default();
        let conn_label = state
            .connections
            .get(conn_id)
            .map_or(conn_id, |c| c.label.as_str());
        let datetime = ts
            .with_timezone(&chrono::Local)
            .format("%Y/%m/%d %H:%M:%S")
            .to_string();
        let mention_text = crate::ui::format_mention_line(
            &datetime,
            conn_label,
            target,
            &nick,
            text,
            state.nick_color_sat,
            state.nick_color_lit,
        );
        let mention_msg = Message {
            id: state.next_message_id(),
            timestamp: ts,
            message_type: MessageType::MentionLog,
            nick: None,
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

    // Resolved up here because the typing clear below needs it, and that has to
    // happen before the ignore check can return.
    //
    // echo-message: when the server echoes our own notice to a user, the target
    // is the recipient (e.g. "bob"). Route to that buffer, not ours.
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

    // A message from this nick means they are no longer typing (spec §1.2).
    //
    // BEFORE the ignore check, like PART/KICK/QUIT/NICK: ignore suppresses the
    // notification, not the state change. A TAGMSG is gated at `Public`/`Msgs`
    // while this handler returns early on `Notices`, so `/ignore alice NOTICES`
    // would otherwise let her typing in and then drop the notice that retracts
    // it, leaving the indicator up for the full TTL.
    if let Some(sender) = nick.as_deref()
        && state.typing.clear(&buffer_id, sender)
    {
        push_typing_web_event(state, &buffer_id);
    }

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

    // RPE2E handshake CTCP dispatch. Travels in NOTICE as
    // `\x01RPEE2E ... \x01`. We intercept before the NOTICE becomes a
    // user-visible buffer line so the raw CTCP never leaks into the UI.
    if try_dispatch_rpe2e_ctcp(state, conn_id, prefix, target, text) == Some(RpEe2eOutcome::Handled)
    {
        return;
    }

    // RPE2E never ships ciphertext in NOTICE (the frame is reserved for the
    // handshake), so a `+RPE2E01` notice is a buggy or malicious peer — there
    // is no decrypt path for it, and rendering it would put a raw wire line
    // in the buffer. Suppress it (E2E users only; for everyone else the
    // prefix is ordinary text).
    if state.e2e_manager.is_some() && text.starts_with("+RPE2E01") {
        tracing::warn!(
            target = %target,
            "suppressing RPE2E ciphertext delivered in a NOTICE (protocol violation)"
        );
        return;
    }

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

#[expect(clippy::too_many_lines)]
fn handle_join(
    state: &mut AppState,
    conn_id: &str,
    our_nick: &str,
    prefix: Option<&Prefix>,
    fields: &JoinFields<'_>,
    tags: Option<HashMap<String, String>>,
) {
    let (nick, ident, host) = extract_nick_userhost(prefix);
    let channel = fields.channel;
    let buffer_id = make_buffer_id(conn_id, channel);

    // extended-join: account from second JOIN arg ("*" means not logged in;
    // IRCnet 2.12 hardcodes "*" — no services on that network)
    let account = match fields.account {
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
    let realname = fields.realname.unwrap_or("");

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
        // Defense-in-depth nicklist reset (mirrors weechat irc-protocol.c:1755-1802):
        // a buffer that already has users means this is a duplicate self-JOIN
        // (ZNC bouncer replays JOIN without an intervening disconnect, /sajoin
        // when already on the channel) — skip it. Otherwise the buffer is
        // either fresh or was wiped by `handle_disconnected`; reset any stale
        // topic/modes/list-modes so the upcoming RPL_TOPIC / RPL_CHANNELMODEIS
        // / RPL_BANLIST replies repopulate from authoritative server state.
        let exists = state.buffers.contains_key(&buffer_id);
        let has_users = exists
            && state
                .buffers
                .get(&buffer_id)
                .is_some_and(|b| !b.users.is_empty());
        if exists && has_users {
            return;
        }
        if exists {
            if let Some(buf) = state.buffers.get_mut(&buffer_id) {
                buf.users.clear();
                buf.last_speakers.clear();
                buf.topic = None;
                buf.topic_set_by = None;
                buf.modes = None;
                buf.mode_params = None;
                buf.list_modes.clear();
            }
        } else {
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
                peer_handle: None,
                log_total_lines: None,
                log_oldest_ts: None,
                log_newest_ts: None,
                history_exhausted: false,
                log_initial_loaded: false,
                pin_backlog: false,
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
        state
            .pending_web_events
            .push(crate::web::protocol::WebEvent::NickEvent {
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
    // ircnet.com/extended-join: uid + ip. Pre-baked as one display param —
    // the theme engine is pure substitution (no conditionals), so theme-side
    // brackets would render a literal "[ ]" on every non-IRCnet join.
    // join_fields sets uid and ip together or not at all.
    let uid_display = match (fields.uid, fields.ip) {
        (Some(uid), Some(ip)) => format!("[{uid} {ip}]"),
        _ => String::new(),
    };

    let mut text = format!("{nick} ({ident}@{host}) has joined {channel}");
    for part in [&account_display, &realname_display, &uid_display] {
        if !part.is_empty() {
            let _ = write!(text, " {part}");
        }
    }

    let id = state.next_message_id();
    state.add_message(
        &buffer_id,
        Message {
            id,
            timestamp: message_timestamp(tags.as_ref()),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text,
            highlight: false,
            event_key: Some("join".to_string()),
            // $0=nick, $1=ident, $2=host, $3=channel, $4=account, $5=realname,
            // $6=uid+ip (ircnet.com/extended-join)
            event_params: Some(vec![
                nick,
                ident,
                host,
                channel.to_string(),
                account_display,
                realname_display,
                uid_display,
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
        .filter(|(_, buf)| buf.connection_id == conn_id && buf.users.contains_key(&nick_lower))
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
        state
            .pending_web_events
            .push(crate::web::protocol::WebEvent::NickEvent {
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

/// COPY an ENABLED DM E2E config from `old_ctx` to `new_ctx` when a peer's
/// handle changes, so the policy follows the peer to its new pseudochannel.
/// Without this the encrypt path keys the next DM under `@<new_handle>`, finds
/// no config, and downgrades to plaintext. Re-handshake re-establishes the
/// session keys under the new context.
///
/// The old config is intentionally LEFT enabled: the keyring's `last_handle`
/// (a TOFU field) must NOT be bumped on mere observation — doing so would make
/// a later handshake under the new handle classify as `Known` instead of
/// `HandleChanged`, bypassing the reverify gate. Leaving `@<old>` enabled means
/// the encrypt path's peer-not-spoken fallback (which resolves `@<old>` from
/// that untouched cache) still encrypts (and re-handshakes) rather than sending
/// plaintext. No-op unless the old config is enabled and the new context isn't
/// already enabled (don't clobber).
/// Returns `Ok(true)` when a config was actually copied to `new_ctx`,
/// `Ok(false)` when there was nothing to migrate (old context not enabled, or
/// `new_ctx` already enabled), and `Err` when a keyring read/write FAULTED. A
/// fault must NOT be conflated with "no config to migrate": on a read error the
/// enabled config may still be stored under `@<old>` and simply unreadable, and
/// on a write error the copy did not land. The caller propagates the fault as a
/// postponed observation so the peer keeps keying under the old (still
/// decryptable) context instead of moving to an unconfigured `@<new>` that would
/// send plaintext.
///
/// `cap_autotrust` is set when the OLD context was resolved via the legacy
/// network-agnostic nick fallback (`legacy_handle_for_nick`): the enabled
/// config may belong to a same-nick peer on a DIFFERENT network, so an
/// `AutoAccept` mode must not carry across — it is capped to `Normal` so a
/// handshake from the (possibly unrelated) peer still prompts the user
/// instead of being silently trusted.
#[allow(
    clippy::redundant_pub_crate,
    reason = "exercised by the e2e integration_tests migration test"
)]
pub(crate) fn migrate_dm_e2e_config(
    mgr: &crate::e2e::E2eManager,
    old_ctx: &str,
    new_ctx: &str,
    cap_autotrust: bool,
) -> crate::e2e::error::Result<bool> {
    use crate::e2e::keyring::{ChannelConfig, ChannelMode};
    let Some(old_cfg) = mgr.keyring().get_channel_config(old_ctx)? else {
        return Ok(false);
    };
    if !old_cfg.enabled {
        return Ok(false);
    }
    if mgr
        .keyring()
        .get_channel_config(new_ctx)?
        .is_some_and(|c| c.enabled)
    {
        return Ok(false);
    }
    let mode = if cap_autotrust && old_cfg.mode == ChannelMode::AutoAccept {
        ChannelMode::Normal
    } else {
        old_cfg.mode
    };
    mgr.keyring().set_channel_config(&ChannelConfig {
        channel: new_ctx.to_string(),
        enabled: true,
        mode,
    })?;
    Ok(true)
}

/// Observe that DM peer `nick`'s current handle is `new_handle` (learned from a
/// PRIVMSG prefix or CHGHOST). Migrate an enabled DM E2E config from any
/// previous context — the buffer's prior handle and/or the keyring's cached
/// handle — to `@<new_handle>`, then refresh the keyring's last-handle cache so
/// the encrypt path keys the next DM under `@<new_handle>` and finds the
/// migrated config. Without this a handle change (reconnect / vhost, delivered
/// as a new PRIVMSG prefix or CHGHOST) silently downgrades the DM to plaintext.
/// No-op when the connection's network label is unknown.
///
/// Returns `false` when the observation was POSTPONED because a keyring
/// operation FAULTED — the cached-handle read, the config migration, or the
/// handle-cache write. A fault is NOT "no previous handle" / "nothing to
/// migrate": migrating (or moving the buffer) past it could strand the enabled
/// config under `@<old>` while the next send keys an unconfigured `@<new>` —
/// plaintext. Callers must then leave the buffer's `peer_handle` untouched too,
/// so the encrypt path keeps keying under the old (still decryptable) context
/// until a later sighting retries. `true` in every other case, including the
/// E2E-not-active no-ops.
fn track_dm_handle_change(
    state: &mut AppState,
    conn_id: &str,
    nick: &str,
    prev_buffer_handle: Option<&str>,
    new_handle: &str,
) -> bool {
    let Some(network) = state.connections.get(conn_id).map(|c| c.label.clone()) else {
        return true;
    };
    let Some(mgr) = state.e2e_manager.clone() else {
        return true;
    };
    let new_ctx =
        crate::e2e::scoped_context(&network, &crate::e2e::context_key(nick, new_handle));
    // Old-context candidates, in decreasing trust: the buffer's prior handle
    // (observed on THIS connection) and the keyring's network-scoped cache
    // row. Only when BOTH are absent (pre-cache keyring, upgrade path) fall
    // back to the legacy `e2e_peers` nick match — that lookup is
    // network-AGNOSTIC, so its result may belong to a same-nick peer on a
    // different network and is flagged (`from_legacy`) so the migration caps
    // AutoAccept and the enable is surfaced to the user rather than silent.
    let cached = match mgr.keyring().cached_dm_handle(nick, &network) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "e2e: cached handle read failed for {nick} on {network}: {e}; \
                 postponing the handle-change observation"
            );
            return false;
        }
    };
    let mut sources: Vec<(String, bool)> = Vec::new();
    if let Some(h) = prev_buffer_handle
        && h != new_handle
    {
        sources.push((h.to_string(), false));
    }
    if let Some(c) = cached.clone() {
        if c != new_handle && !sources.iter().any(|(s, _)| *s == c) {
            sources.push((c, false));
        }
    } else if sources.is_empty() {
        // Legacy network-agnostic nick fallback. A read fault here is NOT "no
        // legacy handle": the enabled config may live under an @<old> context
        // reachable only via this lookup, so swallowing the error and skipping
        // migration would strand it while the buffer moves to @<new> — the same
        // plaintext downgrade the cached-read and cache-write siblings above
        // postpone on. Fail closed: postpone and retry on the next sighting.
        match mgr.keyring().legacy_handle_for_nick(nick) {
            Ok(Some(legacy)) if legacy != new_handle => sources.push((legacy, true)),
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    "e2e: legacy handle read failed for {nick}: {e}; \
                     postponing the handle-change observation"
                );
                return false;
            }
        }
    }
    for (old_h, from_legacy) in sources {
        // Migrate from BOTH the scoped and the legacy-unscoped old context:
        // a pre-scoping database keeps its enabled flag under the bare
        // `@<handle>` row, and only the scoped-or-fallback read sees it.
        let old_ctx =
            crate::e2e::scoped_context(&network, &crate::e2e::context_key(nick, &old_h));
        let migrated = match migrate_dm_e2e_config(&mgr, &old_ctx, &new_ctx, from_legacy) {
            Ok(m) => m,
            Err(e) => {
                // A keyring fault is NOT "nothing to migrate": the enabled
                // config may be stranded (unreadable) under @<old>, or the copy
                // to @<new> may not have landed. Postpone so callers keep the
                // buffer on the old (still decryptable) context rather than
                // moving to an unconfigured @<new> that would send plaintext.
                tracing::warn!(
                    "e2e: DM config migration failed for {nick} on {network} \
                     ({old_ctx} -> {new_ctx}): {e}; postponing the handle-change observation"
                );
                return false;
            }
        };
        if migrated && from_legacy {
            // The legacy fallback matched by nick alone across ALL networks —
            // the enabled config may belong to someone else entirely. Never
            // enable E2E for a peer silently on that evidence: tell the user
            // so they can veto with /e2e off.
            let dm_buffer_id = make_buffer_id(conn_id, nick);
            emit_e2e_message(
                state,
                &dm_buffer_id,
                "e2e_warning",
                false,
                format!(
                    "E2E enabled for {nick} ({new_ctx}) — carried over from a \
                     pre-upgrade nick match ({old_ctx}); if this is a different \
                     {nick}, run /e2e off"
                ),
            );
        }
    }
    // Cache the observed handle under (network, nick), so the cached lookup
    // stays scoped per-network (a same-nick peer on another network keeps its
    // own handle). Skip the write when the row already holds this handle —
    // this runs per received handshake notice and per query-buffer creation.
    //
    // A write fault must fail CLOSED: the config was migrated to @<new>, but
    // the resolver used by a later `/msg <nick>` (after the query buffer is
    // closed) reads this cache to rebuild the context. If the write is lost the
    // cache still points at @<old> (or nothing), so moving the buffer's handle
    // to @<new> now would leave the reopened DM keying a context the cache can't
    // reproduce — a plaintext downgrade. Postpone instead; @<old> stays enabled
    // and decryptable, and the next sighting retries the cache write.
    if cached.as_deref() != Some(new_handle)
        && let Err(e) = mgr.keyring().cache_dm_handle(&network, nick, new_handle)
    {
        tracing::warn!(
            "e2e: DM handle cache write failed for {nick} on {network}: {e}; \
             postponing the handle-change observation"
        );
        return false;
    }
    // NB: we deliberately do NOT bump the keyring's `last_handle` here. That
    // field drives TOFU `HandleChanged` classification; updating it on mere
    // observation (before a signed handshake) would bypass the reverify gate.
    // The copied config under `@<new>` plus the still-enabled `@<old>` cover
    // both the live and cached encrypt contexts without it.
    true
}

/// Observe a DM peer's current server-stamped handle for `(conn_id, nick)` —
/// learned from a PRIVMSG prefix or an RPE2E handshake — deriving the previous
/// handle from the open query buffer. Migrates the DM E2E config + refreshes the
/// network-scoped handle cache via [`track_dm_handle_change`], then refreshes the
/// query buffer's `peer_handle` so the encrypt path stops keying outgoing DMs
/// under a stale handle. No-op when the connection's network label is unknown.
/// The buffer mutation only fires on an actual change; caching is unconditional
/// (it also covers channel handshakes).
fn observe_dm_peer_handle(state: &mut AppState, conn_id: &str, nick: &str, new_handle: &str) {
    let dm_buffer_id = make_buffer_id(conn_id, nick);
    let prev_buffer_handle = state
        .buffers
        .get(&dm_buffer_id)
        .filter(|b| b.buffer_type == BufferType::Query)
        .and_then(|b| b.peer_handle.clone());
    // A postponed observation (keyring read fault) must not move the
    // buffer's handle either — see `track_dm_handle_change`.
    if track_dm_handle_change(state, conn_id, nick, prev_buffer_handle.as_deref(), new_handle)
        && prev_buffer_handle.as_deref() != Some(new_handle)
        && let Some(buf) = state.buffers.get_mut(&dm_buffer_id)
        && buf.buffer_type == BufferType::Query
    {
        buf.peer_handle = Some(new_handle.to_string());
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
    // The CHGHOST prefix carries the peer's OLD ident@host
    // (`nick!olduser@oldhost`), which we need to migrate the DM E2E config
    // even when no query buffer is open.
    let (nick, old_ident, old_host) = extract_nick_userhost(prefix);
    if nick.is_empty() {
        return;
    }

    let nick_lower = nick.to_lowercase();
    let new_handle = format!("{new_user}@{new_host}");

    // Our own CHGHOST: keep our tracked handle current (vhost from services,
    // oper, etc.) so the recipient-keyed DM context follows. Own-handle flows
    // exclusively through `set_own_handle`; the peer-handle cache + DM-config
    // migration below must NOT run for our own nick (it would cache our handle
    // under our own nick in the PEER cache and drive a spurious peer migration).
    let is_own = state
        .connections
        .get(conn_id)
        .is_some_and(|c| c.nick.eq_ignore_ascii_case(&nick));
    if is_own {
        set_own_handle(state, conn_id, new_handle.clone());
    }

    // Migrate the DM E2E config + refresh the handle cache to the new handle —
    // INDEPENDENT of whether the peer's query buffer is open — using the OLD
    // handle carried in the CHGHOST prefix. Without this a vhost change
    // downgrades the next DM to plaintext. Never for our own nick (see above).
    // Runs BEFORE the buffer updates: a postponed observation (keyring read
    // fault) must leave the query buffer's peer_handle on the old (still
    // decryptable) context too — see `track_dm_handle_change`.
    let prev_handle = if old_ident.is_empty() || old_host.is_empty() {
        None
    } else {
        Some(format!("{old_ident}@{old_host}"))
    };
    let observed = is_own
        || track_dm_handle_change(state, conn_id, &nick, prev_handle.as_deref(), &new_handle);

    // Update ident/host + the cached DM peer_handle in shared buffers.
    for buf in state.buffers.values_mut() {
        if buf.connection_id != conn_id {
            continue;
        }
        if let Some(entry) = buf.users.get_mut(&nick_lower) {
            entry.ident = Some(new_user.to_string());
            entry.host = Some(new_host.to_string());
        }
        // A DM/query buffer caches the peer's handle outside the nicklist; it
        // drives the E2E encrypt context, so it must track the new host. Skip
        // for our own nick — a query named after ourselves is not a peer DM.
        if !is_own
            && observed
            && buf.buffer_type == BufferType::Query
            && buf.name.eq_ignore_ascii_case(&nick)
        {
            buf.peer_handle = Some(new_handle.clone());
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
            remove_case_insensitive(&mut conn.silent_who_channels, channel);
            remove_case_insensitive(&mut conn.silent_banlist_channels, channel);
        }
    } else {
        // Always update nick list regardless of ignore
        state.remove_nick(&buffer_id, &nick);
        state
            .pending_web_events
            .push(crate::web::protocol::WebEvent::NickEvent {
                buffer_id: buffer_id.clone(),
                kind: crate::web::protocol::NickEventKind::Part,
                nick: nick.clone(),
                new_nick: None,
                prefix: None,
                modes: None,
                away: None,
                message: reason.map(ToString::to_string),
            });

        // A parting user is no longer typing — regardless of ignore (spec §1.2).
        if state.typing.clear(&buffer_id, &nick) {
            push_typing_web_event(state, &buffer_id);
        }

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
        state
            .pending_web_events
            .push(crate::web::protocol::WebEvent::NickEvent {
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

    // A quitter is no longer typing anywhere on this connection, regardless
    // of ignore/netsplit (spec §1.2).
    for buf_id in state.typing.clear_nick_on_connection(conn_id, &nick) {
        push_typing_web_event(state, &buf_id);
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
#[expect(
    clippy::too_many_lines,
    reason = "nick change fan-out + web NickEvent broadcasting"
)]
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
        state
            .pending_web_events
            .push(crate::web::protocol::WebEvent::ConnectionStatus {
                conn_id: conn_id.to_string(),
                label: conn.label.clone(),
                connected: conn.status == crate::state::connection::ConnectionStatus::Connected,
                nick: new_nick.to_string(),
            });
    }

    // A DM peer's cached `ident@host` is keyed by (network, nick) — carry
    // it across the rename BEFORE any early return below (an ignored nick
    // still changes nick). Otherwise a later `/msg <new_nick>` with no live
    // query buffer resolves no handle, misses the peer's enabled
    // `@<handle>` config, and downgrades to plaintext — the exact fail-open
    // the send gate exists to prevent. Never for our OWN rename: the cache
    // holds PEER handles, and a stale row under our old nick (a previous
    // holder) would get mislabeled as the new nick's peer.
    if !old_nick.is_empty()
        && old_nick != our_nick
        && let Some(mgr) = state.e2e_manager.as_ref()
        && let Some(conn) = state.connections.get(conn_id)
        && let Err(e) = mgr
            .keyring()
            .rename_dm_nick(&conn.label, &old_nick, new_nick)
    {
        tracing::warn!("e2e: dm handle cache rename '{old_nick}' -> '{new_nick}' failed: {e}");
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
            // The old identity is no longer typing anywhere on this connection,
            // regardless of ignore (spec §1.2).
            for buf_id in state.typing.clear_nick_on_connection(conn_id, &old_nick) {
                push_typing_web_event(state, &buf_id);
            }

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

    // The old identity is no longer typing anywhere on this connection (spec §1.2).
    for buf_id in state.typing.clear_nick_on_connection(conn_id, &old_nick) {
        push_typing_web_event(state, &buf_id);
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
        state
            .pending_web_events
            .push(crate::web::protocol::WebEvent::NickEvent {
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

    // The person who was kicked stopped typing — not the kicker, and
    // regardless of ignore (spec §1.2).
    if state.typing.clear(&buffer_id, kicked_user) {
        push_typing_web_event(state, &buffer_id);
    }

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
        let text = format!("You were kicked from {channel} by {kicker} ({reason_str})");
        // Use the connection label for the server buffer ID (not the connection id).
        let server_buffer_id = state.connections.get(conn_id).map_or_else(
            || make_buffer_id(conn_id, conn_id),
            |c| make_buffer_id(conn_id, &c.label),
        );
        let kick_params = Some(vec![
            our_nick.to_string(),
            kicker,
            channel.to_string(),
            reason_str.to_string(),
        ]);

        // Helper: build a "kicked" notification message, taking text/params/tags
        // by reference (clone) or by value (move) on last call.
        let make_kick_msg = |state: &mut AppState,
                             t: String,
                             p: Option<Vec<String>>,
                             tg: Option<HashMap<String, String>>|
         -> Message {
            let id = state.next_message_id();
            Message {
                id,
                timestamp: ts,
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: t,
                highlight: true,
                event_key: Some("kicked".to_string()),
                event_params: p,
                log_msg_id: None,
                log_ref_id: None,
                tags: tg,
            }
        };

        // Add to server buffer (always visible, never removed).
        let msg = make_kick_msg(state, text.clone(), kick_params.clone(), tags.clone());
        state.add_message(&server_buffer_id, msg);

        // Also add to the channel buffer before removal so the web client
        // sees it in the channel history (it may still be displayed briefly).
        let msg = make_kick_msg(state, text.clone(), kick_params.clone(), tags.clone());
        state.add_message(&buffer_id, msg);

        // Remove the channel buffer (falls back to previous or first buffer).
        state.remove_buffer(&buffer_id);

        // Add a reminder to the landing buffer so the user sees it immediately.
        let landing_id = state
            .active_buffer_id
            .clone()
            .unwrap_or_else(|| server_buffer_id.clone());
        if landing_id != server_buffer_id {
            let msg = make_kick_msg(state, text, kick_params, tags);
            state.add_message(&landing_id, msg);
        }

        // Clean up any pending silent WHO for this channel.
        if let Some(conn) = state.connections.get_mut(conn_id) {
            remove_case_insensitive(&mut conn.silent_who_channels, channel);
            remove_case_insensitive(&mut conn.silent_banlist_channels, channel);
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
        state
            .pending_web_events
            .push(crate::web::protocol::WebEvent::TopicChanged {
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
                apply_channel_mode(state, &buffer_id, mode, &nick);
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
    set_by: &str,
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
        state
            .pending_web_events
            .push(crate::web::protocol::WebEvent::NickEvent {
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

    if matches!(mode_enum, ChannelMode::Ban) {
        if let Some(mask) = param
            && let Some(buf) = state.buffers.get_mut(buffer_id)
        {
            if adding {
                upsert_list_mode_entry(
                    buf,
                    BAN_MODE_KEY,
                    mask.to_string(),
                    set_by.to_string(),
                    Utc::now().timestamp(),
                );
            } else {
                remove_list_mode_entry(buf, BAN_MODE_KEY, mask);
            }
        }
        return;
    }

    // Channel modes (not nick prefix, not list modes) — update buf.modes
    // Skip list modes (e, I, R) and nick prefix modes (already handled above)
    let ch = channel_mode_letter(mode_enum);
    let is_list_mode = matches!(
        mode_enum,
        ChannelMode::Exception | ChannelMode::InviteException | ChannelMode::Reop
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

fn upsert_list_mode_entry(
    buf: &mut Buffer,
    mode_key: &str,
    mask: String,
    set_by: String,
    set_at: i64,
) -> usize {
    let entries = buf.list_modes.entry(mode_key.to_string()).or_default();
    if let Some(pos) = entries
        .iter()
        .position(|entry| entry.mask.eq_ignore_ascii_case(&mask))
    {
        entries[pos].set_by = set_by;
        entries[pos].set_at = set_at;
        return pos + 1;
    }

    entries.push(ListEntry {
        mask,
        set_by,
        set_at,
    });
    if entries.len() > MAX_LIST_MODE_ENTRIES {
        entries.drain(..entries.len() - MAX_LIST_MODE_ENTRIES);
    }
    entries.len()
}

fn remove_list_mode_entry(buf: &mut Buffer, mode_key: &str, mask: &str) -> bool {
    let Some(entries) = buf.list_modes.get_mut(mode_key) else {
        return false;
    };

    let original_len = entries.len();
    entries.retain(|entry| !entry.mask.eq_ignore_ascii_case(mask));
    let new_len = entries.len();
    if new_len == 0 {
        buf.list_modes.remove(mode_key);
    }
    new_len != original_len
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
        ChannelMode::RegisteredOnly => 'r',
        ChannelMode::Reop => 'R',
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
                emit_event(
                    state,
                    &target_buf,
                    "whois_header",
                    format!(
                        "%Z7aa2f7───── WHOIS {} ──────────────────────────%N",
                        args[1]
                    ),
                    vec![args[1].clone()],
                );
                emit_event(
                    state,
                    &target_buf,
                    "whois",
                    format!(
                        "%Zc0caf5{}%Z565f89 ({}@{})%N %Za9b1d6{}%N",
                        args[1], args[2], args[3], args[5]
                    ),
                    vec![
                        args[1].clone(),
                        args[2].clone(),
                        args[3].clone(),
                        args[5].clone(),
                    ],
                );
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
                emit_event(
                    state,
                    &target_buf,
                    "whois_server",
                    format!("%Z565f89  server: %Za9b1d6{}{info}%N", args[2]),
                    vec![args[1].clone(), args[2].clone(), args[3].clone(), info],
                );
            }
        }
        // RPL_WHOISOPERATOR: args = [our_nick, nick, "is an IRC operator"]
        Response::RPL_WHOISOPERATOR => {
            if args.len() >= 3 {
                let target_buf = whois_buffer(state, conn_id);
                emit_event(
                    state,
                    &target_buf,
                    "whois_oper",
                    format!("  %Zbb9af7{}%N", args[2]),
                    vec![args[1].clone(), args[2].clone()],
                );
            }
        }
        // RPL_WHOISIDLE: args = [our_nick, nick, idle_secs, signon_time, ...]
        Response::RPL_WHOISIDLE => {
            if args.len() >= 3 {
                let target_buf = whois_buffer(state, conn_id);
                let idle = args[2].parse::<u64>().unwrap_or(0);
                let idle_display = format_duration(idle);
                if args.len() >= 4
                    && let Ok(ts) = args[3].parse::<i64>()
                {
                    let dt = chrono::DateTime::from_timestamp(ts, 0)
                        .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                        .unwrap_or_default();
                    emit_event(
                        state,
                        &target_buf,
                        "whois_idle_signon",
                        format!(
                            "%Z565f89  idle: %Za9b1d6{idle_display}%Z565f89, signon: %Za9b1d6{dt}%N"
                        ),
                        vec![args[1].clone(), idle_display, dt],
                    );
                } else {
                    emit_event(
                        state,
                        &target_buf,
                        "whois_idle",
                        format!("%Z565f89  idle: %Za9b1d6{idle_display}%N"),
                        vec![args[1].clone(), idle_display],
                    );
                }
            }
        }
        // RPL_WHOISCHANNELS: args = [our_nick, nick, channels]
        Response::RPL_WHOISCHANNELS => {
            if args.len() >= 3 {
                let target_buf = whois_buffer(state, conn_id);
                emit_event(
                    state,
                    &target_buf,
                    "whois_channels",
                    format!("%Z565f89  channels: %Za9b1d6{}%N", args[2]),
                    vec![args[1].clone(), args[2].clone()],
                );
            }
        }
        // RPL_WHOISCERTFP: args = [our_nick, nick, fingerprint]
        Response::RPL_WHOISCERTFP => {
            if args.len() >= 3 {
                let target_buf = whois_buffer(state, conn_id);
                emit_event(
                    state,
                    &target_buf,
                    "whois_certfp",
                    format!("%Z565f89  certfp: %Za9b1d6{}%N", args[2]),
                    vec![args[1].clone(), args[2].clone()],
                );
            }
        }
        // RPL_WHOISKEYVALUE: args = [our_nick, target, key, visibility, value]
        Response::RPL_WHOISKEYVALUE => {
            if args.len() >= 5 {
                let target_buf = whois_buffer(state, conn_id);
                emit_event(
                    state,
                    &target_buf,
                    "whois_keyvalue",
                    format!("%Z565f89  {}: %Za9b1d6{}%N", args[2], args[4]),
                    vec![
                        args[1].clone(),
                        args[2].clone(),
                        args[3].clone(),
                        args[4].clone(),
                    ],
                );
            }
        }
        // RPL_ENDOFWHOIS: args = [our_nick, nick, "End of WHOIS list"]
        Response::RPL_ENDOFWHOIS => {
            let target_buf = whois_buffer(state, conn_id);
            let nick = args.get(1).cloned().unwrap_or_default();
            let text = args.get(2).cloned().unwrap_or_default();
            emit_event(
                state,
                &target_buf,
                "end_of_whois",
                "%Z7aa2f7─────────────────────────────────────────────%N",
                vec![nick, text],
            );
        }

        // RPL_AWAY: args = [our_nick, nick, away_message]
        Response::RPL_AWAY => {
            if args.len() >= 3 {
                let target_buf = whois_buffer(state, conn_id);
                emit_event(
                    state,
                    &target_buf,
                    "whois_away",
                    format!("%Z565f89  away: %Ze0af68{}%N", args[2]),
                    vec![args[1].clone(), args[2].clone()],
                );
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
                let silent = state
                    .connections
                    .get(conn_id)
                    .is_some_and(|c| {
                        contains_case_insensitive(&c.silent_banlist_channels, channel)
                    });

                let buf_id = crate::state::buffer::make_buffer_id(conn_id, channel);
                let index = state.buffers.get_mut(&buf_id).map_or(0, |buf| {
                    upsert_list_mode_entry(
                        buf,
                        BAN_MODE_KEY,
                        mask.clone(),
                        set_by.clone(),
                        set_at,
                    )
                });

                if silent {
                    return;
                }

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
            let channel = args.get(1).map_or("", String::as_str);
            let was_silent = state
                .connections
                .get_mut(conn_id)
                .is_some_and(|conn| {
                    remove_case_insensitive(&mut conn.silent_banlist_channels, channel)
                });
            if was_silent {
                return;
            }
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
            // params: [our_nick, channel, user, host, server, nick, flags,
            //          ":<hop> <realname>"] — ircnet-ircd (2.11+) inserts its
            // 4-char SID before the realname in the trailing.
            if args.len() >= 8 {
                let channel = &args[1];
                let conn = state.connections.get(conn_id);
                let silent = conn
                    .is_some_and(|c| contains_case_insensitive(&c.silent_who_channels, channel));
                let ircnet = conn.is_some_and(|c| c.isupport_parsed.is_ircnet_lineage());
                let user = &args[2];
                let host = &args[3];
                let nick = &args[5];
                let flags = &args[6];

                // Mirror the WHOX path: keep the nick entry fresh so silent
                // auto-WHO is not wasted on servers without WHOX.
                let buffer_id = make_buffer_id(conn_id, channel);
                update_who_nick_entry(
                    state,
                    &buffer_id,
                    nick,
                    user,
                    host,
                    flags.starts_with('G'),
                    WhoAccount::Keep,
                );

                if !silent {
                    let realname = whoreply_realname(&args[7], ircnet);
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
                        any_silent |= remove_case_insensitive(&mut conn.silent_who_channels, ch);
                    }
                    any_silent
                } else {
                    remove_case_insensitive(&mut conn.silent_who_channels, target)
                }
            } else {
                false
            };
            if !was_silent {
                let target_buf = active_or_server_buffer(state, conn_id);
                emit(state, &target_buf, "%Z565f89End of WHO list%N");
            }
        }
        Response::RPL_USERHOST => {
            handle_userhost_reply(state, conn_id, args);
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

        // 401/402/263 can answer a WHOIS but are not WHOIS-specific: 401 also
        // answers PRIVMSG/NOTICE/INVITE/KICK aimed at a missing nick, 402 any
        // command taking a server parameter, and 263 throttles any command at
        // all. Numeric dispatch here is stateless, so it cannot know which
        // command provoked the reply — the keys stay generic rather than
        // asserting a WHOIS context we cannot verify, which would also drag
        // WHOIS block styling onto a failed /msg.
        Response::ERR_NOSUCHNICK | Response::ERR_NOSUCHSERVER | Response::RPL_TRYAGAIN => {
            if args.len() >= 3 {
                let key = match response {
                    Response::ERR_NOSUCHNICK => "no_such_nick",
                    Response::ERR_NOSUCHSERVER => "no_such_server",
                    _ => "try_again",
                };
                // RPL_TRYAGAIN is a 2xx, so the catch-all below would route it
                // to the server buffer — away from the command that provoked it.
                let target_buf = active_or_server_buffer(state, conn_id);
                emit_event(
                    state,
                    &target_buf,
                    key,
                    format!("%Zf7768e! %Za9b1d6{}%N %Z565f89{}%N", args[1], args[2]),
                    vec![args[1].clone(), args[2].clone()],
                );
            }
        }

        _ => {
            if matches!(
                response,
                Response::ERR_CHANOPRIVSNEEDED
                    | Response::ERR_NOSUCHCHANNEL
                    | Response::ERR_NOTONCHANNEL
            ) && let Some(channel) = args.get(1)
                && state
                    .connections
                    .get_mut(conn_id)
                    .is_some_and(|conn| {
                        remove_case_insensitive(&mut conn.silent_banlist_channels, channel)
                    })
            {
                return;
            }
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

fn emit_event(
    state: &mut AppState,
    buffer_id: &str,
    event_key: &str,
    text: impl Into<String>,
    event_params: Vec<String>,
) {
    let id = state.next_message_id();
    state.add_message(
        buffer_id,
        Message {
            id,
            timestamp: Utc::now(),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text: text.into(),
            highlight: false,
            event_key: Some(event_key.to_string()),
            event_params: Some(event_params),
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

fn handle_whois_account(state: &mut AppState, conn_id: &str, args: &[String]) {
    if args.len() >= 3 {
        let target_buf = whois_buffer(state, conn_id);
        let text = args.get(3).cloned().unwrap_or_default();
        emit_event(
            state,
            &target_buf,
            "whois_account",
            format!("%Z565f89  account: %Za9b1d6{}%N", args[2]),
            vec![args[1].clone(), args[2].clone(), text],
        );
    }
}

/// Every theme key the WHOIS path can emit, plus the three generic error keys
/// a WHOIS can provoke. `shipped_themes_define_every_whois_key` tests both
/// bundled themes against this list, so a new key cannot ship with only one
/// theme updated — add new keys here.
#[cfg(test)]
pub const WHOIS_EVENT_KEYS: &[&str] = &[
    "whois_header",
    "whois",
    "whois_server",
    "whois_oper",
    "whois_idle",
    "whois_idle_signon",
    "whois_channels",
    "whois_away",
    "whois_account",
    "whois_secure",
    "whois_certfp",
    "whois_keyvalue",
    "whois_special",
    "whois_registered",
    "whois_help",
    "whois_bot",
    "whois_actually",
    "whois_host",
    "whois_modes",
    "end_of_whois",
    "no_such_nick",
    "no_such_server",
    "try_again",
];

/// Theme event key for WHOIS numerics whose payload is freeform prose and
/// which irc-proto has no `Response` variant for. Single source of truth for
/// both the dispatch guard and the per-numeric theming.
///
/// `args` is the full numeric argument list (`[our_nick, ...]`). It is only
/// inspected for 377, which is `RPL_SPAM` (post-MOTD announcement text) in
/// `AustHex` and a WHOIS usermode line elsewhere. The `usermodes` literal is a
/// protocol token, not admin-authored prose, so keying on it is safe.
fn whois_freeform_key(numeric: &str, args: &[String]) -> Option<&'static str> {
    match numeric {
        "307" => Some("whois_registered"),
        "310" => Some("whois_help"),
        "335" => Some("whois_bot"),
        "338" => Some("whois_actually"),
        // Prose with no dedicated key: 320 is IRCnet's catch-all
        // `RPL_WHOISEXTRA` (cloak and TLS lines both, with server-configured
        // text), 337 is hybrid's `RPL_WHOISTEXT` webirc info, and 275 is
        // `RPL_USINGSSL` on Bahamut but `RPL_STATSDLINE` on hybrid/charybdis —
        // stateless numeric dispatch cannot tell those two apart, so it
        // renders 275 as prose rather than asserting "secure: TLS" over a
        // /stats line.
        "320" | "337" | "275" => Some("whois_special"),
        // 378 is Unreal's `RPL_WHOISHOST`, 327 rusnet's.
        "327" | "378" => Some("whois_host"),
        // 379 is Unreal's `RPL_WHOISMODES`; 326 carries oper privileges as a
        // mode string; 377 is the `<me> usermodes <nick> <modes>` form.
        "326" | "379" => Some("whois_modes"),
        // The 4-arg length check is load-bearing: 377's nick sits at args[2],
        // so a 3-arg `<me> usermodes <nick>` has nothing left for the text
        // slot. Claiming it here would make `handle_whois_freeform` bail and
        // drop the line entirely; returning None lets it reach the generic
        // catch-all and still display.
        "377" if args.len() >= 4 && args[1] == "usermodes" => Some("whois_modes"),
        _ => None,
    }
}

/// Fields carried by a JOIN, unified across both wire forms.
#[derive(Debug, Clone, Copy)]
pub struct JoinFields<'a> {
    pub channel: &'a str,
    pub account: Option<&'a str>,
    pub realname: Option<&'a str>,
    /// `ircnet.com/extended-join` only: server-assigned UID (matches WHOX `U`).
    pub uid: Option<&'a str>,
    /// `ircnet.com/extended-join` only: client IP as seen by the server.
    pub ip: Option<&'a str>,
}

/// Extract join fields from either JOIN wire form: the crate-parsed
/// `Command::JOIN` (1-3 args) or the 6-arg `ircnet.com/extended-join` Raw
/// variant (`<channel> <uid> <ip> <netjoin> <account> :<realname>`).
/// `None` for anything else.
pub fn join_fields(command: &Command) -> Option<JoinFields<'_>> {
    match command {
        Command::JOIN(channel, account, realname) => Some(JoinFields {
            channel,
            account: account.as_deref(),
            realname: realname.as_deref(),
            uid: None,
            ip: None,
        }),
        Command::Raw(cmd, args) if cmd.eq_ignore_ascii_case("JOIN") && args.len() == 6 => {
            Some(JoinFields {
                channel: &args[0],
                account: Some(&args[4]),
                realname: Some(&args[5]),
                uid: Some(&args[1]),
                ip: Some(&args[2]),
            })
        }
        _ => None,
    }
}

/// WHOIS numerics whose payload is freeform prose: args = [`our_nick`, nick,
/// text...]. Middle args (338's host/ip values) join into the text so every
/// known wire variant renders.
fn handle_whois_freeform(state: &mut AppState, conn_id: &str, numeric: &str, args: &[String]) {
    let Some(event_key) = whois_freeform_key(numeric, args) else {
        return;
    };
    // 377's WHOIS form is `<me> usermodes <nick> <modes>` — the nick sits one
    // position further right than in every other freeform numeric, so reading
    // args[1] there would put the `usermodes` literal in the nick slot.
    let (nick_idx, text_from) = if numeric == "377" { (2, 3) } else { (1, 2) };
    if args.len() <= text_from {
        return;
    }
    let text = args[text_from..].join(" ");
    let target_buf = whois_buffer(state, conn_id);
    emit_event(
        state,
        &target_buf,
        event_key,
        format!("%Z565f89  %Za9b1d6{text}%N"),
        vec![args[nick_idx].clone(), text],
    );
}

fn handle_whois_secure(state: &mut AppState, conn_id: &str, args: &[String]) {
    if args.len() >= 2 {
        let target_buf = whois_buffer(state, conn_id);
        let text = args
            .get(2)
            .cloned()
            .unwrap_or_else(|| "is using a secure connection".to_string());
        emit_event(
            state,
            &target_buf,
            "whois_secure",
            "%Z565f89  secure: %Z9ece6aTLS%N",
            vec![args[1].clone(), "TLS".to_string(), text],
        );
    }
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
///
/// Wraps within 1..=999: both ircu and ircnet-ircd cap the token at 3 chars
/// (ircnet drops longer ones and replies with token "0", which is also why 0
/// itself is skipped).
pub fn next_who_token(state: &mut AppState, conn_id: &str) -> String {
    if let Some(conn) = state.connections.get_mut(conn_id) {
        conn.who_token_counter = conn.who_token_counter % 999 + 1;
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
    let fields = build_whox_fields(state, conn_id)?;
    if silent && let Some(conn) = state.connections.get_mut(conn_id) {
        conn.silent_who_channels.insert(channel.to_string());
    }
    Some((channel.to_string(), fields))
}

/// The `%fields,token` argument for a WHOX WHO command, or `None` when the
/// server lacks WHOX. Shared by manual `/who` and the auto-WHO batcher so
/// both always request the same field layout (which the 354 parser's
/// arg-count disambiguation depends on).
pub fn build_whox_fields(state: &mut AppState, conn_id: &str) -> Option<String> {
    let selector = state
        .connections
        .get(conn_id)?
        .isupport_parsed
        .whox_request()?;
    let token = next_who_token(state, conn_id);
    Some(format!("{selector},{token}"))
}

/// Handle a WHOX reply (numeric 354 / `RPL_WHOSPCRPL`).
///
/// Our field selector `%tcuihnfar` produces responses with fields:
///   `[our_nick, token, channel, user, ip, host, nick, flags, account, realname]`
/// On `IRCnet`-lineage servers we request `%tcuihnfaUr` instead, which inserts
/// the user's UID between account and realname (11 args). The layout is
/// disambiguated by arg count — we only ever send those two selectors.
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
    // The realname is the trailing parameter — always the LAST arg (kept even
    // when empty by irc-proto's suffix handling). The UID sits at 9 only in
    // the exact 11-arg IRCnet layout; anything longer is an unknown layout,
    // where guessing a middle field would corrupt the display.
    let (uid, realname) = if args.len() == 11 {
        (Some(args[9].as_str()), &args[10])
    } else {
        (None, &args[args.len() - 1])
    };

    // Auto-WHO replies are silent — update state only, no display
    let silent = state
        .connections
        .get(conn_id)
        .is_some_and(|c| contains_case_insensitive(&c.silent_who_channels, channel));

    // Parse account: "0" means not logged in
    let account = (account_raw != "0").then(|| account_raw.clone());

    let buffer_id = make_buffer_id(conn_id, channel);
    update_who_nick_entry(
        state,
        &buffer_id,
        nick,
        user,
        host,
        flags.starts_with('G'),
        WhoAccount::Set(account.clone()),
    );

    // Only display for manual /who — auto-WHO on join is silent
    if !silent {
        let target_buf = active_or_server_buffer(state, conn_id);
        let account_str = account.as_deref().unwrap_or("");
        let uid_str = uid.map_or_else(String::new, |u| format!(" {u}"));
        emit(
            state,
            &target_buf,
            &format!(
                "%Zc0caf5{nick}%Z565f89 ({user}@{host}) [{flags}] {channel}%Za9b1d6 {realname}%Z565f89 [{account_str}]{uid_str}%N"
            ),
        );
    }
}

/// Whether a WHO-family reply carries an account field to store.
enum WhoAccount {
    /// 352 — no account on the wire; leave the stored value untouched.
    Keep,
    /// 354 — overwrite with the parsed account (`None` = not logged in).
    Set(Option<String>),
}

/// Update a channel nick entry from a WHO (352) or WHOX (354) reply. Both
/// reply paths share this so the field writes can never drift apart.
fn update_who_nick_entry(
    state: &mut AppState,
    buffer_id: &str,
    nick: &str,
    ident: &str,
    host: &str,
    away: bool,
    account: WhoAccount,
) {
    if let Some(buf) = state.buffers.get_mut(buffer_id)
        && let Some(entry) = buf.users.get_mut(&nick.to_lowercase())
    {
        tracing::trace!(%nick, %buffer_id, %away, "WHO: updating nick entry");
        entry.ident = Some(ident.to_string());
        entry.host = Some(host.to_string());
        entry.away = away;
        if let WhoAccount::Set(account) = account {
            entry.account = account;
        }
    } else {
        tracing::trace!(%nick, %buffer_id, "WHO: buffer or nick not found for update");
    }
}

/// True for an `IRCnet` server SID: exactly 4 chars, `[0-9][0-9A-Z]{3}`
/// (`ircd/s_id.c` `sid_valid`, SIDLEN=4).
fn is_ircnet_sid(token: &str) -> bool {
    token.len() == 4
        && token.as_bytes()[0].is_ascii_digit()
        && token
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
}

/// Extract the realname from a 352 trailing parameter (`<hop> <realname>`;
/// on `IRCnet`-lineage servers `<hop> <sid> <realname>` per `ircd/s_err.c`'s
/// `RPL_WHOREPLY` format string). Defensive on both counts: the hopcount is
/// stripped only when the first token is all digits, and the SID only when
/// the token actually matches the SID shape — so a non-conforming trailing
/// (bouncers, services) or a false-positive lineage classification degrades
/// to showing extra tokens instead of eating realname words.
fn whoreply_realname(trailing: &str, strip_sid: bool) -> &str {
    let is_hop = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    let rest = match trailing.split_once(' ') {
        Some((first, rest)) if is_hop(first) => rest,
        Some(_) => return trailing,
        None => return if is_hop(trailing) { "" } else { trailing },
    };
    if !strip_sid {
        return rest;
    }
    match rest.split_once(' ') {
        Some((sid, realname)) if is_ircnet_sid(sid) => realname,
        None if is_ircnet_sid(rest) => "",
        _ => rest,
    }
}

/// Best-effort E2E decryption for an incoming PRIVMSG. Returns the
/// decrypted plaintext as an owned `String` when the wire line parses and
/// decrypts successfully; returns `None` otherwise (leaving `text`
/// untouched for the rest of `handle_privmsg`).
///
/// The `sender_handle` must be built from the raw IRC prefix (`ident@host`)
/// — that is what the `E2eManager` keyring is keyed on. On `MissingKey`
/// the function also enqueues an outbound KEYREQ (subject to the
/// per-peer rate limiter) addressed back to `sender_nick` so the
/// initiator-side handshake starts automatically the first time an
/// encrypted line arrives from an unknown peer.
/// Returns `(display_text, transient)`. `transient = true` marks lines that
/// must never be logged under the server @msgid (the awaiting-session
/// placeholder, Err-arm rejections) — see `handle_privmsg`.
fn try_decrypt_e2e(
    state: &mut AppState,
    conn_id: &str,
    sender_nick: &str,
    sender_handle: &str,
    channel: &str,
    text: &str,
    is_own: bool,
) -> Option<(String, bool)> {
    let mgr = state.e2e_manager.clone()?;
    if !text.starts_with("+RPE2E01") {
        return None;
    }
    // Our own echo-message echo of an encrypted PRIVMSG is not a thing
    // we can decrypt (we have no incoming session keyed on our own
    // handle), and firing an auto-KEYREQ here would (a) send a NOTICE
    // to ourselves that goes nowhere and (b) leave a stale entry in
    // `self.pending` that blocks later reciprocal KEYREQs in
    // `/e2e accept`. `input.rs::handle_plain_message` already wrote a
    // local plaintext echo before the server round-tripped the wire
    // back to us, so the correct response is to swallow the echoed
    // ciphertext entirely. Returning `Some("")` suppresses the raw
    // wire from leaking into the buffer.
    if is_own {
        return Some((String::new(), false));
    }
    match mgr.decrypt_incoming(sender_handle, channel, text) {
        Ok(crate::e2e::manager::DecryptOutcome::Plaintext(s)) => Some((s, false)),
        Ok(crate::e2e::manager::DecryptOutcome::MissingKey {
            handle,
            channel: ch,
        }) => {
            // No session yet — fire a KEYREQ to the sender if the rate
            // limiter allows. The message will stay hidden behind the
            // placeholder until the responder's KEYRSP installs the
            // session; after that, subsequent ciphertext lines decrypt
            // normally.
            if mgr.allow_keyreq(&handle) {
                match mgr.build_keyreq_for_peer(&ch, Some(&handle)) {
                    Ok(req) => {
                        let ctcp = mgr.encode_keyreq_ctcp(&req);
                        state.pending_e2e_sends.push(crate::state::PendingE2eSend {
                            connection_id: conn_id.to_string(),
                            target: sender_nick.to_string(),
                            notice_text: ctcp,
                        });
                    }
                    Err(e) => tracing::warn!("build_keyreq failed for {ch}: {e}"),
                }
            }
            Some((
                format!(
                    "{}{handle}]",
                    crate::e2e::AWAITING_SESSION_PLACEHOLDER_PREFIX
                ),
                true,
            ))
        }
        Ok(crate::e2e::manager::DecryptOutcome::Rejected(reason)) => {
            // Deterministic rejections (wrong key, replay window, untrusted)
            // stay logged: a replay of the same line rejects identically.
            Some((format!("[E2E rejected: {reason}]"), false))
        }
        Err(e) => {
            tracing::warn!("e2e decrypt error on {channel}: {e}");
            // A malformed +RPE2E01 line (truncated relay, corrupted base64,
            // DB fault mid-decrypt) must NOT fall through to rendering — and
            // logging — the raw wire line; surface a rejection like every
            // other decrypt failure. The CHATHISTORY sibling
            // (`decrypt_chathistory_text`) skips such rows for the same
            // reason. Non-ciphertext text (impossible today: parse succeeds
            // before any fallible read) still passes through untouched.
            // Transient: this arm also fires on a TRANSIENT keyring fault
            // over a perfectly valid ciphertext — a logged row would
            // permanently block the decryptable CHATHISTORY replay.
            text.starts_with("+RPE2E01").then(|| {
                (
                    "[E2E rejected: malformed or undecryptable ciphertext]".to_string(),
                    true,
                )
            })
        }
    }
}

/// Outcome of attempting to dispatch a CTCP body as an RPE2E handshake.
#[derive(Debug, PartialEq, Eq)]
enum RpEe2eOutcome {
    /// Message was an RPE2E CTCP and has been fully handled — do not
    /// render it in the normal NOTICE/PRIVMSG buffer.
    Handled,
    /// Not RPE2E traffic — caller continues with normal rendering.
    NotE2e,
}

/// Try to dispatch an incoming CTCP body as an RPE2E KEYREQ/KEYRSP.
/// Returns `None` if the E2E manager is not initialized (caller treats
/// this as "not handled" and falls through to the default rendering).
/// Returns `Some(Handled)` if the body was an RPE2E CTCP (even if the
/// crypto rejected it — we still want to suppress the raw body from
/// surfacing in the UI). Returns `Some(NotE2e)` if the body was not a
/// RPEE2E tag so the caller can keep rendering it.
#[expect(
    clippy::too_many_lines,
    reason = "RPE2E handshake dispatch keeps request/response flow together"
)]
fn try_dispatch_rpe2e_ctcp(
    state: &mut AppState,
    conn_id: &str,
    prefix: Option<&Prefix>,
    target: &str,
    text: &str,
) -> Option<RpEe2eOutcome> {
    use crate::e2e::handshake::HandshakeMsg;

    // Strip optional CTCP framing \x01...\x01. Servers sometimes drop the
    // trailing byte, so accept both variants and anything in between.
    let trimmed = text.strip_prefix('\x01').unwrap_or(text);
    let inner = trimmed.strip_suffix('\x01').unwrap_or(trimmed);
    if !inner.starts_with(crate::e2e::handshake::CTCP_TAG) {
        return Some(RpEe2eOutcome::NotE2e);
    }
    let mgr = state.e2e_manager.clone()?;

    let (nick, ident, host) = extract_nick_userhost(prefix);
    let sender_handle = format!("{ident}@{host}");

    // echo-message: the server echoes our own KEYREQ/KEYRSP/REKEY NOTICEs back
    // with OUR full prefix, and this dispatch runs before any is_own check in
    // the callers. Processing the echo would cache our own nick→handle in the
    // PEER handle cache — exactly the pollution handle_chghost's is_own guard
    // prevents — and feed our own handshake into the KEYREQ handler. Swallow
    // it: the echo is not user-renderable CTCP either.
    if state
        .connections
        .get(conn_id)
        .is_some_and(|c| c.nick.eq_ignore_ascii_case(&nick))
    {
        return Some(RpEe2eOutcome::Handled);
    }

    // Track the sender's current handle BEFORE parsing: even a handshake that
    // fails to parse (truncated relay, newer protocol revision) carries a
    // server-stamped prefix, and handle_privmsg's generic peer-handle tracking
    // is suppressed for every handshake-looking text on the assumption that
    // THIS function performs the update. A KEYREQ/KEYRSP/REKEY can be the
    // first signal of a peer's new host: a DM-only reconnect sends no CHGHOST
    // (no shared channel), and the peer may re-handshake before any PRIVMSG.
    // For a DM the handshake's `c=` context is `@<sender_handle>`, so unless
    // the enabled DM config is first migrated from `@<old_handle>`,
    // `effective_channel_mode` finds nothing under `req.channel` and silently
    // drops the handshake (no KEYRSP, no trust notice). Server prefixes carry
    // no ident@host — skip those.
    if !ident.is_empty() && !host.is_empty() {
        observe_dm_peer_handle(state, conn_id, &nick, &sender_handle);
    }

    let parsed = match crate::e2e::handshake::parse(inner) {
        Ok(Some(msg)) => msg,
        Ok(None) => return Some(RpEe2eOutcome::NotE2e),
        Err(e) => {
            tracing::warn!("rpe2e handshake parse error: {e}");
            emit_e2e_debug(
                state,
                conn_id,
                None,
                format!("[E2E debug] RX handshake parse error from {sender_handle}: {e}"),
            );
            return Some(RpEe2eOutcome::Handled); // suppress bad body
        }
    };
    // Scope the wire c= to this connection's network before any keyring
    // access (see `e2e::scoped_context`): `#rust` on two networks — or two
    // peers behind identical handles — must not share config or session
    // rows. The manager re-derives the wire form for everything that goes
    // back out, so the scope prefix never leaves the process.
    let network = state
        .connections
        .get(conn_id)
        .map(|c| c.label.clone())
        .unwrap_or_default();
    let parsed = {
        use crate::e2e::handshake::HandshakeMsg as HM;
        match parsed {
            HM::Req(mut req) => {
                req.channel = crate::e2e::scoped_context(&network, &req.channel);
                HM::Req(req)
            }
            HM::Rsp(mut rsp) => {
                rsp.channel = crate::e2e::scoped_context(&network, &rsp.channel);
                HM::Rsp(rsp)
            }
            HM::Rekey(mut rk) => {
                rk.channel = crate::e2e::scoped_context(&network, &rk.channel);
                HM::Rekey(rk)
            }
        }
    };
    // RPEE2E target is always us — the channel being negotiated is
    // carried inside the payload rather than in the IRC target.
    let _ = target;

    match parsed {
        HandshakeMsg::Req(req) => {
            emit_e2e_debug(
                state,
                conn_id,
                Some(&req.channel),
                format!(
                    "[E2E debug] RX KEYREQ from {nick} ({sender_handle}) for {}",
                    req.channel
                ),
            );
            let result = mgr.handle_keyreq_with_nick(&sender_handle, Some(&nick), &req);
            surface_pending_trust_changes(state, conn_id, &mgr);
            surface_pending_accept_requests(state, conn_id, &mgr);
            match result {
                Ok(Some(rsp)) => {
                    let body = mgr.encode_keyrsp_ctcp(&rsp);
                    state.pending_e2e_sends.push(crate::state::PendingE2eSend {
                        connection_id: conn_id.to_string(),
                        target: nick.clone(),
                        notice_text: body,
                    });
                    emit_e2e_debug(
                        state,
                        conn_id,
                        Some(&req.channel),
                        format!("[E2E debug] queued KEYRSP to {nick} for {}", req.channel),
                    );
                    // Symmetric handshake (spec §5.3, G13): drain any
                    // reciprocal KEYREQs queued by `handle_keyreq` so
                    // the us→peer direction gets a fresh NOTICE in the
                    // same tick as the KEYRSP. Each reciprocal targets
                    // the same peer who just initiated the handshake.
                    for out in mgr.take_pending_outbound_keyreqs() {
                        let ctcp = mgr.encode_keyreq_ctcp(&out.req);
                        state.pending_e2e_sends.push(crate::state::PendingE2eSend {
                            connection_id: conn_id.to_string(),
                            target: nick.clone(),
                            notice_text: ctcp,
                        });
                        emit_e2e_debug(
                            state,
                            conn_id,
                            Some(&out.channel),
                            format!(
                                "[E2E debug] queued reciprocal KEYREQ to {nick} for {}",
                                out.channel
                            ),
                        );
                    }
                    Some(RpEe2eOutcome::Handled)
                }
                Ok(None) => {
                    emit_e2e_debug(
                        state,
                        conn_id,
                        Some(&req.channel),
                        format!(
                            "[E2E debug] KEYREQ from {nick} ({sender_handle}) is pending on {}",
                            req.channel
                        ),
                    );
                    Some(RpEe2eOutcome::Handled)
                }
                Err(e) => {
                    tracing::warn!("handle_keyreq error: {e}");
                    emit_e2e_debug(
                        state,
                        conn_id,
                        Some(&req.channel),
                        format!(
                            "[E2E debug] KEYREQ from {nick} ({sender_handle}) failed on {}: {e}",
                            req.channel
                        ),
                    );
                    Some(RpEe2eOutcome::Handled)
                }
            }
        }
        HandshakeMsg::Rsp(rsp) => {
            emit_e2e_debug(
                state,
                conn_id,
                Some(&rsp.channel),
                format!(
                    "[E2E debug] RX KEYRSP from {nick} ({sender_handle}) for {}",
                    rsp.channel
                ),
            );
            let result = mgr.handle_keyrsp(&sender_handle, &rsp);
            surface_pending_trust_changes(state, conn_id, &mgr);
            if let Err(e) = result {
                tracing::warn!("handle_keyrsp error: {e}");
                emit_e2e_debug(
                    state,
                    conn_id,
                    Some(&rsp.channel),
                    format!(
                        "[E2E debug] KEYRSP from {nick} ({sender_handle}) failed on {}: {e}",
                        rsp.channel
                    ),
                );
            } else {
                emit_e2e_debug(
                    state,
                    conn_id,
                    Some(&rsp.channel),
                    format!(
                        "[E2E debug] KEYRSP from {nick} ({sender_handle}) installed session on {}",
                        rsp.channel
                    ),
                );
                // A session just came up. The ciphertext that TRIGGERED the
                // handshake was rendered only as the transient
                // "[E2E: awaiting session with …]" placeholder (DM and
                // channel alike) — queue a CHATHISTORY gap-fill of the
                // conversation so that line is re-fetched, decrypted under
                // the fresh session, and spliced in (the splice sweeps the
                // placeholder). Without this the first encrypted line of a
                // new session is neither replaced nor recoverable from
                // local history.
                let wire = crate::e2e::wire_context(&rsp.channel);
                let gapfill_target = if wire.starts_with('@') {
                    nick.clone()
                } else {
                    wire.to_string()
                };
                let gapfill = crate::state::PendingE2eGapfill {
                    connection_id: conn_id.to_string(),
                    target: gapfill_target,
                };
                // Dedup: a second KEYRSP for the same conversation (peer
                // retry, multi-peer channel) must not stack a duplicate
                // fetch behind the retry loop in `App::handle_irc_event`.
                if !state.pending_e2e_gapfills.contains(&gapfill) {
                    state.pending_e2e_gapfills.push(gapfill);
                }
            }
            Some(RpEe2eOutcome::Handled)
        }
        HandshakeMsg::Rekey(rekey) => {
            emit_e2e_debug(
                state,
                conn_id,
                Some(&rekey.channel),
                format!(
                    "[E2E debug] RX REKEY from {nick} ({sender_handle}) for {}",
                    rekey.channel
                ),
            );
            let result = mgr.handle_rekey(&sender_handle, &rekey);
            surface_pending_trust_changes(state, conn_id, &mgr);
            if let Err(e) = result {
                tracing::warn!("handle_rekey error: {e}");
                emit_e2e_debug(
                    state,
                    conn_id,
                    Some(&rekey.channel),
                    format!(
                        "[E2E debug] REKEY from {nick} ({sender_handle}) failed on {}: {e}",
                        rekey.channel
                    ),
                );
            } else {
                emit_e2e_debug(
                    state,
                    conn_id,
                    Some(&rekey.channel),
                    format!(
                        "[E2E debug] REKEY from {nick} ({sender_handle}) applied on {}",
                        rekey.channel
                    ),
                );
            }
            Some(RpEe2eOutcome::Handled)
        }
    }
}

fn e2e_debug_enabled() -> bool {
    std::env::var("REPARTEE_E2E_DEBUG_BUFFER").is_ok_and(|v| {
        let v = v.trim();
        !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
    })
}

fn emit_e2e_debug(
    state: &mut AppState,
    conn_id: &str,
    channel: Option<&str>,
    text: impl Into<String>,
) {
    if !e2e_debug_enabled() {
        return;
    }
    let text = text.into();
    let target_buffer = channel
        // Contexts may carry a network-scope prefix — buffers key by the
        // wire name.
        .map(|channel| make_buffer_id(conn_id, crate::e2e::wire_context(channel)))
        .filter(|id| state.buffers.contains_key(id))
        .unwrap_or_else(|| active_or_server_buffer(state, conn_id));
    let id = state.next_message_id();
    let event_param = text.clone();
    state.add_message(
        &target_buffer,
        Message {
            id,
            timestamp: Utc::now(),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text,
            highlight: false,
            event_key: Some("e2e_info".to_string()),
            event_params: Some(vec![event_param]),
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
        },
    );
}

fn emit_e2e_message(
    state: &mut AppState,
    buffer_id: &str,
    event_key: &str,
    highlight: bool,
    text: String,
) {
    let id = state.next_message_id();
    state.add_message(
        buffer_id,
        Message {
            id,
            timestamp: Utc::now(),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text: text.clone(),
            highlight,
            event_key: Some(event_key.to_string()),
            event_params: Some(vec![text]),
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
        },
    );
}

fn parse_userhost_reply(entry: &str) -> Option<(String, String)> {
    let (nick_part, userhost_part) = entry.split_once('=')?;
    let nick = nick_part.trim_end_matches('*');
    let userhost = userhost_part
        .strip_prefix('+')
        .or_else(|| userhost_part.strip_prefix('-'))
        .unwrap_or(userhost_part);
    let (ident, host) = userhost.split_once('@')?;
    Some((nick.to_string(), format!("{ident}@{host}")))
}

#[expect(
    clippy::too_many_lines,
    reason = "linear reply handler — one arm per deferred USERHOST action"
)]
fn handle_userhost_reply(state: &mut AppState, conn_id: &str, args: &[String]) {
    if args.len() < 2 {
        return;
    }
    let replies = args[1..].join(" ");
    // Seed our own handle from any reply entry matching our nick, regardless
    // of pending requests — a self-USERHOST on connect carries no pending
    // request but must still populate the recipient-keyed DM context.
    if let Some(our_nick) = state.connections.get(conn_id).map(|c| c.nick.clone()) {
        for entry in replies.split_whitespace() {
            if let Some((nick, handle)) = parse_userhost_reply(entry)
                && nick.eq_ignore_ascii_case(&our_nick)
            {
                set_own_handle(state, conn_id, handle);
            }
        }
    }
    if state.pending_userhost_requests.is_empty() {
        return;
    }
    for entry in replies.split_whitespace() {
        let Some((nick, handle)) = parse_userhost_reply(entry) else {
            continue;
        };
        let mut idx = 0usize;
        while idx < state.pending_userhost_requests.len() {
            let req = &state.pending_userhost_requests[idx];
            if req.connection_id != conn_id || !req.nick.eq_ignore_ascii_case(&nick) {
                idx += 1;
                continue;
            }
            let req = state.pending_userhost_requests.remove(idx);
            match req.action {
                crate::state::PendingUserhostAction::E2eForget {
                    buffer_id,
                    target,
                    channel,
                    all,
                } => {
                    let Some(mgr) = state.e2e_manager.clone() else {
                        emit_e2e_message(
                            state,
                            &buffer_id,
                            "e2e_error",
                            true,
                            "USERHOST resolved but E2E is disabled".to_string(),
                        );
                        continue;
                    };
                    let result = if all {
                        mgr.forget_peer_everywhere(&handle)
                    } else if let Some(channel) = channel.as_deref() {
                        // DM: also clears the recipient-keyed @<own> incoming
                        // session, matching the direct perform_e2e_forget path.
                        // Recompute @<own> from our CURRENT own handle rather
                        // than a value captured at command time — a CHGHOST
                        // between the command and this reply moves our handle,
                        // and the live incoming session is keyed under the
                        // current @<own>. Only DMs (peer context `@<peer>`) have
                        // a distinct own context; a channel forget has own==peer.
                        // The captured context may be network-scoped — the
                        // DM test looks at its wire part.
                        let own_channel = if crate::e2e::wire_context(channel).starts_with('@') {
                            let network = state
                                .connections
                                .get(conn_id)
                                .map(|c| c.label.clone())
                                .unwrap_or_default();
                            let own = state
                                .connections
                                .get(conn_id)
                                .and_then(|c| c.own_handle.as_deref())
                                .map(|h| crate::e2e::scoped_context(&network, &format!("@{h}")));
                            if own.is_none() {
                                // Our own handle is unknown (reset at every
                                // registration until the self-USERHOST reply).
                                // Forgetting only @<peer> would leave the
                                // peer's TRUSTED incoming session under @<own>
                                // alive while reporting success — their
                                // messages would keep decrypting as trusted.
                                // Refuse, like every sibling /e2e command.
                                emit_e2e_message(
                                    state,
                                    &buffer_id,
                                    "e2e_error",
                                    true,
                                    format!(
                                        "/e2e forget {target}: own handle not yet \
                                         known — nothing removed; retry in a moment"
                                    ),
                                );
                                continue;
                            }
                            own
                        } else {
                            None
                        };
                        mgr.forget_peer_on_dm_contexts(&handle, channel, own_channel.as_deref())
                    } else {
                        emit_e2e_message(
                            state,
                            &buffer_id,
                            "e2e_error",
                            true,
                            format!("/e2e forget: no channel context for {target}"),
                        );
                        continue;
                    };
                    match result {
                        Ok(deleted) if all => emit_e2e_message(
                            state,
                            &buffer_id,
                            "e2e_warning",
                            false,
                            format!(
                                "forgot {target} ({handle}) globally — removed {deleted} row(s)"
                            ),
                        ),
                        Ok(deleted) => emit_e2e_message(
                            state,
                            &buffer_id,
                            "e2e_warning",
                            false,
                            format!(
                                "forgot {target} ({handle}) on {} — removed {deleted} row(s)",
                                channel
                                    .as_deref()
                                    .map_or_else(String::new, crate::e2e::display_context)
                            ),
                        ),
                        Err(e) => emit_e2e_message(
                            state,
                            &buffer_id,
                            "e2e_error",
                            true,
                            format!("/e2e forget: {e}"),
                        ),
                    }
                }
            }
        }
    }
}

/// Format the human-readable body for a TOFU/trust change, WITHOUT the
/// `[E2E] ` banner. Returns `None` for `Known`/`New` (which never surface a
/// user notice).
///
/// The banner is omitted deliberately: the live render path applies the
/// theme's `e2e_warning` / `e2e_error` template (`%Z..[E2E]%N $*`), which
/// prepends `[E2E]` itself. [`e2e_event_message`] re-adds the banner to the
/// stored `text` for the backlog/fallback path, where `event_key` is
/// stripped and `text` is rendered verbatim.
#[allow(
    clippy::redundant_pub_crate,
    reason = "exercised by the ui::message_line render-roundtrip regression test"
)]
pub(crate) fn trust_change_body(
    change: &crate::e2e::manager::TrustChange,
) -> Option<(String, &'static str)> {
    use crate::e2e::manager::TrustChange;
    let pair = match change {
        TrustChange::FingerprintChanged {
            handle,
            old_fp,
            new_fp,
        } => {
            let old_hex = hex::encode(old_fp);
            let new_hex = hex::encode(new_fp);
            let short_old = &old_hex[..old_hex.len().min(16)];
            let short_new = &new_hex[..new_hex.len().min(16)];
            (
                format!(
                    "WARNING: {handle} identity key has CHANGED\n      \
                     old fp: {short_old}\n      \
                     new fp: {short_new}\n      \
                     run /e2e reverify {handle} to accept the new key"
                ),
                "e2e_error",
            )
        }
        TrustChange::HandleChanged {
            old_handle,
            new_handle,
            fingerprint,
        } => {
            let fp_hex = hex::encode(fingerprint);
            let short = &fp_hex[..fp_hex.len().min(16)];
            (
                format!(
                    "notice: known key {short} appeared under new handle\n      \
                     old handle: {old_handle}\n      \
                     new handle: {new_handle}\n      \
                     run /e2e reverify {new_handle} to accept"
                ),
                "e2e_warning",
            )
        }
        TrustChange::Revoked {
            handle,
            fingerprint,
        } => {
            let fp_hex = hex::encode(fingerprint);
            let short = &fp_hex[..fp_hex.len().min(16)];
            (
                format!(
                    "ERROR: peer {handle} (fp={short}) is REVOKED; \
                     handshake refused. run /e2e unrevoke {handle} to restore"
                ),
                "e2e_error",
            )
        }
        TrustChange::Known | TrustChange::New => return None,
    };
    Some(pair)
}

/// Build a themed `[E2E]` event line. `body` must NOT include the `[E2E] `
/// banner — the theme's `e2e_*` template (`[E2E] $*`) prepends it on the
/// live render path. We store `text = "[E2E] {body}"` for the
/// storage/backlog fallback (where `event_key` is stripped and `text` is
/// rendered verbatim) and pass `body` through `event_params` so the live
/// template's `$*` expands to the full message instead of collapsing to a
/// bare `[E2E]`.
#[allow(
    clippy::redundant_pub_crate,
    reason = "exercised by the ui::message_line render-roundtrip regression test"
)]
pub(crate) fn e2e_event_message(
    id: u64,
    body: String,
    event_key: &str,
    highlight: bool,
) -> Message {
    Message {
        id,
        timestamp: Utc::now(),
        message_type: MessageType::Event,
        nick: None,
        nick_mode: None,
        text: format!("[E2E] {body}"),
        highlight,
        event_key: Some(event_key.to_string()),
        event_params: Some(vec![body]),
        log_msg_id: None,
        log_ref_id: None,
        tags: None,
    }
}

/// Drain all pending TOFU warnings from the manager and emit them as
/// themed `[E2E]` event messages. Each notice targets the channel the
/// handshake referenced (from the KEYREQ/KEYRSP payload); if that channel
/// has no buffer yet the message falls back to the active-or-server
/// buffer so the warning still reaches the user.
fn surface_pending_trust_changes(
    state: &mut AppState,
    conn_id: &str,
    mgr: &crate::e2e::E2eManager,
) {
    let notices = mgr.take_pending_trust_changes();
    if notices.is_empty() {
        return;
    }
    for notice in notices {
        let target_buffer = if notice.channel.is_empty() {
            active_or_server_buffer(state, conn_id)
        } else {
            // Contexts may be network-scoped; buffers key by the wire name.
            let cand = make_buffer_id(conn_id, crate::e2e::wire_context(&notice.channel));
            if state.buffers.contains_key(&cand) {
                cand
            } else {
                active_or_server_buffer(state, conn_id)
            }
        };
        // Known / New never produce a notice; everything else carries the
        // body through event_params so the live theme template renders it.
        let Some((body, event_key)) = trust_change_body(&notice.change) else {
            continue;
        };
        let id = state.next_message_id();
        state.add_message(&target_buffer, e2e_event_message(id, body, event_key, true));
    }
}

/// Drain the manager's Normal-mode pending-accept queue and render each
/// prompt in the buffer that corresponds to the channel carried in the
/// KEYREQ payload. Falls back to the active-or-server buffer if that
/// channel has no local buffer yet (e.g. PM pseudochannel before the
/// Query buffer exists).
fn surface_pending_accept_requests(
    state: &mut AppState,
    conn_id: &str,
    mgr: &crate::e2e::E2eManager,
) {
    let requests = mgr.take_pending_accept_requests();
    if requests.is_empty() {
        return;
    }
    for req in requests {
        let target_buffer = if req.channel.is_empty() {
            active_or_server_buffer(state, conn_id)
        } else {
            // Contexts may be network-scoped; buffers key by the wire name.
            let cand = make_buffer_id(conn_id, crate::e2e::wire_context(&req.channel));
            if state.buffers.contains_key(&cand) {
                cand
            } else {
                active_or_server_buffer(state, conn_id)
            }
        };
        let body = format!(
            "Pending key exchange from {who} for {channel}.\n      \
             Run /e2e accept <nick> or /e2e decline <nick>.",
            who = req.nick.as_ref().map_or_else(
                || req.handle.clone(),
                |nick| format!("{nick} ({})", req.handle)
            ),
            channel = crate::e2e::display_context(&req.channel),
        );
        let id = state.next_message_id();
        state.add_message(
            &target_buffer,
            e2e_event_message(id, body, "e2e_pending_accept", true),
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

    #[test]
    fn trust_change_body_handle_changed_excludes_banner_and_carries_details() {
        use crate::e2e::manager::TrustChange;
        let (body, key) = trust_change_body(&TrustChange::HandleChanged {
            old_handle: "~r@a.host".to_string(),
            new_handle: "~r@b.host".to_string(),
            fingerprint: [0xAB; 16],
        })
        .expect("HandleChanged must produce a notice");
        assert_eq!(key, "e2e_warning");
        // The theme's e2e_* template prepends "[E2E]"; baking it into the
        // body would double the banner on the live path.
        assert!(!body.contains("[E2E]"), "body must not embed banner: {body}");
        assert!(body.contains("appeared under new handle"));
        assert!(body.contains("~r@a.host"));
        assert!(body.contains("~r@b.host"));
    }

    #[test]
    fn trust_change_body_known_and_new_produce_no_notice() {
        use crate::e2e::manager::TrustChange;
        assert!(trust_change_body(&TrustChange::Known).is_none());
        assert!(trust_change_body(&TrustChange::New).is_none());
    }

    #[test]
    fn e2e_event_message_carries_body_in_event_params() {
        // Regression guard for the empty-[E2E] live-render bug: the body MUST
        // be in event_params so the theme template's `$*` expands to it.
        let msg = e2e_event_message(7, "WARNING: key changed".to_string(), "e2e_error", true);
        assert!(matches!(msg.message_type, MessageType::Event));
        assert_eq!(msg.event_key.as_deref(), Some("e2e_error"));
        assert_eq!(
            msg.event_params,
            Some(vec!["WARNING: key changed".to_string()]),
            "body must travel through event_params for the live theme template"
        );
        // Stored text keeps the banner for the backlog/fallback path, where
        // event_key is stripped and `text` is rendered verbatim.
        assert_eq!(msg.text, "[E2E] WARNING: key changed");
    }

    #[expect(
        clippy::too_many_lines,
        reason = "flat fixture used by every test in this module"
    )]
    fn make_test_state() -> AppState {
        let mut state = AppState::new();
        state.add_connection(Connection {
            id: "test".to_string(),
            label: "TestServer".to_string(),
            status: ConnectionStatus::Connected,
            own_handle: None,
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
            chathistory: crate::irc::chathistory::HistoryState::new(),
            who_token_counter: 0,
            multiline: None,
            batch_ref_counter: 0,
            silent_who_channels: std::collections::HashSet::new(),
            silent_banlist_channels: std::collections::HashSet::new(),
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
            peer_handle: None,
            log_total_lines: None,
            log_oldest_ts: None,
            log_newest_ts: None,
            history_exhausted: false,
            log_initial_loaded: false,
            pin_backlog: false,
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
            peer_handle: None,
            log_total_lines: None,
            log_oldest_ts: None,
            log_newest_ts: None,
            history_exhausted: false,
            log_initial_loaded: false,
            pin_backlog: false,
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
            peer_handle: None,
            log_total_lines: None,
            log_oldest_ts: None,
            log_newest_ts: None,
            history_exhausted: false,
            log_initial_loaded: false,
            pin_backlog: false,
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

    #[test]
    fn privmsg_flood_exemption_bypasses_duplicate_flood() {
        let mut state = make_test_state();
        state.flood_exemptions.push("*!*@trusted.host".to_string());
        for text in ["spam", "a", "spam", "b", "spam"] {
            let msg = make_irc_msg(
                Some("alice!~user@trusted.host"),
                Command::PRIVMSG("#test".into(), text.into()),
            );
            handle_irc_message(&mut state, "test", &msg);
        }

        let buf = state.buffers.get("test/#test").unwrap();
        let spam_count = buf.messages.iter().filter(|msg| msg.text == "spam").count();
        assert_eq!(spam_count, 3);
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

    #[test]
    fn nick_change_carries_e2e_dm_handle_cache() {
        // Regression: an E2E peer renames while their query buffer is NOT
        // open. The send gate resolves the peer handle by nick via the
        // keyring cache — if the NICK handler leaves the cache keyed under
        // the old nick, `/msg <new_nick>` resolves no handle, misses the
        // enabled `@<handle>` config, and downgrades to PLAINTEXT.
        use crate::e2e::keyring::Keyring;
        use crate::e2e::manager::E2eManager;
        use std::sync::{Arc, Mutex};

        let conn = crate::storage::db::open_database(false).unwrap();
        let mgr = E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(conn)))).unwrap();
        mgr.keyring()
            .cache_dm_handle("TestServer", "frank", "~frank@f.host")
            .unwrap();

        let mut state = make_test_state();
        state.e2e_manager = Some(Arc::new(mgr));
        // No open query buffer for frank — resolution must come from the
        // keyring cache alone.
        let msg = make_irc_msg(Some("frank!user@host"), Command::NICK("frankie".into()));
        handle_irc_message(&mut state, "test", &msg);

        assert_eq!(
            state
                .resolve_query_peer_handle("test/frankie", "frankie")
                .unwrap()
                .as_deref(),
            Some("~frank@f.host"),
            "the send gate must still resolve the peer's handle after the rename"
        );
    }

    // === handle_kick tests ===

    #[test]
    fn kick_our_own_removes_buffer_and_notifies() {
        let mut state = make_test_state();
        let msg = make_irc_msg(
            Some("op!user@host"),
            Command::KICK("#test".into(), "me".into(), Some("behave".into())),
        );
        handle_irc_message(&mut state, "test", &msg);

        // Channel buffer is removed.
        assert!(!state.buffers.contains_key("test/#test"));

        // Kick message appears in server buffer.
        let server_id = make_buffer_id("test", "TestServer");
        let server_buf = state.buffers.get(&server_id).unwrap();
        let server_msg = server_buf.messages.back().unwrap();
        assert!(server_msg.text.contains("You were kicked from #test by op"));
        assert!(server_msg.text.contains("behave"));
        assert!(server_msg.highlight);
        assert_eq!(server_msg.event_key.as_deref(), Some("kicked"));
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

    #[test]
    fn registered_only_and_reop_render_distinct_mode_letters() {
        let registered = [irc::proto::Mode::Plus(
            irc::proto::ChannelMode::RegisteredOnly,
            None,
        )];
        let reop = [irc::proto::Mode::Plus(
            irc::proto::ChannelMode::Reop,
            Some("*!*@ops".to_string()),
        )];

        assert_eq!(build_channel_mode_string(&registered), "+r");
        assert_eq!(build_channel_mode_string(&reop), "+R *!*@ops");
    }

    #[test]
    fn reop_mode_is_list_mode_not_channel_mode() {
        let mut state = make_test_state();
        let msg = IrcMessage::new(
            Some("oper!user@host"),
            "MODE",
            vec!["#test", "+R", "*!*@ops"],
        )
        .unwrap();

        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(buf.modes.as_deref().is_none_or(str::is_empty));
        assert_eq!(
            buf.messages.back().unwrap().text,
            "oper sets mode +R *!*@ops on #test"
        );
    }

    #[test]
    fn ban_mode_adds_cached_ban_entry() {
        let mut state = make_test_state();
        let msg = IrcMessage::new(
            Some("oper!user@host"),
            "MODE",
            vec!["#test", "+b", "*!*@bad.example"],
        )
        .unwrap();

        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        let bans = buf.list_modes.get(BAN_MODE_KEY).unwrap();
        assert_eq!(bans.len(), 1);
        assert_eq!(bans[0].mask, "*!*@bad.example");
        assert_eq!(bans[0].set_by, "oper");
        assert!(bans[0].set_at > 0);
    }

    #[test]
    fn unban_mode_removes_cached_ban_entry_case_insensitively() {
        let mut state = make_test_state();

        for (mode, mask) in [("+b", "*!*@Bad.Example"), ("-b", "*!*@bad.example")] {
            let msg =
                IrcMessage::new(Some("oper!user@host"), "MODE", vec!["#test", mode, mask]).unwrap();
            handle_irc_message(&mut state, "test", &msg);
        }

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(buf.list_modes.get(BAN_MODE_KEY).is_none_or(Vec::is_empty));
    }

    #[test]
    fn list_mode_batches_preserve_parameters_in_display() {
        let cases = [
            ("+RRR", "oper sets mode +RRR a b c on #test"),
            ("-RRR", "oper sets mode -RRR a b c on #test"),
            ("+eee", "oper sets mode +eee a b c on #test"),
            ("-eee", "oper sets mode -eee a b c on #test"),
            ("+III", "oper sets mode +III a b c on #test"),
            ("-III", "oper sets mode -III a b c on #test"),
        ];

        for (mode, expected) in cases {
            let mut state = make_test_state();
            let msg = IrcMessage::new(
                Some("oper!user@host"),
                "MODE",
                vec!["#test", mode, "a", "b", "c"],
            )
            .unwrap();

            handle_irc_message(&mut state, "test", &msg);

            let buf = state.buffers.get("test/#test").unwrap();
            assert_eq!(buf.messages.back().unwrap().text, expected);
        }
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

    #[test]
    fn disconnect_drops_the_negotiated_caps_and_isupport() {
        // Both are only ever (re)populated at RPL_WELCOME, but the IRC handle is
        // re-inserted at `HandleReady` — as soon as the socket is up, long before
        // CAP negotiation. Anything that consults the caps between the two (the
        // `+typing` gate does, on every tick) would be reading the PREVIOUS
        // session's answer, on a server that may have changed it.
        let mut state = make_test_state();
        {
            let conn = state.connections.get_mut("test").expect("conn");
            conn.enabled_caps.insert("message-tags".to_string());
            conn.isupport_parsed.parse_tokens(&["CLIENTTAGDENY=*"]);
        }

        handle_disconnected(&mut state, "test", None);

        let conn = state.connections.get("test").expect("conn");
        assert!(conn.enabled_caps.is_empty(), "stale caps survived the drop");
        assert!(
            conn.isupport_parsed.client_tag_allowed("typing"),
            "stale ISUPPORT survived the drop"
        );
    }

    #[test]
    fn disconnect_clears_typing_on_that_connection_only() {
        // No `done` can arrive over a socket that is gone: without this the peers
        // stay "typing" for the full TTL and survive into the reconnect.
        //
        // The "…only" half needs a SECOND connection to mean anything: one
        // network dropping says nothing about another's peers, whose sockets are
        // still up and will send their own `done`. (The tracker-level scoping is
        // covered by `clear_connection_drops_every_buffer_on_that_connection` in
        // `state/typing.rs`; this pins the wiring of it to `handle_disconnected`.)
        let mut state = make_test_state();
        let mut other = state.connections.get("test").expect("conn").clone();
        other.id = "other".to_string();
        other.label = "OtherServer".to_string();
        state.add_connection(other);
        state.add_buffer(make_channel_buffer("test", "#rust"));
        state.add_buffer(make_channel_buffer("other", "#rust"));
        handle_irc_message(&mut state, "test", &tagmsg("alice", "#rust", "+typing", "active"));
        handle_irc_message(&mut state, "other", &tagmsg("bob", "#rust", "+typing", "active"));
        assert_eq!(state.typing.nicks("test/#rust"), vec!["alice"]);
        assert_eq!(state.typing.nicks("other/#rust"), vec!["bob"]);
        state.pending_web_events.clear();

        handle_disconnected(&mut state, "test", None);

        assert!(state.typing.nicks("test/#rust").is_empty());
        assert_eq!(
            state.typing.nicks("other/#rust"),
            vec!["bob"],
            "a drop on one connection must not clear another's typing"
        );
        // The web clients are told, or their indicator hangs there forever.
        assert!(
            state.pending_web_events.iter().any(|e| matches!(
                e,
                crate::web::protocol::WebEvent::Typing { buffer_id, nicks }
                    if buffer_id == "test/#rust" && nicks.is_empty()
            )),
            "the cleared set must be pushed to the web clients"
        );
        // ...and only about the buffer that actually changed.
        assert!(
            !state.pending_web_events.iter().any(|e| matches!(
                e,
                crate::web::protocol::WebEvent::Typing { buffer_id, .. }
                    if buffer_id == "other/#rust"
            )),
            "the untouched connection must not be broadcast as cleared"
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

    // === ircnet.com/extended-join tests ===
    // Wire format: :src JOIN <channel> <uid> <ip> <netjoin> <account> :<realname>
    // Six args exceed irc-proto's JOIN arity, so it arrives as Command::Raw.

    fn enable_ircnet_extended_join(state: &mut AppState) {
        state
            .connections
            .get_mut("test")
            .unwrap()
            .enabled_caps
            .insert("ircnet.com/extended-join".to_string());
    }

    #[test]
    fn ircnet_extended_join_without_cap_ignores_extension_fields() {
        // A 6-arg JOIN from a server that did NOT ack the cap has unknown
        // field semantics — the join must still land (args[0] is always the
        // channel) but nothing should be read as account/realname.
        let mut state = make_test_state();
        let msg = make_irc_msg(
            Some("carol!user@host"),
            Command::Raw(
                "JOIN".to_string(),
                vec![
                    "#test".to_string(),
                    "key1".to_string(),
                    "key2".to_string(),
                    "0".to_string(),
                    "something".to_string(),
                    "trailing".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(buf.users.contains_key("carol"), "join itself must land");
        assert_eq!(
            buf.users.get("carol").unwrap().account,
            None,
            "args[4] must not be trusted as account without the cap"
        );
        let m = buf.messages.back().unwrap();
        let params = m.event_params.as_ref().unwrap();
        assert_eq!(params[4], "", "no account display without the cap");
        assert_eq!(params[5], "", "no realname display without the cap");
        assert_eq!(params[6], "", "no uid display without the cap");
    }

    #[test]
    fn ircnet_extended_join_with_account_and_realname() {
        let mut state = make_test_state();
        enable_ircnet_extended_join(&mut state);
        let msg = make_irc_msg(
            Some("ejtest435!~ejtest435@92.206.50.109"),
            Command::Raw(
                "JOIN".to_string(),
                vec![
                    "#test".to_string(),
                    "528HAAD32".to_string(),
                    "92.206.50.109".to_string(),
                    "0".to_string(),
                    "acct".to_string(),
                    "extended-join test".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(buf.users.contains_key("ejtest435"));
        assert_eq!(
            buf.users.get("ejtest435").unwrap().account.as_deref(),
            Some("acct")
        );
        let m = buf.messages.back().unwrap();
        assert_eq!(m.event_key.as_deref(), Some("join"));
        let params = m.event_params.as_ref().unwrap();
        assert_eq!(params[4], "[acct]"); // $4 = account
        assert_eq!(params[5], "extended-join test"); // $5 = realname
        assert_eq!(params[6], "[528HAAD32 92.206.50.109]"); // $6 = uid+ip
        assert!(m.text.contains("[528HAAD32 92.206.50.109]"));
    }

    #[test]
    fn ircnet_extended_join_star_account_is_none() {
        let mut state = make_test_state();
        enable_ircnet_extended_join(&mut state);
        let msg = make_irc_msg(
            Some("ejtest435!~ejtest435@92.206.50.109"),
            Command::Raw(
                "JOIN".to_string(),
                vec![
                    "#test".to_string(),
                    "528HAAD32".to_string(),
                    "92.206.50.109".to_string(),
                    "0".to_string(),
                    "*".to_string(),
                    "extended-join test".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert_eq!(buf.users.get("ejtest435").unwrap().account, None);
        let m = buf.messages.back().unwrap();
        let params = m.event_params.as_ref().unwrap();
        assert_eq!(params[4], ""); // no account display
        assert_eq!(params[5], "extended-join test");
    }

    #[test]
    fn ircnet_extended_join_own_join_creates_buffer() {
        let mut state = make_test_state();
        enable_ircnet_extended_join(&mut state);
        let msg = make_irc_msg(
            Some("me!~me@host.example"),
            Command::Raw(
                "JOIN".to_string(),
                vec![
                    "#testy".to_string(),
                    "528HAAD32".to_string(),
                    "10.0.0.1".to_string(),
                    "0".to_string(),
                    "*".to_string(),
                    "my realname".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        assert!(state.buffers.contains_key("test/#testy"));
        assert_eq!(state.active_buffer_id.as_deref(), Some("test/#testy"));
    }

    #[test]
    fn ircnet_extended_join_netjoin_flag_untracked_still_displays() {
        // netjoin=1 without a tracked split (we saw no QUITs) must not be
        // silently dropped — the join line still shows.
        let mut state = make_test_state();
        enable_ircnet_extended_join(&mut state);
        let msg = make_irc_msg(
            Some("carol!user@host"),
            Command::Raw(
                "JOIN".to_string(),
                vec![
                    "#test".to_string(),
                    "528HAAD32".to_string(),
                    "10.0.0.1".to_string(),
                    "1".to_string(),
                    "*".to_string(),
                    "Real Name".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(buf.users.contains_key("carol"));
        let m = buf.messages.back().unwrap();
        assert_eq!(m.event_key.as_deref(), Some("join"));
    }

    #[test]
    fn ircnet_extended_join_full_wire_line_shows_uid_and_ip() {
        // End-to-end through the real irc-proto parser: the exact line an
        // IRCnet 2.12.0 server emits (channel.c sendto_channel_butserv_caps,
        // CAP_IRCNET_EXTENDED_JOIN branch). Guards against parse-layer drift
        // that Command::Raw-constructing tests can't see.
        use std::str::FromStr as _;
        let mut state = make_test_state();
        enable_ircnet_extended_join(&mut state);
        let msg = IrcMessage::from_str(
            ":ejtest435!~ejtest435@92.206.50.109 JOIN #test 528HAAD32 92.206.50.109 0 * :extended-join test\r\n",
        )
        .expect("ircnet extended-join wire line must parse");
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert!(buf.users.contains_key("ejtest435"));
        let m = buf.messages.back().unwrap();
        assert_eq!(m.event_key.as_deref(), Some("join"));
        assert!(
            m.text.contains("528HAAD32"),
            "uid must appear in text (web renders text): {}",
            m.text
        );
        let params = m.event_params.as_ref().unwrap();
        assert_eq!(
            params.get(6).map(String::as_str),
            Some("[528HAAD32 92.206.50.109]"),
            "$6 must carry uid+ip for themed rendering"
        );
    }

    #[test]
    fn standard_extended_join_has_empty_uid_param() {
        // IRCv3 extended-join (3-arg Command::JOIN) has no uid/ip — $6 must
        // exist (themes reference it) but stay empty.
        let mut state = make_test_state();
        let msg = make_irc_msg(
            Some("dave!user@host"),
            Command::JOIN("#test".into(), Some("acct".into()), Some("Real Name".into())),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        let m = buf.messages.back().unwrap();
        let params = m.event_params.as_ref().unwrap();
        assert_eq!(params.get(6).map(String::as_str), Some(""));
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
            peer_handle: None,
            log_total_lines: None,
            log_oldest_ts: None,
            log_newest_ts: None,
            history_exhausted: false,
            log_initial_loaded: false,
            pin_backlog: false,
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
            peer_handle: None,
            log_total_lines: None,
            log_oldest_ts: None,
            log_newest_ts: None,
            history_exhausted: false,
            log_initial_loaded: false,
            pin_backlog: false,
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
    fn chghost_updates_query_peer_handle() {
        let mut state = make_test_state();
        // A DM/query buffer caching the peer's old handle. The peer is not in
        // a nicklist, so the nicklist update misses it — but the E2E encrypt
        // context keys on peer_handle and must track the new host.
        let q_id = make_buffer_id("test", "bob");
        state.add_buffer(Buffer {
            id: q_id,
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
            peer_handle: Some("~bob@old.host".to_string()),
            log_total_lines: None,
            log_oldest_ts: None,
            log_newest_ts: None,
            history_exhausted: false,
            log_initial_loaded: false,
            pin_backlog: false,
        });

        let msg = make_irc_msg(
            Some("bob!~bob@old.host"),
            Command::CHGHOST("~bob".into(), "user/bob".into()),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/bob").unwrap();
        assert_eq!(buf.peer_handle.as_deref(), Some("~bob@user/bob"));
    }

    #[test]
    fn keyreq_first_signal_migrates_dm_handle_before_policy_lookup() {
        use crate::e2e::keyring::{ChannelConfig, ChannelMode, Keyring};
        use crate::e2e::manager::E2eManager;
        use std::sync::{Arc, Mutex};

        // Peer "alice" had an enabled DM E2E config at ~alice@old.host. She
        // reconnects from ~alice@new.host and — with no shared channel, so no
        // CHGHOST, and before sending any PRIVMSG — her FIRST signal of the new
        // host is an RPE2E KEYREQ. Its recipient-keyed context is
        // @~alice@new.host; unless the enabled config is migrated from
        // @~alice@old.host BEFORE the policy lookup, `effective_channel_mode`
        // finds nothing under `req.channel`, the handshake is silently dropped
        // (no KEYRSP), and the encrypt path keeps keying on the stale handle.
        let old_handle = "~alice@old.host";
        let new_handle = "~alice@new.host";
        let old_ctx = format!("@{old_handle}");
        let new_ctx = format!("@{new_handle}");

        // Bob (us): manager with the DM enabled under the OLD context only,
        // AutoAccept so a successful lookup auto-responds with a KEYRSP.
        let bob_conn = crate::storage::db::open_database(false).unwrap();
        let bob = E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(bob_conn)))).unwrap();
        bob.keyring()
            .set_channel_config(&ChannelConfig {
                channel: old_ctx,
                enabled: true,
                mode: ChannelMode::AutoAccept,
            })
            .unwrap();

        // Alice: builds a real, signed, parseable KEYREQ stamped with her NEW
        // handle (recipient-keyed by her own handle).
        let alice_conn = crate::storage::db::open_database(false).unwrap();
        let alice =
            E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(alice_conn)))).unwrap();
        let req = alice.build_keyreq(&new_ctx).unwrap();
        let ctcp = alice.encode_keyreq_ctcp(&req);

        // Bob's AppState: manager attached, an open query buffer for "alice"
        // still caching her OLD handle.
        let mut state = make_test_state();
        state.e2e_manager = Some(Arc::new(bob));
        state.add_buffer(Buffer {
            id: make_buffer_id("test", "alice"),
            connection_id: "test".to_string(),
            buffer_type: BufferType::Query,
            name: "alice".to_string(),
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
            peer_handle: Some(old_handle.to_string()),
            log_total_lines: None,
            log_oldest_ts: None,
            log_newest_ts: None,
            history_exhausted: false,
            log_initial_loaded: false,
            pin_backlog: false,
        });

        // Deliver the KEYREQ as a NOTICE from alice's NEW host.
        let msg = make_irc_msg(
            Some(&format!("alice!{new_handle}")),
            Command::NOTICE("me".into(), ctcp),
        );
        handle_irc_message(&mut state, "test", &msg);

        let mgr = state.e2e_manager.as_ref().unwrap();
        // The enabled DM config followed the handle to the new context —
        // written network-scoped (the test connection's label).
        assert!(
            mgr.keyring()
                .get_channel_config(&crate::e2e::scoped_context("TestServer", &new_ctx))
                .unwrap()
                .is_some_and(|c| c.enabled),
            "KEYREQ-first handle change must migrate the enabled DM config to @<new>"
        );
        // ...so the policy lookup succeeds and a KEYRSP is queued (not dropped)...
        assert!(
            !state.pending_e2e_sends.is_empty(),
            "migrated config must let the KEYREQ produce a KEYRSP instead of being silently dropped"
        );
        // ...and the query buffer now keys the encrypt path on the new handle.
        assert_eq!(
            state.buffers.get("test/alice").unwrap().peer_handle.as_deref(),
            Some(new_handle)
        );
    }

    #[test]
    fn e2e_awaiting_own_identity_placeholder_is_transient_not_logged() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let mut state = make_test_state();
        state.log_tx = Some(tx);
        // own_handle is None (make_test_state default), so an encrypted DM can't
        // be decrypted yet: handle_privmsg shows the "awaiting our own identity"
        // placeholder. The handle-learned hook re-fetches and decrypts this exact
        // line via CHATHISTORY, so the placeholder must NOT be logged under the
        // server @msgid — otherwise the decryptable replay collapses into
        // INSERT OR IGNORE on the unique (network, msg_id) index and is lost.
        let mut msg = make_irc_msg(
            Some("alice!~alice@her.host"),
            Command::PRIVMSG("me".into(), "+RPE2E01 c=@~me@my.host m=ciphertext".into()),
        );
        msg.tags = Some(vec![irc::proto::message::Tag(
            "msgid".to_string(),
            Some("server-msgid-1".to_string()),
        )]);
        handle_irc_message(&mut state, "test", &msg);

        // The placeholder is rendered live in the query buffer...
        let buf = state.buffers.get("test/alice").unwrap();
        let placeholder = buf
            .messages
            .iter()
            .find(|m| m.text == "[E2E: awaiting our own identity]")
            .expect("placeholder must be rendered live");
        // ...carrying NO @msgid, so the later decrypted CHATHISTORY replay (same
        // server @msgid) isn't deduped against it in surface_history_rows.
        assert!(
            placeholder.tags.is_none(),
            "placeholder must drop the server tags/@msgid so the decrypted replay can surface"
        );
        // ...and is NOT persisted, so the server @msgid stays free for the
        // post-handle decrypted replay.
        assert!(
            rx.try_recv().is_err(),
            "awaiting-own-identity placeholder must not be logged under the server @msgid"
        );
    }

    #[test]
    fn userhost_deferred_forget_clears_own_context_incoming_session() {
        use crate::e2e::keyring::{IncomingSession, Keyring, TrustStatus};
        use crate::e2e::manager::E2eManager;
        use std::sync::{Arc, Mutex};

        // DM with peer "alice". Recipient-keyed: alice's TRUSTED incoming session
        // (her messages → us) lives under OUR own context @<own>, not @<alice>.
        // `/e2e forget alice` (a bare nick) defers to USERHOST; the reply handler
        // must forget BOTH contexts, else alice's messages keep decrypting after
        // the user was told she was forgotten.
        let own_handle = "~me@my.host";
        let alice_handle = "~alice@her.host";
        let own_ctx = format!("@{own_handle}");
        let peer_ctx = format!("@{alice_handle}");

        let conn = crate::storage::db::open_database(false).unwrap();
        let mgr = E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(conn)))).unwrap();
        mgr.keyring()
            .install_incoming_session_strict(&IncomingSession {
                handle: alice_handle.to_string(),
                channel: own_ctx.clone(),
                fingerprint: [0x11; 16],
                sk: [0x22; 32],
                status: TrustStatus::Trusted,
                created_at: 0,
            })
            .unwrap();
        assert!(
            mgr.keyring()
                .get_incoming_session(alice_handle, &own_ctx)
                .unwrap()
                .is_some(),
            "precondition: incoming session installed under @<own>"
        );

        let mut state = make_test_state();
        state.e2e_manager = Some(Arc::new(mgr));
        // Our own handle must be known so the deferred handler can recompute the
        // recipient-keyed @<own> context at execution time.
        if let Some(conn) = state.connections.get_mut("test") {
            conn.own_handle = Some(own_handle.to_string());
        }
        // Queue the deferred forget exactly as e2e_forget does. No @<own> is
        // captured — the reply handler derives it from the current own handle.
        state
            .pending_userhost_requests
            .push(crate::state::PendingUserhostRequest {
                connection_id: "test".to_string(),
                nick: "alice".to_string(),
                action: crate::state::PendingUserhostAction::E2eForget {
                    buffer_id: make_buffer_id("test", "alice"),
                    target: "alice".to_string(),
                    channel: Some(peer_ctx),
                    all: false,
                },
            });

        // USERHOST reply resolving alice's handle.
        handle_userhost_reply(
            &mut state,
            "test",
            &["me".to_string(), format!("alice=+{alice_handle}")],
        );

        let mgr = state.e2e_manager.as_ref().unwrap();
        assert!(
            mgr.keyring()
                .get_incoming_session(alice_handle, &own_ctx)
                .unwrap()
                .is_none(),
            "deferred forget must delete the recipient-keyed @<own> incoming session"
        );
    }

    #[test]
    fn deferred_e2e_forget_uses_current_own_handle_after_chghost() {
        // Regression: a CHGHOST that moves OUR own handle between `/e2e forget`
        // and the USERHOST reply must not strand the forget under a stale @<own>.
        // The peer's live incoming session lives under the CURRENT @<own>; the
        // deferred handler recomputes that context rather than using a value
        // captured at command time.
        use crate::e2e::keyring::{IncomingSession, Keyring, TrustStatus};
        use crate::e2e::manager::E2eManager;
        use std::sync::{Arc, Mutex};

        let new_own = "~me@new.host";
        let alice_handle = "~alice@her.host";
        let new_own_ctx = format!("@{new_own}");

        let conn = crate::storage::db::open_database(false).unwrap();
        let mgr = E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(conn)))).unwrap();
        // Alice's live incoming session is keyed under our NEW own context.
        mgr.keyring()
            .install_incoming_session_strict(&IncomingSession {
                handle: alice_handle.to_string(),
                channel: new_own_ctx.clone(),
                fingerprint: [0x11; 16],
                sk: [0x22; 32],
                status: TrustStatus::Trusted,
                created_at: 0,
            })
            .unwrap();

        let mut state = make_test_state();
        state.e2e_manager = Some(Arc::new(mgr));
        // CHGHOST already moved our handle to the new value by the time the
        // reply is processed.
        if let Some(conn) = state.connections.get_mut("test") {
            conn.own_handle = Some(new_own.to_string());
        }
        state
            .pending_userhost_requests
            .push(crate::state::PendingUserhostRequest {
                connection_id: "test".to_string(),
                nick: "alice".to_string(),
                action: crate::state::PendingUserhostAction::E2eForget {
                    buffer_id: make_buffer_id("test", "alice"),
                    target: "alice".to_string(),
                    channel: Some(format!("@{alice_handle}")),
                    all: false,
                },
            });

        handle_userhost_reply(
            &mut state,
            "test",
            &["me".to_string(), format!("alice=+{alice_handle}")],
        );

        let mgr = state.e2e_manager.as_ref().unwrap();
        assert!(
            mgr.keyring()
                .get_incoming_session(alice_handle, &new_own_ctx)
                .unwrap()
                .is_none(),
            "deferred forget must clear the session under the CURRENT @<own>, not a stale one"
        );
    }

    #[test]
    fn deferred_e2e_forget_refuses_when_own_handle_unknown() {
        // Regression: with our own handle unknown (reset at every registration
        // until the self-USERHOST reply), a DM forget cannot reach the
        // recipient-keyed @<own> incoming session. It must REFUSE — a partial
        // forget that clears only @<peer> while reporting success leaves the
        // peer's trusted session decrypting their messages.
        use crate::e2e::keyring::{IncomingSession, Keyring, TrustStatus};
        use crate::e2e::manager::E2eManager;
        use std::sync::{Arc, Mutex};

        let own_handle = "~me@my.host";
        let alice_handle = "~alice@her.host";
        let own_ctx = format!("@{own_handle}");

        let conn = crate::storage::db::open_database(false).unwrap();
        let mgr = E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(conn)))).unwrap();
        mgr.keyring()
            .install_incoming_session_strict(&IncomingSession {
                handle: alice_handle.to_string(),
                channel: own_ctx.clone(),
                fingerprint: [0x11; 16],
                sk: [0x22; 32],
                status: TrustStatus::Trusted,
                created_at: 0,
            })
            .unwrap();

        let mut state = make_test_state();
        state.e2e_manager = Some(Arc::new(mgr));
        // own_handle stays None (make_test_state default) — the reconnect gap.
        state
            .pending_userhost_requests
            .push(crate::state::PendingUserhostRequest {
                connection_id: "test".to_string(),
                nick: "alice".to_string(),
                action: crate::state::PendingUserhostAction::E2eForget {
                    buffer_id: make_buffer_id("test", "alice"),
                    target: "alice".to_string(),
                    channel: Some(format!("@{alice_handle}")),
                    all: false,
                },
            });

        handle_userhost_reply(
            &mut state,
            "test",
            &["me".to_string(), format!("alice=+{alice_handle}")],
        );

        let mgr = state.e2e_manager.as_ref().unwrap();
        assert!(
            mgr.keyring()
                .get_incoming_session(alice_handle, &own_ctx)
                .unwrap()
                .is_some(),
            "refused forget must leave the @<own> session intact (no partial forget)"
        );
    }

    #[test]
    fn rpe2e_dispatch_swallows_own_echo_without_caching_own_handle() {
        // echo-message: the server echoes our own KEYREQ/KEYRSP NOTICEs back
        // with our full prefix. The dispatch must swallow the echo WITHOUT
        // caching our own nick→handle in the PEER handle cache (that row would
        // later resolve a stranger who takes our old nick to OUR handle) and
        // without feeding our own handshake into the KEYREQ handler.
        use crate::e2e::keyring::Keyring;
        use crate::e2e::manager::E2eManager;
        use std::sync::{Arc, Mutex};

        let conn = crate::storage::db::open_database(false).unwrap();
        let mgr = E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(conn)))).unwrap();
        let mut state = make_test_state();
        state.e2e_manager = Some(Arc::new(mgr));

        let prefix = Prefix::new_from_str("me!~me@my.host");
        let text = format!("\x01{} KEYREQ v=1 c=@x garbage\x01", crate::e2e::handshake::CTCP_TAG);
        let outcome = try_dispatch_rpe2e_ctcp(&mut state, "test", Some(&prefix), "bob", &text);

        assert_eq!(
            outcome,
            Some(RpEe2eOutcome::Handled),
            "own echo must be swallowed, not rendered or re-processed"
        );
        let mgr = state.e2e_manager.as_ref().unwrap();
        assert_eq!(
            mgr.keyring().cached_dm_handle("me", "TestServer").unwrap(),
            None,
            "own nick must never enter the PEER handle cache"
        );
    }

    #[test]
    fn rpe2e_dispatch_tracks_peer_handle_even_when_parse_fails() {
        // A handshake-looking message that fails to parse (truncated relay,
        // newer protocol revision) still carries a server-stamped prefix — and
        // handle_privmsg's generic tracking is suppressed for handshake-looking
        // texts on the assumption that the dispatcher updates the handle. The
        // observation must therefore happen BEFORE the parse early-returns.
        use crate::e2e::keyring::Keyring;
        use crate::e2e::manager::E2eManager;
        use std::sync::{Arc, Mutex};

        let conn = crate::storage::db::open_database(false).unwrap();
        let mgr = E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(conn)))).unwrap();
        let mut state = make_test_state();
        state.e2e_manager = Some(Arc::new(mgr));

        let prefix = Prefix::new_from_str("bob!~bob@new.host");
        let text = format!("\x01{} KEYREQ truncated-garbage\x01", crate::e2e::handshake::CTCP_TAG);
        let _ = try_dispatch_rpe2e_ctcp(&mut state, "test", Some(&prefix), "me", &text);

        let mgr = state.e2e_manager.as_ref().unwrap();
        assert_eq!(
            mgr.keyring()
                .cached_dm_handle("bob", "TestServer")
                .unwrap()
                .as_deref(),
            Some("~bob@new.host"),
            "peer handle must be tracked even when the handshake body fails to parse"
        );
    }

    #[test]
    fn legacy_nick_match_migration_caps_autotrust_and_warns() {
        // The legacy e2e_peers fallback matches by nick alone across ALL
        // networks. When it is the ONLY migration source (fresh cache, upgrade
        // path), the enabled config may belong to a same-nick peer elsewhere:
        // the migration must not carry AutoAccept across (a stranger's
        // handshake would be silently trusted) and must tell the user.
        use crate::e2e::keyring::{
            ChannelConfig, ChannelMode, Keyring, PeerRecord, TrustStatus,
        };
        use crate::e2e::manager::E2eManager;
        use std::sync::{Arc, Mutex};

        let old_net_handle = "~bob@neta.host";
        let new_net_handle = "~bob@netb.host";

        let conn = crate::storage::db::open_database(false).unwrap();
        let mgr = E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(conn)))).unwrap();
        // Pre-upgrade keyring: bob is an enabled AutoAccept E2E peer on NetA,
        // recorded only in e2e_peers; the network-scoped cache is empty.
        mgr.keyring()
            .upsert_peer(&PeerRecord {
                fingerprint: [0x33; 16],
                pubkey: [0x44; 32],
                last_handle: Some(old_net_handle.to_string()),
                last_nick: Some("bob".to_string()),
                first_seen: 0,
                last_seen: 100,
                global_status: TrustStatus::Trusted,
            })
            .unwrap();
        mgr.keyring()
            .set_channel_config(&ChannelConfig {
                channel: format!("@{old_net_handle}"),
                enabled: true,
                mode: ChannelMode::AutoAccept,
            })
            .unwrap();

        let mut state = make_test_state();
        state.e2e_manager = Some(Arc::new(mgr));
        // Open query buffer for bob so the warning lands somewhere visible.
        let bob_buf = make_buffer_id("test", "bob");
        let mut buf = make_channel_buffer("test", "bob");
        buf.buffer_type = BufferType::Query;
        state.add_buffer(buf);

        // First DM from a "bob" on this network (prev buffer handle: none).
        track_dm_handle_change(&mut state, "test", "bob", None, new_net_handle);

        let mgr = state.e2e_manager.as_ref().unwrap();
        let migrated = mgr
            .keyring()
            .get_channel_config(&crate::e2e::scoped_context(
                "TestServer",
                &format!("@{new_net_handle}"),
            ))
            .unwrap()
            .expect("config must migrate so the DM never downgrades to plaintext");
        assert!(migrated.enabled);
        assert_eq!(
            migrated.mode,
            ChannelMode::Normal,
            "AutoAccept must be capped to Normal for a network-agnostic nick match"
        );
        let buf = state.buffers.get(&bob_buf).expect("bob query buffer");
        assert!(
            buf.messages
                .iter()
                .any(|m| m.text.contains("pre-upgrade nick match")),
            "the legacy-sourced enable must be surfaced to the user"
        );
    }

    #[test]
    fn own_ciphertext_echo_without_own_handle_is_swallowed() {
        // A nick-only prefix on our own echoed E2E DM (no ident@host to seed
        // own_handle) must not render the raw +RPE2E01 wire line: the
        // plaintext was already echoed locally at send time.
        let mut state = make_test_state();
        let prefix = Prefix::new_from_str("me");
        handle_privmsg(
            &mut state,
            "test",
            "me",
            Some(&prefix),
            "bob",
            "+RPE2E01 AAAA BBBB",
            None,
        );
        assert!(
            !state.buffers.contains_key(&make_buffer_id("test", "bob")),
            "swallowed echo must not create a buffer with raw ciphertext"
        );
    }

    #[test]
    fn incoming_e2e_context_is_recipient_keyed_for_dms() {
        // Channel: always Some(name), handles irrelevant.
        let sc = |wire: &str| crate::e2e::scoped_context("Net", wire);
        assert_eq!(
            incoming_e2e_context("Net", "#x", Some("~me@h")),
            Some(sc("#x"))
        );
        assert_eq!(incoming_e2e_context("Net", "#x", None), Some(sc("#x")));
        // DM (target is our own nick) with own handle known: keyed by OUR own
        // handle (recipient) — what the sender encrypted the AAD under.
        assert_eq!(
            incoming_e2e_context("Net", "me", Some("~me@host")),
            Some(sc("@~me@host"))
        );
        // DM with own handle UNKNOWN: None — must NOT fall back to the sender
        // (that would fire a KEYREQ for the wrong DM direction); the caller
        // waits until our handle is learned (USERHOST / echo / CHGHOST).
        assert_eq!(incoming_e2e_context("Net", "me", None), None);
    }

    // === own-handle tracking (for recipient-keyed DM E2E) ===

    #[test]
    fn own_handle_captured_from_echo_message() {
        let mut state = make_test_state(); // nick "me", conn "test"
        // echo-message echoes our own outgoing PM back with our full prefix.
        let msg = make_irc_msg(
            Some("me!~me@host.example"),
            Command::PRIVMSG("bob".into(), "hi".into()),
        );
        handle_irc_message(&mut state, "test", &msg);
        assert_eq!(
            state.connections.get("test").unwrap().own_handle.as_deref(),
            Some("~me@host.example")
        );
    }

    #[test]
    fn own_handle_captured_from_own_chghost() {
        let mut state = make_test_state();
        let msg = make_irc_msg(
            Some("me!~me@old.host"),
            Command::CHGHOST("~me".into(), "user/me".into()),
        );
        handle_irc_message(&mut state, "test", &msg);
        assert_eq!(
            state.connections.get("test").unwrap().own_handle.as_deref(),
            Some("~me@user/me")
        );
    }

    #[test]
    fn own_handle_captured_from_self_userhost_reply() {
        let mut state = make_test_state();
        // RPL_USERHOST with no pending request must still seed our own handle.
        handle_userhost_reply(
            &mut state,
            "test",
            &["me".to_string(), "me=+~me@host.example".to_string()],
        );
        assert_eq!(
            state.connections.get("test").unwrap().own_handle.as_deref(),
            Some("~me@host.example")
        );
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
            peer_handle: None,
            log_total_lines: None,
            log_oldest_ts: None,
            log_newest_ts: None,
            history_exhausted: false,
            log_initial_loaded: false,
            pin_backlog: false,
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
    fn cap_new_captures_multiline_limits_and_del_clears() {
        let mut state = make_test_state();
        handle_cap_new(
            &mut state,
            "test",
            Some("draft/multiline=max-bytes=4096,max-lines=24"),
            None,
        );
        assert_eq!(
            state.connections.get("test").unwrap().multiline,
            Some(crate::irc::multiline::MultilineLimits {
                max_bytes: 4096,
                max_lines: 24
            })
        );

        handle_cap_del(&mut state, "test", Some("draft/multiline"), None);
        assert!(state.connections.get("test").unwrap().multiline.is_none());
    }

    #[test]
    fn cap_nak_clears_multiline_limits() {
        let mut state = make_test_state();
        handle_cap_new(&mut state, "test", Some("draft/multiline=max-bytes=8192"), None);
        assert!(state.connections.get("test").unwrap().multiline.is_some());
        handle_cap_nak(&mut state, "test", Some("draft/multiline"), None);
        assert!(state.connections.get("test").unwrap().multiline.is_none());
    }

    #[test]
    fn fail_batch_multiline_surfaces_error() {
        let mut state = make_test_state();
        let msg = IrcMessage {
            tags: None,
            prefix: None,
            command: Command::Raw(
                "FAIL".to_string(),
                vec![
                    "BATCH".to_string(),
                    "MULTILINE_MAX_LINES".to_string(),
                    "24".to_string(),
                    "too many lines".to_string(),
                ],
            ),
        };
        handle_irc_message(&mut state, "test", &msg);
        let found = state.buffers.values().any(|b| {
            b.messages
                .iter()
                .any(|m| m.text.contains("MULTILINE_MAX_LINES"))
        });
        assert!(found, "FAIL BATCH MULTILINE_* should surface a status message");
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
            peer_handle: None,
            log_total_lines: None,
            log_oldest_ts: None,
            log_newest_ts: None,
            history_exhausted: false,
            log_initial_loaded: false,
            pin_backlog: false,
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
    fn whoreply_strips_hopcount_from_realname() {
        // 352 trailing is ":<hopcount> <realname>" — the hopcount must not
        // leak into the displayed realname.
        let mut state = make_whox_state();
        state.set_active_buffer("test/testserver");

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
                    "H".to_string(),
                    "3 Real Name".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/testserver").unwrap();
        let text = &buf.messages.back().unwrap().text;
        assert!(text.contains("Real Name"));
        assert!(!text.contains("3 Real Name"), "hopcount leaked: {text}");
    }

    #[test]
    fn whoreply_ircnet_strips_sid_and_updates_nicklist() {
        // ircnet-ircd (2.11+) sends 352 as ":<hop> <sid> <realname>" — the
        // 4-char SID must be stripped on IRCnet-lineage servers, and the
        // reply must update the nick entry like the WHOX path does.
        let mut state = make_whox_state();
        if let Some(conn) = state.connections.get_mut("test") {
            conn.isupport_parsed.parse_tokens(&["IDCHAN=!:5"]);
        }
        state.set_active_buffer("test/testserver");

        let msg = make_irc_msg(
            None,
            Command::Response(
                Response::RPL_WHOREPLY,
                vec![
                    "me".to_string(),
                    "#test".to_string(),
                    "~alice".to_string(),
                    "host.example".to_string(),
                    "irc.atw.hu".to_string(),
                    "alice".to_string(),
                    "G".to_string(),
                    "2 111A Alice Real".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        let entry = buf.users.get("alice").unwrap();
        assert_eq!(entry.ident.as_deref(), Some("~alice"));
        assert_eq!(entry.host.as_deref(), Some("host.example"));
        assert!(entry.away, "G flag must mark away");

        let server_buf = state.buffers.get("test/testserver").unwrap();
        let text = &server_buf.messages.back().unwrap().text;
        assert!(text.contains("Alice Real"));
        assert!(!text.contains("111A"), "SID leaked into display: {text}");
    }

    #[test]
    fn whoreply_updates_nicklist_even_when_silent() {
        // Auto-WHO on non-WHOX servers replies with 352 — state must update
        // even though nothing is displayed, else the query is wasted.
        let mut state = make_whox_state();
        if let Some(conn) = state.connections.get_mut("test") {
            conn.silent_who_channels.insert("#test".to_string());
        }
        state.set_active_buffer("test/testserver");

        let msg = make_irc_msg(
            None,
            Command::Response(
                Response::RPL_WHOREPLY,
                vec![
                    "me".to_string(),
                    "#test".to_string(),
                    "~alice".to_string(),
                    "host.example".to_string(),
                    "irc.net".to_string(),
                    "alice".to_string(),
                    "G".to_string(),
                    "0 Alice Real".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let server_buf = state.buffers.get("test/testserver").unwrap();
        assert!(
            server_buf.messages.is_empty(),
            "silent WHO must not display"
        );
        let buf = state.buffers.get("test/#test").unwrap();
        let entry = buf.users.get("alice").unwrap();
        assert_eq!(entry.ident.as_deref(), Some("~alice"));
        assert!(entry.away);
    }

    #[test]
    fn whox_reply_with_ircnet_uid_field() {
        // IRCnet's WHOX extension letter 'U' inserts the user's UID between
        // account and realname: 11 args instead of 10.
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
                    "host.example".to_string(),
                    "alice".to_string(),
                    "H".to_string(),
                    "0".to_string(),
                    "528HAAD32".to_string(),
                    "Alice Real".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").unwrap();
        let entry = buf.users.get("alice").unwrap();
        assert_eq!(entry.ident.as_deref(), Some("~alice"));
        assert_eq!(entry.account, None, "account \"0\" means not logged in");

        let server_buf = state.buffers.get("test/testserver").unwrap();
        let text = &server_buf.messages.back().unwrap().text;
        assert!(text.contains("Alice Real"), "realname shifted: {text}");
        assert!(text.contains("528HAAD32"), "uid missing from display: {text}");
    }

    #[test]
    fn whoreply_realname_defensive_edges() {
        // RFC shape: "<hop> <realname>"; ircnet: "<hop> <sid> <realname>".
        // Non-conforming trailings (bouncers/services) must degrade to
        // showing the text verbatim, never eating realname words.
        assert_eq!(whoreply_realname("0 John Doe", false), "John Doe");
        assert_eq!(whoreply_realname("2 111A Alice Real", true), "Alice Real");
        // No leading hopcount → whole trailing is the realname.
        assert_eq!(whoreply_realname("Real Name", false), "Real Name");
        assert_eq!(whoreply_realname("Bob", false), "Bob");
        // Hop only, no realname.
        assert_eq!(whoreply_realname("0", false), "");
        // ircnet-classified but the second token is not SID-shaped
        // ([0-9][0-9A-Z]{3}) — keep it (false-positive lineage guard).
        assert_eq!(whoreply_realname("3 John Smith", true), "John Smith");
        assert_eq!(whoreply_realname("3 111a Alice", true), "111a Alice");
        // SID-shaped single remainder → empty realname.
        assert_eq!(whoreply_realname("3 111A", true), "");
    }

    #[test]
    fn whox_reply_overlong_takes_trailing_as_realname() {
        // A 354 with more args than either known layout (12+) must still
        // show the trailing (always the last arg) as the realname instead
        // of a middle field.
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
                    "host.example".to_string(),
                    "alice".to_string(),
                    "H".to_string(),
                    "0".to_string(),
                    "111A".to_string(),
                    "528HAAD32".to_string(),
                    "Alice Real".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let server_buf = state.buffers.get("test/testserver").unwrap();
        let text = &server_buf.messages.back().unwrap().text;
        assert!(
            text.contains("Alice Real"),
            "realname must come from the trailing: {text}"
        );
    }

    #[test]
    fn next_who_token_stays_within_three_chars_and_nonzero() {
        // Both ircu and ircnet-ircd cap the WHOX token at 3 chars (ircnet
        // silently drops longer ones and replies with token "0").
        let mut state = make_test_state();
        for _ in 0..1500 {
            let token = next_who_token(&mut state, "test");
            assert!(token.len() <= 3, "token too long: {token}");
            assert_ne!(token, "0", "token 0 collides with the server default");
        }
    }

    #[test]
    fn build_whox_who_requests_uid_on_ircnet_lineage() {
        let mut state = make_whox_state();
        if let Some(conn) = state.connections.get_mut("test") {
            conn.isupport_parsed.parse_tokens(&["IDCHAN=!:5"]);
        }
        let (_, fields) = build_whox_who(&mut state, "test", "#test", false).unwrap();
        assert!(
            fields.starts_with("%tcuihnfaUr,"),
            "IRCnet lineage must request the UID field: {fields}"
        );

        // Non-IRCnet WHOX server keeps the standard selector.
        let mut state = make_whox_state();
        let (_, fields) = build_whox_who(&mut state, "test", "#test", false).unwrap();
        assert!(
            fields.starts_with("%tcuihnfar,"),
            "standard servers must not request unknown letters: {fields}"
        );
    }

    #[test]
    fn whois_user_and_server_emit_theme_params() {
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");

        for command in [
            Command::Response(
                Response::RPL_WHOISUSER,
                vec![
                    "me".to_string(),
                    "alice".to_string(),
                    "user".to_string(),
                    "host.example".to_string(),
                    "*".to_string(),
                    "Alice Example".to_string(),
                ],
            ),
            Command::Response(
                Response::RPL_WHOISSERVER,
                vec![
                    "me".to_string(),
                    "alice".to_string(),
                    "irc.example".to_string(),
                    "Example IRCd".to_string(),
                ],
            ),
        ] {
            let msg = make_irc_msg(None, command);
            handle_irc_message(&mut state, "test", &msg);
        }

        let buf = state.buffers.get("test/testserver").unwrap();
        let keys: Vec<&str> = buf
            .messages
            .iter()
            .filter_map(|msg| msg.event_key.as_deref())
            .collect();

        assert_eq!(keys, vec!["whois_header", "whois", "whois_server"]);
        assert_eq!(
            buf.messages[1].event_params.as_deref(),
            Some(
                &[
                    "alice".to_string(),
                    "user".to_string(),
                    "host.example".to_string(),
                    "Alice Example".to_string(),
                ][..]
            )
        );
        assert_eq!(
            buf.messages[2].event_params.as_deref(),
            Some(
                &[
                    "alice".to_string(),
                    "irc.example".to_string(),
                    "Example IRCd".to_string(),
                    " (Example IRCd)".to_string(),
                ][..]
            )
        );
    }

    #[test]
    fn whois_detail_responses_emit_theme_keys() {
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");

        for command in [
            Command::Response(
                Response::RPL_WHOISOPERATOR,
                vec![
                    "me".to_string(),
                    "alice".to_string(),
                    "is an IRC operator".to_string(),
                ],
            ),
            Command::Response(
                Response::RPL_WHOISIDLE,
                vec![
                    "me".to_string(),
                    "alice".to_string(),
                    "65".to_string(),
                    "1700000000".to_string(),
                ],
            ),
            Command::Response(
                Response::RPL_WHOISCHANNELS,
                vec![
                    "me".to_string(),
                    "alice".to_string(),
                    "@#ops +#chat".to_string(),
                ],
            ),
            Command::Response(
                Response::RPL_WHOISCERTFP,
                vec![
                    "me".to_string(),
                    "alice".to_string(),
                    "0123456789abcdef".to_string(),
                ],
            ),
            Command::Response(
                Response::RPL_WHOISKEYVALUE,
                vec![
                    "me".to_string(),
                    "alice".to_string(),
                    "metadata".to_string(),
                    "public".to_string(),
                    "value".to_string(),
                ],
            ),
            Command::Response(
                Response::RPL_AWAY,
                vec![
                    "me".to_string(),
                    "alice".to_string(),
                    "gone for lunch".to_string(),
                ],
            ),
            Command::Response(
                Response::RPL_ENDOFWHOIS,
                vec![
                    "me".to_string(),
                    "alice".to_string(),
                    "End of WHOIS".to_string(),
                ],
            ),
        ] {
            let msg = make_irc_msg(None, command);
            handle_irc_message(&mut state, "test", &msg);
        }

        let buf = state.buffers.get("test/testserver").unwrap();
        let keys: Vec<&str> = buf
            .messages
            .iter()
            .filter_map(|msg| msg.event_key.as_deref())
            .collect();

        assert_eq!(
            keys,
            vec![
                "whois_oper",
                "whois_idle_signon",
                "whois_channels",
                "whois_certfp",
                "whois_keyvalue",
                "whois_away",
                "end_of_whois"
            ]
        );
    }

    #[test]
    fn whois_raw_account_and_secure_are_themeable() {
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");

        for command in [
            Command::Raw(
                "330".to_string(),
                vec![
                    "me".to_string(),
                    "alice".to_string(),
                    "alice_account".to_string(),
                    "is logged in as".to_string(),
                ],
            ),
            Command::Raw(
                "671".to_string(),
                vec![
                    "me".to_string(),
                    "alice".to_string(),
                    "is using a secure connection".to_string(),
                ],
            ),
        ] {
            let msg = make_irc_msg(None, command);
            handle_irc_message(&mut state, "test", &msg);
        }

        let buf = state.buffers.get("test/testserver").unwrap();

        assert_eq!(buf.messages[0].event_key.as_deref(), Some("whois_account"));
        assert_eq!(buf.messages[1].event_key.as_deref(), Some("whois_secure"));
        assert_eq!(
            buf.messages[1].event_params.as_deref(),
            Some(
                &[
                    "alice".to_string(),
                    "TLS".to_string(),
                    "is using a secure connection".to_string(),
                ][..]
            )
        );
    }

    #[test]
    fn whois_raw_320_special_is_themeable() {
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");

        let msg = make_irc_msg(
            None,
            Command::Raw(
                "320".to_string(),
                vec![
                    "me".to_string(),
                    "kofany".to_string(),
                    "is a Cloaked Connection (Spoof)".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/testserver").unwrap();
        let m = buf.messages.back().unwrap();
        assert_eq!(m.event_key.as_deref(), Some("whois_special"));
        assert!(m.text.contains("is a Cloaked Connection (Spoof)"));
        assert_eq!(
            m.event_params.as_deref(),
            Some(
                &[
                    "kofany".to_string(),
                    "is a Cloaked Connection (Spoof)".to_string(),
                ][..]
            )
        );
    }

    #[test]
    fn whois_raw_freeform_numerics_get_event_keys() {
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");

        for (numeric, expected_key, text) in [
            ("307", "whois_registered", "is a registered nick"),
            ("310", "whois_help", "is available for help"),
            ("335", "whois_bot", "is a Bot on TestNet"),
            ("338", "whois_actually", "is actually using host"),
            ("378", "whois_host", "is connecting from *@1.2.3.4 1.2.3.4"),
            ("379", "whois_modes", "is using modes +iwx"),
        ] {
            let msg = make_irc_msg(
                None,
                Command::Raw(
                    numeric.to_string(),
                    vec!["me".to_string(), "alice".to_string(), text.to_string()],
                ),
            );
            handle_irc_message(&mut state, "test", &msg);

            let buf = state.buffers.get("test/testserver").unwrap();
            let m = buf.messages.back().unwrap();
            assert_eq!(
                m.event_key.as_deref(),
                Some(expected_key),
                "numeric {numeric} should map to {expected_key}"
            );
            assert!(m.text.contains(text), "numeric {numeric} text lost");
            assert_eq!(
                m.event_params.as_deref(),
                Some(&["alice".to_string(), text.to_string()][..]),
                "numeric {numeric} params"
            );
        }
    }

    #[test]
    fn shipped_themes_define_every_whois_key() {
        for (name, src) in [
            ("default.theme", include_str!("../../themes/default.theme")),
            ("spring.theme", include_str!("../../themes/spring.theme")),
        ] {
            let theme: crate::theme::ThemeFile =
                toml::from_str(src).unwrap_or_else(|e| panic!("{name} must parse: {e}"));
            for key in WHOIS_EVENT_KEYS {
                assert!(
                    theme.formats.events.contains_key(*key),
                    "{name} is missing event format `{key}`"
                );
            }
        }
    }

    #[test]
    fn whois_extra_freeform_numerics_get_event_keys() {
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");

        for (numeric, expected_key, text) in [
            ("326", "whois_modes", "has oper privs: +Aa"),
            ("327", "whois_host", "real.host.example 1.2.3.4 Real hostname/IP"),
            ("337", "whois_special", "is connected via a webirc gateway"),
            ("275", "whois_special", "is using a secure connection (SSL)"),
        ] {
            let msg = make_irc_msg(
                None,
                Command::Raw(
                    numeric.to_string(),
                    vec!["me".to_string(), "alice".to_string(), text.to_string()],
                ),
            );
            handle_irc_message(&mut state, "test", &msg);

            let buf = state.buffers.get("test/testserver").unwrap();
            let m = buf.messages.back().unwrap();
            assert_eq!(
                m.event_key.as_deref(),
                Some(expected_key),
                "numeric {numeric} should map to {expected_key}"
            );
            assert_eq!(
                m.event_params.as_deref(),
                Some(&["alice".to_string(), text.to_string()][..]),
                "numeric {numeric} params"
            );
        }
    }

    #[test]
    fn whois_377_maps_to_modes_only_with_usermodes_literal() {
        // AustHex uses 377 as RPL_SPAM for post-MOTD announcement text. Only
        // the `<me> usermodes <nick> <modes>` shape is a WHOIS usermode line,
        // and `usermodes` is a protocol literal rather than admin-authored
        // prose, so keying on it is safe.
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");

        let whois_form = make_irc_msg(
            None,
            Command::Raw(
                "377".to_string(),
                vec![
                    "me".to_string(),
                    "usermodes".to_string(),
                    "alice".to_string(),
                    "+iwx".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &whois_form);
        let buf = state.buffers.get("test/testserver").unwrap();
        let m = buf.messages.back().unwrap();
        assert_eq!(m.event_key.as_deref(), Some("whois_modes"));
        assert_eq!(
            m.event_params.as_deref(),
            Some(&["alice".to_string(), "+iwx".to_string()][..]),
            "377 must put the nick in $0, not the `usermodes` literal"
        );

        let spam_form = make_irc_msg(
            None,
            Command::Raw(
                "377".to_string(),
                vec![
                    "me".to_string(),
                    "alice".to_string(),
                    "Network announcement text".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &spam_form);
        let buf = state.buffers.get("test/testserver").unwrap();
        let m = buf.messages.back().unwrap();
        assert_eq!(
            m.event_key, None,
            "RPL_SPAM form of 377 must not be themed as a WHOIS line"
        );
    }

    #[test]
    fn every_whois_line_puts_the_nick_first() {
        // $0 is the nick for every whois_* key, so a theme author can write
        // "$0" without checking which numeric produced the line.
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");

        for (numeric, args) in [
            ("307", vec!["me", "alice", "is a registered nick"]),
            ("310", vec!["me", "alice", "is available for help"]),
            ("320", vec!["me", "alice", "is a Cloaked Connection (Spoof)"]),
            ("326", vec!["me", "alice", "has oper privs: +Aa"]),
            ("327", vec!["me", "alice", "real.host 1.2.3.4"]),
            ("335", vec!["me", "alice", "is a Bot"]),
            ("337", vec!["me", "alice", "webirc gateway"]),
            ("338", vec!["me", "alice", "is actually using host"]),
            ("275", vec!["me", "alice", "is using a secure connection (SSL)"]),
            ("378", vec!["me", "alice", "is connecting from *@h 1.2.3.4"]),
            ("379", vec!["me", "alice", "is using modes +iwx"]),
            ("377", vec!["me", "usermodes", "alice", "+iwx"]),
        ] {
            let msg = make_irc_msg(
                None,
                Command::Raw(
                    numeric.to_string(),
                    args.iter().map(|s| (*s).to_string()).collect(),
                ),
            );
            handle_irc_message(&mut state, "test", &msg);

            let buf = state.buffers.get("test/testserver").unwrap();
            let m = buf.messages.back().unwrap();
            assert!(
                m.event_key
                    .as_deref()
                    .is_some_and(|k| k.starts_with("whois")),
                "numeric {numeric} lost its whois key"
            );
            assert_eq!(
                m.event_params
                    .as_ref()
                    .and_then(|p| p.first())
                    .map(String::as_str),
                Some("alice"),
                "numeric {numeric} must put the nick in $0"
            );
        }
    }

    #[test]
    fn whois_377_short_form_still_displays() {
        // `<me> usermodes <nick>` with no mode string passes the usermodes
        // guard but has nothing left for the text slot. It must fall through
        // to the generic catch-all rather than being silently dropped.
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");

        let msg = make_irc_msg(
            None,
            Command::Raw(
                "377".to_string(),
                vec![
                    "me".to_string(),
                    "usermodes".to_string(),
                    "alice".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/testserver").unwrap();
        let m = buf
            .messages
            .back()
            .expect("short 377 must still display something");
        assert_eq!(m.event_key, None);
        assert_eq!(m.text, "usermodes alice");
    }

    #[test]
    fn whois_raw_320_short_form_falls_to_catch_all() {
        // A 2-arg freeform numeric (no separate nick token) must not be
        // swallowed — it should fall through to the generic numeric
        // catch-all that displays args[1..], as it did before the themed
        // freeform handlers existed.
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");

        let msg = make_irc_msg(
            None,
            Command::Raw(
                "320".to_string(),
                vec!["me".to_string(), "is a Cloaked Connection".to_string()],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/testserver").unwrap();
        let m = buf.messages.back().expect("short 320 must still display");
        assert_eq!(m.event_key, None);
        assert_eq!(m.text, "is a Cloaked Connection");
    }

    #[test]
    fn whois_raw_actually_joins_middle_args() {
        // ircu-style 338 puts values in middle args with a trailing description:
        // [me, nick, user@host, ip, "Actual user@host, Actual IP"]
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");

        let msg = make_irc_msg(
            None,
            Command::Raw(
                "338".to_string(),
                vec![
                    "me".to_string(),
                    "alice".to_string(),
                    "~u@1.2.3.4".to_string(),
                    "1.2.3.4".to_string(),
                    "Actual user@host, Actual IP".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/testserver").unwrap();
        let m = buf.messages.back().unwrap();
        assert_eq!(m.event_key.as_deref(), Some("whois_actually"));
        assert!(m.text.contains("~u@1.2.3.4 1.2.3.4 Actual user@host, Actual IP"));
    }

    #[test]
    fn whois_idle_without_signon_uses_idle_theme_key() {
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");

        let msg = make_irc_msg(
            None,
            Command::Response(
                Response::RPL_WHOISIDLE,
                vec!["me".to_string(), "alice".to_string(), "65".to_string()],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/testserver").unwrap();

        assert_eq!(buf.messages[0].event_key.as_deref(), Some("whois_idle"));
    }

    #[test]
    fn whois_error_numerics_get_event_keys() {
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");

        for (response, args, expected_key) in [
            (
                Response::ERR_NOSUCHNICK,
                vec!["me", "ghost", "No such nick/channel"],
                "no_such_nick",
            ),
            (
                Response::ERR_NOSUCHSERVER,
                vec!["me", "irc.example.net", "No such server"],
                "no_such_server",
            ),
            (
                Response::RPL_TRYAGAIN,
                vec!["me", "WHOIS", "Please wait a while and try again."],
                "try_again",
            ),
        ] {
            let msg = make_irc_msg(
                None,
                Command::Response(response, args.iter().map(|s| (*s).to_string()).collect()),
            );
            handle_irc_message(&mut state, "test", &msg);

            let buf = state.buffers.get("test/testserver").unwrap();
            let m = buf.messages.back().unwrap();
            assert_eq!(
                m.event_key.as_deref(),
                Some(expected_key),
                "{response:?} should map to {expected_key}"
            );
            assert_eq!(
                m.event_params.as_deref(),
                Some(&[args[1].to_string(), args[2].to_string()][..]),
                "{response:?} params"
            );
        }
    }

    #[test]
    fn try_again_lands_in_active_window_not_server_buffer() {
        // 263 is a 2xx, so the generic catch-all sent it to the server buffer
        // while the rest of the WHOIS reply went to the active window —
        // splitting one logical reply across two buffers.
        let mut state = make_test_state();
        state.set_active_buffer("test/#test");

        let msg = make_irc_msg(
            None,
            Command::Response(
                Response::RPL_TRYAGAIN,
                vec![
                    "me".to_string(),
                    "WHOIS".to_string(),
                    "Please wait a while and try again.".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#test").expect("active buffer");
        let m = buf.messages.back().expect("263 must land in active window");
        assert_eq!(m.event_key.as_deref(), Some("try_again"));
    }

    #[test]
    fn banlist_response_upserts_cached_ban_entry() {
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");

        for (set_by, timestamp) in [("alice", "1700000000"), ("bob", "1700000100")] {
            let msg = make_irc_msg(
                None,
                Command::Response(
                    Response::RPL_BANLIST,
                    vec![
                        "me".to_string(),
                        "#test".to_string(),
                        "*!*@bad.example".to_string(),
                        set_by.to_string(),
                        timestamp.to_string(),
                    ],
                ),
            );
            handle_irc_message(&mut state, "test", &msg);
        }

        let buf = state.buffers.get("test/#test").unwrap();
        let bans = buf.list_modes.get(BAN_MODE_KEY).unwrap();
        assert_eq!(bans.len(), 1);
        assert_eq!(bans[0].set_by, "bob");
        assert_eq!(bans[0].set_at, 1_700_000_100);
    }

    #[test]
    fn silent_banlist_sync_updates_state_without_display() {
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");
        if let Some(conn) = state.connections.get_mut("test") {
            conn.silent_banlist_channels.insert("#test".to_string());
        }

        let list_msg = make_irc_msg(
            None,
            Command::Response(
                Response::RPL_BANLIST,
                vec![
                    "me".to_string(),
                    "#test".to_string(),
                    "*!*@bad.example".to_string(),
                    "oper".to_string(),
                    "1700000000".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &list_msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert_eq!(buf.list_modes.get(BAN_MODE_KEY).unwrap().len(), 1);
        assert!(
            state
                .buffers
                .get("test/testserver")
                .unwrap()
                .messages
                .is_empty()
        );

        let end_msg = make_irc_msg(
            None,
            Command::Response(
                Response::RPL_ENDOFBANLIST,
                vec![
                    "me".to_string(),
                    "#test".to_string(),
                    "End of channel ban list".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &end_msg);

        let conn = state.connections.get("test").unwrap();
        assert!(!conn.silent_banlist_channels.contains("#test"));
        assert!(
            state
                .buffers
                .get("test/testserver")
                .unwrap()
                .messages
                .is_empty()
        );
    }

    #[test]
    fn silent_banlist_sync_suppresses_case_variant_channel() {
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");
        if let Some(conn) = state.connections.get_mut("test") {
            conn.silent_banlist_channels.insert("#Test".to_string());
        }

        let list_msg = make_irc_msg(
            None,
            Command::Response(
                Response::RPL_BANLIST,
                vec![
                    "me".to_string(),
                    "#test".to_string(),
                    "*!*@bad.example".to_string(),
                    "oper".to_string(),
                    "1700000000".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &list_msg);

        let buf = state.buffers.get("test/#test").unwrap();
        assert_eq!(buf.list_modes.get(BAN_MODE_KEY).unwrap().len(), 1);
        assert!(
            state
                .buffers
                .get("test/testserver")
                .unwrap()
                .messages
                .is_empty()
        );

        let end_msg = make_irc_msg(
            None,
            Command::Response(
                Response::RPL_ENDOFBANLIST,
                vec![
                    "me".to_string(),
                    "#TEST".to_string(),
                    "End of channel ban list".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &end_msg);

        let conn = state.connections.get("test").unwrap();
        assert!(conn.silent_banlist_channels.is_empty());
        assert!(
            state
                .buffers
                .get("test/testserver")
                .unwrap()
                .messages
                .is_empty()
        );
    }

    #[test]
    fn silent_mode_sync_suppresses_channel_creation_notice() {
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");
        if let Some(conn) = state.connections.get_mut("test") {
            conn.silent_banlist_channels.insert("#Test".to_string());
        }

        let msg = make_irc_msg(
            None,
            Command::Raw(
                "329".to_string(),
                vec![
                    "me".to_string(),
                    "#test".to_string(),
                    "1700000000".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let channel_buf = state.buffers.get("test/#test").unwrap();
        assert!(channel_buf.messages.is_empty());
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
    fn silent_whox_reply_suppresses_case_variant_channel() {
        let mut state = make_whox_state();
        state.set_active_buffer("test/testserver");

        if let Some(conn) = state.connections.get_mut("test") {
            conn.silent_who_channels.insert("#Test".to_string());
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
    fn silent_who_end_cleans_up_case_variant_channel() {
        let mut state = make_whox_state();
        state.set_active_buffer("test/testserver");

        if let Some(conn) = state.connections.get_mut("test") {
            conn.silent_who_channels.insert("#Test".to_string());
        }

        let msg = make_irc_msg(
            None,
            Command::Response(
                Response::RPL_ENDOFWHO,
                vec![
                    "me".to_string(),
                    "#TEST".to_string(),
                    "End of WHO list".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let conn = state.connections.get("test").unwrap();
        assert!(conn.silent_who_channels.is_empty());

        let server_buf = state.buffers.get("test/testserver").unwrap();
        assert!(server_buf.messages.is_empty());
    }

    #[test]
    fn stale_cleanup_contract_silent_flags_suppress_late_replies() {
        // Regression for the autoconnect leak on Solanum: the 30s stale-batch
        // cleanup in App::check_stale_who_batches drops `channel_query_in_flight`
        // and `channel_query_sent_at` so the next WHO batch can start, but it
        // MUST leave `silent_who_channels` and `silent_banlist_channels`
        // populated. Solanum rate-limits RPL_WHOSPCRPL on large channels so
        // the corresponding RPL_ENDOFWHO / RPL_BANLIST / RPL_ENDOFBANLIST can
        // arrive past the cleanup window — if the silent flags were stripped
        // alongside the in-flight tracking they would leak to the active
        // buffer ("End of WHO list" / ban entries / "End of ban list").
        //
        // This test exercises the exact post-cleanup state at the AppState
        // layer (in_flight lives on App and is irrelevant to suppression —
        // only the silent flags gate the reply handlers).
        let mut state = make_whox_state();
        state.set_active_buffer("test/testserver");
        if let Some(conn) = state.connections.get_mut("test") {
            conn.silent_who_channels.insert("#big".to_string());
            conn.silent_banlist_channels.insert("#big".to_string());
        }

        let endwho = make_irc_msg(
            None,
            Command::Response(
                Response::RPL_ENDOFWHO,
                vec!["me".into(), "#big".into(), "End of WHO list".into()],
            ),
        );
        handle_irc_message(&mut state, "test", &endwho);

        let banlist = make_irc_msg(
            None,
            Command::Response(
                Response::RPL_BANLIST,
                vec![
                    "me".into(),
                    "#big".into(),
                    "*!*@spammer.example".into(),
                    "oper".into(),
                    "1700000000".into(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &banlist);

        let endban = make_irc_msg(
            None,
            Command::Response(
                Response::RPL_ENDOFBANLIST,
                vec!["me".into(), "#big".into(), "End of channel ban list".into()],
            ),
        );
        handle_irc_message(&mut state, "test", &endban);

        let server_buf = state.buffers.get("test/testserver").unwrap();
        assert!(
            server_buf.messages.is_empty(),
            "late WHO/banlist replies must stay suppressed after stale cleanup"
        );
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

    // === Disconnect / rejoin nicklist lifecycle ===

    #[test]
    fn disconnect_wipes_channel_nicklists_for_connection() {
        let mut state = make_test_state();
        let chan_id = make_buffer_id("test", "#test");
        state.add_nick(
            &chan_id,
            NickEntry {
                nick: "alice".into(),
                prefix: String::new(),
                modes: String::new(),
                away: false,
                account: None,
                ident: None,
                host: None,
            },
        );
        state
            .buffers
            .get_mut(&chan_id)
            .unwrap()
            .last_speakers
            .push("alice".into());

        handle_disconnected(&mut state, "test", None);

        let buf = state.buffers.get(&chan_id).unwrap();
        assert!(buf.users.is_empty(), "users should be wiped on disconnect");
        assert!(buf.last_speakers.is_empty());
        // Channel name still recorded for rejoin
        let conn = state.connections.get("test").unwrap();
        assert!(conn.joined_channels.iter().any(|c| c == "#test"));
    }

    #[test]
    fn disconnect_does_not_touch_other_connections() {
        let mut state = make_test_state();
        // Second connection with its own channel
        state.add_connection(Connection {
            id: "other".into(),
            label: "Other".into(),
            status: ConnectionStatus::Connected,
            own_handle: None,
            nick: "me".into(),
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
            origin_config: state.connections.get("test").unwrap().origin_config.clone(),
            local_ip: None,
            enabled_caps: std::collections::HashSet::new(),
            chathistory: crate::irc::chathistory::HistoryState::new(),
            who_token_counter: 0,
            multiline: None,
            batch_ref_counter: 0,
            silent_who_channels: std::collections::HashSet::new(),
            silent_banlist_channels: std::collections::HashSet::new(),
        });
        let other_chan = make_buffer_id("other", "#other");
        state.add_buffer(make_channel_buffer("other", "#other"));
        state.add_nick(
            &other_chan,
            NickEntry {
                nick: "bob".into(),
                prefix: String::new(),
                modes: String::new(),
                away: false,
                account: None,
                ident: None,
                host: None,
            },
        );

        handle_disconnected(&mut state, "test", None);

        // "other" connection's nicklist must not be wiped
        assert_eq!(state.buffers.get(&other_chan).unwrap().users.len(), 1);
    }

    #[test]
    fn self_join_to_existing_buffer_with_users_is_ignored() {
        // Defense against ZNC bouncer replays / stray double-JOINs: if the
        // nicklist is already populated, treat the JOIN as a duplicate.
        let mut state = make_test_state();
        let chan_id = make_buffer_id("test", "#test");
        // Pre-populate with a stale message we expect NOT to repeat.
        let initial_msgs = state.buffers.get(&chan_id).unwrap().messages.len();

        let msg = make_irc_msg(
            Some("me!ident@host"),
            Command::JOIN("#test".into(), None, None),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get(&chan_id).unwrap();
        // No new join message added (we returned early).
        assert_eq!(buf.messages.len(), initial_msgs);
        // Existing nicks preserved.
        assert!(buf.users.contains_key("me"));
    }

    #[test]
    fn self_join_to_empty_existing_buffer_resets_stale_state() {
        // After disconnect → reconnect rejoin: buffer exists but users were
        // wiped by handle_disconnected. The JOIN must reset stale topic/modes
        // so the fresh RPL_TOPIC / RPL_CHANNELMODEIS rebuild from scratch.
        let mut state = make_test_state();
        let chan_id = make_buffer_id("test", "#test");
        {
            let buf = state.buffers.get_mut(&chan_id).unwrap();
            buf.users.clear();
            buf.topic = Some("stale topic".into());
            buf.topic_set_by = Some("stale_setter".into());
            buf.modes = Some("nt".into());
            buf.list_modes.insert("b".into(), vec![]);
        }

        let msg = make_irc_msg(
            Some("me!ident@host"),
            Command::JOIN("#test".into(), None, None),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get(&chan_id).unwrap();
        assert!(buf.topic.is_none(), "topic should be cleared on rejoin");
        assert!(buf.topic_set_by.is_none());
        assert!(buf.modes.is_none());
        assert!(buf.list_modes.is_empty());
    }

    #[test]
    fn self_join_creates_buffer_when_missing() {
        let mut state = make_test_state();
        let chan_id = make_buffer_id("test", "#fresh");
        assert!(!state.buffers.contains_key(&chan_id));

        let msg = make_irc_msg(
            Some("me!ident@host"),
            Command::JOIN("#fresh".into(), None, None),
        );
        handle_irc_message(&mut state, "test", &msg);

        assert!(state.buffers.contains_key(&chan_id));
        assert_eq!(state.active_buffer_id.as_deref(), Some(chan_id.as_str()));
    }

    // === Phase B: receive-path robustness ===

    #[test]
    fn malformed_ciphertext_renders_e2e_rejection_not_raw() {
        // A truncated/corrupted +RPE2E01 line makes WireChunk::parse return
        // Err; the old code fell through to rendering (and logging) the RAW
        // wire line. It must surface an [E2E] rejection instead.
        use crate::e2e::keyring::Keyring;
        use crate::e2e::manager::E2eManager;
        use std::sync::{Arc, Mutex};

        let mut state = make_test_state();
        let conn = crate::storage::db::open_database(false).unwrap();
        let mgr = E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(conn)))).unwrap();
        state.e2e_manager = Some(Arc::new(mgr));
        state.connections.get_mut("test").unwrap().own_handle = Some("~me@host".to_string());

        let prefix = Prefix::new_from_str("bob!~bob@b.host");
        handle_privmsg(
            &mut state,
            "test",
            "me",
            Some(&prefix),
            "me",
            "+RPE2E01 truncated-garbage",
            None,
        );

        let buf = state
            .buffers
            .get(&make_buffer_id("test", "bob"))
            .expect("query buffer for the sender");
        let last = buf.messages.back().expect("a rendered line");
        assert!(
            last.text.starts_with("[E2E"),
            "must render an [E2E] rejection, got: {}",
            last.text
        );
        assert!(
            !last.text.contains("+RPE2E01"),
            "raw ciphertext must never render: {}",
            last.text
        );
    }

    #[test]
    fn missing_session_placeholder_is_transient_and_tagless() {
        // The first DM from a peer with no installed session shows the
        // "[E2E: awaiting session with …]" placeholder. It must NOT keep the
        // server @msgid tags: persisting it under the real @msgid blocks the
        // decrypted CHATHISTORY replay (unique (network, msg_id) index) —
        // losing the message forever. Same rule as the awaiting-own-identity
        // placeholder.
        use crate::e2e::keyring::Keyring;
        use crate::e2e::manager::E2eManager;
        use std::sync::{Arc, Mutex};

        let mut state = make_test_state();
        let ours_conn = crate::storage::db::open_database(false).unwrap();
        let ours = E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(ours_conn)))).unwrap();
        state.e2e_manager = Some(Arc::new(ours));
        state.connections.get_mut("test").unwrap().own_handle = Some("~me@host".to_string());

        // Peer encrypts a valid wire line to our decrypt context; we have no
        // incoming session for them -> MissingKey.
        let peer_conn = crate::storage::db::open_database(false).unwrap();
        let peer = E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(peer_conn)))).unwrap();
        let wire = peer
            .encrypt_outgoing("@~me@host", "the lost first message")
            .unwrap()
            .remove(0);

        let mut tags = HashMap::new();
        tags.insert("msgid".to_string(), "MSGID-FIRST".to_string());
        let prefix = Prefix::new_from_str("bob!~bob@b.host");
        handle_privmsg(&mut state, "test", "me", Some(&prefix), "me", &wire, Some(tags));

        let buf = state
            .buffers
            .get(&make_buffer_id("test", "bob"))
            .expect("query buffer for the sender");
        let last = buf.messages.back().expect("a rendered line");
        assert!(
            last.text.starts_with("[E2E: awaiting session with"),
            "expected the awaiting-session placeholder, got: {}",
            last.text
        );
        assert!(
            last.tags.is_none(),
            "placeholder must be tagless so the decrypted replay can splice under the real @msgid"
        );
    }

    #[test]
    fn keyrsp_install_queues_dm_gapfill() {
        // When a KEYRSP installs the DM incoming session, the query must be
        // queued for a CHATHISTORY re-fetch: the message that TRIGGERED the
        // handshake was replaced by a placeholder and is otherwise lost.
        use crate::e2e::keyring::{ChannelConfig, ChannelMode, Keyring};
        use crate::e2e::manager::E2eManager;
        use std::sync::{Arc, Mutex};

        let mut state = make_test_state();
        let ours_conn = crate::storage::db::open_database(false).unwrap();
        let ours = Arc::new(
            E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(ours_conn)))).unwrap(),
        );
        state.e2e_manager = Some(Arc::clone(&ours));

        // We (recipient) issued the KEYREQ for our own decrypt context —
        // network-scoped, as every production caller does now.
        let req = ours
            .build_keyreq(&crate::e2e::scoped_context("TestServer", "@~me@host"))
            .unwrap();

        // Alice answers with a KEYRSP (AutoAccept so the reply is immediate).
        let alice_conn = crate::storage::db::open_database(false).unwrap();
        let alice =
            E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(alice_conn)))).unwrap();
        alice
            .keyring()
            .set_channel_config(&ChannelConfig {
                channel: "@~me@host".to_string(),
                enabled: true,
                mode: ChannelMode::AutoAccept,
            })
            .unwrap();
        let rsp = alice
            .handle_keyreq_with_nick("~me@host", Some("me"), &req)
            .unwrap()
            .expect("AutoAccept responds with a KEYRSP");
        let body = alice.encode_keyrsp_ctcp(&rsp);

        let prefix = Prefix::new_from_str("alice!~alice@a.host");
        let outcome = try_dispatch_rpe2e_ctcp(&mut state, "test", Some(&prefix), "me", &body);
        assert_eq!(outcome, Some(RpEe2eOutcome::Handled));
        assert!(
            state
                .pending_e2e_gapfills
                .iter()
                .any(|g| g.connection_id == "test" && g.target == "alice"),
            "a successful DM session install must queue a query gap-fill"
        );
    }

    #[test]
    fn decrypted_message_looking_like_placeholder_keeps_its_tags() {
        // A peer's legitimate message whose PLAINTEXT starts with the
        // placeholder text must not be misfiled as transient/tagless —
        // transiency is signaled out-of-band, not sniffed from the text.
        use crate::e2e::keyring::{ChannelConfig, ChannelMode, Keyring};
        use crate::e2e::manager::E2eManager;
        use std::sync::{Arc, Mutex};

        let mut state = make_test_state();
        let ours_conn = crate::storage::db::open_database(false).unwrap();
        let ours = Arc::new(
            E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(ours_conn)))).unwrap(),
        );
        state.e2e_manager = Some(Arc::clone(&ours));
        state.connections.get_mut("test").unwrap().own_handle = Some("~me@host".to_string());

        // Establish the incoming session: we KEYREQ, bob KEYRSPs.
        let scoped = crate::e2e::scoped_context("TestServer", "@~me@host");
        let req = ours.build_keyreq(&scoped).unwrap();
        let bob_conn = crate::storage::db::open_database(false).unwrap();
        let bob = E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(bob_conn)))).unwrap();
        bob.keyring()
            .set_channel_config(&ChannelConfig {
                channel: "@~me@host".to_string(),
                enabled: true,
                mode: ChannelMode::AutoAccept,
            })
            .unwrap();
        let rsp = bob
            .handle_keyreq_with_nick("~me@host", Some("me"), &req)
            .unwrap()
            .unwrap();
        let mut scoped_rsp = rsp;
        scoped_rsp.channel = crate::e2e::scoped_context("TestServer", &scoped_rsp.channel);
        ours.handle_keyrsp("~bob@b.host", &scoped_rsp).unwrap();

        let tricky = format!(
            "{}~mallory@m.host] just kidding",
            crate::e2e::AWAITING_SESSION_PLACEHOLDER_PREFIX
        );
        let wire = bob.encrypt_outgoing("@~me@host", &tricky).unwrap().remove(0);
        let mut tags = HashMap::new();
        tags.insert("msgid".to_string(), "REAL-MSGID".to_string());
        let prefix = Prefix::new_from_str("bob!~bob@b.host");
        handle_privmsg(&mut state, "test", "me", Some(&prefix), "me", &wire, Some(tags));

        let buf = state
            .buffers
            .get(&make_buffer_id("test", "bob"))
            .expect("query buffer");
        let last = buf.messages.back().expect("a rendered line");
        assert_eq!(last.text, tricky, "the decrypted plaintext must render as-is");
        assert!(
            last.tags.is_some(),
            "a legitimate decrypted message must keep its server tags"
        );
    }

    #[test]
    fn malformed_ciphertext_rejection_is_tagless() {
        // The Err arm also fires on a transient keyring fault over a valid
        // ciphertext — logging the rejection under the real @msgid would
        // permanently block the decryptable CHATHISTORY replay.
        use crate::e2e::keyring::Keyring;
        use crate::e2e::manager::E2eManager;
        use std::sync::{Arc, Mutex};

        let mut state = make_test_state();
        let conn = crate::storage::db::open_database(false).unwrap();
        let mgr = E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(conn)))).unwrap();
        state.e2e_manager = Some(Arc::new(mgr));
        state.connections.get_mut("test").unwrap().own_handle = Some("~me@host".to_string());

        let mut tags = HashMap::new();
        tags.insert("msgid".to_string(), "MSGID-MALFORMED".to_string());
        let prefix = Prefix::new_from_str("bob!~bob@b.host");
        handle_privmsg(
            &mut state,
            "test",
            "me",
            Some(&prefix),
            "me",
            "+RPE2E01 truncated-garbage",
            Some(tags),
        );

        let buf = state
            .buffers
            .get(&make_buffer_id("test", "bob"))
            .expect("query buffer");
        let last = buf.messages.back().expect("a rendered line");
        assert!(last.text.starts_with("[E2E rejected"));
        assert!(
            last.tags.is_none(),
            "an Err-arm rejection must not occupy the (network, msg_id) row"
        );
    }

    #[test]
    fn keyrsp_install_queues_channel_gapfill_too() {
        // The MissingKey placeholder is transient for channels as well — a
        // KEYRSP that installs a CHANNEL session must queue the same
        // CHATHISTORY re-fetch, or the first encrypted channel line is
        // neither replaced nor recoverable from local history.
        use crate::e2e::keyring::{ChannelConfig, ChannelMode, Keyring};
        use crate::e2e::manager::E2eManager;
        use std::sync::{Arc, Mutex};

        let mut state = make_test_state();
        let ours_conn = crate::storage::db::open_database(false).unwrap();
        let ours = Arc::new(
            E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(ours_conn)))).unwrap(),
        );
        state.e2e_manager = Some(Arc::clone(&ours));

        let scoped = crate::e2e::scoped_context("TestServer", "#test");
        let req = ours.build_keyreq(&scoped).unwrap();

        let alice_conn = crate::storage::db::open_database(false).unwrap();
        let alice =
            E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(alice_conn)))).unwrap();
        alice
            .keyring()
            .set_channel_config(&ChannelConfig {
                channel: "#test".to_string(),
                enabled: true,
                mode: ChannelMode::AutoAccept,
            })
            .unwrap();
        let rsp = alice
            .handle_keyreq_with_nick("~me@host", Some("me"), &req)
            .unwrap()
            .expect("AutoAccept responds with a KEYRSP");
        let body = alice.encode_keyrsp_ctcp(&rsp);

        let prefix = Prefix::new_from_str("alice!~alice@a.host");
        let outcome = try_dispatch_rpe2e_ctcp(&mut state, "test", Some(&prefix), "me", &body);
        assert_eq!(outcome, Some(RpEe2eOutcome::Handled));
        assert!(
            state
                .pending_e2e_gapfills
                .iter()
                .any(|g| g.connection_id == "test" && g.target == "#test"),
            "a channel session install must queue a channel gap-fill"
        );
    }

    #[test]
    fn ciphertext_in_notice_is_suppressed() {
        // RPE2E never ships ciphertext in NOTICE; a peer that does anyway
        // (buggy or malicious) must not get the raw wire rendered.
        use crate::e2e::keyring::Keyring;
        use crate::e2e::manager::E2eManager;
        use std::sync::{Arc, Mutex};

        let mut state = make_test_state();
        let conn = crate::storage::db::open_database(false).unwrap();
        let mgr = E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(conn)))).unwrap();
        state.e2e_manager = Some(Arc::new(mgr));

        let prefix = Prefix::new_from_str("bob!~bob@b.host");
        handle_notice(
            &mut state,
            "test",
            Some(&prefix),
            "me",
            "+RPE2E01 AAAA BBBB CCCC",
            None,
        );

        for buf in state.buffers.values() {
            assert!(
                buf.messages.iter().all(|m| !m.text.contains("+RPE2E01")),
                "raw ciphertext leaked into buffer {}",
                buf.id
            );
        }
    }

    // === TAGMSG / typing tests ===
    //
    // NOTE: `make_test_state()` creates a connection with id "test" (nick
    // "me"), not "conn1" as the task brief's tests assume — every "conn1" in
    // the brief has been substituted with "test" below.

    fn tagmsg(from: &str, target: &str, tag: &str, value: &str) -> IrcMessage {
        format!("@{tag}={value} :{from}!u@h TAGMSG {target}\r\n")
            .parse()
            .expect("valid TAGMSG")
    }

    #[test]
    fn tagmsg_sets_typing_without_touching_the_buffer() {
        let mut state = make_test_state();
        state.add_buffer(make_channel_buffer("test", "#rust"));
        let before = state.buffers["test/#rust"].messages.len();

        handle_irc_message(&mut state, "test", &tagmsg("alice", "#rust", "+typing", "active"));

        assert_eq!(state.typing.nicks("test/#rust"), vec!["alice"]);
        // The message-tags spec forbids showing TAGMSG in history.
        assert_eq!(state.buffers["test/#rust"].messages.len(), before);
        assert_eq!(state.buffers["test/#rust"].unread_count, 0);
        assert_eq!(
            state.buffers["test/#rust"].activity,
            crate::state::buffer::ActivityLevel::None
        );
    }

    #[test]
    fn tagmsg_done_clears_typing() {
        let mut state = make_test_state();
        state.add_buffer(make_channel_buffer("test", "#rust"));
        handle_irc_message(&mut state, "test", &tagmsg("alice", "#rust", "+typing", "active"));
        handle_irc_message(&mut state, "test", &tagmsg("alice", "#rust", "+typing", "done"));
        assert!(state.typing.nicks("test/#rust").is_empty());
    }

    #[test]
    fn typing_show_off_drops_the_notification_entirely() {
        // The setting must gate INGESTION: gating only the render arm would leave the
        // tracker full and keep broadcasting WebEvent::Typing to the browser.
        let mut state = make_test_state();
        state.typing_show = false;
        state.add_buffer(make_channel_buffer("test", "#rust"));
        // `add_buffer` itself enqueues a `BufferCreated` web event; drain it
        // so the assertion below only reflects what `handle_irc_message` did.
        state.pending_web_events.clear();
        handle_irc_message(&mut state, "test", &tagmsg("alice", "#rust", "+typing", "active"));
        assert!(state.typing.nicks("test/#rust").is_empty());
        assert!(state.pending_web_events.is_empty());
    }

    #[test]
    fn our_own_tagmsg_echo_is_ignored() {
        // echo-message reflects our own TAGMSG back at us (spec §3.2).
        let mut state = make_test_state();
        state.add_buffer(make_channel_buffer("test", "#rust"));
        let our_nick = state.connections["test"].nick.clone();
        handle_irc_message(&mut state, "test", &tagmsg(&our_nick, "#rust", "+typing", "active"));
        assert!(state.typing.nicks("test/#rust").is_empty());
    }

    #[test]
    fn replayed_tagmsg_in_a_batch_is_ignored() {
        // chathistory / event-playback would otherwise show phantom typing (spec §3.3).
        let mut state = make_test_state();
        state.add_buffer(make_channel_buffer("test", "#rust"));
        let msg: IrcMessage = "@batch=1;+typing=active :alice!u@h TAGMSG #rust\r\n"
            .parse()
            .expect("valid");
        handle_irc_message(&mut state, "test", &msg);
        assert!(state.typing.nicks("test/#rust").is_empty());
    }

    #[test]
    fn tagmsg_without_a_typing_tag_is_ignored() {
        let mut state = make_test_state();
        state.add_buffer(make_channel_buffer("test", "#rust"));
        handle_irc_message(&mut state, "test", &tagmsg("alice", "#rust", "+example-tag", "x"));
        assert!(state.typing.nicks("test/#rust").is_empty());
    }

    #[test]
    fn tagmsg_never_creates_a_buffer() {
        // Otherwise a stranger could pop a query window open just by typing.
        let mut state = make_test_state();
        let our_nick = state.connections["test"].nick.clone();
        handle_irc_message(&mut state, "test", &tagmsg("stranger", &our_nick, "+typing", "active"));
        assert!(!state.buffers.contains_key("test/stranger"));
        assert!(state.typing.nicks("test/stranger").is_empty());
    }

    #[test]
    fn tagmsg_to_an_existing_query_sets_typing_keyed_by_sender() {
        let mut state = make_test_state();
        let our_nick = state.connections["test"].nick.clone();
        let mut buf = make_channel_buffer("test", "alice");
        buf.buffer_type = crate::state::buffer::BufferType::Query;
        buf.id = "test/alice".to_string();
        buf.name = "alice".to_string();
        state.add_buffer(buf);

        handle_irc_message(&mut state, "test", &tagmsg("alice", &our_nick, "+typing", "active"));
        assert_eq!(state.typing.nicks("test/alice"), vec!["alice"]);
    }

    #[test]
    fn statusmsg_prefixed_target_resolves_to_the_channel() {
        let mut state = make_test_state();
        state
            .connections
            .get_mut("test")
            .expect("conn")
            .isupport_parsed
            .parse_tokens(&["STATUSMSG=@+"]);
        state.add_buffer(make_channel_buffer("test", "#rust"));
        handle_irc_message(&mut state, "test", &tagmsg("alice", "@#rust", "+typing", "active"));
        assert_eq!(state.typing.nicks("test/#rust"), vec!["alice"]);
    }

    #[test]
    fn ampersand_channel_is_not_mistaken_for_a_statusmsg_prefix() {
        // `&local` is a CHANNEL. Stripping the `&` would resolve it as a query.
        let mut state = make_test_state();
        state
            .connections
            .get_mut("test")
            .expect("conn")
            .isupport_parsed
            .parse_tokens(&["STATUSMSG=@+"]);
        state.add_buffer(make_channel_buffer("test", "&local"));
        handle_irc_message(&mut state, "test", &tagmsg("alice", "&local", "+typing", "active"));
        assert_eq!(state.typing.nicks("test/&local"), vec!["alice"]);
    }

    #[test]
    fn a_message_from_the_sender_clears_their_typing() {
        let mut state = make_test_state();
        state.add_buffer(make_channel_buffer("test", "#rust"));
        handle_irc_message(&mut state, "test", &tagmsg("alice", "#rust", "+typing", "active"));
        assert_eq!(state.typing.nicks("test/#rust"), vec!["alice"]);

        let privmsg: IrcMessage = ":alice!u@h PRIVMSG #rust :done typing\r\n".parse().expect("valid");
        handle_irc_message(&mut state, "test", &privmsg);
        assert!(state.typing.nicks("test/#rust").is_empty());
    }

    #[test]
    fn kick_clears_the_kicked_user_not_the_kicker() {
        // handle_kick binds `kicker` and `kicked_user` — there is no `nick`.
        let mut state = make_test_state();
        state.add_buffer(make_channel_buffer("test", "#rust"));
        handle_irc_message(&mut state, "test", &tagmsg("alice", "#rust", "+typing", "active"));
        handle_irc_message(&mut state, "test", &tagmsg("bob", "#rust", "+typing", "active"));

        // bob kicks alice: alice stops typing, bob does not.
        let kick: IrcMessage = ":bob!u@h KICK #rust alice :out\r\n".parse().expect("valid");
        handle_irc_message(&mut state, "test", &kick);
        assert_eq!(state.typing.nicks("test/#rust"), vec!["bob"]);
    }

    /// A second network alongside `make_test_state`'s `test`, so a per-connection
    /// claim can actually be falsified.
    fn add_second_connection(state: &mut AppState, id: &str) {
        let mut conn = state.connections["test"].clone();
        conn.id = id.to_string();
        conn.label = format!("{id}Server");
        state.add_connection(conn);
    }

    #[test]
    fn quit_clears_typing_only_on_that_connection() {
        // The same nick is typing in the same channel name on two networks —
        // an everyday thing on #rust. A QUIT belongs to ONE connection, so it
        // must not clear the other's indicator. With a single-connection
        // fixture this test would pass even if `handle_quit` cleared every
        // connection, which is exactly what it is here to rule out.
        let mut state = make_test_state();
        add_second_connection(&mut state, "other");
        state.add_buffer(make_channel_buffer("test", "#rust"));
        state.add_buffer(make_channel_buffer("other", "#rust"));
        handle_irc_message(&mut state, "test", &tagmsg("alice", "#rust", "+typing", "active"));
        handle_irc_message(&mut state, "other", &tagmsg("alice", "#rust", "+typing", "active"));
        assert_eq!(state.typing.nicks("test/#rust"), vec!["alice"]);
        assert_eq!(state.typing.nicks("other/#rust"), vec!["alice"]);

        let quit: IrcMessage = ":alice!u@h QUIT :bye\r\n".parse().expect("valid");
        handle_irc_message(&mut state, "test", &quit);
        assert!(state.typing.nicks("test/#rust").is_empty());
        assert_eq!(
            state.typing.nicks("other/#rust"),
            vec!["alice"],
            "a QUIT on one network says nothing about the other"
        );
    }

    #[test]
    fn quit_from_an_ignored_user_still_clears_their_typing() {
        // Ignore suppresses the notification, not the state change — the same
        // convention the nick-list update right next to it follows.
        let mut state = make_test_state();
        state.add_buffer(make_channel_buffer("test", "#rust"));
        handle_irc_message(&mut state, "test", &tagmsg("alice", "#rust", "+typing", "active"));
        assert_eq!(state.typing.nicks("test/#rust"), vec!["alice"]);

        state.ignores.push(crate::config::IgnoreEntry {
            mask: "alice".to_string(),
            levels: vec![IgnoreLevel::All],
            channels: None,
        });
        let quit: IrcMessage = ":alice!u@h QUIT :bye\r\n".parse().expect("valid");
        handle_irc_message(&mut state, "test", &quit);
        assert!(state.typing.nicks("test/#rust").is_empty());
    }

    fn ignore(state: &mut AppState, mask: &str, levels: Vec<IgnoreLevel>) {
        state.ignores.push(crate::config::IgnoreEntry {
            mask: mask.to_string(),
            levels,
            channels: None,
        });
    }

    #[test]
    fn an_ignored_users_channel_typing_never_enters_the_tracker() {
        // A channel TAGMSG is gated at `Public`, the same level as their PRIVMSGs.
        let mut state = make_test_state();
        state.add_buffer(make_channel_buffer("test", "#rust"));
        ignore(&mut state, "alice", vec![IgnoreLevel::Public]);

        handle_irc_message(&mut state, "test", &tagmsg("alice", "#rust", "+typing", "active"));
        assert!(
            state.typing.nicks("test/#rust").is_empty(),
            "an ignored user must not appear in the typing indicator"
        );

        // ...and the gate is the LEVEL, not the mere presence of an ignore:
        // ignoring someone's notices says nothing about their channel typing.
        let mut state = make_test_state();
        state.add_buffer(make_channel_buffer("test", "#rust"));
        ignore(&mut state, "alice", vec![IgnoreLevel::Notices]);
        handle_irc_message(&mut state, "test", &tagmsg("alice", "#rust", "+typing", "active"));
        assert_eq!(state.typing.nicks("test/#rust"), vec!["alice"]);
    }

    #[test]
    fn an_ignored_users_query_typing_never_enters_the_tracker() {
        // A TAGMSG aimed at us is gated at `Msgs`, like a PM.
        let mut state = make_test_state();
        let our_nick = state.connections["test"].nick.clone();
        let mut buf = make_channel_buffer("test", "alice");
        buf.buffer_type = crate::state::buffer::BufferType::Query;
        buf.id = "test/alice".to_string();
        buf.name = "alice".to_string();
        state.add_buffer(buf);
        ignore(&mut state, "alice", vec![IgnoreLevel::Msgs]);

        handle_irc_message(&mut state, "test", &tagmsg("alice", &our_nick, "+typing", "active"));
        assert!(state.typing.nicks("test/alice").is_empty());
    }

    #[test]
    fn an_ignored_action_still_clears_the_senders_typing() {
        // The levels do not line up: a TAGMSG is gated at `Public`, but a CTCP
        // ACTION is gated at `Actions`. `/ignore alice ACTIONS` therefore lets
        // her typing in and then drops the very message that retracts it — so
        // the clear must happen BEFORE the ignore returns, as it does for
        // PART/KICK/QUIT/NICK.
        let mut state = make_test_state();
        state.add_buffer(make_channel_buffer("test", "#rust"));
        handle_irc_message(&mut state, "test", &tagmsg("alice", "#rust", "+typing", "active"));
        assert_eq!(state.typing.nicks("test/#rust"), vec!["alice"]);

        ignore(&mut state, "alice", vec![IgnoreLevel::Actions]);
        let action: IrcMessage = ":alice!u@h PRIVMSG #rust :\x01ACTION waves\x01\r\n"
            .parse()
            .expect("valid");
        handle_irc_message(&mut state, "test", &action);

        assert!(
            state.typing.nicks("test/#rust").is_empty(),
            "her message arrived — she is no longer typing, ignored or not"
        );
        // The ignore still suppressed the line itself.
        assert!(
            state.buffers["test/#rust"]
                .messages
                .iter()
                .all(|m| !m.text.contains("waves")),
            "the ignore must still hide the message"
        );
    }

    #[test]
    fn an_ignored_notice_still_clears_the_senders_typing() {
        // Same mismatch: TAGMSG at `Msgs`, NOTICE at `Notices`.
        let mut state = make_test_state();
        let mut buf = make_channel_buffer("test", "alice");
        buf.buffer_type = crate::state::buffer::BufferType::Query;
        buf.id = "test/alice".to_string();
        buf.name = "alice".to_string();
        state.add_buffer(buf);
        let our_nick = state.connections["test"].nick.clone();
        handle_irc_message(&mut state, "test", &tagmsg("alice", &our_nick, "+typing", "active"));
        assert_eq!(state.typing.nicks("test/alice"), vec!["alice"]);

        ignore(&mut state, "alice", vec![IgnoreLevel::Notices]);
        let notice: IrcMessage = format!(":alice!u@h NOTICE {our_nick} :heads up\r\n")
            .parse()
            .expect("valid");
        handle_irc_message(&mut state, "test", &notice);

        assert!(state.typing.nicks("test/alice").is_empty());
        assert!(
            state.buffers["test/alice"]
                .messages
                .iter()
                .all(|m| !m.text.contains("heads up")),
            "the ignore must still hide the notice"
        );
    }

    #[test]
    fn kick_clears_typing_whatever_case_the_kicker_typed() {
        // The KICK <user> parameter is not the server-canonical spelling the
        // TAGMSG prefix carried — it is whatever the kicker typed. The nicklist
        // removal right next to it already folds case; the typing clear must too,
        // or a kicked user keeps "typing" in a channel they are no longer in.
        let mut state = make_test_state();
        state.add_buffer(make_channel_buffer("test", "#rust"));
        handle_irc_message(&mut state, "test", &tagmsg("Alice", "#rust", "+typing", "active"));
        assert_eq!(state.typing.nicks("test/#rust"), vec!["Alice"]);

        let kick: IrcMessage = ":bob!u@h KICK #rust aLiCe :out\r\n".parse().expect("valid");
        handle_irc_message(&mut state, "test", &kick);
        assert!(state.typing.nicks("test/#rust").is_empty());
    }

    // === The `irc.typing` script event ===
    //
    // `emit_irc_to_scripts` needs an `App` (whose constructor touches disk), so
    // the arm that builds the event is a free function over `&AppState`. These
    // drive it directly — the old test here asserted `events::TYPING ==
    // "irc.typing"`, a tautology about a constant that would have passed with
    // the whole arm deleted.

    use crate::app::scripting::typing_script_params;

    #[test]
    fn a_tagmsg_hands_a_script_the_documented_params() {
        // `docs/src/content/scripting-api.md` documents `nick`, `target` and
        // `state`. (`connection_id` is added by the caller for every event.)
        let state = make_test_state();
        let params = typing_script_params(&state, "test", &tagmsg("alice", "#rust", "+typing", "active"))
            .expect("a typing TAGMSG is scriptable");
        assert_eq!(params["nick"], "alice");
        assert_eq!(params["target"], "#rust");
        assert_eq!(params["state"], "active");
        assert_eq!(params.len(), 3);

        // Every state reaches the script under its spec name — a script that
        // only ever saw `active` could not tell when to take an indicator down.
        for value in ["paused", "done"] {
            let params =
                typing_script_params(&state, "test", &tagmsg("alice", "#rust", "+typing", value))
                    .expect("scriptable");
            assert_eq!(params["state"], value);
        }
        // And the legacy pre-ratification tag is understood here too.
        let params =
            typing_script_params(&state, "test", &tagmsg("alice", "#rust", "+draft/typing", "active"))
                .expect("scriptable");
        assert_eq!(params["state"], "active");
    }

    #[test]
    fn a_replayed_tagmsg_is_never_handed_to_a_script() {
        // chathistory / event-playback replay (spec §3.3). Without the batch
        // guard a script would act on typing from hours ago — the display path
        // drops it, and the two must not disagree.
        let state = make_test_state();
        let msg: IrcMessage = "@batch=1;+typing=active :alice!u@h TAGMSG #rust\r\n"
            .parse()
            .expect("valid");
        assert!(typing_script_params(&state, "test", &msg).is_none());
    }

    #[test]
    fn our_own_echoed_tagmsg_is_never_handed_to_a_script() {
        // `echo-message` reflects our own TAGMSG back at us (spec §3.2). A
        // script told that we are typing at ourselves is a feedback loop.
        let state = make_test_state();
        let our_nick = state.connections["test"].nick.clone();
        let msg = tagmsg(&our_nick, "#rust", "+typing", "active");
        assert!(typing_script_params(&state, "test", &msg).is_none());
        // Case-insensitively — the network may spell our nick differently.
        let msg = tagmsg(&our_nick.to_uppercase(), "#rust", "+typing", "active");
        assert!(typing_script_params(&state, "test", &msg).is_none());
    }

    #[test]
    fn a_tagmsg_with_nothing_scriptable_in_it_emits_nothing() {
        let state = make_test_state();
        // No typing tag at all.
        assert!(
            typing_script_params(&state, "test", &tagmsg("alice", "#rust", "+example", "x"))
                .is_none()
        );
        // An unparseable value.
        assert!(
            typing_script_params(&state, "test", &tagmsg("alice", "#rust", "+typing", "wat"))
                .is_none()
        );
        // Not a TAGMSG.
        let privmsg: IrcMessage = ":alice!u@h PRIVMSG #rust :hi\r\n".parse().expect("valid");
        assert!(typing_script_params(&state, "test", &privmsg).is_none());
    }

    #[test]
    fn the_script_target_is_statusmsg_stripped_like_the_display_path() {
        // The display path resolves `@#rust` to `#rust`, and `target` is
        // documented to scripts as "Channel or your nick". Handing over the raw
        // `@#rust` means a script matching `target == "#rust"` silently misses
        // every status-prefixed notification the status line does show.
        let mut state = make_test_state();
        state
            .connections
            .get_mut("test")
            .expect("conn")
            .isupport_parsed
            .parse_tokens(&["STATUSMSG=@+"]);

        let params = typing_script_params(&state, "test", &tagmsg("alice", "@#rust", "+typing", "active"))
            .expect("scriptable");
        assert_eq!(params["target"], "#rust");

        // And `&local` is a CHANNEL, not a status-prefixed one — stripping it
        // would hand the script a query name for a channel event.
        let params =
            typing_script_params(&state, "test", &tagmsg("alice", "&local", "+typing", "active"))
                .expect("scriptable");
        assert_eq!(params["target"], "&local");
    }
}

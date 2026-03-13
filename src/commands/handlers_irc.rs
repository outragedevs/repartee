#![allow(clippy::redundant_pub_crate)]

use super::helpers::add_local_event;
use crate::app::App;

// === Connection ===

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_connect(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(
            app,
            "Usage: /connect <server-id|label|address>[:<port>] [-tls] [-bind=<ip>]",
        );
        return;
    }

    let target = args[0].to_lowercase();

    // Parse flags from remaining args
    let mut flag_tls = false;
    let mut flag_bind: Option<String> = None;
    for arg in args.iter().skip(1) {
        if arg == "-tls" {
            flag_tls = true;
        } else if let Some(ip) = arg.strip_prefix("-bind=") {
            flag_bind = Some(ip.to_string());
        }
    }

    // 1. Try exact server ID match
    if let Some(server_config) = app.config.servers.get(&target) {
        let mut cfg = server_config.clone();
        if flag_tls {
            cfg.tls = true;
        }
        if let Some(ip) = flag_bind {
            cfg.bind_ip = Some(ip);
        }
        spawn_connection(app, &target, &cfg);
        return;
    }

    // 2. Try server label match (case-insensitive)
    {
        let found = app
            .config
            .servers
            .iter()
            .find(|(_, srv)| srv.label.to_lowercase() == target);
        if let Some((id, srv)) = found {
            let id = id.clone();
            let mut cfg = srv.clone();
            if flag_tls {
                cfg.tls = true;
            }
            if let Some(ip) = flag_bind {
                cfg.bind_ip = Some(ip);
            }
            spawn_connection(app, &id, &cfg);
            return;
        }
    }

    // 3. Ad-hoc connection: parse as address[:port]
    let raw_target = &args[0]; // preserve original case for label
    let mut address = raw_target.clone();
    let mut port: u16 = 6667;
    let mut tls = flag_tls;

    // Parse address:port
    if let Some(colon_pos) = raw_target.rfind(':') {
        let port_str = &raw_target[colon_pos + 1..];
        if let Ok(p) = port_str.parse::<u16>() {
            address = raw_target[..colon_pos].to_string();
            port = p;
        }
    }

    // Also accept port as second positional arg (not starting with -)
    if args.len() > 1
        && !args[1].starts_with('-')
        && let Ok(p) = args[1].parse::<u16>()
    {
        port = p;
    }

    // -tls auto-adjusts port from default
    if tls && port == 6667 {
        port = 6697;
    }
    // High port implies TLS
    if port == 6697 && !tls {
        tls = true;
    }

    // Generate a connection ID from the address
    let conn_id: String = address
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();

    // Check if already connected
    if app.irc_handles.contains_key(&conn_id) {
        add_local_event(app, &format!("Already connected to {address}"));
        return;
    }

    let adhoc_config = crate::config::ServerConfig {
        label: address.clone(),
        address,
        port,
        tls,
        tls_verify: true,
        autoconnect: false,
        channels: vec![],
        nick: None,
        username: None,
        realname: None,
        password: None,
        sasl_user: None,
        sasl_pass: None,
        bind_ip: flag_bind,
        encoding: None,
        auto_reconnect: None,
        reconnect_delay: None,
        reconnect_max_retries: None,
        autosendcmd: None,
        sasl_mechanism: None,
        client_cert_path: None,
    };

    spawn_connection(app, &conn_id, &adhoc_config);
}

/// Shared logic: set up connection state and spawn async connect task.
fn spawn_connection(app: &mut App, conn_id: &str, server_config: &crate::config::ServerConfig) {
    // Check if already connected
    if app.irc_handles.contains_key(conn_id) {
        add_local_event(
            app,
            &format!("Already connected to {}", server_config.label),
        );
        return;
    }

    app.setup_connection(conn_id, server_config);

    let general = app.config.general.clone();
    let tx = app.irc_tx.clone();
    let id = conn_id.to_string();
    let cfg = server_config.clone();

    tokio::spawn(async move {
        match crate::irc::connect_server(&id, &cfg, &general).await {
            Ok((handle, mut rx)) => {
                let _ = tx.send(crate::irc::IrcEvent::HandleReady(
                    handle.conn_id.clone(),
                    handle.sender,
                    handle.local_ip,
                ));
                while let Some(event) = rx.recv().await {
                    if tx.send(event).is_err() {
                        break;
                    }
                }
            }
            Err(e) => {
                let _ = tx.send(crate::irc::IrcEvent::Disconnected(id, Some(e.to_string())));
            }
        }
    });
}

pub(crate) fn cmd_disconnect(app: &mut App, args: &[String]) {
    let default_quit = crate::constants::default_quit_message();
    let quit_msg = if args.is_empty() {
        default_quit.as_str()
    } else {
        &args[0]
    };

    let Some(conn_id) = app.active_conn_id().map(str::to_owned) else {
        add_local_event(app, "No active connection");
        return;
    };

    // Disable auto-reconnect when user explicitly disconnects
    if let Some(conn) = app.state.connections.get_mut(&conn_id) {
        conn.should_reconnect = false;
        conn.next_reconnect = None;
    }

    if let Some(handle) = app.irc_handles.get(&conn_id) {
        let _ = handle.sender.send_quit(quit_msg);
    }
    app.irc_handles.remove(&conn_id);
    crate::irc::events::handle_disconnected(&mut app.state, &conn_id, None);
}

// === Channel ===

pub(crate) fn cmd_join(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /join <channel> [key]");
        return;
    }

    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    // First arg could be a key if second channel is specified, but typically:
    // /join channel [key]  or  /join #a #b #c
    let mut i = 0;
    while i < args.len() {
        let mut channel = args[i].clone();
        // Auto-prepend # if no channel prefix
        if !channel.starts_with('#')
            && !channel.starts_with('&')
            && !channel.starts_with('+')
            && !channel.starts_with('!')
        {
            channel = format!("#{channel}");
        }

        // Check if next arg is a key (not a channel name)
        let key = if i + 1 < args.len()
            && !args[i + 1].starts_with('#')
            && !args[i + 1].starts_with('&')
            && !args[i + 1].starts_with('+')
            && !args[i + 1].starts_with('!')
        {
            i += 1;
            Some(args[i].clone())
        } else {
            None
        };

        let result = key.map_or_else(
            || sender.send_join(&channel),
            |key| sender.send(irc::proto::Command::JOIN(channel.clone(), Some(key), None)),
        );

        if let Err(e) = result {
            add_local_event(app, &format!("Failed to join {channel}: {e}"));
        }
        i += 1;
    }
}

pub(crate) fn cmd_part(app: &mut App, args: &[String]) {
    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    let (channel, reason) = if args.is_empty() {
        let Some(buf) = app.state.active_buffer() else {
            return;
        };
        (buf.name.clone(), None)
    } else if args.len() == 1 {
        if crate::irc::formatting::is_channel(&args[0]) {
            (args[0].clone(), None)
        } else {
            let Some(buf) = app.state.active_buffer() else {
                return;
            };
            (buf.name.clone(), Some(args[0].as_str()))
        }
    } else {
        (args[0].clone(), Some(args[1].as_str()))
    };

    let default_part = crate::constants::default_quit_message();
    let part_reason = reason.unwrap_or(default_part.as_str());
    let result = sender.send(irc::proto::Command::PART(
        channel,
        Some(part_reason.to_string()),
    ));
    if let Err(e) = result {
        add_local_event(app, &format!("Failed to part: {e}"));
    }
}

pub(crate) fn cmd_topic(app: &mut App, args: &[String]) {
    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    if args.is_empty() {
        if let Some(buf) = app.state.active_buffer() {
            match &buf.topic {
                Some(topic) => {
                    let setter = buf.topic_set_by.as_deref().unwrap_or("unknown");
                    add_local_event(
                        app,
                        &format!("Topic for {}: {} (set by {setter})", buf.name, topic),
                    );
                }
                None => {
                    add_local_event(app, &format!("No topic set for {}", buf.name));
                }
            }
        }
        return;
    }

    let (channel, topic) = if args.len() == 1 {
        if crate::irc::formatting::is_channel(&args[0]) {
            let _ = sender.send(irc::proto::Command::TOPIC(args[0].clone(), None));
            return;
        }
        let Some(buf) = app.state.active_buffer() else {
            return;
        };
        (buf.name.clone(), args[0].clone())
    } else {
        (args[0].clone(), args[1].clone())
    };

    if let Err(e) = sender.send(irc::proto::Command::TOPIC(channel, Some(topic))) {
        add_local_event(app, &format!("Failed to set topic: {e}"));
    }
}

pub(crate) fn cmd_kick(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /kick <nick> [reason]");
        return;
    }

    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    let nick = &args[0];
    let reason = if args.len() > 1 {
        Some(args[1].clone())
    } else {
        None
    };

    let Some(buf) = app.state.active_buffer() else {
        return;
    };
    let channel = buf.name.clone();

    if let Err(e) = sender.send(irc::proto::Command::KICK(channel, nick.clone(), reason)) {
        add_local_event(app, &format!("Failed to kick {nick}: {e}"));
    }
}

pub(crate) fn cmd_invite(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /invite <nick> [channel]");
        return;
    }

    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    let nick = &args[0];
    let channel = if args.len() > 1 {
        args[1].clone()
    } else {
        let Some(buf) = app.state.active_buffer() else {
            return;
        };
        buf.name.clone()
    };

    if let Err(e) = sender.send(irc::proto::Command::INVITE(nick.clone(), channel)) {
        add_local_event(app, &format!("Failed to invite {nick}: {e}"));
    }
}

pub(crate) fn cmd_names(app: &mut App, args: &[String]) {
    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    let channel = if args.is_empty() {
        let Some(buf) = app.state.active_buffer() else {
            return;
        };
        buf.name.clone()
    } else {
        args[0].clone()
    };

    if let Err(e) = sender.send(irc::proto::Command::NAMES(Some(channel), None)) {
        add_local_event(app, &format!("Failed to send NAMES: {e}"));
    }
}

// === Mode commands ===

pub(crate) fn cmd_mode(app: &mut App, args: &[String]) {
    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    if args.is_empty() {
        // Query own user modes
        let Some(conn_id) = app.active_conn_id() else {
            add_local_event(app, "Not connected");
            return;
        };
        let nick = app
            .state
            .connections
            .get(conn_id)
            .map(|c| c.nick.clone())
            .unwrap_or_default();
        let _ = sender.send(irc::proto::Command::Raw("MODE".to_string(), vec![nick]));
        return;
    }

    // Send MODE with all args
    let _ = sender.send(irc::proto::Command::Raw("MODE".to_string(), args.to_vec()));
}

fn set_nick_mode(app: &mut App, mode_char: char, adding: bool, args: &[String]) {
    if args.is_empty() {
        let cmd = match (mode_char, adding) {
            ('o', true) => "op",
            ('o', false) => "deop",
            ('v', true) => "voice",
            ('v', false) => "devoice",
            _ => "mode",
        };
        add_local_event(app, &format!("Usage: /{cmd} <nick> [nick2...]"));
        return;
    }

    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    let channel = match app.state.active_buffer() {
        Some(b) if b.buffer_type == crate::state::buffer::BufferType::Channel => b.name.clone(),
        _ => {
            add_local_event(app, "Not in a channel");
            return;
        }
    };

    let sign = if adding { "+" } else { "-" };
    let modes: String = std::iter::repeat_n(mode_char, args.len()).collect();
    let mut cmd_args = vec![channel, format!("{sign}{modes}")];
    cmd_args.extend(args.iter().cloned());
    let _ = sender.send(irc::proto::Command::Raw("MODE".to_string(), cmd_args));
}

pub(crate) fn cmd_op(app: &mut App, args: &[String]) {
    set_nick_mode(app, 'o', true, args);
}

pub(crate) fn cmd_deop(app: &mut App, args: &[String]) {
    set_nick_mode(app, 'o', false, args);
}

pub(crate) fn cmd_voice(app: &mut App, args: &[String]) {
    set_nick_mode(app, 'v', true, args);
}

pub(crate) fn cmd_devoice(app: &mut App, args: &[String]) {
    set_nick_mode(app, 'v', false, args);
}

pub(crate) fn cmd_ban(app: &mut App, args: &[String]) {
    // `/ban -a <account>` shorthand: compose an account extban mask
    if args.len() >= 2 && args[0] == "-a" {
        let account = &args[1];
        let Some(sender) = app.active_irc_sender().cloned() else {
            add_local_event(app, "Not connected");
            return;
        };
        let channel = match app.state.active_buffer() {
            Some(b) if b.buffer_type == crate::state::buffer::BufferType::Channel => b.name.clone(),
            _ => {
                add_local_event(app, "Not in a channel");
                return;
            }
        };
        let conn_id = app
            .state
            .active_buffer()
            .map(|b| b.connection_id.clone())
            .unwrap_or_default();
        let extban_info = app
            .state
            .connections
            .get(&conn_id)
            .and_then(|c| c.isupport_parsed.extban());
        let Some((prefix, types)) = extban_info else {
            add_local_event(app, "Server does not advertise EXTBAN support");
            return;
        };
        if !types.contains('a') {
            add_local_event(app, "Server EXTBAN does not support account type ('a')");
            return;
        }
        let mask = crate::irc::extban::compose_account_ban(account, Some(prefix));
        let _ = sender.send(irc::proto::Command::Raw(
            "MODE".to_string(),
            vec![channel, "+b".to_string(), mask],
        ));
        return;
    }
    list_mode_set(app, args, 'b');
}

pub(crate) fn cmd_unban(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /unban <number|mask|wildcard> [...]");
        return;
    }
    list_mode_unset_smart(app, args, 'b', "unban");
}

pub(crate) fn cmd_kickban(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /kb <nick> [reason]");
        return;
    }

    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    let channel = match app.state.active_buffer() {
        Some(b) if b.buffer_type == crate::state::buffer::BufferType::Channel => b.name.clone(),
        _ => {
            add_local_event(app, "Not in a channel");
            return;
        }
    };

    let nick = &args[0];
    let reason = if args.len() > 1 {
        args[1..].join(" ")
    } else {
        nick.clone()
    };

    // Resolve ban mask from cached WHOX data (ident + host)
    // Falls back to nick!*@* if user info is not available
    let ban_mask = app
        .state
        .active_buffer()
        .and_then(|buf| buf.users.get(&nick.to_lowercase()))
        .and_then(|entry| match (&entry.ident, &entry.host) {
            (Some(ident), Some(host)) => Some(format!("*!*{ident}@{host}")),
            _ => None,
        })
        .unwrap_or_else(|| format!("{nick}!*@*"));

    // KICK first, then BAN (same order as kokoirc)
    let _ = sender.send(irc::proto::Command::KICK(
        channel.clone(),
        nick.clone(),
        Some(reason),
    ));
    let _ = sender.send(irc::proto::Command::Raw(
        "MODE".to_string(),
        vec![channel, "+b".to_string(), ban_mask],
    ));
}

// Generic list mode helper: request list or set/unset mode
fn list_mode_set(app: &mut App, args: &[String], mode_char: char) {
    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };
    let channel = match app.state.active_buffer() {
        Some(b) if b.buffer_type == crate::state::buffer::BufferType::Channel => b.name.clone(),
        _ => {
            add_local_event(app, "Not in a channel");
            return;
        }
    };
    if args.is_empty() {
        // Clear stored list before requesting fresh one from server
        if let Some(buf) = app.state.active_buffer_mut() {
            buf.list_modes.remove(&mode_char.to_string());
        }
        let _ = sender.send(irc::proto::Command::Raw(
            "MODE".to_string(),
            vec![channel, format!("+{mode_char}")],
        ));
    } else {
        let _ = sender.send(irc::proto::Command::Raw(
            "MODE".to_string(),
            vec![channel, format!("+{mode_char}"), args[0].clone()],
        ));
    }
}

/// Unset list modes with support for numeric indices and wildcard patterns.
///
/// - Numeric args (e.g. `1`, `3`) index into the stored list (1-based).
/// - Args containing `*` or `?` are matched against stored entries (like irssi's `/unban *`).
/// - Everything else is sent as a literal mask.
fn list_mode_unset_smart(app: &mut App, args: &[String], mode_char: char, cmd_name: &str) {
    if args.is_empty() {
        add_local_event(
            app,
            &format!("Usage: /{cmd_name} <number|mask|wildcard> [...]"),
        );
        return;
    }
    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };
    let channel = match app.state.active_buffer() {
        Some(b) if b.buffer_type == crate::state::buffer::BufferType::Channel => b.name.clone(),
        _ => {
            add_local_event(app, "Not in a channel");
            return;
        }
    };

    let mode_key = mode_char.to_string();
    let entries: Vec<crate::state::buffer::ListEntry> = app
        .state
        .active_buffer()
        .and_then(|b| b.list_modes.get(&mode_key))
        .cloned()
        .unwrap_or_default();

    let mut masks: Vec<String> = Vec::new();
    for arg in args {
        if let Ok(num) = arg.parse::<usize>() {
            // Numeric index into stored list (1-based)
            if num >= 1 && num <= entries.len() {
                masks.push(entries[num - 1].mask.clone());
            } else {
                add_local_event(
                    app,
                    &format!("{cmd_name}: #{num} out of range (1-{})", entries.len()),
                );
            }
        } else if arg.contains('*') || arg.contains('?') {
            // Wildcard pattern — match against stored list entries
            let re = crate::irc::ignore::wildcard_to_regex(arg);
            let mut found = false;
            for entry in &entries {
                if re.is_match(&entry.mask) {
                    masks.push(entry.mask.clone());
                    found = true;
                }
            }
            if !found {
                add_local_event(app, &format!("{cmd_name}: no entries matching '{arg}'"));
            }
        } else {
            // Literal mask — send as-is
            masks.push(arg.clone());
        }
    }

    for mask in &masks {
        let _ = sender.send(irc::proto::Command::Raw(
            "MODE".to_string(),
            vec![channel.clone(), format!("-{mode_char}"), mask.clone()],
        ));
    }
}

pub(crate) fn cmd_except(app: &mut App, args: &[String]) {
    list_mode_set(app, args, 'e');
}

pub(crate) fn cmd_unexcept(app: &mut App, args: &[String]) {
    list_mode_unset_smart(app, args, 'e', "unexcept");
}

pub(crate) fn cmd_invex(app: &mut App, args: &[String]) {
    list_mode_set(app, args, 'I');
}

pub(crate) fn cmd_uninvex(app: &mut App, args: &[String]) {
    list_mode_unset_smart(app, args, 'I', "uninvex");
}

pub(crate) fn cmd_reop(app: &mut App, args: &[String]) {
    list_mode_set(app, args, 'R');
}

pub(crate) fn cmd_unreop(app: &mut App, args: &[String]) {
    list_mode_unset_smart(app, args, 'R', "unreop");
}

pub(crate) fn cmd_cycle(app: &mut App, args: &[String]) {
    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    let (channel, reason) = if args.is_empty() {
        let Some(buf) = app.state.active_buffer() else {
            return;
        };
        if buf.buffer_type != crate::state::buffer::BufferType::Channel {
            add_local_event(app, "Not in a channel");
            return;
        }
        (buf.name.clone(), None)
    } else if crate::irc::formatting::is_channel(&args[0]) {
        let reason = if args.len() > 1 {
            Some(args[1].as_str())
        } else {
            None
        };
        (args[0].clone(), reason)
    } else {
        // Treat first arg as reason for current channel
        let Some(buf) = app.state.active_buffer() else {
            return;
        };
        if buf.buffer_type != crate::state::buffer::BufferType::Channel {
            add_local_event(app, "Not in a channel");
            return;
        }
        (buf.name.clone(), Some(args[0].as_str()))
    };

    // Collect the channel key if one is set (to rejoin key-protected channels)
    let key = app
        .state
        .active_buffer()
        .and_then(|b| b.mode_params.as_ref())
        .and_then(|p| p.get("k").cloned());

    // PART
    let part_result = reason.map_or_else(
        || sender.send(irc::proto::Command::PART(channel.clone(), None)),
        |reason| {
            sender.send(irc::proto::Command::PART(
                channel.clone(),
                Some(reason.to_string()),
            ))
        },
    );
    if let Err(e) = part_result {
        add_local_event(app, &format!("Failed to cycle {channel}: {e}"));
        return;
    }

    // JOIN (sent immediately — IRC guarantees command ordering on a single connection)
    let join_result = key.map_or_else(
        || sender.send_join(&channel),
        |key| sender.send(irc::proto::Command::JOIN(channel.clone(), Some(key), None)),
    );
    if let Err(e) = join_result {
        add_local_event(app, &format!("Failed to rejoin {channel}: {e}"));
    }
}

/// Create a query buffer for `target` if one doesn't already exist.
/// When `skip_channels` is true, channel targets are not created (used by /msg).
fn ensure_query_buffer(app: &mut App, conn_id: &str, target: &str, skip_channels: bool) -> String {
    let buffer_id = crate::state::buffer::make_buffer_id(conn_id, target);
    let should_create = !app.state.buffers.contains_key(&buffer_id)
        && (!skip_channels || !crate::irc::formatting::is_channel(target));
    if should_create {
        app.state.add_buffer(crate::state::buffer::Buffer {
            id: buffer_id.clone(),
            connection_id: conn_id.to_string(),
            buffer_type: crate::state::buffer::BufferType::Query,
            name: target.to_string(),
            messages: Vec::new(),
            activity: crate::state::buffer::ActivityLevel::None,
            unread_count: 0,
            last_read: chrono::Utc::now(),
            topic: None,
            topic_set_by: None,
            users: std::collections::HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: std::collections::HashMap::new(),
            last_speakers: Vec::new(),
        });
    }
    buffer_id
}

// === Messaging ===

pub(crate) fn cmd_msg(app: &mut App, args: &[String]) {
    if args.len() < 2 {
        add_local_event(app, "Usage: /msg <target> <message>");
        return;
    }

    let target = &args[0];
    let text = &args[1];

    // DCC CHAT routing: /msg =nick sends via DCC, not IRC.
    if let Some(dcc_nick) = target.strip_prefix('=') {
        if let Some(record) = app.dcc.find_connected(dcc_nick) {
            let record_id = record.id.clone();
            let conn_id = record.conn_id.clone();
            if let Err(e) = app.dcc.send_chat_line(&record_id, text) {
                add_local_event(app, &format!("DCC send error: {e}"));
                return;
            }
            // Display locally in the DCC buffer
            let buf_name = format!("={dcc_nick}");
            let buffer_id = crate::state::buffer::make_buffer_id(&conn_id, &buf_name);
            let our_nick = app
                .state
                .connections
                .values()
                .next()
                .map(|c| c.nick.clone())
                .unwrap_or_default();
            let msg_id = app.state.next_message_id();
            app.state.add_message(
                &buffer_id,
                crate::state::buffer::Message {
                    id: msg_id,
                    timestamp: chrono::Utc::now(),
                    message_type: crate::state::buffer::MessageType::Message,
                    nick: Some(our_nick),
                    nick_mode: None,
                    text: text.clone(),
                    highlight: false,
                    event_key: None,
                    event_params: None,
                    log_msg_id: None,
                    log_ref_id: None,
                    tags: std::collections::HashMap::new(),
                },
            );
        } else {
            add_local_event(app, &format!("No active DCC CHAT session with {dcc_nick}"));
        }
        return;
    }

    let (conn_id, nick) = {
        let Some(conn_id) = app.active_conn_id().map(str::to_owned) else {
            add_local_event(app, "No active connection");
            return;
        };
        let nick = app
            .state
            .connections
            .get(&conn_id)
            .map(|c| c.nick.clone())
            .unwrap_or_default();
        (conn_id, nick)
    };

    // Create query buffer if needed (skip channels for /msg)
    let buffer_id = ensure_query_buffer(app, &conn_id, target, true);

    // When echo-message is enabled, skip local display — the server echo is authoritative.
    let echo_message_enabled = app
        .state
        .connections
        .get(&conn_id)
        .is_some_and(|c| c.enabled_caps.contains("echo-message"));

    // Split long messages at word boundaries to stay within IRC byte limits.
    let chunks = crate::irc::split_irc_message(text, crate::irc::MESSAGE_MAX_BYTES);
    let own_mode = app.state.nick_prefix(&buffer_id, &nick);
    for chunk in chunks {
        if let Some(handle) = app.irc_handles.get(&conn_id)
            && let Err(e) = handle.sender.send_privmsg(target, &chunk)
        {
            add_local_event(app, &format!("Failed to send message: {e}"));
            return;
        }

        if !echo_message_enabled {
            let id = app.state.next_message_id();
            app.state.add_message(
                &buffer_id,
                crate::state::buffer::Message {
                    id,
                    timestamp: chrono::Utc::now(),
                    message_type: crate::state::buffer::MessageType::Message,
                    nick: Some(nick.clone()),
                    nick_mode: own_mode.map(|c| c.to_string()),
                    text: chunk,
                    highlight: false,
                    event_key: None,
                    event_params: None,
                    log_msg_id: None,
                    log_ref_id: None,
                    tags: std::collections::HashMap::new(),
                },
            );
        }
    }
    // /msg stays in the current window — buffer is created but not switched to.
    // Use /query to open and switch to a conversation.
}

pub(crate) fn cmd_query(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /query <nick> [message]");
        return;
    }

    let target = &args[0];
    let Some(conn_id) = app.active_conn_id().map(str::to_owned) else {
        add_local_event(app, "No active connection");
        return;
    };

    // Create query buffer if it doesn't exist (allow channels for /query)
    let buffer_id = ensure_query_buffer(app, &conn_id, target, false);

    // Switch to the query buffer
    app.state.set_active_buffer(&buffer_id);

    // If a message was provided, send it
    if args.len() >= 2 {
        let text = &args[1];
        let nick = app
            .state
            .connections
            .get(&conn_id)
            .map(|c| c.nick.clone())
            .unwrap_or_default();

        if let Some(handle) = app.irc_handles.get(&conn_id)
            && let Err(e) = handle.sender.send_privmsg(target, text)
        {
            add_local_event(app, &format!("Failed to send message: {e}"));
            return;
        }

        // When echo-message is enabled, skip local display — the server echo is authoritative.
        let echo_message_enabled = app
            .state
            .connections
            .get(&conn_id)
            .is_some_and(|c| c.enabled_caps.contains("echo-message"));

        if !echo_message_enabled {
            let own_mode = app.state.nick_prefix(&buffer_id, &nick);
            let id = app.state.next_message_id();
            app.state.add_message(
                &buffer_id,
                crate::state::buffer::Message {
                    id,
                    timestamp: chrono::Utc::now(),
                    message_type: crate::state::buffer::MessageType::Message,
                    nick: Some(nick),
                    nick_mode: own_mode.map(|c| c.to_string()),
                    text: text.clone(),
                    highlight: false,
                    event_key: None,
                    event_params: None,
                    log_msg_id: None,
                    log_ref_id: None,
                    tags: std::collections::HashMap::new(),
                },
            );
        }
    }
}

pub(crate) fn cmd_me(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /me <action>");
        return;
    }

    let action_text = &args[0];
    let Some(buf) = app.state.active_buffer() else {
        return;
    };
    let target = buf.name.clone();
    let conn_id = buf.connection_id.clone();
    let buf_type = buf.buffer_type.clone();

    // DCC CHAT: send ACTION via DCC channel, not IRC.
    if buf_type == crate::state::buffer::BufferType::DccChat {
        let dcc_nick = target.strip_prefix('=').unwrap_or(&target);
        if let Some(record) = app.dcc.find_connected(dcc_nick) {
            let record_id = record.id.clone();
            let ctcp = format!("\x01ACTION {action_text}\x01");
            if let Err(e) = app.dcc.send_chat_line(&record_id, &ctcp) {
                add_local_event(app, &format!("DCC send error: {e}"));
                return;
            }
            // Display locally
            let our_nick = app
                .state
                .connections
                .values()
                .next()
                .map(|c| c.nick.clone())
                .unwrap_or_default();
            let buffer_id = app.state.active_buffer_id.clone().unwrap_or_default();
            let msg_id = app.state.next_message_id();
            app.state.add_message(
                &buffer_id,
                crate::state::buffer::Message {
                    id: msg_id,
                    timestamp: chrono::Utc::now(),
                    message_type: crate::state::buffer::MessageType::Action,
                    nick: Some(our_nick),
                    nick_mode: None,
                    text: action_text.clone(),
                    highlight: false,
                    event_key: None,
                    event_params: None,
                    log_msg_id: None,
                    log_ref_id: None,
                    tags: std::collections::HashMap::new(),
                },
            );
        } else {
            add_local_event(app, "No active DCC CHAT session for this buffer");
        }
        return;
    }

    let nick = app
        .state
        .connections
        .get(&conn_id)
        .map(|c| c.nick.clone())
        .unwrap_or_default();

    let Some(handle) = app.irc_handles.get(&conn_id) else {
        add_local_event(app, "Not connected");
        return;
    };
    let ctcp = format!("\x01ACTION {action_text}\x01");
    if let Err(e) = handle.sender.send_privmsg(&target, &ctcp) {
        add_local_event(app, &format!("Failed to send action: {e}"));
        return;
    }

    // When echo-message is enabled, skip local display — the server echo is authoritative.
    let echo_message_enabled = app
        .state
        .connections
        .get(&conn_id)
        .is_some_and(|c| c.enabled_caps.contains("echo-message"));

    if !echo_message_enabled {
        let buffer_id = app.state.active_buffer_id.clone().unwrap_or_default();
        let own_mode = app.state.nick_prefix(&buffer_id, &nick);
        let id = app.state.next_message_id();
        app.state.add_message(
            &buffer_id,
            crate::state::buffer::Message {
                id,
                timestamp: chrono::Utc::now(),
                message_type: crate::state::buffer::MessageType::Action,
                nick: Some(nick),
                nick_mode: own_mode.map(|c| c.to_string()),
                text: action_text.clone(),
                highlight: false,
                event_key: None,
                event_params: None,
                log_msg_id: None,
                log_ref_id: None,
                tags: std::collections::HashMap::new(),
            },
        );
    }
}

pub(crate) fn cmd_nick(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /nick <new_nick>");
        return;
    }

    let new_nick = &args[0];

    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    if let Err(e) = sender.send(irc::proto::Command::NICK(new_nick.clone())) {
        add_local_event(app, &format!("Failed to change nick: {e}"));
    }
}

pub(crate) fn cmd_notice(app: &mut App, args: &[String]) {
    if args.len() < 2 {
        add_local_event(app, "Usage: /notice <target> <message>");
        return;
    }

    let target = &args[0];
    let text = &args[1];

    if let Some(sender) = app.active_irc_sender() {
        if let Err(e) = sender.send_notice(target, text) {
            add_local_event(app, &format!("Failed to send notice: {e}"));
        }
    } else {
        add_local_event(app, "Not connected");
    }
}

// === Info ===

pub(crate) fn cmd_whois(app: &mut App, args: &[String]) {
    let nick = if args.is_empty() {
        whois_default_nick(app)
    } else {
        Some(args[0].clone())
    };
    let Some(nick) = nick else {
        add_local_event(app, "Usage: /whois <nick>");
        return;
    };

    if let Some(sender) = app.active_irc_sender() {
        if let Err(e) = sender.send(irc::proto::Command::WHOIS(None, nick)) {
            add_local_event(app, &format!("Failed to send WHOIS: {e}"));
        }
    } else {
        add_local_event(app, "Not connected");
    }
}

pub(crate) fn cmd_wii(app: &mut App, args: &[String]) {
    let nick = if args.is_empty() {
        whois_default_nick(app)
    } else {
        Some(args[0].clone())
    };
    let Some(nick) = nick else {
        add_local_event(app, "Usage: /wii <nick>");
        return;
    };

    if let Some(sender) = app.active_irc_sender() {
        // WHOIS nick nick — queries the user's server for idle info
        if let Err(e) = sender.send(irc::proto::Command::WHOIS(Some(nick.clone()), nick)) {
            add_local_event(app, &format!("Failed to send WHOIS: {e}"));
        }
    } else {
        add_local_event(app, "Not connected");
    }
}

/// Default nick for /whois when no argument given.
/// In a query buffer: use the query target. Otherwise: use our own nick.
fn whois_default_nick(app: &App) -> Option<String> {
    use crate::state::buffer::BufferType;
    let buf = app.state.active_buffer()?;
    if buf.buffer_type == BufferType::Query {
        return Some(buf.name.clone());
    }
    let conn = app.state.connections.get(&buf.connection_id)?;
    Some(conn.nick.clone())
}

pub(crate) fn cmd_version(app: &mut App, args: &[String]) {
    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    if args.is_empty() {
        // Server version
        let _ = sender.send(irc::proto::Command::Raw("VERSION".to_string(), vec![]));
    } else {
        // CTCP VERSION to nick
        let ctcp = "\x01VERSION\x01".to_string();
        let _ = sender.send_privmsg(&args[0], &ctcp);
    }
}

pub(crate) fn cmd_quote(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /quote <raw command>");
        return;
    }

    let raw = &args[0];

    if let Some(sender) = app.active_irc_sender() {
        let parts: Vec<&str> = raw.splitn(2, ' ').collect();
        let command = parts[0].to_string();
        #[allow(clippy::option_if_let_else)]
        let args_vec: Vec<String> = if parts.len() > 1 {
            let rest = parts[1];
            if let Some(colon_pos) = rest.find(" :") {
                let before_trailing = &rest[..colon_pos];
                let trailing = &rest[colon_pos + 2..];
                let mut args: Vec<String> = before_trailing
                    .split_whitespace()
                    .map(String::from)
                    .collect();
                args.push(trailing.to_string());
                args
            } else if let Some(trailing) = rest.strip_prefix(':') {
                vec![trailing.to_string()]
            } else {
                rest.split_whitespace().map(String::from).collect()
            }
        } else {
            vec![]
        };
        if let Err(e) = sender.send(irc::proto::Command::Raw(command, args_vec)) {
            add_local_event(app, &format!("Failed to send: {e}"));
        }
    } else {
        add_local_event(app, "Not connected");
    }
}

// === Away ===

pub(crate) fn cmd_away(app: &mut App, args: &[String]) {
    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    let result = if args.is_empty() {
        // Clear away status
        sender.send(irc::proto::Command::AWAY(None))
    } else {
        // Set away with reason
        sender.send(irc::proto::Command::AWAY(Some(args[0].clone())))
    };
    if let Err(e) = result {
        add_local_event(app, &format!("Failed to send AWAY: {e}"));
    }
}

// === List ===

pub(crate) fn cmd_list(app: &mut App, args: &[String]) {
    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    let result = if args.is_empty() {
        sender.send(irc::proto::Command::LIST(None, None))
    } else {
        sender.send(irc::proto::Command::LIST(Some(args[0].clone()), None))
    };
    if let Err(e) = result {
        add_local_event(app, &format!("Failed to send LIST: {e}"));
    }
}

// === Who ===

pub(crate) fn cmd_who(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /who <target>");
        return;
    }

    let Some(conn_id) = app.active_conn_id().map(str::to_owned) else {
        add_local_event(app, "No active connection");
        return;
    };

    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    let target = &args[0];

    // Send WHOX if supported, otherwise standard WHO (never silent — manual request)
    let result = if let Some((who_target, fields)) =
        crate::irc::events::build_whox_who(&mut app.state, &conn_id, target, false)
    {
        sender.send(irc::proto::Command::Raw(
            "WHO".to_string(),
            vec![who_target, fields],
        ))
    } else {
        sender.send(irc::proto::Command::WHO(Some(target.clone()), None))
    };
    if let Err(e) = result {
        add_local_event(app, &format!("Failed to send WHO: {e}"));
    }
}

// === Whowas ===

pub(crate) fn cmd_whowas(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /whowas <nick>");
        return;
    }

    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    if let Err(e) = sender.send(irc::proto::Command::WHOWAS(args[0].clone(), None, None)) {
        add_local_event(app, &format!("Failed to send WHOWAS: {e}"));
    }
}

// === Server Query Commands (RFC 2812 3.4) ===

pub(crate) fn cmd_info(app: &mut App, args: &[String]) {
    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    let server = args.first().cloned();
    if let Err(e) = sender.send(irc::proto::Command::INFO(server)) {
        add_local_event(app, &format!("Failed to send INFO: {e}"));
    }
}

pub(crate) fn cmd_admin(app: &mut App, args: &[String]) {
    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    let server = args.first().cloned();
    if let Err(e) = sender.send(irc::proto::Command::ADMIN(server)) {
        add_local_event(app, &format!("Failed to send ADMIN: {e}"));
    }
}

pub(crate) fn cmd_lusers(app: &mut App, args: &[String]) {
    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    let mask = args.first().cloned();
    let server = args.get(1).cloned();
    if let Err(e) = sender.send(irc::proto::Command::LUSERS(mask, server)) {
        add_local_event(app, &format!("Failed to send LUSERS: {e}"));
    }
}

pub(crate) fn cmd_time(app: &mut App, args: &[String]) {
    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    let server = args.first().cloned();
    if let Err(e) = sender.send(irc::proto::Command::TIME(server)) {
        add_local_event(app, &format!("Failed to send TIME: {e}"));
    }
}

pub(crate) fn cmd_links(app: &mut App, args: &[String]) {
    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    let remote = args.first().cloned();
    let mask = args.get(1).cloned();
    if let Err(e) = sender.send(irc::proto::Command::LINKS(remote, mask)) {
        add_local_event(app, &format!("Failed to send LINKS: {e}"));
    }
}

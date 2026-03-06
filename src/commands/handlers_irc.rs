#![allow(clippy::redundant_pub_crate)]

use crate::app::App;
use super::helpers::add_local_event;

// === Connection ===

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
    let quit_msg = if args.is_empty() {
        "Leaving"
    } else {
        &args[0]
    };

    let Some(conn_id) = app.active_conn_id() else {
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
            && args.len() == 2
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

    let result = if let Some(reason) = reason {
        sender.send(irc::proto::Command::PART(channel, Some(reason.to_string())))
    } else {
        sender.send(irc::proto::Command::PART(channel, None))
    };
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
                    let setter = buf
                        .topic_set_by
                        .as_deref()
                        .unwrap_or("unknown");
                    add_local_event(
                        app,
                        &format!("Topic for {}: {} (set by {setter})", buf.name, topic),
                    );
                }
                None => {
                    add_local_event(
                        app,
                        &format!("No topic set for {}", buf.name),
                    );
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
        let nick = app
            .state
            .connections
            .get(&app.active_conn_id().unwrap_or_default())
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
    list_mode_set(app, args, 'b');
}

pub(crate) fn cmd_unban(app: &mut App, args: &[String]) {
    list_mode_unset(app, args, 'b', "unban");
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
        args[1].clone()
    } else {
        nick.clone()
    };

    // Ban with nick!*@* mask (simple fallback — no USERHOST lookup)
    let ban_mask = format!("{nick}!*@*");
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

fn list_mode_unset(app: &mut App, args: &[String], mode_char: char, cmd_name: &str) {
    if args.is_empty() {
        add_local_event(app, &format!("Usage: /{cmd_name} <mask>"));
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
    for mask in args {
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
    list_mode_unset(app, args, 'e', "unexcept");
}

pub(crate) fn cmd_invex(app: &mut App, args: &[String]) {
    list_mode_set(app, args, 'I');
}

pub(crate) fn cmd_uninvex(app: &mut App, args: &[String]) {
    list_mode_unset(app, args, 'I', "uninvex");
}

pub(crate) fn cmd_reop(app: &mut App, args: &[String]) {
    list_mode_set(app, args, 'R');
}

pub(crate) fn cmd_unreop(app: &mut App, args: &[String]) {
    list_mode_unset(app, args, 'R', "unreop");
}

// === Messaging ===

pub(crate) fn cmd_msg(app: &mut App, args: &[String]) {
    if args.len() < 2 {
        add_local_event(app, "Usage: /msg <target> <message>");
        return;
    }

    let target = &args[0];
    let text = &args[1];

    let (conn_id, nick) = {
        let Some(conn_id) = app.active_conn_id() else {
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

    if let Some(handle) = app.irc_handles.get(&conn_id)
        && let Err(e) = handle.sender.send_privmsg(target, text)
    {
        add_local_event(
            app,
            &format!("Failed to send message: {e}"),
        );
        return;
    }

    // Create query buffer if needed and echo
    let buffer_id = crate::state::buffer::make_buffer_id(&conn_id, target);
    if !app.state.buffers.contains_key(&buffer_id) && !crate::irc::formatting::is_channel(target) {
        app.state.add_buffer(crate::state::buffer::Buffer {
            id: buffer_id.clone(),
            connection_id: conn_id.clone(),
            buffer_type: crate::state::buffer::BufferType::Query,
            name: target.clone(),
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
        });
    }

    // When echo-message is enabled, skip local display — the server echo is authoritative.
    let echo_message_enabled = app
        .state
        .connections
        .get(&conn_id)
        .is_some_and(|c| c.enabled_caps.contains("echo-message"));

    if !echo_message_enabled {
        let id = app.state.next_message_id();
        app.state.add_message(
            &buffer_id,
            crate::state::buffer::Message {
                id,
                timestamp: chrono::Utc::now(),
                message_type: crate::state::buffer::MessageType::Message,
                nick: Some(nick),
                nick_mode: None,
                text: text.clone(),
                highlight: false,
                event_key: None,
                event_params: None, log_msg_id: None, log_ref_id: None,
                tags: std::collections::HashMap::new(),
            },
        );
    }
    app.state.set_active_buffer(&buffer_id);
}

pub(crate) fn cmd_query(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /query <nick> [message]");
        return;
    }

    let target = &args[0];
    let Some(conn_id) = app.active_conn_id() else {
        add_local_event(app, "No active connection");
        return;
    };

    // Create query buffer if it doesn't exist
    let buffer_id = crate::state::buffer::make_buffer_id(&conn_id, target);
    if !app.state.buffers.contains_key(&buffer_id) {
        app.state.add_buffer(crate::state::buffer::Buffer {
            id: buffer_id.clone(),
            connection_id: conn_id.clone(),
            buffer_type: crate::state::buffer::BufferType::Query,
            name: target.clone(),
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
        });
    }

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
            let id = app.state.next_message_id();
            app.state.add_message(
                &buffer_id,
                crate::state::buffer::Message {
                    id,
                    timestamp: chrono::Utc::now(),
                    message_type: crate::state::buffer::MessageType::Message,
                    nick: Some(nick),
                    nick_mode: None,
                    text: text.clone(),
                    highlight: false,
                    event_key: None,
                    event_params: None, log_msg_id: None, log_ref_id: None,
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
    let nick = app
        .state
        .connections
        .get(&conn_id)
        .map(|c| c.nick.clone())
        .unwrap_or_default();

    if let Some(handle) = app.irc_handles.get(&conn_id) {
        let ctcp = format!("\x01ACTION {action_text}\x01");
        if let Err(e) = handle.sender.send_privmsg(&target, &ctcp) {
            add_local_event(app, &format!("Failed to send action: {e}"));
            return;
        }
    }

    // When echo-message is enabled, skip local display — the server echo is authoritative.
    let echo_message_enabled = app
        .state
        .connections
        .get(&conn_id)
        .is_some_and(|c| c.enabled_caps.contains("echo-message"));

    if !echo_message_enabled {
        let buffer_id = app.state.active_buffer_id.clone().unwrap_or_default();
        let id = app.state.next_message_id();
        app.state.add_message(
            &buffer_id,
            crate::state::buffer::Message {
                id,
                timestamp: chrono::Utc::now(),
                message_type: crate::state::buffer::MessageType::Action,
                nick: Some(nick),
                nick_mode: None,
                text: action_text.clone(),
                highlight: false,
                event_key: None,
                event_params: None, log_msg_id: None, log_ref_id: None,
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
    if args.is_empty() {
        add_local_event(app, "Usage: /whois <nick>");
        return;
    }

    if let Some(sender) = app.active_irc_sender() {
        if let Err(e) = sender.send(irc::proto::Command::WHOIS(None, args[0].clone())) {
            add_local_event(app, &format!("Failed to send WHOIS: {e}"));
        }
    } else {
        add_local_event(app, "Not connected");
    }
}

pub(crate) fn cmd_wii(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /wii <nick>");
        return;
    }

    if let Some(sender) = app.active_irc_sender() {
        // WHOIS nick nick — queries the user's server for idle info
        if let Err(e) = sender.send(irc::proto::Command::WHOIS(
            Some(args[0].clone()),
            args[0].clone(),
        )) {
            add_local_event(app, &format!("Failed to send WHOIS: {e}"));
        }
    } else {
        add_local_event(app, "Not connected");
    }
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
                let mut args: Vec<String> =
                    before_trailing.split_whitespace().map(String::from).collect();
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

    let Some(sender) = app.active_irc_sender().cloned() else {
        add_local_event(app, "Not connected");
        return;
    };

    if let Err(e) = sender.send(irc::proto::Command::WHO(Some(args[0].clone()), None)) {
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

use crate::app::App;

pub type CommandHandler = fn(&mut App, &[String]);

pub struct CommandDef {
    pub handler: CommandHandler,
    pub description: &'static str,
    pub usage: &'static str,
}

pub fn get_commands() -> Vec<(&'static str, CommandDef)> {
    vec![
        (
            "quit",
            CommandDef {
                handler: cmd_quit,
                description: "Quit the client",
                usage: "/quit [message]",
            },
        ),
        (
            "help",
            CommandDef {
                handler: cmd_help,
                description: "Show command list",
                usage: "/help [command]",
            },
        ),
        (
            "clear",
            CommandDef {
                handler: cmd_clear,
                description: "Clear active buffer",
                usage: "/clear",
            },
        ),
        (
            "close",
            CommandDef {
                handler: cmd_close,
                description: "Close active buffer",
                usage: "/close",
            },
        ),
        // === IRC commands ===
        (
            "connect",
            CommandDef {
                handler: cmd_connect,
                description: "Connect to a server defined in config",
                usage: "/connect <server_id>",
            },
        ),
        (
            "disconnect",
            CommandDef {
                handler: cmd_disconnect,
                description: "Disconnect from current server",
                usage: "/disconnect [message]",
            },
        ),
        (
            "join",
            CommandDef {
                handler: cmd_join,
                description: "Join a channel",
                usage: "/join <channel> [key]",
            },
        ),
        (
            "part",
            CommandDef {
                handler: cmd_part,
                description: "Leave a channel",
                usage: "/part [channel] [message]",
            },
        ),
        (
            "msg",
            CommandDef {
                handler: cmd_msg,
                description: "Send a private message",
                usage: "/msg <target> <message>",
            },
        ),
        (
            "me",
            CommandDef {
                handler: cmd_me,
                description: "Send a CTCP ACTION",
                usage: "/me <action>",
            },
        ),
        (
            "nick",
            CommandDef {
                handler: cmd_nick,
                description: "Change nickname",
                usage: "/nick <new_nick>",
            },
        ),
        (
            "topic",
            CommandDef {
                handler: cmd_topic,
                description: "View or set channel topic",
                usage: "/topic [channel] [topic]",
            },
        ),
        (
            "kick",
            CommandDef {
                handler: cmd_kick,
                description: "Kick a user from channel",
                usage: "/kick <nick> [reason]",
            },
        ),
        (
            "notice",
            CommandDef {
                handler: cmd_notice,
                description: "Send a notice",
                usage: "/notice <target> <message>",
            },
        ),
        (
            "invite",
            CommandDef {
                handler: cmd_invite,
                description: "Invite a user to a channel",
                usage: "/invite <nick> [channel]",
            },
        ),
        (
            "whois",
            CommandDef {
                handler: cmd_whois,
                description: "WHOIS query on a user",
                usage: "/whois <nick>",
            },
        ),
        (
            "quote",
            CommandDef {
                handler: cmd_quote,
                description: "Send a raw IRC command",
                usage: "/quote <raw command>",
            },
        ),
        (
            "names",
            CommandDef {
                handler: cmd_names,
                description: "Request NAMES list for a channel",
                usage: "/names [channel]",
            },
        ),
    ]
}

pub fn get_command_names() -> Vec<&'static str> {
    get_commands().iter().map(|(name, _)| *name).collect()
}

// === Basic commands ===

fn cmd_quit(app: &mut App, args: &[String]) {
    let quit_msg = if args.is_empty() {
        "Leaving"
    } else {
        &args[0]
    };
    for handle in app.irc_handles.values() {
        let _ = handle.sender.send_quit(quit_msg);
    }
    app.should_quit = true;
}

fn cmd_help(app: &mut App, _args: &[String]) {
    let commands = get_commands();
    let mut help_text = String::from("Available commands:");
    for (_name, def) in &commands {
        help_text.push_str(&format!("\n  {} — {}", def.usage, def.description));
    }

    super::helpers::add_local_event(app, &help_text);
}

fn cmd_clear(app: &mut App, _args: &[String]) {
    if let Some(buf) = app.state.active_buffer_mut() {
        buf.messages.clear();
    }
}

fn cmd_close(app: &mut App, _args: &[String]) {
    if let Some(active_id) = app.state.active_buffer_id.clone() {
        app.state.remove_buffer(&active_id);
    }
}

// === IRC commands ===

fn cmd_connect(app: &mut App, args: &[String]) {
    if args.is_empty() {
        super::helpers::add_local_event(app, "Usage: /connect <server_id>");
        return;
    }
    let server_id = &args[0];

    // Check if already connected
    if app.irc_handles.contains_key(server_id.as_str()) {
        super::helpers::add_local_event(
            app,
            &format!("Already connected to {server_id}"),
        );
        return;
    }

    // We need to spawn the connection asynchronously since command handlers are sync.
    let server_config = match app.config.servers.get(server_id.as_str()) {
        Some(cfg) => cfg.clone(),
        None => {
            let available: Vec<&String> = app.config.servers.keys().collect();
            super::helpers::add_local_event(
                app,
                &format!(
                    "Unknown server: {server_id}. Available: {}",
                    if available.is_empty() {
                        "(none configured)".to_string()
                    } else {
                        available.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                    }
                ),
            );
            return;
        }
    };

    let conn_id = server_id.to_string();

    // Create connection entry and server buffer immediately
    app.state.add_connection(crate::state::connection::Connection {
        id: conn_id.clone(),
        label: server_config.label.clone(),
        status: crate::state::connection::ConnectionStatus::Connecting,
        nick: server_config
            .nick
            .as_deref()
            .unwrap_or(&app.config.general.nick)
            .to_string(),
        user_modes: String::new(),
        isupport: std::collections::HashMap::new(),
        error: None,
        lag: None,
    });

    let server_buf_id =
        crate::state::buffer::make_buffer_id(&conn_id, &server_config.label);
    app.state.add_buffer(crate::state::buffer::Buffer {
        id: server_buf_id.clone(),
        connection_id: conn_id.clone(),
        buffer_type: crate::state::buffer::BufferType::Server,
        name: server_config.label.clone(),
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
    app.state.set_active_buffer(&server_buf_id);

    let id = app.state.next_message_id();
    app.state.add_message(
        &server_buf_id,
        crate::state::buffer::Message {
            id,
            timestamp: chrono::Utc::now(),
            message_type: crate::state::buffer::MessageType::Event,
            nick: None,
            nick_mode: None,
            text: format!("Connecting to {}...", server_config.label),
            highlight: false,
            event_key: None,
            event_params: None,
        },
    );

    // Spawn async connection task
    let general = app.config.general.clone();
    let tx = app.irc_tx.clone();
    let id_clone = conn_id;

    tokio::spawn(async move {
        match crate::irc::connect_server(&id_clone, &server_config, &general).await {
            Ok((handle, mut rx)) => {
                // Send the sender back to the main thread via HandleReady
                let _ = tx.send(crate::irc::IrcEvent::HandleReady(
                    handle.conn_id.clone(),
                    handle.sender,
                ));

                // Forward all events from the per-connection receiver to the shared sender
                while let Some(event) = rx.recv().await {
                    if tx.send(event).is_err() {
                        break;
                    }
                }
            }
            Err(e) => {
                let _ = tx.send(crate::irc::IrcEvent::Disconnected(
                    id_clone,
                    Some(e.to_string()),
                ));
            }
        }
    });
}

fn cmd_disconnect(app: &mut App, args: &[String]) {
    let quit_msg = if args.is_empty() {
        "Leaving"
    } else {
        &args[0]
    };

    let conn_id = match app.active_conn_id() {
        Some(id) => id,
        None => {
            super::helpers::add_local_event(app, "No active connection");
            return;
        }
    };

    if let Some(handle) = app.irc_handles.get(&conn_id) {
        let _ = handle.sender.send_quit(quit_msg);
    }
    app.irc_handles.remove(&conn_id);
    crate::irc::events::handle_disconnected(&mut app.state, &conn_id, None);
}

fn cmd_join(app: &mut App, args: &[String]) {
    if args.is_empty() {
        super::helpers::add_local_event(app, "Usage: /join <channel> [key]");
        return;
    }

    let sender = match app.active_irc_sender() {
        Some(s) => s.clone(),
        None => {
            super::helpers::add_local_event(app, "Not connected");
            return;
        }
    };

    for channel in args {
        if let Err(e) = sender.send_join(channel) {
            super::helpers::add_local_event(
                app,
                &format!("Failed to join {channel}: {e}"),
            );
        }
    }
}

fn cmd_part(app: &mut App, args: &[String]) {
    let sender = match app.active_irc_sender() {
        Some(s) => s.clone(),
        None => {
            super::helpers::add_local_event(app, "Not connected");
            return;
        }
    };

    let (channel, reason) = if args.is_empty() {
        // Part current channel
        let buf = match app.state.active_buffer() {
            Some(b) => b,
            None => return,
        };
        (buf.name.clone(), None)
    } else if args.len() == 1 {
        if crate::irc::formatting::is_channel(&args[0]) {
            (args[0].clone(), None)
        } else {
            // It's a reason, part current channel
            let buf = match app.state.active_buffer() {
                Some(b) => b,
                None => return,
            };
            (buf.name.clone(), Some(args[0].as_str()))
        }
    } else {
        (args[0].clone(), Some(args[1].as_str()))
    };

    if let Some(reason) = reason {
        let _ = sender.send(irc::proto::Command::PART(channel, Some(reason.to_string())));
    } else {
        let _ = sender.send(irc::proto::Command::PART(channel, None));
    }
}

fn cmd_msg(app: &mut App, args: &[String]) {
    if args.len() < 2 {
        super::helpers::add_local_event(app, "Usage: /msg <target> <message>");
        return;
    }

    let target = &args[0];
    let text = &args[1];

    let (conn_id, nick) = {
        let conn_id = match app.active_conn_id() {
            Some(id) => id,
            None => {
                super::helpers::add_local_event(app, "No active connection");
                return;
            }
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
        super::helpers::add_local_event(
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
            connection_id: conn_id,
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
        });
    }

    let id = app.state.next_message_id();
    app.state.add_message(
        &buffer_id,
        crate::state::buffer::Message {
            id,
            timestamp: chrono::Utc::now(),
            message_type: crate::state::buffer::MessageType::Message,
            nick: Some(nick),
            nick_mode: None,
            text: text.to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
        },
    );
    app.state.set_active_buffer(&buffer_id);
}

fn cmd_me(app: &mut App, args: &[String]) {
    if args.is_empty() {
        super::helpers::add_local_event(app, "Usage: /me <action>");
        return;
    }

    let action_text = &args[0];
    let buf = match app.state.active_buffer() {
        Some(b) => b,
        None => return,
    };
    let target = buf.name.clone();
    let conn_id = buf.connection_id.clone();
    let nick = app
        .state
        .connections
        .get(&conn_id)
        .map(|c| c.nick.clone())
        .unwrap_or_default();

    // Send CTCP ACTION
    if let Some(handle) = app.irc_handles.get(&conn_id) {
        let ctcp = format!("\x01ACTION {action_text}\x01");
        let _ = handle.sender.send_privmsg(&target, &ctcp);
    }

    // Echo locally
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
            text: action_text.to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
        },
    );
}

fn cmd_nick(app: &mut App, args: &[String]) {
    if args.is_empty() {
        super::helpers::add_local_event(app, "Usage: /nick <new_nick>");
        return;
    }

    let new_nick = &args[0];

    let sender = match app.active_irc_sender() {
        Some(s) => s.clone(),
        None => {
            super::helpers::add_local_event(app, "Not connected");
            return;
        }
    };

    let _ = sender.send(irc::proto::Command::NICK(new_nick.to_string()));
}

fn cmd_topic(app: &mut App, args: &[String]) {
    let sender = match app.active_irc_sender() {
        Some(s) => s.clone(),
        None => {
            super::helpers::add_local_event(app, "Not connected");
            return;
        }
    };

    if args.is_empty() {
        // Show current topic
        if let Some(buf) = app.state.active_buffer() {
            match &buf.topic {
                Some(topic) => {
                    let setter = buf
                        .topic_set_by
                        .as_deref()
                        .unwrap_or("unknown");
                    super::helpers::add_local_event(
                        app,
                        &format!("Topic for {}: {} (set by {setter})", buf.name, topic),
                    );
                }
                None => {
                    super::helpers::add_local_event(
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
            // Just requesting topic for a specific channel
            let _ = sender.send(irc::proto::Command::TOPIC(args[0].clone(), None));
            return;
        }
        // Setting topic on current channel
        let buf = match app.state.active_buffer() {
            Some(b) => b,
            None => return,
        };
        (buf.name.clone(), args[0].clone())
    } else {
        (args[0].clone(), args[1].clone())
    };

    let _ = sender.send(irc::proto::Command::TOPIC(channel, Some(topic)));
}

fn cmd_kick(app: &mut App, args: &[String]) {
    if args.is_empty() {
        super::helpers::add_local_event(app, "Usage: /kick <nick> [reason]");
        return;
    }

    let sender = match app.active_irc_sender() {
        Some(s) => s.clone(),
        None => {
            super::helpers::add_local_event(app, "Not connected");
            return;
        }
    };

    let nick = &args[0];
    let reason = if args.len() > 1 {
        Some(args[1].clone())
    } else {
        None
    };

    let channel = match app.state.active_buffer() {
        Some(b) => b.name.clone(),
        None => return,
    };

    let _ = sender.send(irc::proto::Command::KICK(
        channel,
        nick.to_string(),
        reason,
    ));
}

fn cmd_notice(app: &mut App, args: &[String]) {
    if args.len() < 2 {
        super::helpers::add_local_event(app, "Usage: /notice <target> <message>");
        return;
    }

    let target = &args[0];
    let text = &args[1];

    if let Some(sender) = app.active_irc_sender() {
        let _ = sender.send_notice(target, text);
    } else {
        super::helpers::add_local_event(app, "Not connected");
    }
}

fn cmd_invite(app: &mut App, args: &[String]) {
    if args.is_empty() {
        super::helpers::add_local_event(app, "Usage: /invite <nick> [channel]");
        return;
    }

    let sender = match app.active_irc_sender() {
        Some(s) => s.clone(),
        None => {
            super::helpers::add_local_event(app, "Not connected");
            return;
        }
    };

    let nick = &args[0];
    let channel = if args.len() > 1 {
        args[1].clone()
    } else {
        match app.state.active_buffer() {
            Some(b) => b.name.clone(),
            None => return,
        }
    };

    let _ = sender.send(irc::proto::Command::INVITE(
        nick.to_string(),
        channel,
    ));
}

fn cmd_whois(app: &mut App, args: &[String]) {
    if args.is_empty() {
        super::helpers::add_local_event(app, "Usage: /whois <nick>");
        return;
    }

    if let Some(sender) = app.active_irc_sender() {
        let _ = sender.send(irc::proto::Command::WHOIS(
            None,
            args[0].to_string(),
        ));
    } else {
        super::helpers::add_local_event(app, "Not connected");
    }
}

fn cmd_quote(app: &mut App, args: &[String]) {
    if args.is_empty() {
        super::helpers::add_local_event(app, "Usage: /quote <raw command>");
        return;
    }

    // Greedy parser gives us the entire raw line as args[0]
    let raw = &args[0];

    if let Some(sender) = app.active_irc_sender() {
        // Parse the raw command into command + arguments
        let parts: Vec<&str> = raw.splitn(2, ' ').collect();
        let command = parts[0].to_string();
        let args_vec: Vec<String> = if parts.len() > 1 {
            parts[1].split_whitespace().map(String::from).collect()
        } else {
            vec![]
        };
        let _ = sender.send(irc::proto::Command::Raw(command, args_vec));
    } else {
        super::helpers::add_local_event(app, "Not connected");
    }
}

fn cmd_names(app: &mut App, args: &[String]) {
    let sender = match app.active_irc_sender() {
        Some(s) => s.clone(),
        None => {
            super::helpers::add_local_event(app, "Not connected");
            return;
        }
    };

    let channel = if args.is_empty() {
        match app.state.active_buffer() {
            Some(b) => b.name.clone(),
            None => return,
        }
    } else {
        args[0].clone()
    };

    let _ = sender.send(irc::proto::Command::NAMES(
        Some(channel),
        None,
    ));
}

use std::collections::{HashMap, HashSet, VecDeque};

use chrono::Utc;

use crate::config;
use crate::irc::{self, IrcEvent, IrcHandle};
use crate::state::buffer::{
    ActivityLevel, Buffer, BufferType, Message, MessageType, make_buffer_id,
};
use crate::state::connection::{Connection, ConnectionStatus};

use super::App;

impl App {
    /// Set up connection state, server buffer, and "Connecting..." message.
    /// Returns the server buffer ID. Shared by autoconnect and /connect command.
    pub fn setup_connection(
        &mut self,
        conn_id: &str,
        server_config: &config::ServerConfig,
    ) -> String {
        // Remove placeholder default Status buffer when first real connection starts
        let default_buf_id = make_buffer_id(Self::DEFAULT_CONN_ID, "Status");
        if self.state.buffers.contains_key(&default_buf_id) {
            self.state.remove_buffer(&default_buf_id);
            self.state.connections.remove(Self::DEFAULT_CONN_ID);
        }

        let auto_reconnect = server_config.auto_reconnect.unwrap_or(true);
        let reconnect_delay = server_config.reconnect_delay.unwrap_or(30);

        self.state.add_connection(Connection {
            id: conn_id.to_string(),
            label: server_config.label.clone(),
            status: ConnectionStatus::Connecting,
            nick: server_config
                .nick
                .as_deref()
                .unwrap_or(&self.config.general.nick)
                .to_string(),
            user_modes: String::new(),
            isupport: HashMap::new(),
            isupport_parsed: crate::irc::isupport::Isupport::new(),
            error: None,
            lag: None,
            lag_pending: false,
            reconnect_attempts: 0,
            reconnect_delay_secs: reconnect_delay,
            next_reconnect: None,
            should_reconnect: auto_reconnect,
            joined_channels: server_config.channels.clone(),
            origin_config: server_config.clone(),
            local_ip: None,
            enabled_caps: HashSet::new(),
            who_token_counter: 0,
            silent_who_channels: HashSet::new(),
        });

        let server_buf_id = make_buffer_id(conn_id, &server_config.label);
        self.state.add_buffer(Buffer {
            id: server_buf_id.clone(),
            connection_id: conn_id.to_string(),
            buffer_type: BufferType::Server,
            name: server_config.label.clone(),
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
        });
        self.state.set_active_buffer(&server_buf_id);

        let id = self.state.next_message_id();
        self.state.add_message(
            &server_buf_id,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: format!("Connecting to {}...", server_config.label),
                highlight: false,
                event_key: None,
                event_params: None,
                log_msg_id: None,
                log_ref_id: None,
                tags: None,
            },
        );

        server_buf_id
    }

    /// Connect to a server defined in config by its key (e.g. "libera").
    /// Used for autoconnect at startup.
    pub(crate) async fn connect_server_async(&mut self, server_id: &str) -> color_eyre::eyre::Result<()> {
        let server_config = match self.config.servers.get(server_id) {
            Some(cfg) => cfg.clone(),
            None => {
                return Ok(());
            }
        };

        let conn_id = server_id.to_string();
        self.setup_connection(&conn_id, &server_config);

        let general = self.config.general.clone();
        match irc::connect_server(&conn_id, &server_config, &general).await {
            Ok((handle, mut rx)) => {
                let tx = self.irc_tx.clone();
                // Store local IP on Connection state (for DCC own-IP fallback)
                if let Some(conn) = self.state.connections.get_mut(&conn_id) {
                    conn.local_ip = handle.local_ip;
                }
                self.irc_handles.insert(conn_id.clone(), handle);

                // Spawn task to forward events from per-connection receiver to shared channel
                let fwd_handle = tokio::spawn(async move {
                    while let Some(event) = rx.recv().await {
                        if tx.send(event).await.is_err() {
                            break;
                        }
                    }
                });
                self.forwarder_handles.insert(conn_id.clone(), fwd_handle);
            }
            Err(e) => {
                crate::irc::events::handle_disconnected(
                    &mut self.state,
                    &conn_id,
                    Some(&e.to_string()),
                );
            }
        }

        Ok(())
    }

    /// Add an event message to the specified buffer.
    pub(crate) fn add_event_to_buffer(&mut self, buffer_id: &str, text: String) {
        let id = self.state.next_message_id();
        self.state.add_local_message(
            buffer_id,
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

    /// Check connections that need reconnecting and spawn reconnect tasks.
    pub(crate) fn check_reconnects(&mut self) {
        let now = std::time::Instant::now();

        // Collect connections that need reconnecting
        let to_reconnect: Vec<String> = self
            .state
            .connections
            .iter()
            .filter(|(id, conn)| {
                matches!(
                    conn.status,
                    ConnectionStatus::Disconnected | ConnectionStatus::Error
                ) && conn.should_reconnect
                    && conn.next_reconnect.is_some_and(|t| t <= now)
                    && *id != Self::DEFAULT_CONN_ID
                    && !self.irc_handles.contains_key(id.as_str())
            })
            .map(|(id, _)| id.clone())
            .collect();

        for conn_id in to_reconnect {
            let Some(conn) = self.state.connections.get_mut(&conn_id) else {
                continue;
            };

            conn.reconnect_attempts += 1;
            let attempts = conn.reconnect_attempts;
            conn.next_reconnect = None;

            let conn = self.state.connections.get(&conn_id);
            let label = conn.map_or_else(|| conn_id.clone(), |c| c.label.clone());
            let server_config = conn.map(|c| c.origin_config.clone());

            let buffer_id = make_buffer_id(&conn_id, &label);
            self.add_event_to_buffer(
                &buffer_id,
                format!("Reconnecting to {label} (attempt {attempts})..."),
            );

            if let Some(conn) = self.state.connections.get_mut(&conn_id) {
                conn.status = ConnectionStatus::Connecting;
            }

            self.spawn_reconnect(&conn_id, server_config, &buffer_id, &label);
        }
    }

    /// Spawn a reconnect task or log failure if no config is available.
    fn spawn_reconnect(
        &mut self,
        conn_id: &str,
        server_config: Option<config::ServerConfig>,
        buffer_id: &str,
        label: &str,
    ) {
        if let Some(cfg) = server_config {
            let general = self.config.general.clone();
            let tx = self.irc_tx.clone();
            let id = conn_id.to_string();
            tokio::spawn(async move {
                match crate::irc::connect_server(&id, &cfg, &general).await {
                    Ok((handle, mut rx)) => {
                        let _ = tx.send(IrcEvent::HandleReady(
                            handle.conn_id.clone(),
                            handle.sender,
                            handle.local_ip,
                            handle.outgoing_handle,
                        )).await;
                        while let Some(event) = rx.recv().await {
                            if tx.send(event).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(IrcEvent::Disconnected(id, Some(e.to_string()))).await;
                    }
                }
            });
        } else {
            if let Some(conn) = self.state.connections.get_mut(conn_id) {
                conn.should_reconnect = false;
                conn.status = ConnectionStatus::Disconnected;
            }
            self.add_event_to_buffer(
                buffer_id,
                format!("Cannot reconnect to {label}: server config not found"),
            );
        }
    }

    /// Execute autosendcmd string after successful connection.
    ///
    /// Format: semicolon-separated commands with optional `WAIT <ms>` delays.
    /// Commands without a leading `/` get one prepended automatically.
    /// `$N` / `${N}` are replaced with the current nick.
    ///
    /// WAIT delays are currently skipped (commands execute immediately).
    pub(crate) fn execute_autosendcmd(&mut self, conn_id: &str, cmds: &str) {
        let nick = self
            .state
            .connections
            .get(conn_id)
            .map(|c| c.nick.clone())
            .unwrap_or_default();

        for part in cmds.split(';') {
            let cmd = part.trim();
            if cmd.is_empty() {
                continue;
            }
            // Skip WAIT delays (async delay support can be added later)
            if cmd.to_uppercase().starts_with("WAIT") {
                continue;
            }
            // Replace $N / ${N} with current nick
            let expanded = cmd.replace("$N", &nick).replace("${N}", &nick);
            // Prepend / if not already a command
            let line = if expanded.starts_with('/') {
                expanded
            } else {
                format!("/{expanded}")
            };
            // Parse and execute as if user typed it
            if let Some(parsed) = crate::commands::parser::parse_command(&line) {
                self.execute_command(&parsed);
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn handle_irc_event(&mut self, event: IrcEvent) {
        match event {
            IrcEvent::HandleReady(conn_id, sender, local_ip, outgoing_handle) => {
                // Store local IP on Connection state (for DCC own-IP fallback)
                if let Some(conn) = self.state.connections.get_mut(&conn_id) {
                    conn.local_ip = local_ip;
                }
                self.irc_handles.insert(
                    conn_id.clone(),
                    IrcHandle {
                        conn_id,
                        sender,
                        local_ip,
                        outgoing_handle,
                    },
                );
            }
            IrcEvent::NegotiationInfo(conn_id, diag) => {
                // Display CAP/SASL diagnostics in status buffer — fires immediately
                // so they're visible even if connection fails before RPL_WELCOME.
                let buf_id = self.state.connections.get(&conn_id).map_or_else(
                    || conn_id.clone(),
                    |c| crate::state::buffer::make_buffer_id(&conn_id, &c.label),
                );
                for msg in &diag {
                    crate::irc::events::emit(&mut self.state, &buf_id, &format!("%Z56b6c2{msg}%N"));
                }
            }
            IrcEvent::Connected(conn_id, enabled_caps) => {
                // Store negotiated caps on connection
                if let Some(conn) = self.state.connections.get_mut(&conn_id) {
                    conn.enabled_caps = enabled_caps;
                }
                // Collect channels to rejoin before handle_connected resets state
                let rejoin_channels = crate::irc::events::channels_to_rejoin(&self.state, &conn_id);
                crate::irc::events::handle_connected(&mut self.state, &conn_id);

                // Broadcast connection status to web clients.
                if let Some(conn) = self.state.connections.get(&conn_id) {
                    self.broadcast_web(crate::web::protocol::WebEvent::ConnectionStatus {
                        conn_id: conn_id.clone(),
                        label: conn.label.clone(),
                        connected: true,
                        nick: conn.nick.clone(),
                    });
                }

                // Notify scripts
                {
                    use crate::scripting::api::events;
                    let nick = self
                        .state
                        .connections
                        .get(&conn_id)
                        .map_or_else(String::new, |c| c.nick.clone());
                    let mut params = HashMap::new();
                    params.insert("connection_id".to_string(), conn_id.clone());
                    params.insert("nick".to_string(), nick);
                    self.emit_script_event(events::CONNECTED, params);
                }

                // Config channels (used for eager buffer creation + rejoin filtering)
                let config_channels: Vec<String> = self
                    .config
                    .servers
                    .iter()
                    .find(|(id, cfg)| *id == &conn_id || cfg.label == conn_id)
                    .map(|(_, cfg)| cfg.channels.clone())
                    .unwrap_or_default();

                // Merge config + rejoin for buffer creation
                let mut all_channels = config_channels.clone();
                for ch in &rejoin_channels {
                    if !all_channels.iter().any(|c| c.eq_ignore_ascii_case(ch)) {
                        all_channels.push(ch.clone());
                    }
                }

                // Execute autosendcmd BEFORE autojoin (e.g. NickServ identify)
                let autosendcmd = self
                    .config
                    .servers
                    .iter()
                    .find(|(id, cfg)| *id == &conn_id || cfg.label == conn_id)
                    .and_then(|(_, cfg)| cfg.autosendcmd.clone())
                    .or_else(|| {
                        self.state
                            .connections
                            .get(&conn_id)
                            .and_then(|c| c.origin_config.autosendcmd.clone())
                    });
                if let Some(cmds) = autosendcmd {
                    self.execute_autosendcmd(&conn_id, &cmds);
                }

                // Eager buffer creation (erssi pattern): create all channel
                // buffers upfront so the buffer list is stable from the start.
                // Buffers are destroyed on join failure (474, 471, etc.).
                for entry in &all_channels {
                    let chan_name = entry.split(' ').next().unwrap_or(entry);
                    let buf_id = make_buffer_id(&conn_id, chan_name);
                    if !self.state.buffers.contains_key(&buf_id) {
                        self.state.add_buffer(Buffer {
                            id: buf_id,
                            connection_id: conn_id.clone(),
                            buffer_type: BufferType::Channel,
                            name: chan_name.to_string(),
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
                        });
                    }
                }

                // Load backlog for eagerly created channel buffers
                for entry in &all_channels {
                    let chan_name = entry.split(' ').next().unwrap_or(entry);
                    let buf_id = make_buffer_id(&conn_id, chan_name);
                    self.load_backlog(&buf_id);
                }

                // Channel joining is handled by the irc crate on ENDOFMOTD:
                // it batches channels into comma-separated JOINs with keys-first
                // ordering and 512-byte splitting. Channels are passed via Config.
                // Rejoin channels (from reconnect) need manual joining since
                // they aren't in the library's config.
                if !rejoin_channels.is_empty()
                    && let Some(handle) = self.irc_handles.get(&conn_id)
                {
                    let extra: Vec<&str> = rejoin_channels
                        .iter()
                        .filter(|ch| {
                            !config_channels.iter().any(|c| {
                                c.split_once(' ')
                                    .map_or(c.as_str(), |(n, _)| n)
                                    .eq_ignore_ascii_case(ch)
                            })
                        })
                        .map(String::as_str)
                        .collect();
                    if !extra.is_empty() {
                        let chanlist = extra.join(",");
                        let _ = handle
                            .sender
                            .send(::irc::proto::Command::JOIN(chanlist, None, None));
                    }
                }
            }
            IrcEvent::Disconnected(conn_id, error) => {
                // DCC connections are peer-to-peer and independent of the IRC
                // server.  Do NOT close DCC records on IRC disconnect.
                crate::irc::events::handle_disconnected(
                    &mut self.state,
                    &conn_id,
                    error.as_deref(),
                );
                // Broadcast disconnection to web clients.
                if let Some(conn) = self.state.connections.get(&conn_id) {
                    self.broadcast_web(crate::web::protocol::WebEvent::ConnectionStatus {
                        conn_id: conn_id.clone(),
                        label: conn.label.clone(),
                        connected: false,
                        nick: conn.nick.clone(),
                    });
                }
                // Notify scripts
                {
                    use crate::scripting::api::events;
                    let mut params = HashMap::new();
                    params.insert("connection_id".to_string(), conn_id.clone());
                    self.emit_script_event(events::DISCONNECTED, params);
                }
                // Abort the outgoing message task BEFORE removing the handle.
                // The Pinger inside Outgoing holds a tx_outgoing clone that
                // keeps the write half of the TCP socket alive (CLOSE-WAIT).
                if let Some(handle) = self.irc_handles.get_mut(&conn_id)
                    && let Some(oh) = handle.outgoing_handle.take()
                {
                    oh.abort();
                }
                self.irc_handles.remove(&conn_id);
                if let Some(fwd) = self.forwarder_handles.remove(&conn_id) {
                    fwd.abort();
                }
                self.lag_pings.remove(&conn_id);
                self.batch_trackers.remove(&conn_id);
                self.channel_query_queues.remove(&conn_id);
                self.channel_query_in_flight.remove(&conn_id);
                self.channel_query_sent_at.remove(&conn_id);
            }
            IrcEvent::Message(conn_id, msg) => {
                // Intercept PONG to update lag measurement
                if let ::irc::proto::Command::PONG(_, _) = &msg.command
                    && let Some(sent_at) = self.lag_pings.get(&conn_id)
                {
                    // Lag will never exceed u64::MAX milliseconds
                    let lag_ms = u64::try_from(sent_at.elapsed().as_millis()).unwrap_or(u64::MAX);
                    if let Some(conn) = self.state.connections.get_mut(&conn_id) {
                        conn.lag = Some(lag_ms);
                        conn.lag_pending = false;
                    }
                }
                // Handle CAP subcommands for cap-notify (runtime capability changes)
                if let ::irc::proto::Command::CAP(_, ref subcmd, ref field3, ref field4) =
                    msg.command
                {
                    use ::irc::proto::command::CapSubCommand;
                    match subcmd {
                        CapSubCommand::NEW => {
                            let to_request = crate::irc::events::handle_cap_new(
                                &mut self.state,
                                &conn_id,
                                field3.as_deref(),
                                field4.as_deref(),
                            );
                            if !to_request.is_empty()
                                && let Some(handle) = self.irc_handles.get(&conn_id)
                            {
                                let req_str = to_request.join(" ");
                                tracing::info!("sending CAP REQ for new caps: {req_str}");
                                let _ = handle.sender.send(::irc::proto::Command::CAP(
                                    None,
                                    CapSubCommand::REQ,
                                    None,
                                    Some(req_str),
                                ));
                            }
                        }
                        CapSubCommand::DEL => {
                            crate::irc::events::handle_cap_del(
                                &mut self.state,
                                &conn_id,
                                field3.as_deref(),
                                field4.as_deref(),
                            );
                        }
                        CapSubCommand::ACK => {
                            crate::irc::events::handle_cap_ack(
                                &mut self.state,
                                &conn_id,
                                field3.as_deref(),
                                field4.as_deref(),
                            );
                        }
                        CapSubCommand::NAK => {
                            crate::irc::events::handle_cap_nak(
                                &mut self.state,
                                &conn_id,
                                field3.as_deref(),
                                field4.as_deref(),
                            );
                        }
                        _ => {}
                    }
                }

                // --- IRCv3 batch interception ---
                // Handle BATCH commands (start/end) and collect @batch-tagged messages.
                if let ::irc::proto::Command::BATCH(ref ref_tag, ref sub, ref params) = msg.command
                {
                    let tracker = self.batch_trackers.entry(conn_id.clone()).or_default();
                    if let Some(tag) = ref_tag.strip_prefix('+') {
                        // Start batch
                        let batch_type = sub
                            .as_ref()
                            .map_or_else(String::new, |s| s.to_str().to_string());
                        let batch_params = params.clone().unwrap_or_default();
                        tracker.start_batch(tag, &batch_type, batch_params);
                        tracing::debug!("batch started: tag={tag} type={batch_type}");
                    } else if let Some(tag) = ref_tag.strip_prefix('-') {
                        // End batch
                        if let Some(batch) = tracker.end_batch(tag) {
                            tracing::debug!(
                                "batch ended: tag={tag} type={} msgs={}",
                                batch.batch_type,
                                batch.messages.len()
                            );
                            crate::irc::batch::process_completed_batch(
                                &mut self.state,
                                &conn_id,
                                &batch,
                            );
                        }
                    }
                    // BATCH commands themselves are not dispatched further
                } else if self
                    .batch_trackers
                    .entry(conn_id.clone())
                    .or_default()
                    .is_batched(&msg)
                {
                    // Message belongs to an open batch — collect it, don't process now
                    if let Some(tracker) = self.batch_trackers.get_mut(&conn_id) {
                        tracker.add_message(*msg);
                    }
                } else {
                    // Normal message processing

                    // Extract channel from RPL_ENDOFNAMES (for auto-WHO/MODE batch).
                    let endofnames_channel = if let ::irc::proto::Command::Response(
                        ::irc::proto::Response::RPL_ENDOFNAMES,
                        ref args,
                    ) = msg.command
                    {
                        args.get(1).cloned()
                    } else {
                        None
                    };

                    // Extract target from RPL_ENDOFWHO (for batch completion).
                    let endofwho_target = if let ::irc::proto::Command::Response(
                        ::irc::proto::Response::RPL_ENDOFWHO,
                        ref args,
                    ) = msg.command
                    {
                        args.get(1).cloned()
                    } else {
                        None
                    };

                    // Update conn.nick from RPL_WELCOME — args[0] is our confirmed nick
                    // after any ERR_NICKNAMEINUSE retries by the irc crate.
                    if let ::irc::proto::Command::Response(
                        ::irc::proto::Response::RPL_WELCOME,
                        ref args,
                    ) = msg.command
                        && let Some(confirmed_nick) = args.first()
                        && let Some(conn) = self.state.connections.get_mut(&conn_id)
                    {
                        conn.nick.clone_from(confirmed_nick);
                    }

                    // Emit to scripts before default handling. If suppressed, skip.
                    if self.emit_irc_to_scripts(&conn_id, &msg) {
                        // Script suppressed — still process channel queries.
                        if let Some(channel) = endofnames_channel {
                            self.queue_channel_query(&conn_id, channel);
                        }
                        if let Some(ref target) = endofwho_target {
                            self.handle_who_batch_complete(&conn_id, target);
                        }
                        return;
                    }

                    // Intercept DCC CTCP before normal IRC handling.
                    // DCC messages arrive as CTCP inside PRIVMSG; events.rs ignores
                    // non-ACTION CTCPs, so we must consume them here to avoid them
                    // appearing as garbled text in the chat view.
                    if let ::irc::proto::Command::PRIVMSG(_, ref text) = msg.command
                        && text.starts_with('\x01')
                        && text.ends_with('\x01')
                        && text.len() > 2
                    {
                        let inner = &text[1..text.len() - 1];
                        if let Some(dcc_msg) = crate::dcc::protocol::parse_dcc_ctcp(inner) {
                            let (nick, ident, host) =
                                crate::irc::formatting::extract_nick_userhost(msg.prefix.as_ref());

                            // A passive DCC response from the peer looks like:
                            //   DCC CHAT CHAT <peer_ip> <peer_port> <our_token>
                            // where port > 0 and passive_token matches what we sent.
                            // We find our pending record by token and connect to the peer.
                            if let Some(token) = dcc_msg.passive_token
                                && dcc_msg.port > 0
                            {
                                let matching_id = self
                                    .dcc
                                    .records
                                    .iter()
                                    .find(|(_, r)| r.passive_token == Some(token))
                                    .map(|(id, _)| id.clone());

                                if let Some(id) = matching_id {
                                    // Update the record to point at the peer's real address.
                                    if let Some(rec) = self.dcc.records.get_mut(&id) {
                                        rec.addr = dcc_msg.addr;
                                        rec.port = dcc_msg.port;
                                        rec.state = crate::dcc::types::DccState::Connecting;
                                    }

                                    let (line_tx, line_rx) = tokio::sync::mpsc::channel(256);
                                    self.dcc.chat_senders.insert(id.clone(), line_tx);

                                    let task_id = id.clone();
                                    let event_tx = self.dcc.dcc_tx.clone();
                                    let timeout_dur =
                                        std::time::Duration::from_secs(self.dcc.timeout_secs);
                                    let peer_addr =
                                        std::net::SocketAddr::new(dcc_msg.addr, dcc_msg.port);

                                    tracing::debug!(
                                        "passive DCC response from {nick}: \
                                         connecting to {peer_addr} (token={token})"
                                    );

                                    tokio::spawn(async move {
                                        crate::dcc::chat::connect_for_chat(
                                            task_id,
                                            peer_addr,
                                            timeout_dur,
                                            event_tx,
                                            line_rx,
                                        )
                                        .await;
                                    });

                                    // Don't fall through to normal IRC handling.
                                    if let Some(channel) = endofnames_channel {
                                        self.queue_channel_query(&conn_id, channel);
                                    }
                                    if let Some(ref target) = endofwho_target {
                                        self.handle_who_batch_complete(&conn_id, target);
                                    }
                                    return;
                                }
                            }

                            // Otherwise this is a fresh incoming DCC CHAT offer.
                            self.handle_dcc_event(crate::dcc::DccEvent::IncomingRequest {
                                nick,
                                conn_id: conn_id.clone(),
                                addr: dcc_msg.addr,
                                port: dcc_msg.port,
                                passive_token: dcc_msg.passive_token,
                                ident,
                                host,
                            });

                            // Don't pass to normal IRC handler — the CTCP is consumed.
                            if let Some(channel) = endofnames_channel {
                                self.queue_channel_query(&conn_id, channel);
                            }
                            if let Some(ref target) = endofwho_target {
                                self.handle_who_batch_complete(&conn_id, target);
                            }
                            return;
                        }
                    }

                    // Snapshot buffer count so we can detect newly created buffers
                    // and feed them with chat history from the log database.
                    let buffers_before = self.state.buffers.len();

                    crate::irc::events::handle_irc_message(&mut self.state, &conn_id, &msg);

                    // Drain pending web events and broadcast + auto-record mentions.
                    self.drain_pending_web_events();
                    // Drain queued RPE2E NOTICE sends (handshake replies,
                    // auto-KEYREQ on MissingKey) produced by the handlers.
                    self.drain_pending_e2e_sends();

                    // Load backlog for any buffers created by handle_irc_message
                    // (e.g. query buffer on first PRIVMSG from a new nick)
                    if self.state.buffers.len() > buffers_before {
                        let new_ids: Vec<String> = self
                            .state
                            .buffers
                            .keys()
                            .skip(buffers_before)
                            .cloned()
                            .collect();
                        for buf_id in &new_ids {
                            self.load_backlog(buf_id);
                        }
                    }

                    // ── DCC: track nick renames ──────────────────────────────────
                    // When a user renames on IRC their DCC record and buffer must
                    // follow, since buffers are named after the peer's nick (=Nick).
                    if let ::irc::proto::Command::NICK(ref new_nick) = msg.command
                        && let Some(::irc::proto::Prefix::Nickname(ref old_nick, _, _)) = msg.prefix
                    {
                        let renames = self.dcc.update_nick(old_nick, new_nick);
                        for (_old_id, _new_id, old_buf_suffix, new_buf_suffix) in renames {
                            let old_buf_id =
                                crate::state::buffer::make_buffer_id(&conn_id, &old_buf_suffix);
                            let new_buf_id =
                                crate::state::buffer::make_buffer_id(&conn_id, &new_buf_suffix);

                            if let Some(mut buf) = self.state.buffers.shift_remove(&old_buf_id) {
                                buf.id.clone_from(&new_buf_id);
                                buf.name = format!("={new_nick}");
                                self.state.buffers.insert(new_buf_id.clone(), buf);

                                // Keep active selection consistent.
                                if self.state.active_buffer_id.as_deref() == Some(&old_buf_id) {
                                    self.state.active_buffer_id = Some(new_buf_id);
                                }
                            }
                        }
                    }

                    // ── DCC: ERR_NOSUCHNICK cleanup ──────────────────────────────
                    // If the IRC server reports that a nick does not exist,
                    // cancel any pending DCC request to that nick so it doesn't
                    // sit in the queue until timeout.
                    if let ::irc::proto::Command::Response(
                        ::irc::proto::Response::ERR_NOSUCHNICK,
                        ref args,
                    ) = msg.command
                        && let Some(target_nick) = args.get(1)
                        && let Some(record) = self.dcc.close_by_nick(target_nick)
                    {
                        crate::commands::helpers::add_local_event(
                            self,
                            &format!("DCC CHAT to {} cancelled: no such nick", record.nick),
                        );
                    }

                    // Queue channel for batched WHO + MODE after join.
                    if let Some(channel) = endofnames_channel {
                        self.queue_channel_query(&conn_id, channel);
                    }

                    // Check if a WHO batch completed.
                    if let Some(ref target) = endofwho_target {
                        self.handle_who_batch_complete(&conn_id, target);
                    }
                }
            }
        }
    }
}

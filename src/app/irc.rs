use std::collections::{HashMap, HashSet, VecDeque};

use chrono::Utc;

use crate::config;
use crate::irc::IrcEvent;
use crate::state::buffer::{
    ActivityLevel, Buffer, BufferType, Message, MessageType, make_buffer_id,
};
use crate::state::connection::{Connection, ConnectionStatus};

use super::App;

impl App {
    fn apply_bouncer_identity(&mut self, id: &str, identity: Option<&crate::irc::bouncer::Identity>, provider: Option<crate::irc::bouncer::Provider>) {
        let account_id = self.bouncer_children.get(id).map_or(id, |child| child.parent.as_str());
        let Some(conn) = self.state.connections.get_mut(id) else { return; };
        let explicit = conn.origin_config.bouncer_control || conn.origin_config.bouncer_network_id.is_some();
        if explicit && provider != Some(crate::irc::bouncer::Provider::Soju) {
            return;
        }
        let scope = identity.map(|identity| {
            let mut scoped = provider.map_or_else(|| conn.origin_config.clone(), |provider| provider.scope_config(&conn.origin_config));
            if !explicit {
                scoped.username = Some(scoped.username.as_deref().unwrap_or(&self.config.general.username)
                    .split(['/', '@']).next().unwrap_or_default().to_string());
                if let Some(login) = scoped.sasl_user.as_mut()
                    && let Some(separator) = login.find(['/', '@'])
                {
                    login.truncate(separator);
                }
            }
            match identity {
                crate::irc::bouncer::Identity::Control => scoped.bouncer_control = true,
                crate::irc::bouncer::Identity::Network(network) => scoped.bouncer_network_id = Some(network.clone()),
            }
            config::network_scope::network_scope(account_id, &scoped, &self.config.general.username)
        });
        let changed = conn.network_scope != scope;
        conn.network_scope = scope;
        conn.bouncer_identity = identity.cloned();
        if identity.is_some() {
            conn.joined_channels.clear();
        }
        if changed {
            let buffers: Vec<_> = self.state.buffers.values()
                .filter(|buffer| buffer.connection_id == id && buffer.buffer_type != BufferType::Server)
                .map(|buffer| buffer.id.clone()).collect();
            for buffer in buffers {
                self.state.remove_buffer(&buffer);
            }
        }
        self.refresh_e2e_configured_networks();
    }

    /// Set up connection state, server buffer, and "Connecting..." message.
    /// Returns the server buffer ID. Shared by autoconnect and /connect command.
    pub fn setup_connection(
        &mut self,
        conn_id: &str,
        server_config: &config::ServerConfig,
    ) -> String {
        self.setup_connection_for_account(conn_id, server_config, conn_id, true)
    }

    pub(crate) fn setup_connection_for_account(
        &mut self,
        conn_id: &str,
        server_config: &config::ServerConfig,
        account_id: &str,
        activate: bool,
    ) -> String {
        // Remove placeholder default Status buffer when first real connection starts
        let default_buf_id = make_buffer_id(Self::DEFAULT_CONN_ID, "Status");
        if self.state.buffers.contains_key(&default_buf_id) {
            self.state.remove_buffer(&default_buf_id);
            self.state.remove_connection(Self::DEFAULT_CONN_ID);
        }

        self.state.add_connection(Connection {
            own_realname: None,
            network_label: None,
            id: conn_id.to_string(),
            label: server_config.label.clone(),
            bouncer_identity: None,
            network_scope: (server_config.bouncer_network_id.is_some() || server_config.bouncer_control)
                .then(|| config::network_scope::network_scope(account_id, server_config, &self.config.general.username)),
            status: ConnectionStatus::Connecting,
            own_handle: None,
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
            reconnect_delay_secs: server_config.reconnect_delay.unwrap_or(30),
            next_reconnect: None,
            should_reconnect: server_config.auto_reconnect.unwrap_or(true),
            joined_channels: if server_config.bouncer_network_id.is_some() || server_config.bouncer_control {
                Vec::new()
            } else {
                server_config.channels.clone()
            },
            origin_config: {
                let mut origin = server_config.clone();
                if (origin.bouncer_control || origin.bouncer_network_id.is_some()) && origin.username.is_none() {
                    origin.username = Some(self.config.general.username.clone());
                }
                origin
            },
            local_ip: None,
            enabled_caps: HashSet::new(),
            chathistory: crate::irc::chathistory::HistoryState::new(),
            who_token_counter: 0,
            multiline: None,
            batch_ref_counter: 0,
            silent_who_channels: HashSet::new(),
            silent_banlist_channels: HashSet::new(),
        });

        // A newly registered connection may add a network the keyring's
        // configured-network set (snapshotted at startup) doesn't know about —
        // an ad-hoc `/connect` or a `/server add` since launch. Refresh it so
        // `legacy_adoption_allowed()` reflects the networks actually in play:
        // a stale count of <=1 would let a scoped miss on this second network
        // fall back to an unscoped legacy E2E row, defeating cross-network
        // isolation.
        self.refresh_e2e_configured_networks();

        let server_buf_id = make_buffer_id(conn_id, &server_config.label);
        self.state.add_buffer_with_focus(Buffer {
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
            log_total_lines: None,
            log_oldest_ts: None,
            log_newest_ts: None,
            history_exhausted: false,
            log_initial_loaded: false,
            pin_backlog: false,
            metadata: crate::irc::metadata::Flags::default(),
        }, activate);
        if activate {
            self.state.set_active_buffer(&server_buf_id);
        }

        self.show_connecting_notice(&server_buf_id, &server_config.label);

        server_buf_id
    }

    fn show_connecting_notice(&mut self, server_buf_id: &str, label: &str) {
        let id = self.state.next_message_id();
        self.state.add_message(
            server_buf_id,
            Message {
                redaction_ref: None,
                redaction_msgid: None,
                log_key: None,
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: format!("Connecting to {label}..."),
                highlight: false,
                event_key: None,
                event_params: None,
                log_msg_id: None,
                log_ref_id: None,
                tags: None,
                wire_origin: None,
                translation_suffix_at: None,
            },
        );
    }

    /// Recompute the keyring's configured-network set from the CURRENT servers
    /// config plus every active connection, so `legacy_adoption_allowed()`
    /// reflects runtime changes — an ad-hoc `/connect` net or a `/server
    /// add`/`remove` since startup, none of which touch the startup snapshot.
    /// A stale set that still reports <=1 network would let a scoped miss on a
    /// newly-connected second network fall back to an unscoped legacy E2E row,
    /// the exact cross-network reuse the scoping prevents. No-op without an E2E
    /// manager.
    pub(crate) fn refresh_e2e_configured_networks(&self) {
        let Some(mgr) = self.state.e2e_manager.as_ref() else {
            return;
        };
        // Real IRC networks only: the union of configured server labels and the
        // labels of live connections, EXCLUDING the UI pseudo-connections (the
        // `_default` status placeholder, the `_shell` PTY, and `_log_*` log
        // browsers). They hold no E2E contexts and must not inflate the
        // isolation count — doing so would wrongly deny the legacy fallback to a
        // genuine single-network user (fail-closed, but a functional regression).
        let labels: HashSet<String> = self
            .config
            .servers
            .iter()
            .map(|(id, server)| self.state.connections.get(id)
                .filter(|connection| connection.bouncer_identity.is_some())
                .map_or_else(|| config::network_scope::network_scope(id, server, &self.config.general.username),
                    |connection| connection.network_key().to_string()))
            .chain(
                self.state
                    .connections
                    .values()
                    .filter(|c| {
                        c.id != Self::DEFAULT_CONN_ID
                            && c.id != Self::SHELL_CONN_ID
                            && !c.id.starts_with(Self::LOG_CONN_PREFIX)
                    })
                    .map(|c| c.network_key().to_string()),
            )
            .collect();
        mgr.keyring().set_configured_networks(labels);
    }

    pub(crate) fn start_autoconnects(&mut self, server_ids: &[String]) {
        for server_id in server_ids {
            let Some(server_config) = self.config.servers.get(server_id).cloned() else {
                continue;
            };
            let label = server_config.label.clone();
            let buffer_id = self.setup_connection(server_id, &server_config);
            self.spawn_reconnect(server_id, Some(server_config), &buffer_id, &label);
        }
    }

    /// Add an event message to the specified buffer.
    pub(crate) fn add_event_to_buffer(&mut self, buffer_id: &str, text: String) {
        let id = self.state.next_message_id();
        self.state.add_local_message(
            buffer_id,
            Message {
                redaction_ref: None,
                redaction_msgid: None,
                log_key: None,
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
                wire_origin: None,
                translation_suffix_at: None,
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
    pub(crate) fn spawn_reconnect(
        &mut self,
        conn_id: &str,
        server_config: Option<config::ServerConfig>,
        buffer_id: &str,
        label: &str,
    ) {
        if let Some(mut cfg) = server_config {
            cfg.bind_ip = crate::irc::resolve_bind_ip(&cfg, self.cli_bind_override.as_deref(), &self.config.general);
            self.start_connection_attempt(conn_id, cfg);
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
            IrcEvent::Attempt(id, generation, event) => {
                if self.connection_attempts.get(&id) == Some(&generation) {
                    self.handle_irc_event(*event);
                }
            }
            IrcEvent::HandleReady(handle) => {
                self.upstream_auth.remove(&handle.conn_id);
                self.account_registration.remove(&handle.conn_id);
                self.reset_server_search(&handle.conn_id);
                self.bouncer_metadata.remove(&handle.conn_id);
                self.bouncer_certificates.remove(&handle.conn_id);
                self.cancel_bouncer_webpush(&handle.conn_id);
                if let Some(rules) = &handle.account_registration_rules {
                    self.account_registration.insert(handle.conn_id.clone(), super::account_registration::Session::from_rules(rules));
                }
                // Store local IP on Connection state (for DCC own-IP fallback)
                if let Some(conn) = self.state.connections.get_mut(&handle.conn_id) {
                    conn.local_ip = handle.local_ip;
                }
                self.apply_bouncer_identity(&handle.conn_id, handle.bouncer_identity.as_ref(), handle.bouncer_provider);
                // A new session for this `conn_id`. Bumping here — rather
                // than on disconnect — is what makes a captured generation
                // mean "the session I was written for": a reconnect moves it,
                // and a connection that never comes back is caught by the
                // handle being absent.
                *self
                    .conn_generations
                    .entry(handle.conn_id.clone())
                    .or_default() += 1;
                self.irc_handles.insert(handle.conn_id.clone(), *handle);
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
            IrcEvent::Connected(conn_id, enabled_caps, multiline_limits) => {
                self.reset_monitor(&conn_id);
                self.reconnect_read_markers(&conn_id);
                self.history_discovery.remove(&conn_id);
                self.bouncer_networks.remove(&conn_id);
                // Store negotiated caps on connection
                if let Some(conn) = self.state.connections.get_mut(&conn_id) {
                    conn.enabled_caps = enabled_caps;
                    conn.multiline = multiline_limits;
                }
                self.reconnect_bouncer_presence(&conn_id);
                if self.state.connections.get(&conn_id).is_some_and(crate::state::connection::Connection::bouncer_control) {
                    self.bouncer_networks.insert(conn_id.clone(), crate::irc::bouncer::NetworkRegistry::default());
                    if !self.state.connections[&conn_id].enabled_caps.contains(crate::irc::bouncer::NETWORKS_NOTIFY_CAP)
                        && let Some(handle) = self.irc_handles.get(&conn_id)
                    {
                        let _ = handle.sender().send(::irc::proto::Command::Raw("BOUNCER".into(), vec!["LISTNETWORKS".into()]));
                    }
                }
                if let Some(conn) = self.state.connections.get(&conn_id)
                    && conn.server_owns_history()
                    && !conn.bouncer_control()
                    && !conn.enabled_caps.contains("draft/chathistory")
                {
                    let buffer_id = crate::state::buffer::make_buffer_id(&conn_id, &conn.label);
                    crate::irc::events::emit(&mut self.state, &buffer_id,
                        "Bouncer history stays on the server. This connection does not support CHATHISTORY; only live messages and server replay are available.");
                }
                // Collect channels to rejoin before handle_connected resets state
                let rejoin_channels = crate::irc::events::channels_to_rejoin(&self.state, &conn_id);
                crate::irc::events::handle_connected(&mut self.state, &conn_id);
                self.refresh_saferate(&conn_id);

                // Stamp the reconnect cutoff now — after handle_connected reset the
                // chathistory state, but before any JOIN echo or post-reconnect
                // traffic can be logged. The gap-fill (fired at end-of-MOTD/NAMES)
                // anchors AFTER the newest row OLDER than this, so it targets the
                // disconnected gap rather than a reconnect-time row.
                if let Some(conn) = self.state.connections.get_mut(&conn_id) {
                    conn.chathistory
                        .set_gapfill_cutoff(chrono::Utc::now().timestamp_millis());
                }

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
                let explicit_binding = self.state.connections.get(&conn_id)
                    .is_some_and(|conn| conn.bouncer_network_id().is_some() || conn.bouncer_control());
                let config_channels: Vec<String> = self
                    .config
                    .servers
                    .iter()
                    .find(|(id, cfg)| *id == &conn_id || cfg.label == conn_id)
                    .filter(|_| !explicit_binding)
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
                            log_total_lines: None,
                            log_oldest_ts: None,
                            log_newest_ts: None,
                            history_exhausted: false,
                            log_initial_loaded: false,
                            pin_backlog: false,
            metadata: crate::irc::metadata::Flags::default(),
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
                            .sender()
                            .send(::irc::proto::Command::JOIN(chanlist, None, None));
                    }
                }

                // Reconnect gap-fill is deferred until end-of-MOTD (see the
                // RPL_ENDOFMOTD/ERR_NOMOTD handling below): at RPL_WELCOME the
                // 005 ISUPPORT lines haven't been parsed and channels aren't
                // joined yet, so a CHATHISTORY request here would use default
                // limits/ref types and could be rejected for non-membership.
            }
            IrcEvent::Disconnected(conn_id, error) => {
                self.upstream_auth.remove(&conn_id);
                self.account_registration.remove(&conn_id);
                self.reset_server_search(&conn_id);
                self.bouncer_metadata.remove(&conn_id);
                self.bouncer_certificates.remove(&conn_id);
                self.cancel_bouncer_webpush(&conn_id);
                self.reset_monitor(&conn_id);
                self.disconnect_bouncer_mutation(&conn_id);

                self.history_discovery.remove(&conn_id);
                self.suspend_bouncer_children(&conn_id);
                if let Some(requested) = self.state.background_join_connections.get_mut(&conn_id) { requested.clear(); }
                self.bouncer_networks.remove(&conn_id);
                // Release anything still waiting on a translation for this
                // connection FIRST. The lines already arrived; holding them
                // for the full timeout after the server is gone means a
                // stuck provider blanks the channel for seconds with no
                // prospect of the answer ever being useful.
                self.flush_translate_queues_for_connection(&conn_id);
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
                self.labeled_requests.remove(&conn_id);
                self.channel_query_queues.remove(&conn_id);
                self.channel_query_in_flight.remove(&conn_id);
                self.channel_query_sent_at.remove(&conn_id);
            }
            IrcEvent::Message(conn_id, msg) => {
                if self.handle_account_registration(&conn_id, &msg) { return; }
                if self.handle_upstream_auth(&conn_id, &msg) { return; }
                if self.handle_bouncer_mutation(&conn_id, &msg) { return; }
                if self.handle_bouncer_network_message(&conn_id, &msg) {
                    return;
                }
                if self.handle_bouncer_webpush(&conn_id, &msg) { return; }
                if self.handle_bouncer_certificates(&conn_id, &msg) { return; }
                if self.finish_metadata_operation(&conn_id, &msg) { return; }
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
                                let _ = handle.sender().send(::irc::proto::Command::CAP(
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
                    self.tick_labels();
                    self.tick_bouncer_metadata();
                    self.tick_bouncer_certificates();
                    self.tick_bouncer_webpush();
                }

                // --- IRCv3 batch interception ---
                // Handle BATCH commands (start/end) and collect @batch-tagged messages.
                if let ::irc::proto::Command::BATCH(ref ref_tag, ref sub, ref params) = msg.command
                {
                    let redaction_ref = self.state.retain_wire_redaction(&conn_id, &msg);
                    let tracker = self.batch_trackers.entry(conn_id.clone()).or_default();
                    tracker.invalidate_isupport_parent(&msg);
                    if let Some(tag) = ref_tag.strip_prefix('+') {
                        // Start batch
                        let batch_type = sub
                            .as_ref()
                            .map_or_else(String::new, |s| s.to_str().to_string());
                        let batch_params = params.clone().unwrap_or_default();
                        let mut opener = (*msg).clone();
                        tracker.inherit_label(&mut opener);
                        tracker.start_batch(tag, &batch_type, batch_params, opener.tags);
                        if let Some(identity) = redaction_ref {
                            tracker.retain_redactions(tag, &[identity]);
                        }
                        tracing::debug!("batch started: tag={tag} type={batch_type}");
                    } else if let Some(tag) = ref_tag.strip_prefix('-') {
                        // End batch
                        if let Some(batch) = tracker.end_batch(tag) {
                            tracing::debug!(
                                "batch ended: tag={tag} type={} msgs={}",
                                batch.batch_type,
                                batch.messages.len()
                            );
                            self.receive_completed_batch(&conn_id, &batch);
                            if batch.parent_ref().is_none() && let Some(label) = batch.opener_tags.as_ref().and_then(|tags| tags.iter().find(|tag| tag.0 == "label")).and_then(|tag| tag.1.as_deref()) {
                                self.finish_labeled_response(&conn_id, label);
                            }
                        }
                    }
                    // BATCH commands themselves are not dispatched further
                } else if self
                    .batch_trackers
                    .entry(conn_id.clone())
                    .or_default()
                    .is_batched(&msg)
                {
                    if crate::irc::redaction::is_redaction(&msg) {
                        self.state.receive_redaction(&conn_id, &msg);
                        self.drain_pending_web_events();
                    }
                    let redaction_ref = self.state.retain_wire_redaction(&conn_id, &msg);
                    let batch_tag = crate::irc::batch::BatchTracker::get_batch_tag_owned(&msg);
                    // Message belongs to an open batch — collect it, don't process now
                    if let Some(tracker) = self.batch_trackers.get_mut(&conn_id) {
                        let mut message = *msg;
                        tracker.inherit_label(&mut message);
                        tracker.add_message(message);
                        if let (Some(tag), Some(identity)) = (batch_tag, redaction_ref) {
                            tracker.retain_redactions(&tag, &[identity]);
                            tracker.refresh_redactions(&tag, &mut self.state, &conn_id);
                        }
                    }
                } else if crate::irc::batch::BatchTracker::get_batch_tag_owned(&msg).is_none() {
                    self.dispatch_live_irc_message(&conn_id, &msg);
                }
            }
        }
    }
}

#[cfg(test)]
mod activity_rename_tests {
    use super::*;

    #[test]
    fn soju_colon_password_rotation_preserves_the_network_scope() {
        use crate::irc::bouncer::{Identity, Provider};
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let mut connection = crate::state::events::tests::make_test_connection();
        connection.origin_config.sasl_user = None;
        connection.origin_config.sasl_pass = None;
        connection.origin_config.sasl_mechanism = None;
        connection.origin_config.username = Some("account".into());
        connection.origin_config.password = Some("old:secret".into());
        connection.origin_config.bouncer_network_id = Some("42".into());
        app.config.servers.insert("libera".into(), connection.origin_config.clone());
        let old_scope = config::network_scope::network_scope("libera", &connection.origin_config, &app.config.general.username);
        app.state.e2e_manager.as_ref().unwrap().keyring().set_channel_config(
            &crate::e2e::keyring::ChannelConfig {
                channel: crate::e2e::scoped_context(&old_scope, "#secret"),
                enabled: true,
                mode: crate::e2e::keyring::ChannelMode::Normal,
            },
        ).unwrap();
        app.state.add_connection(connection);
        let identity = Identity::Network("42".into());
        app.apply_bouncer_identity("libera", Some(&identity), Some(Provider::Soju));
        let scope = app.state.connections["libera"].network_key().to_string();
        assert!(matches!(app.state.e2e_send_plan_for_target("libera", "#secret", "private"),
            Err(crate::app::e2e_gate::E2eRefusal::BouncerScopeChanged)));
        app.state.connections.get_mut("libera").unwrap().origin_config.password = Some("new:secret".into());
        app.apply_bouncer_identity("libera", Some(&identity), Some(Provider::Soju));
        assert_eq!(app.state.connections["libera"].network_key(), scope);
        app.state.connections.get_mut("libera").unwrap().origin_config.username = Some("other-account".into());
        app.apply_bouncer_identity("libera", Some(&identity), Some(Provider::Soju));
        assert_ne!(app.state.connections["libera"].network_key(), scope);
    }

    #[test]
    fn discovered_bouncer_scope_preserves_login_and_isolates_rebinding() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let mut connection = crate::state::events::tests::make_test_connection();
        connection.origin_config.username = Some("account/network@laptop".into());
        connection.origin_config.sasl_user = Some("account/network@laptop".into());
        let original = connection.origin_config.clone();
        app.state.add_connection(connection);
        app.state.e2e_manager.as_ref().unwrap().keyring().set_channel_config(
            &crate::e2e::keyring::ChannelConfig {
                channel: crate::e2e::scoped_context(&original.label, "#secret"),
                enabled: true,
                mode: crate::e2e::keyring::ChannelMode::Normal,
            },
        ).unwrap();
        app.apply_bouncer_identity("libera", Some(&crate::irc::bouncer::Identity::Network("42".into())), None);
        assert!(matches!(
            app.state.e2e_send_plan_for_target("libera", "#secret", "private content"),
            Err(crate::app::e2e_gate::E2eRefusal::BouncerScopeChanged)
        ));
        let conn = &app.state.connections["libera"];
        assert!(conn.server_owns_history());
        assert_eq!(conn.bouncer_network_id(), Some("42"));
        assert_eq!(conn.origin_config.username, original.username);
        assert!(conn.origin_config.bouncer_network_id.is_none());
        assert!(conn.joined_channels.is_empty());
        let scope = conn.network_key().to_string();
        app.state.add_buffer(Buffer::for_test("libera", BufferType::Channel, "#old-network"));
        let origin = &mut app.state.connections.get_mut("libera").unwrap().origin_config;
        origin.username = Some("account/renamed@desktop".into());
        origin.sasl_user = Some("account/renamed@desktop".into());
        app.apply_bouncer_identity("libera", Some(&crate::irc::bouncer::Identity::Network("42".into())), None);
        assert_eq!(app.state.connections["libera"].network_key(), scope);
        assert!(app.state.buffers.contains_key("libera/#old-network"));
        app.apply_bouncer_identity("libera", Some(&crate::irc::bouncer::Identity::Network("43".into())), None);
        assert_ne!(app.state.connections["libera"].network_key(), scope);
        assert!(!app.state.buffers.contains_key("libera/#old-network"));
        app.apply_bouncer_identity("libera", Some(&crate::irc::bouncer::Identity::Control), None);
        assert!(app.state.connections["libera"].bouncer_control());
        app.apply_bouncer_identity("libera", None, None);
        assert!(!app.state.connections["libera"].server_owns_history());
    }

    #[test]
    fn dcc_nick_change_keeps_older_unread_chat_first() {
        use crate::dcc::types::{DccRecord, DccState, DccType};
        let mut app = crate::app::input::submit_typing_tests::test_app();
        app.state.add_connection(crate::state::events::tests::make_test_connection());
        app.state.add_buffer(Buffer::for_test("libera", BufferType::DccChat, "=alice"));
        app.state.add_buffer(Buffer::for_test("libera", BufferType::Channel, "#newer"));
        app.state.set_activity("libera/=alice", ActivityLevel::Activity);
        app.state.set_activity("libera/#newer", ActivityLevel::Activity);
        app.dcc.records.insert("alice".into(), DccRecord {
            id: "alice".into(), dcc_type: DccType::Chat, nick: "alice".into(),
            conn_id: "libera".into(), addr: "127.0.0.1".parse().unwrap(), port: 12345,
            state: DccState::Connected, passive_token: None,
            created: std::time::Instant::now(), started: None, bytes_transferred: 0,
            mirc_ctcp: true, ident: "user".into(), host: "example.invalid".into(),
        });
        let message = ":alice!user@example.invalid NICK alicia".parse().unwrap();
        app.handle_irc_event(IrcEvent::Message("libera".into(), Box::new(message)));
        assert!(!app.state.buffers.contains_key("libera/=alice"));
        assert_eq!(app.state.next_activity_buffer().as_deref(), Some("libera/=alicia"));
        app.state.set_active_buffer("libera/=alicia");
        assert_eq!(app.state.next_activity_buffer().as_deref(), Some("libera/#newer"));
    }

    #[test]
    fn explicit_bouncer_binding_does_not_rejoin_stale_or_configured_channels() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let mut connection = crate::state::events::tests::make_test_connection();
        connection.origin_config.bouncer_network_id = Some("42".into());
        connection.origin_config.channels = vec!["#configured".into()];
        connection.joined_channels = vec!["#stale".into()];
        app.config.servers.insert("libera".into(), connection.origin_config.clone());
        app.state.add_connection(connection);
        app.state.add_buffer(Buffer::for_test("libera", BufferType::Channel, "#stale"));
        let sender = crate::irc::IrcSender::capturing(0);
        app.irc_handles.insert("libera".into(), crate::irc::IrcHandle::new("libera".into(), sender.clone(), None, None));
        app.handle_irc_event(IrcEvent::Connected("libera".into(), HashSet::from([crate::irc::bouncer::NETWORKS_CAP.into()]), None));
        assert!(!app.state.buffers.contains_key("libera/#configured"));
        assert!(sender.captured().iter().all(|message| !matches!(message.command, ::irc::proto::Command::JOIN(..))));
    }

}

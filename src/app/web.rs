use super::App;

impl App {
    fn e2e_debug_enabled() -> bool {
        std::env::var("REPARTEE_E2E_DEBUG_BUFFER").is_ok_and(|v| {
            let v = v.trim();
            !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
        })
    }

    fn emit_e2e_debug(&mut self, conn_id: &str, channel: Option<&str>, text: impl Into<String>) {
        if !Self::e2e_debug_enabled() {
            return;
        }
        let text = text.into();
        let buffer_id = channel
            .map(|channel| crate::state::buffer::make_buffer_id(conn_id, channel))
            .filter(|id| self.state.buffers.contains_key(id))
            .or_else(|| {
                self.state
                    .active_buffer()
                    .filter(|buf| buf.connection_id == conn_id)
                    .map(|buf| buf.id.clone())
            })
            .or_else(|| {
                self.state
                    .connections
                    .get(conn_id)
                    .map(|conn| crate::state::buffer::make_buffer_id(conn_id, &conn.label))
            });
        let Some(buffer_id) = buffer_id else { return };
        let id = self.state.next_message_id();
        let event_param = text.clone();
        self.state.add_message(
            &buffer_id,
            crate::state::buffer::Message {
                id,
                timestamp: chrono::Utc::now(),
                message_type: crate::state::buffer::MessageType::Event,
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

    /// Broadcast a `WebEvent` to all connected web clients.
    pub(crate) fn broadcast_web(&self, event: crate::web::protocol::WebEvent) {
        let _ = self.web_broadcaster.send(event);
    }

    /// Stop the web server if running. Aborts the accept loop task and
    /// clears per-session state (sessions, rate limiter, snapshot).
    /// The `web_broadcaster` and `web_cmd_tx/rx` channel survive — they
    /// are owned by `App` and reused across restarts.
    pub(crate) fn stop_web_server(&mut self) {
        if let Some(handle) = self.web_server_handle.take() {
            handle.abort();
            tracing::info!("web server stopped");
            crate::commands::helpers::add_local_event(self, "Web server stopped");
        }
        self.web_sessions = None;
        self.web_rate_limiter = None;
        self.web_state_snapshot = None;
        self.web_active_buffers.clear();
        // Detach the preview extractor from AppState too — otherwise
        // message_to_wire keeps populating `previews` for messages that
        // no client can render.
        self.state.web_preview_extractor = None;
    }

    /// Start the web server (HTTPS + WebSocket). Creates fresh session
    /// store, rate limiter, and state snapshot. Reuses the existing
    /// `web_broadcaster` and `web_cmd_tx` channel.
    ///
    /// Does nothing if `web.enabled` is false or `web.password` is empty.
    #[expect(
        clippy::too_many_lines,
        reason = "linear startup sequence; splitting would obscure ordering"
    )]
    pub(crate) async fn start_web_server(&mut self) {
        if !self.config.web.enabled {
            return;
        }
        if self.config.web.password.is_empty() {
            tracing::warn!("web.enabled=true but web.password is empty — set WEB_PASSWORD in .env");
            crate::commands::helpers::add_local_event(
                self,
                "web.enabled=true but web.password is empty — set WEB_PASSWORD in .env",
            );
            return;
        }

        // Make sure the session secret exists before constructing the store
        // (the store HMACs raw tokens with this secret; rotating it is what
        // logs everyone out).
        let env_path = crate::constants::env_path();
        if let Err(e) = crate::config::ensure_session_secret(&mut self.config.web, &env_path) {
            tracing::warn!("could not initialise WEB_SESSION_SECRET: {e}");
        }

        let session_path = crate::constants::home_dir().join("web_sessions.bin");
        let session_store = match crate::web::auth::SessionStore::load(
            &session_path,
            self.config.web.session_secret.clone(),
            self.config.web.session_days,
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("session store load failed ({e}), starting empty");
                crate::web::auth::SessionStore::with_days(
                    self.config.web.session_secret.clone(),
                    self.config.web.session_days,
                )
            }
        };
        let sessions = std::sync::Arc::new(tokio::sync::Mutex::new(session_store));
        let limiter =
            std::sync::Arc::new(tokio::sync::Mutex::new(crate::web::auth::RateLimiter::new()));
        self.web_sessions = Some(std::sync::Arc::clone(&sessions));
        self.web_rate_limiter = Some(std::sync::Arc::clone(&limiter));

        let snapshot = std::sync::Arc::new(parking_lot::RwLock::new(
            crate::web::server::WebStateSnapshot {
                buffers: Vec::new(),
                connections: Vec::new(),
                mention_count: 0,
                active_buffer_id: None,
                timestamp_format: self.config.web.timestamp_format.clone(),
                emotes_enabled: self.config.emotes.web_enabled(),
            },
        ));
        self.web_state_snapshot = Some(std::sync::Arc::clone(&snapshot));

        // Build the preview extractor (if enabled). Both AppState and
        // AppHandle share the same Arc so registry lookups in the handler
        // see what extraction wrote.
        let preview_extractor = if self.config.web.image_previews {
            let secret = if self.config.web.session_secret.is_empty() {
                vec![0u8; 32]
            } else {
                self.config.web.session_secret.clone()
            };
            Some(std::sync::Arc::new(
                crate::web::preview::WebPreviewExtractor::new(
                    secret,
                    self.config.web.image_previews_max_per_msg as usize,
                    self.config.web.thumbnail_cache_mb,
                ),
            ))
        } else {
            None
        };
        self.state
            .web_preview_extractor
            .clone_from(&preview_extractor);

        let handle = std::sync::Arc::new(crate::web::server::AppHandle {
            broadcaster: std::sync::Arc::clone(&self.web_broadcaster),
            web_cmd_tx: self.web_cmd_tx.clone(),
            password: self.config.web.password.clone(),
            username: self.config.web.username.clone(),
            session_store: sessions,
            rate_limiter: limiter,
            session_cookie_max_age: i64::from(self.config.web.session_days) * 86_400,
            preview_extractor,
            web_state_snapshot: Some(snapshot),
        });

        match crate::web::server::start(&self.config.web, handle).await {
            Ok(h) => {
                self.web_server_handle = Some(h);
                tracing::info!(
                    "web frontend at https://{}:{}",
                    self.config.web.bind_address,
                    self.config.web.port
                );
                crate::commands::helpers::add_local_event(
                    self,
                    &format!(
                        "Web server listening on https://{}:{}",
                        self.config.web.bind_address, self.config.web.port
                    ),
                );
            }
            Err(e) => {
                tracing::error!("failed to start web server: {e}");
                crate::commands::helpers::add_local_event(
                    self,
                    &format!("Failed to start web server: {e}"),
                );
            }
        }
    }

    /// Drain pending web events queued during IRC event processing.
    pub(crate) fn drain_pending_web_events(&mut self) {
        let events = std::mem::take(&mut self.state.pending_web_events);
        if !events.is_empty() {
            tracing::debug!(count = events.len(), "draining {} web events", events.len());
        }
        let mut structural_change = false;
        for event in events {
            match &event {
                crate::web::protocol::WebEvent::BufferCreated { buffer } => {
                    tracing::debug!(buffer_id = %buffer.id, "broadcasting BufferCreated");
                    structural_change = true;
                }
                crate::web::protocol::WebEvent::BufferClosed { buffer_id } => {
                    tracing::debug!(%buffer_id, "broadcasting BufferClosed");
                    structural_change = true;
                }
                crate::web::protocol::WebEvent::ActiveBufferChanged { buffer_id } => {
                    // Broadcast so the TUI and every web session stay 1:1 in
                    // sync — switching the active buffer anywhere (TUI, any tab,
                    // phone) propagates everywhere. Also structural so a
                    // newly-connecting session's SyncInit snapshot reflects the
                    // new active buffer. (Clients ignore the echo for a buffer
                    // they already switched to, and may opt out via the
                    // `web_follow_tui_buffer` localStorage flag.)
                    structural_change = true;
                    // …except shell buffers: they're per-session web terminals,
                    // so a followed session would render an unusable ShellView
                    // and have its shell I/O rejected. Don't propagate a switch
                    // into a shell (e.g. the TUI opening its own /shell).
                    if self
                        .state
                        .buffers
                        .get(buffer_id)
                        .is_some_and(|b| b.buffer_type == crate::state::buffer::BufferType::Shell)
                    {
                        continue;
                    }
                }
                crate::web::protocol::WebEvent::ConnectionStatus { .. }
                | crate::web::protocol::WebEvent::SettingsChanged { .. } => {
                    structural_change = true;
                }
                _ => {}
            }
            if let crate::web::protocol::WebEvent::MentionAlert {
                ref buffer_id,
                ref message,
            } = event
            {
                self.record_mention(buffer_id, message);
            }
            self.broadcast_web(event);
        }
        if structural_change {
            // A new WS session connecting right now would otherwise
            // get up to 1 second of stale `SyncInit` data (buffer
            // list / connection list / active_buffer_id). Refreshing
            // eagerly closes that window.
            self.refresh_web_state_snapshot();
        }
    }

    /// Rewrite the shared `WebStateSnapshot` from the current `AppState`.
    /// Called both from the 1 s background tick (safety net) and from
    /// `drain_pending_web_events` whenever a structural change is in the
    /// queue (buffer add/remove, active-buffer flip, etc.). The lock is
    /// held briefly and never across an `.await`.
    pub(crate) fn refresh_web_state_snapshot(&self) {
        let Some(ref snapshot) = self.web_state_snapshot else {
            return;
        };
        let mention_count = self
            .storage
            .as_ref()
            .and_then(|s| {
                s.db.try_lock()
                    .ok()
                    .and_then(|db| crate::storage::query::get_unread_mention_count(&db).ok())
            })
            .unwrap_or(0);
        let init = crate::web::snapshot::build_sync_init(
            &self.state,
            mention_count,
            &self.config.web.timestamp_format,
            self.config.emotes.web_enabled(),
        );
        if let crate::web::protocol::WebEvent::SyncInit {
            buffers,
            connections,
            mention_count,
            active_buffer_id,
            timestamp_format,
            emotes_enabled,
            ..
        } = init
        {
            let mut snap = snapshot.write();
            snap.buffers = buffers;
            snap.connections = connections;
            snap.mention_count = mention_count;
            snap.active_buffer_id = active_buffer_id;
            snap.timestamp_format = timestamp_format;
            snap.emotes_enabled = emotes_enabled;
        }
    }

    /// Drain any queued RPE2E CTCP NOTICE sends produced by the E2E
    /// event handlers and ship them via the appropriate connection's IRC
    /// sender. Mirrors `drain_pending_web_events` and runs right after it
    /// inside the IRC event loop so handshake traffic reaches the wire
    /// in the same dispatch turn.
    pub(crate) fn drain_pending_e2e_sends(&mut self) {
        let pending: Vec<crate::state::PendingE2eSend> =
            std::mem::take(&mut self.state.pending_e2e_sends);
        for send in pending {
            let parsed = {
                let trimmed = send
                    .notice_text
                    .strip_prefix('\x01')
                    .unwrap_or(&send.notice_text);
                let inner = trimmed.strip_suffix('\x01').unwrap_or(trimmed);
                crate::e2e::handshake::parse(inner).ok().flatten()
            };
            let debug_line = parsed.as_ref().map(|msg| match msg {
                crate::e2e::handshake::HandshakeMsg::Req(req) => (
                    req.channel.as_str(),
                    format!(
                        "[E2E debug] TX KEYREQ to {} for {}",
                        send.target, req.channel
                    ),
                ),
                crate::e2e::handshake::HandshakeMsg::Rsp(rsp) => (
                    rsp.channel.as_str(),
                    format!(
                        "[E2E debug] TX KEYRSP to {} for {}",
                        send.target, rsp.channel
                    ),
                ),
                crate::e2e::handshake::HandshakeMsg::Rekey(rekey) => (
                    rekey.channel.as_str(),
                    format!(
                        "[E2E debug] TX REKEY to {} for {}",
                        send.target, rekey.channel
                    ),
                ),
            });
            let Some(handle) = self.irc_handles.get(&send.connection_id) else {
                tracing::warn!(
                    connection_id = %send.connection_id,
                    "e2e send dropped: no IRC handle for connection"
                );
                if let Some((channel, line)) = debug_line.as_ref() {
                    self.emit_e2e_debug(
                        &send.connection_id,
                        Some(channel),
                        format!("{line} failed: no IRC handle for connection"),
                    );
                }
                continue;
            };
            if let Err(e) = handle.sender.send_notice(&send.target, &send.notice_text) {
                tracing::warn!(
                    target = %send.target,
                    error = %e,
                    "e2e send_notice failed"
                );
                if let Some((channel, line)) = debug_line.as_ref() {
                    self.emit_e2e_debug(
                        &send.connection_id,
                        Some(channel),
                        format!("{line} failed: {e}"),
                    );
                }
            } else if let Some((channel, line)) = debug_line {
                self.emit_e2e_debug(&send.connection_id, Some(channel), line);
            }
        }
    }

    /// Insert a mention into the `SQLite` mentions table.
    pub(crate) fn record_mention(&self, buffer_id: &str, msg: &crate::web::protocol::WireMessage) {
        let Some(ref storage) = self.storage else {
            return;
        };
        let Ok(db) = storage.db.lock() else {
            return;
        };
        let (network, buffer) = crate::web::snapshot::split_buffer_id(buffer_id);
        let channel = self
            .state
            .buffers
            .get(buffer_id)
            .map_or(buffer, |b| b.name.as_str());
        let nick = msg.nick.as_deref().unwrap_or("");
        let _ = crate::storage::query::insert_mention(
            &db,
            msg.timestamp,
            network,
            buffer,
            channel,
            nick,
            &msg.text,
        );
    }

    /// Dispatch a command received from a web client.
    #[expect(
        clippy::too_many_lines,
        reason = "web command dispatch is intentionally flat and security checks are local"
    )]
    pub(crate) fn handle_web_command(
        &mut self,
        cmd: crate::web::protocol::WebCommand,
        session_id: &str,
    ) {
        use crate::web::protocol::WebCommand;
        use crate::web::snapshot;

        match cmd {
            WebCommand::WebConnect { initial_buffer_id } => {
                if let Some(buffer_id) = initial_buffer_id {
                    self.web_active_buffers
                        .insert(session_id.to_string(), buffer_id);
                }
            }
            WebCommand::SendMessage { buffer_id, text } => {
                self.web_send_message(&buffer_id, &text);
            }
            WebCommand::SwitchBuffer { buffer_id } => {
                // Flip the GLOBAL active buffer so the TUI and every other web
                // session follow (1:1 sync across all clients) — but ONLY for
                // channels/queries. Shell buffers are per-session terminals
                // (each web session owns its own PTY, keyed by its per-session
                // active buffer); syncing them would make followed sessions
                // render an unusable ShellView whose ShellInput/ShellResize the
                // server rejects, and would drag the TUI into a web shell. So a
                // shell switch stays purely local to the initiating session.
                let is_shell = self
                    .state
                    .buffers
                    .get(&buffer_id)
                    .is_some_and(|b| b.buffer_type == crate::state::buffer::BufferType::Shell);
                if !is_shell {
                    self.state.set_active_buffer(&buffer_id);
                }
                // Per-session tracking is always needed for shell input/screen
                // routing (a web shell is keyed by the session's active buffer).
                self.web_active_buffers
                    .insert(session_id.to_string(), buffer_id.clone());
                let web_id = format!("web-{session_id}");
                if self.shell_mgr.has_web_session(&web_id) {
                    self.force_broadcast_web_shell_screen(&web_id);
                } else if let Some(shell_id) = self
                    .shell_mgr
                    .session_id_for_buffer(&buffer_id)
                    .map(ToString::to_string)
                {
                    self.force_broadcast_shell_screen(&shell_id);
                }
            }
            WebCommand::MarkRead { buffer_id, .. } => {
                self.web_mark_read(&buffer_id);
            }
            WebCommand::FetchMessages {
                buffer_id,
                limit,
                before,
                before_id,
            } => {
                self.web_fetch_messages(&buffer_id, limit, before, before_id, session_id);
            }
            WebCommand::FetchNickList { buffer_id } => {
                if let Some(crate::web::protocol::WebEvent::NickList {
                    buffer_id: bid,
                    nicks,
                    ..
                }) = snapshot::build_nick_list(&self.state, &buffer_id)
                {
                    self.broadcast_web(crate::web::protocol::WebEvent::NickList {
                        buffer_id: bid,
                        nicks,
                        session_id: Some(session_id.to_string()),
                    });
                }
            }
            WebCommand::FetchMentions => {
                self.web_fetch_mentions(session_id);
            }
            WebCommand::RunCommand { buffer_id, text } => {
                self.web_run_command(&buffer_id, &text);
            }
            WebCommand::ShellInput { buffer_id, data } => {
                if self.web_active_buffers.get(session_id) != Some(&buffer_id) {
                    tracing::debug!(%session_id, %buffer_id, "ignoring shell input for inactive web buffer");
                    return;
                }
                if !self
                    .state
                    .buffers
                    .get(&buffer_id)
                    .is_some_and(|b| b.buffer_type == crate::state::buffer::BufferType::Shell)
                {
                    tracing::debug!(%session_id, %buffer_id, "ignoring shell input for non-shell buffer");
                    return;
                }
                let web_id = format!("web-{session_id}");
                if let Ok(bytes) =
                    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &data)
                {
                    self.shell_mgr.write_web(&web_id, &bytes);
                }
            }
            WebCommand::WebDisconnect => {
                self.web_active_buffers.remove(session_id);
                self.shell_mgr.close_web_by_session(session_id);
            }
            WebCommand::ShellResize {
                buffer_id,
                cols,
                rows,
            } => {
                if self.web_active_buffers.get(session_id) != Some(&buffer_id) {
                    tracing::debug!(%session_id, %buffer_id, "ignoring shell resize for inactive web buffer");
                    return;
                }
                if !self
                    .state
                    .buffers
                    .get(&buffer_id)
                    .is_some_and(|b| b.buffer_type == crate::state::buffer::BufferType::Shell)
                {
                    tracing::debug!(%session_id, %buffer_id, "ignoring shell resize for non-shell buffer");
                    return;
                }
                let web_id = format!("web-{session_id}");
                if self.shell_mgr.has_web_session(&web_id) {
                    self.shell_mgr.resize_web(&web_id, cols, rows);
                } else if let Err(e) = self.shell_mgr.open_web(session_id, cols, rows) {
                    tracing::warn!("failed to open web shell: {e}");
                    return;
                }
                self.force_broadcast_web_shell_screen(&web_id);
            }
            WebCommand::SaveServer(cmd) => {
                let form = crate::ui::wizard::server::WebServerForm {
                    id: cmd.id,
                    network: cmd.network,
                    address: cmd.address,
                    port: cmd.port,
                    tls: cmd.tls,
                    tls_verify: cmd.tls_verify,
                    autoconnect: cmd.autoconnect,
                    channels: cmd.channels,
                    nick: cmd.nick,
                    username: cmd.username,
                    realname: cmd.realname,
                    bind_ip: cmd.bind_ip,
                    encoding: cmd.encoding,
                    sasl_user: cmd.sasl_user,
                    sasl_mechanism: cmd.sasl_mechanism,
                    autosendcmd: cmd.autosendcmd,
                    client_cert_path: cmd.client_cert_path,
                    auto_reconnect: cmd.auto_reconnect,
                    reconnect_delay: cmd.reconnect_delay,
                    reconnect_max_retries: cmd.reconnect_max_retries,
                    password: cmd.password,
                    sasl_pass: cmd.sasl_pass,
                };
                self.web_save_server(&form, session_id);
            }
        }
    }

    /// Apply a web-wizard server form: validate, persist via the shared
    /// `apply_server_config`, and report the outcome to the requesting client.
    ///
    /// On failure a `WebEvent::Error` is sent to the submitting session so the
    /// web user gets feedback (the modal closes optimistically client-side, so a
    /// silent failure would otherwise be invisible and invite a duplicate
    /// re-submit). It is targeted to `session_id` so other connected clients
    /// don't surface an error toast for a form they never submitted.
    fn web_save_server(&mut self, form: &crate::ui::wizard::server::WebServerForm, session_id: &str) {
        let built = match crate::ui::wizard::server::build_from_web(form, &self.config.servers) {
            Ok(built) => built,
            Err(msg) => {
                tracing::warn!("web SaveServer rejected: {msg}");
                self.broadcast_web(crate::web::protocol::WebEvent::Error {
                    message: format!("Add server failed: {msg}"),
                    session_id: Some(session_id.to_string()),
                });
                return;
            }
        };
        let cfg_path = crate::constants::config_path();
        let env_path = crate::constants::env_path();
        let id = built.id.clone();
        let result = crate::commands::handlers_admin::apply_server_config(
            &mut self.config,
            &cfg_path,
            &env_path,
            &built.id,
            built.config,
            built.password,
            built.sasl_pass,
        );
        self.cached_config_toml = None;
        match result {
            Ok(()) => tracing::info!("web wizard saved server '{id}'"),
            Err(e) => {
                tracing::warn!("web SaveServer failed to persist: {e}");
                self.broadcast_web(crate::web::protocol::WebEvent::Error {
                    message: format!("Server '{id}' could not be saved: {e}"),
                    session_id: Some(session_id.to_string()),
                });
            }
        }
    }

    /// Execute a command from a web client in the context of a buffer.
    ///
    /// We temporarily flip `state.active_buffer_id` to the target
    /// buffer, run `handle_submit`, then restore the previous active
    /// buffer. This is intentional, not a bug:
    ///
    /// - `handle_submit` and everything it transitively calls (script
    ///   hooks, command dispatch) is fully synchronous, so the
    ///   active-buffer "flip window" never overlaps another tokio
    ///   task. Scripts running inside the dispatch see the target
    ///   buffer, which is the correct context for the command.
    /// - `set_active_buffer_silent` is the `_silent` variant
    ///   specifically so this flip does NOT broadcast
    ///   `ActiveBufferChanged` to the TUI or other web sessions; only
    ///   the running command observes it.
    ///
    /// The alternative — threading an explicit `buffer_id` through
    /// every `handle_submit` callee — would be a cross-cutting refactor
    /// for no functional change, since the flip is already invisible
    /// outside the synchronous call.
    fn web_run_command(&mut self, buffer_id: &str, text: &str) {
        let prior = self.state.active_buffer_id.clone();
        self.set_active_buffer_silent(buffer_id);
        self.handle_submit(text);
        if let Some(id) = prior {
            self.set_active_buffer_silent(&id);
        } else {
            self.state.active_buffer_id = None;
        }
    }

    fn set_active_buffer_silent(&mut self, buffer_id: &str) {
        if !self.state.buffers.contains_key(buffer_id) {
            return;
        }
        self.state.active_buffer_id = Some(buffer_id.to_string());
        if let Some(buf) = self.state.buffers.get_mut(buffer_id) {
            buf.activity = crate::state::buffer::ActivityLevel::None;
            buf.unread_count = 0;
        }
    }

    /// Send a message from a web client to IRC.
    fn web_send_message(&mut self, buffer_id: &str, text: &str) {
        self.web_run_command(buffer_id, text);
    }

    /// Mark a buffer as read from a web client.
    fn web_mark_read(&mut self, buffer_id: &str) {
        if let Some(buf) = self.state.buffers.get_mut(buffer_id) {
            buf.unread_count = 0;
            buf.activity = crate::state::buffer::ActivityLevel::None;
        }
        self.broadcast_web(crate::web::protocol::WebEvent::ActivityChanged {
            buffer_id: buffer_id.to_string(),
            activity: 0,
            unread_count: 0,
        });
    }

    /// Fetch messages for a web client.
    #[expect(
        clippy::too_many_lines,
        reason = "linear pagination/cache fallthrough; splitting would obscure flow"
    )]
    fn web_fetch_messages(
        &self,
        buffer_id: &str,
        limit: u32,
        before: Option<i64>,
        before_id: Option<i64>,
        session_id: &str,
    ) {
        if buffer_id == Self::MENTIONS_BUFFER_ID {
            if let Some(buf) = self.state.buffers.get(buffer_id) {
                let capped = limit.min(500) as usize;
                let extractor = self.state.web_preview_extractor.as_deref();
                let msgs: Vec<_> = buf
                    .messages
                    .iter()
                    .rev()
                    .take(capped)
                    .rev()
                    .map(|m| crate::web::snapshot::message_to_wire(m, extractor))
                    .collect();
                tracing::debug!(
                    %buffer_id, count = msgs.len(),
                    "web FetchMessages: sending {} in-memory mention messages", msgs.len()
                );
                self.broadcast_web(crate::web::protocol::WebEvent::Messages {
                    buffer_id: buffer_id.to_string(),
                    messages: msgs,
                    has_more: false,
                    session_id: Some(session_id.to_string()),
                });
            }
            return;
        }

        // Initial load (no scroll-back cursor): serve from in-memory buffer.
        // This includes messages that haven't been flushed to DB yet (log writer
        // has a 1s flush interval + batch size of 50).
        if before.is_none()
            && let Some(buf) = self.state.buffers.get(buffer_id)
        {
            let capped = limit.min(500) as usize;
            let extractor = self.state.web_preview_extractor.as_deref();
            let msgs: Vec<_> = buf
                .messages
                .iter()
                .rev()
                .take(capped)
                .rev()
                .map(|m| crate::web::snapshot::message_to_wire(m, extractor))
                .collect();
            if !msgs.is_empty() {
                // More history is available if the in-memory page itself was
                // truncated, OR a log DB exists that may hold older rows than
                // what's in memory. Without the latter, a buffer with fewer than
                // `capped` live messages would report `has_more = false` and the
                // web client would never scroll back into the logs. A brand-new
                // channel with no DB history just gets one empty scroll-back fetch.
                let has_more = buf.messages.len() > capped || self.storage.is_some();
                tracing::debug!(
                    %buffer_id, count = msgs.len(),
                    "web FetchMessages: sending {} in-memory messages", msgs.len()
                );
                self.broadcast_web(crate::web::protocol::WebEvent::Messages {
                    buffer_id: buffer_id.to_string(),
                    messages: msgs,
                    has_more,
                    session_id: Some(session_id.to_string()),
                });
                return;
            }
        }

        // If the in-memory buffer was empty (e.g. brand new buffer or post-reconnect
        // before messages arrive), fall through to DB. Also used for scroll-back.
        // Every path below MUST send exactly one Messages reply (even empty) so
        // the client clears its per-buffer in-flight guard and doesn't block
        // future scroll-up fetches.
        let send_empty = |app: &Self| {
            app.broadcast_web(crate::web::protocol::WebEvent::Messages {
                buffer_id: buffer_id.to_string(),
                messages: Vec::new(),
                has_more: false,
                session_id: Some(session_id.to_string()),
            });
        };
        let Some(ref storage) = self.storage else {
            tracing::warn!("web FetchMessages: storage not available");
            send_empty(self);
            return;
        };
        let Ok(db) = storage.db.lock() else {
            tracing::warn!("web FetchMessages: failed to lock db");
            send_empty(self);
            return;
        };
        let capped_limit = limit.min(500) as usize;
        let (conn_id, buffer) = crate::web::snapshot::split_buffer_id(buffer_id);
        let network = self
            .state
            .connections
            .get(conn_id)
            .map_or_else(|| conn_id.to_string(), |c| c.label.clone());
        // Read-side key so encrypted logs decrypt on web scroll-back (previously
        // `None` => the client received ciphertext).
        let key = storage.crypto_key.as_ref();
        // `before` is the oldest loaded row's full-millisecond `@time` (current
        // bundle) or a whole-SECOND timestamp (previous bundle — the WS protocol
        // is not version-gated). `paginate_web_history` picks the matching keyset:
        // the subsecond keyset for a millis cursor (so a CHATHISTORY-backfilled
        // same-second row at e.g. `.200`, inserted after an already-loaded `.500`
        // row and hence a larger rowid, is returned rather than skipped), and the
        // lossless whole-second keyset for a legacy seconds cursor (flooring it to
        // `.000` on the subsecond keyset would drop same-second sub-`.000` rows).
        let messages = crate::storage::query::paginate_web_history(
            &db,
            &network,
            buffer,
            before.map(|b| (b, before_id)),
            capped_limit + 1,
            storage.encrypt,
            key,
        );
        match messages {
            Ok(msgs) => {
                let has_more = msgs.len() > capped_limit;
                // The query fetches `capped_limit + 1` rows newest-first then
                // reverses to chronological order, so the extra sentinel row sits
                // at the FRONT (oldest). When more history
                // exists, skip that front row — truncating the tail instead
                // would drop the NEWEST row (the one adjacent to the cursor),
                // which the next page can never re-fetch, leaving a permanent
                // one-message gap between pages.
                let skip = usize::from(has_more);
                tracing::debug!(
                    %buffer_id, count = msgs.len() - skip, %has_more,
                    "web FetchMessages: sending {} messages", msgs.len() - skip
                );
                let extractor = self.state.web_preview_extractor.as_deref();
                let wire: Vec<_> = msgs
                    .iter()
                    .skip(skip)
                    .map(|m| crate::web::snapshot::stored_to_wire(m, extractor))
                    .collect();
                self.broadcast_web(crate::web::protocol::WebEvent::Messages {
                    buffer_id: buffer_id.to_string(),
                    messages: wire,
                    has_more,
                    session_id: Some(session_id.to_string()),
                });
            }
            Err(e) => {
                tracing::warn!(%buffer_id, error = %e, "web FetchMessages: query failed");
                drop(db);
                send_empty(self);
            }
        }
    }

    /// Fetch unread mentions for a web client.
    fn web_fetch_mentions(&self, session_id: &str) {
        let Some(ref storage) = self.storage else {
            return;
        };
        let Ok(db) = storage.db.lock() else {
            return;
        };
        if let Ok(mentions) = crate::storage::query::get_unread_mentions(&db) {
            let wire: Vec<_> = mentions
                .iter()
                .map(|m| crate::web::protocol::WireMention {
                    id: m.id,
                    timestamp: m.timestamp,
                    buffer_id: format!("{}/{}", m.network, m.buffer),
                    channel: m.channel.clone(),
                    nick: m.nick.clone(),
                    text: m.text.clone(),
                })
                .collect();
            self.broadcast_web(crate::web::protocol::WebEvent::MentionsList {
                mentions: wire,
                session_id: Some(session_id.to_string()),
            });
        }
    }
}

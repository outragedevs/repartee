use super::App;

impl App {
    fn prepare_web_image_extractors(&mut self) {
        let secret = if self.config.web.session_secret.is_empty() {
            vec![0u8; 32]
        } else {
            self.config.web.session_secret.clone()
        };
        let extractor = std::sync::Arc::new(crate::web::preview::WebPreviewExtractor::new(
            secret, self.config.web.image_previews_max_per_msg as usize,
            self.config.web.thumbnail_cache_mb,
        ));
        self.state.web_preview_extractor = self.config.web.image_previews.then(|| std::sync::Arc::clone(&extractor));
        self.state.web_icon_extractor = Some(extractor);
    }

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
                redaction_ref: None,
                redaction_msgid: None,
                log_key: None,
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
                wire_origin: None,
                translation_suffix_at: None,
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
        self.web_buffer_unconfirmed.clear();
        self.pending_history_pages.clear();
        while let Some(session_id) = self.state.web_history_buffers.keys().next().cloned() {
            self.release_web_history(&session_id);
        }
        // Detach the preview extractor from AppState too — otherwise
        // message_to_wire keeps populating `previews` for messages that
        // no client can render.
        self.state.web_preview_extractor = None;
        self.state.web_icon_extractor = None;
    }

    /// Start the web server (HTTPS + WebSocket). Creates fresh session
    /// store, rate limiter, and state snapshot. Reuses the existing
    /// `web_broadcaster` and `web_cmd_tx` channel.
    ///
    /// Does nothing if `web.enabled` is false or `web.password` is empty.
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
                typing: std::collections::HashMap::new(),
                statusbar_items: crate::web::snapshot::statusbar_item_names(
                    &self.config.statusbar,
                ),
                statusbar_enabled: self.config.statusbar.enabled,
            },
        ));
        self.web_state_snapshot = Some(std::sync::Arc::clone(&snapshot));

        self.prepare_web_image_extractors();
        let preview_extractor = self.state.web_preview_extractor.clone();
        let icon_extractor = self.state.web_icon_extractor.clone();

        let handle = std::sync::Arc::new(crate::web::server::AppHandle {
            broadcaster: std::sync::Arc::clone(&self.web_broadcaster),
            web_cmd_tx: self.web_cmd_tx.clone(),
            password: self.config.web.password.clone(),
            username: self.config.web.username.clone(),
            session_store: sessions,
            rate_limiter: limiter,
            session_cookie_max_age: i64::from(self.config.web.session_days) * 86_400,
            icon_extractor,
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
                crate::web::protocol::WebEvent::BufferCreated { buffer, .. } => {
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
                    //
                    // Decided BEFORE the confirmation bookkeeping below: an
                    // event no browser is ever sent cannot have moved a tab, so
                    // doubting every session afterwards invents doubt out of
                    // nothing — and the doubt is not free. A session marked
                    // unconfirmed has a failed send WITHHELD from its composer
                    // by `deferred_retry_text`, which is the one path where
                    // getting the text back is the point.
                    if self
                        .state
                        .buffers
                        .get(buffer_id)
                        .is_some_and(|b| b.buffer_type == crate::state::buffer::BufferType::Shell)
                    {
                        continue;
                    }
                    // A tab that follows this changes buffer without telling
                    // us, and the opt-out lives in the browser, so afterwards
                    // the recorded buffer is a guess — except for a session
                    // already recorded AT the new buffer, which ends up there
                    // whether it followed or not. That exemption is what keeps
                    // a session's own `SwitchBuffer` from immediately marking
                    // itself unconfirmed: the switch that raised this event is
                    // the same one that recorded it. See
                    // `App::web_buffer_unconfirmed`.
                    for (session, recorded) in &self.web_active_buffers {
                        if recorded == buffer_id {
                            self.web_buffer_unconfirmed.remove(session);
                        } else {
                            self.web_buffer_unconfirmed.insert(session.clone());
                        }
                    }
                }
                crate::web::protocol::WebEvent::NetworkIcon { .. }
                | crate::web::protocol::WebEvent::ConnectionRemoved { .. }
                | crate::web::protocol::WebEvent::BufferRenamed { .. }
                | crate::web::protocol::WebEvent::ConnectionStatus { .. }
                | crate::web::protocol::WebEvent::SettingsChanged { .. }
                | crate::web::protocol::WebEvent::BufferE2eChanged { .. }
                | crate::web::protocol::WebEvent::BufferMetadataChanged { .. }
                | crate::web::protocol::WebEvent::MentionsRedacted { .. }
                // Structural so the shared snapshot — the source of a
                // *connecting* session's SyncInit — picks up the new item list
                // immediately, not up to a tick later.
                | crate::web::protocol::WebEvent::StatusbarConfig { .. } => {
                    structural_change = true;
                }
                _ => {}
            }
            if let crate::web::protocol::WebEvent::MentionAlert {
                ref buffer_id,
                ref message,
            } = event
            {
                if message.msgid.as_deref()
                    .and_then(|id| self.state.redaction_key(buffer_id, id))
                    .and_then(|key| self.state.redaction_registry.get(&key))
                    .is_some_and(|identity| identity.notice().is_some())
                {
                    continue;
                }
                self.record_mention(buffer_id, message);
            }
            if matches!(event, crate::web::protocol::WebEvent::RedactMessage { .. }) {
                self.inline_previews.purge();
                let mut message_ids = Vec::new();
                self.volatile_mentions.retain(|(_, mention, identity)| {
                    let keep = identity.as_ref().is_none_or(|identity| identity.notice().is_none());
                    if !keep { message_ids.push(mention.source_message_id); }
                    keep
                });
                if !message_ids.is_empty() {
                    structural_change = true;
                    self.broadcast_web(crate::web::protocol::WebEvent::MentionsRedacted {
                        message_ids,
                    });
                }
            }
            self.broadcast_web(event);
        }
        self.flush_server_history_pages();
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
                    .and_then(|db| crate::storage::query::get_unread_mentions(&db).ok())
                    .map(|rows| u32::try_from(rows.iter().filter(|row| !Self::mention_uses_server_history(&row.network, &row.buffer)).count()).unwrap_or(u32::MAX))
            })
            .unwrap_or(0)
            .saturating_add(u32::try_from(self.volatile_mentions.len()).unwrap_or(u32::MAX));
        let init = crate::web::snapshot::build_sync_init(
            &self.state,
            mention_count,
            &self.config.web.timestamp_format,
            self.config.emotes.web_enabled(),
            &self.config.statusbar,
        );
        if let crate::web::protocol::WebEvent::SyncInit {
            buffers,
            connections,
            mention_count,
            active_buffer_id,
            timestamp_format,
            emotes_enabled,
            typing,
            statusbar_items,
            statusbar_enabled,
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
            snap.typing = typing;
            snap.statusbar_items = statusbar_items;
            snap.statusbar_enabled = statusbar_enabled;
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
            if let Err(e) = handle.sender().send_notice(&send.target, &send.notice_text) {
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
    pub(crate) fn record_mention(&mut self, buffer_id: &str, msg: &crate::web::protocol::WireMessage) {
        if self.server_owns_buffer_history(buffer_id) {
            let (conn_id, target) = crate::web::snapshot::split_buffer_id(buffer_id);
            let scope = self.state.connections[conn_id].network_key().to_string();
            let identity = msg.msgid.as_deref().and_then(|id| self.state.redaction_key(buffer_id, id))
                .map(|key| self.state.redaction_registry.track(key));
            if identity.as_ref().is_some_and(|identity| identity.notice().is_some()) {
                return;
            }
            let id = self.volatile_mentions.back().map_or(-1, |(_, mention, _)| mention.id.saturating_sub(1));
            self.volatile_mentions.push_back((scope, crate::web::protocol::WireMention {
                id,
                source_message_id: msg.id,
                timestamp: msg.timestamp,
                buffer_id: buffer_id.to_string(),
                channel: self.state.buffers.get(buffer_id).map_or(target, |buf| buf.name.as_str()).to_string(),
                nick: msg.nick.clone().unwrap_or_default(),
                text: msg.text.clone(),
            }, identity));
            while self.volatile_mentions.len() > 1000 {
                self.volatile_mentions.pop_front();
            }
            return;
        }
        let Some(ref storage) = self.storage else {
            return;
        };
        let Ok(db) = storage.db.lock() else {
            return;
        };
        let (network, buffer) = crate::web::snapshot::split_buffer_id(buffer_id);
        let network = self.state.connections.get(network)
            .and_then(|connection| connection.network_scope.as_deref())
            .unwrap_or(network);
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

    /// Take a client's word for where it is.
    ///
    /// A tab is marked unconfirmed when a TUI-driven `ActiveBufferChanged`
    /// goes out, because whether it follows is a `localStorage` flag only the
    /// browser can see. Every command that names a `buffer_id` settles that
    /// question outright — the tab is telling us which composer the text came
    /// from — so the guess is replaced by the fact and the doubt cleared.
    ///
    /// Not cosmetic. `deferred_retry_text` withholds a refused message
    /// entirely from a session whose buffer it does not know, since it cannot
    /// tell which CONNECTION the retry would resolve against. Leaving a tab
    /// unconfirmed after it has just spoken means its author does not get
    /// their own text back, on the one path where getting it back matters.
    fn confirm_web_buffer(&mut self, session_id: &str, buffer_id: &str) {
        self.web_active_buffers
            .insert(session_id.to_string(), buffer_id.to_string());
        self.web_buffer_unconfirmed.remove(session_id);
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
            WebCommand::WebPush(request) => self.handle_webpush_request(&request, session_id),
            WebCommand::UploadFile { submission } => {
                if let Ok(mut guard) = submission.lock()
                    && let Some(request) = guard.take() {
                    self.start_web_upload(request);
                }
            }
            WebCommand::Presence { present } => self.update_presence_browser(session_id, present),
            WebCommand::WebConnect { initial_buffer_id } => {
                self.register_presence_browser(session_id);
                if let Some(buffer_id) = initial_buffer_id {
                    self.web_active_buffers
                        .insert(session_id.to_string(), buffer_id);
                    self.web_buffer_unconfirmed.remove(session_id);
                }
            }
            WebCommand::Typing { buffer_id, typing } => {
                // One source per session. Collapsing all sessions into one
                // predicate would let a freshly-opened, empty second tab retract
                // the typing that the terminal is doing right now.
                self.on_web_typing(session_id, &buffer_id, typing);
            }
            WebCommand::SendMessage { buffer_id, text } => {
                // The tab just named the buffer its text came from, which is
                // the answer to the question `web_buffer_unconfirmed` records
                // not knowing.
                self.confirm_web_buffer(session_id, &buffer_id);
                // Mark who is submitting for the duration of the command, so
                // a refusal (translation, E2E) returns the text to THIS
                // browser rather than the terminal's input line.
                self.submit_origin =
                    crate::app::translate::SubmitOrigin::Web(session_id.to_string());
                // Run the submit FIRST and report what it actually put on the
                // wire — a message the E2E gate refuses (or a dead connection
                // swallows) must leave the `done` we owe the peers outstanding.
                let sent_message = self.web_send_message(&buffer_id, &text);
                self.submit_origin = crate::app::translate::SubmitOrigin::Tui;
                self.on_typing_submit(
                    &crate::app::typing::TypingSource::Web(session_id.to_string()),
                    &buffer_id,
                    sent_message,
                );
            }
            WebCommand::SwitchBuffer { buffer_id } => {
                if self.state.web_history_buffers.get(session_id).is_some_and(|id| id != &buffer_id) {
                    self.release_web_history(session_id);
                }
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
                // The session just told us where it is.
                self.web_buffer_unconfirmed.remove(session_id);
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
            WebCommand::MarkRead { buffer_id, message_id, .. } => {
                self.web_mark_read(&buffer_id, message_id, session_id);
            }
            WebCommand::FetchMessages {
                buffer_id,
                limit,
                before,
                before_id,
                before_message_id,
            } => {
                if self.server_owns_buffer_history(&buffer_id) {
                    self.fetch_server_history_page(&buffer_id, limit, before, before_message_id, session_id);
                } else {
                    self.web_fetch_messages(&buffer_id, limit, before, before_id, session_id);
                }
            }
            WebCommand::CollapseBacklog { buffer_id } => {
                if self.state.web_history_buffers.get(session_id) == Some(&buffer_id) {
                    self.release_web_history(session_id);
                }
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
                // The same origin scope as `SendMessage`, for the same
                // reason. The web composer dispatches every `/`-prefixed line
                // as `RunCommand`, and `/msg`, `/query <peer> <text>` and
                // `/me` all reach the outgoing translation gate — so a
                // refusal must return the text to THIS browser, not to the
                // terminal's input line where its author cannot see it.
                self.confirm_web_buffer(session_id, &buffer_id);
                self.submit_origin =
                    crate::app::translate::SubmitOrigin::Web(session_id.to_string());
                let sent_message = self.web_run_command(&buffer_id, &text);
                self.submit_origin = crate::app::translate::SubmitOrigin::Tui;
                self.on_typing_submit(
                    &crate::app::typing::TypingSource::Web(session_id.to_string()),
                    &buffer_id,
                    sent_message,
                );
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
                self.remove_presence_browser(session_id);
                self.release_web_history(session_id);
                self.web_active_buffers.remove(session_id);
                self.web_buffer_unconfirmed.remove(session_id);
                self.on_web_session_gone(session_id);
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
                    sasl_key_path: cmd.sasl_key_path,
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
        // config.servers changed in-memory regardless of save outcome; keep the
        // keyring's legacy-adoption isolation count in step — see the helper.
        self.refresh_e2e_configured_networks();
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
    /// Returns whether a real message reached the wire — see `App::handle_submit`.
    fn web_run_command(&mut self, buffer_id: &str, text: &str) -> bool {
        let prior = self.state.active_buffer_id.clone();
        self.set_active_buffer_silent(buffer_id);
        let sent_message = self.handle_submit(text);
        if let Some(id) = prior {
            self.set_active_buffer_silent(&id);
        } else {
            self.state.active_buffer_id = None;
        }
        sent_message
    }

    fn set_active_buffer_silent(&mut self, buffer_id: &str) {
        if !self.state.buffers.contains_key(buffer_id) {
            return;
        }
        self.state.active_buffer_id = Some(buffer_id.to_string());
        self.state.clear_activity(buffer_id);
    }

    /// Send a message from a web client to IRC. Returns whether it reached the wire.
    fn web_send_message(&mut self, buffer_id: &str, text: &str) -> bool {
        self.web_run_command(buffer_id, text)
    }

    /// Mark a buffer as read from a web client.
    fn web_mark_read(&mut self, buffer_id: &str, message_id: Option<u64>, session_id: &str) {
        if self.state.uses_read_markers(buffer_id) {
            if let Some(message_id) = message_id {
                self.mark_visible_message_read(buffer_id, message_id);
            } else {
                self.broadcast_web(crate::web::protocol::WebEvent::Error {
                    message: "This browser tab uses an older client. Reload the page to synchronize read status. Your unread messages have been preserved.".into(),
                    session_id: Some(session_id.to_string()),
                });
            }
            return;
        }
        if self.state.buffer_uses_server_history(buffer_id)
            && let Some(message_id) = message_id
        {
            self.state.clear_visible_read_rows(buffer_id, message_id);
            self.drain_pending_web_events();
            return;
        }
        self.state.clear_visible_activity(buffer_id);
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
            .map_or_else(|| conn_id.to_string(), |c| c.network_key().to_string());
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
        let mut wire = self.storage.as_ref().and_then(|storage| {
            let db = storage.db.lock().ok()?;
            crate::storage::query::get_unread_mentions(&db).ok()
        }).unwrap_or_default().into_iter()
            .filter(|mention| !Self::mention_uses_server_history(&mention.network, &mention.buffer))
            .map(|mention| crate::web::protocol::WireMention {
                id: mention.id,
                source_message_id: 0,
                timestamp: mention.timestamp,
                buffer_id: self.mention_target(&mention.network)
                    .map_or_else(String::new, |(id, _)| crate::state::buffer::make_buffer_id(&id, &mention.buffer)),
                channel: mention.channel,
                nick: mention.nick,
                text: mention.text,
            }).collect::<Vec<_>>();
        wire.extend(self.volatile_mentions.iter().map(|(scope, mention, _)| {
            let mut mention = mention.clone();
            let (_, target) = crate::web::snapshot::split_buffer_id(&mention.buffer_id);
            mention.buffer_id = self.mention_target(scope)
                .map_or_else(String::new, |(id, _)| crate::state::buffer::make_buffer_id(&id, target));
            mention
        }));
        wire.sort_by_key(|mention| std::cmp::Reverse(mention.timestamp));
        self.broadcast_web(crate::web::protocol::WebEvent::MentionsList {
            through: self.state.message_counter,
            mentions: wire,
            session_id: Some(session_id.to_string()),
        });
    }
}

#[cfg(test)]
mod activity_read_tests {
    use crate::state::buffer::{ActivityLevel, Buffer, BufferType};

    #[tokio::test]
    async fn legacy_web_read_requires_reload_and_preserves_unread() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let config = toml::from_str("label='Bouncer'\naddress='bnc.example.org'\nport=6697\ntls=true\nchannels=[]\nbouncer_network_id='42'").unwrap();
        app.setup_connection("account", &config);
        app.state.connections.get_mut("account").unwrap().enabled_caps.insert("draft/read-marker".into());
        app.state.add_buffer_with_focus(Buffer::empty("account", BufferType::Query, "Peer"), false);
        let message = crate::state::events::tests::make_test_message(&mut app.state, "unread");
        app.state.add_transient_message_with_activity("account/peer", message, ActivityLevel::Activity);
        let mut receiver = app.web_broadcaster.subscribe();
        let command = serde_json::from_str(r#"{"type":"MarkRead","buffer_id":"account/peer","up_to":9999999999999}"#).unwrap();
        app.handle_web_command(command, "old-browser");
        assert_eq!(app.state.buffers["account/peer"].unread_count, 1);
        assert!(app.read_markers.is_empty());
        assert!(matches!(receiver.try_recv().unwrap(), crate::web::protocol::WebEvent::Error { message, session_id } if session_id.as_deref() == Some("old-browser") && message.contains("Reload")));
    }

    #[tokio::test]
    async fn capability_loss_preserves_active_web_arrivals_until_reported_visible() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let config = toml::from_str("label='Bouncer'\naddress='bnc.example.org'\nport=6697\ntls=true\nchannels=[]\nbouncer_network_id='42'").unwrap();
        app.setup_connection("account", &config);
        app.state.connections.get_mut("account").unwrap().enabled_caps.insert("draft/read-marker".into());
        app.state.add_buffer_with_focus(Buffer::empty("account", BufferType::Query, "Peer"), false);
        app.state.set_active_buffer("account/peer");
        crate::irc::events::handle_cap_del(&mut app.state, "account", Some("draft/read-marker"), None);
        let first = crate::state::events::tests::make_test_message(&mut app.state, "first unseen");
        let id = first.id;
        app.state.add_transient_message_with_activity("account/peer", first, ActivityLevel::Activity);
        let second = crate::state::events::tests::make_test_message(&mut app.state, "later unseen");
        app.state.add_transient_message_with_activity("account/peer", second, ActivityLevel::Activity);
        app.set_active_buffer_silent("account/peer");
        assert_eq!(app.state.buffers["account/peer"].unread_count, 2);
        app.web_mark_read("account/peer", Some(id), "browser");
        assert_eq!(app.state.buffers["account/peer"].unread_count, 1);
    }

    #[tokio::test]
    async fn bouncer_without_marker_capability_keeps_local_web_read_clearing() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let config = toml::from_str("label='Bouncer'\naddress='bnc.example.org'\nport=6697\ntls=true\nchannels=[]\nbouncer_network_id='42'").unwrap();
        app.setup_connection("account", &config);
        app.state.add_buffer_with_focus(Buffer::empty("account", BufferType::Query, "Peer"), false);
        let message = crate::state::events::tests::make_test_message(&mut app.state, "unread");
        app.state.add_transient_message_with_activity("account/peer", message, ActivityLevel::Activity);
        assert_eq!(app.state.buffers["account/peer"].unread_count, 1);
        app.web_mark_read("account/peer", None, "browser");
        assert_eq!(app.state.buffers["account/peer"].unread_count, 0);
        assert_eq!(app.state.buffers["account/peer"].activity, ActivityLevel::None);
    }

    #[test]
    fn web_reads_and_silent_switches_remove_shortcut_candidates() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        for name in ["#current", "#old", "#new"] {
            app.state.add_buffer(Buffer::for_test("net", BufferType::Channel, name));
        }
        app.state.set_active_buffer("net/#current");
        app.state.set_activity("net/#old", ActivityLevel::Activity);
        app.state.set_activity("net/#new", ActivityLevel::Activity);
        app.web_mark_read("net/#old", None, "browser");
        assert_eq!(app.state.next_activity_buffer().as_deref(), Some("net/#new"));
        app.state.set_activity("net/#old", ActivityLevel::Activity);
        assert_eq!(app.state.next_activity_buffer().as_deref(), Some("net/#new"));
        app.set_active_buffer_silent("net/#new");
        assert_eq!(app.state.next_activity_buffer().as_deref(), Some("net/#old"));
        app.web_mark_read("net/#old", None, "browser");
        assert!(app.state.next_activity_buffer().is_none());
    }
}

#[cfg(test)]
mod network_icon_configuration_tests {
    #[tokio::test]
    async fn icons_are_provisioned_with_message_previews_disabled() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        assert!(!app.config.web.image_previews);
        app.prepare_web_image_extractors();
        assert!(app.state.web_icon_extractor.is_some());
        assert!(app.state.web_preview_extractor.is_none());
        app.config.web.image_previews = true;
        app.prepare_web_image_extractors();
        assert!(app.state.web_icon_extractor.is_some());
        assert!(app.state.web_preview_extractor.is_some());
        app.stop_web_server();
        assert!(app.state.web_icon_extractor.is_none());
        assert!(app.state.web_preview_extractor.is_none());
    }
}

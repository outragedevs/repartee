use super::App;

impl App {
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

        let sessions = std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::web::auth::SessionStore::with_hours(self.config.web.session_hours),
        ));
        let limiter = std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::web::auth::RateLimiter::new(),
        ));
        self.web_sessions = Some(std::sync::Arc::clone(&sessions));
        self.web_rate_limiter = Some(std::sync::Arc::clone(&limiter));

        let snapshot = std::sync::Arc::new(std::sync::RwLock::new(
            crate::web::server::WebStateSnapshot {
                buffers: Vec::new(),
                connections: Vec::new(),
                mention_count: 0,
                active_buffer_id: None,
                timestamp_format: self.config.web.timestamp_format.clone(),
            },
        ));
        self.web_state_snapshot = Some(std::sync::Arc::clone(&snapshot));

        let handle = std::sync::Arc::new(crate::web::server::AppHandle {
            broadcaster: std::sync::Arc::clone(&self.web_broadcaster),
            web_cmd_tx: self.web_cmd_tx.clone(),
            password: self.config.web.password.clone(),
            session_store: sessions,
            rate_limiter: limiter,
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
        for event in events {
            match &event {
                crate::web::protocol::WebEvent::BufferCreated { buffer } => {
                    tracing::debug!(buffer_id = %buffer.id, "broadcasting BufferCreated");
                }
                crate::web::protocol::WebEvent::BufferClosed { buffer_id } => {
                    tracing::debug!(%buffer_id, "broadcasting BufferClosed");
                }
                crate::web::protocol::WebEvent::ActiveBufferChanged { buffer_id } => {
                    tracing::debug!(%buffer_id, "broadcasting ActiveBufferChanged");
                    if let Some(shell_id) = self
                        .shell_mgr
                        .session_id_for_buffer(buffer_id)
                        .map(ToString::to_string)
                    {
                        self.force_broadcast_shell_screen(&shell_id);
                    }
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
    pub(crate) fn handle_web_command(&mut self, cmd: crate::web::protocol::WebCommand, session_id: &str) {
        use crate::web::protocol::WebCommand;
        use crate::web::snapshot;

        match cmd {
            WebCommand::SendMessage { buffer_id, text } => {
                self.web_send_message(&buffer_id, &text);
            }
            WebCommand::SwitchBuffer { buffer_id } => {
                self.state.set_active_buffer(&buffer_id);
                self.update_shell_input_state();
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
            } => {
                self.web_fetch_messages(&buffer_id, limit, before, session_id);
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
            WebCommand::ShellInput { buffer_id: _, data } => {
                let web_id = format!("web-{session_id}");
                if let Ok(bytes) = base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    &data,
                ) {
                    self.shell_mgr.write_web(&web_id, &bytes);
                }
            }
            WebCommand::WebDisconnect => {
                self.shell_mgr.close_web_by_session(session_id);
            }
            WebCommand::ShellResize {
                buffer_id: _,
                cols,
                rows,
            } => {
                let web_id = format!("web-{session_id}");
                if self.shell_mgr.has_web_session(&web_id) {
                    self.shell_mgr.resize_web(&web_id, cols, rows);
                } else if let Err(e) = self.shell_mgr.open_web(session_id, cols, rows) {
                    tracing::warn!("failed to open web shell: {e}");
                    return;
                }
                self.force_broadcast_web_shell_screen(&web_id);
            }
        }
    }

    /// Execute a command from a web client in the context of a buffer.
    fn web_run_command(&mut self, buffer_id: &str, text: &str) {
        let prior = self.state.active_buffer_id.clone();
        self.state.set_active_buffer(buffer_id);
        self.handle_submit(text);
        if let Some(id) = prior {
            self.state.set_active_buffer(&id);
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
    fn web_fetch_messages(&self, buffer_id: &str, limit: u32, before: Option<i64>, session_id: &str) {
        if buffer_id == Self::MENTIONS_BUFFER_ID {
            if let Some(buf) = self.state.buffers.get(buffer_id) {
                let capped = limit.min(500) as usize;
                let msgs: Vec<_> = buf
                    .messages
                    .iter()
                    .rev()
                    .take(capped)
                    .rev()
                    .map(crate::web::snapshot::message_to_wire)
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
            let msgs: Vec<_> = buf
                .messages
                .iter()
                .rev()
                .take(capped)
                .rev()
                .map(crate::web::snapshot::message_to_wire)
                .collect();
            if !msgs.is_empty() {
                let has_more = buf.messages.len() > capped;
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
        let Some(ref storage) = self.storage else {
            tracing::warn!("web FetchMessages: storage not available");
            return;
        };
        let Ok(db) = storage.db.lock() else {
            tracing::warn!("web FetchMessages: failed to lock db");
            return;
        };
        let capped_limit = limit.min(500) as usize;
        let (conn_id, buffer) = crate::web::snapshot::split_buffer_id(buffer_id);
        let network = self.state.connections.get(conn_id)
            .map_or_else(|| conn_id.to_string(), |c| c.label.clone());
        let messages = crate::storage::query::get_messages(
            &db,
            &network,
            buffer,
            before,
            capped_limit + 1,
            storage.encrypt,
            None,
        );
        match messages {
            Ok(mut msgs) => {
                let has_more = msgs.len() > capped_limit;
                msgs.truncate(capped_limit);
                tracing::debug!(
                    %buffer_id, count = msgs.len(), %has_more,
                    "web FetchMessages: sending {} messages", msgs.len()
                );
                let wire: Vec<_> = msgs
                    .iter()
                    .map(crate::web::snapshot::stored_to_wire)
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

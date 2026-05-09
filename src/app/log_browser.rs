//! Log-browser-only methods on `App`. Only invoked when
//! `log_browser_mode == true`. Keeps the chat-mode call sites in
//! `app/mod.rs` free of log-mode branches.

use std::collections::{HashMap, HashSet, VecDeque};

use chrono::Utc;
use color_eyre::eyre::{Result, eyre};

use crate::config;
use crate::state::buffer::{ActivityLevel, Buffer, BufferType, make_buffer_id};
use crate::state::connection::{Connection, ConnectionStatus};

use super::App;

impl App {
    /// Connection ID prefix used for log-mode pseudo-networks. Distinct
    /// from any real network identifier (which the user picks in their
    /// config TOML) so live and log buffers never collide on
    /// `make_buffer_id`.
    pub const LOG_CONN_PREFIX: &'static str = "_log_";

    /// Build an `App` instance configured for the read-only log browser.
    /// No IRC, no scripts, no web server, no socket listener — just a
    /// SQLite connection backing a sidebar built from the message log.
    pub fn new_log_browser() -> Result<Self> {
        let mut app = Self::new()?;
        app.log_browser_mode = true;

        let log_db = crate::storage::load_log_db(&app.config.logging)
            .map_err(|e| eyre!("{e}"))?;
        app.log_db = Some(log_db);

        // Wipe state populated by the chat-mode `App::new` (default
        // Status buffer, any state derived from `[servers]`) so
        // `build_log_catalog` sees a clean sidebar.
        app.state.connections.clear();
        app.state.buffers.clear();
        app.state.active_buffer_id = None;
        app.state.previous_buffer_id = None;

        app.build_log_catalog()?;
        Ok(app)
    }

    /// Populate `state.connections` and `state.buffers` from the distinct
    /// `(network, buffer)` pairs in the log database. Each network
    /// becomes a synthetic `Connection`, each buffer a `BufferType::Log`
    /// placeholder with empty `messages` (filled lazily when first
    /// activated).
    pub fn build_log_catalog(&mut self) -> Result<()> {
        let log_db = self
            .log_db
            .as_ref()
            .ok_or_else(|| eyre!("log catalog requires log_db"))?;
        let networks = {
            let db = log_db.db.lock().expect("log db poisoned");
            crate::storage::query::list_networks(&db)
                .map_err(|e| eyre!("list_networks: {e}"))?
        };

        // Look up friendly labels from the user's chat config when present
        // ("libera" -> "Libera Chat" if they configured a server with
        // that id). Falls back to the network id verbatim.
        let label_for = |net: &str| -> String {
            self.config
                .servers
                .get(net)
                .map_or_else(|| net.to_string(), |c| c.label.clone())
        };

        let mut first_buffer_id: Option<String> = None;

        for net in &networks {
            let conn_id = format!("{}{net}", Self::LOG_CONN_PREFIX);
            self.state.add_connection(Connection {
                id: conn_id.clone(),
                label: label_for(net),
                status: ConnectionStatus::Connected,
                nick: String::new(),
                user_modes: String::new(),
                isupport: HashMap::new(),
                isupport_parsed: crate::irc::isupport::Isupport::new(),
                error: None,
                lag: None,
                lag_pending: false,
                reconnect_attempts: 0,
                reconnect_delay_secs: 0,
                next_reconnect: None,
                should_reconnect: false,
                joined_channels: Vec::new(),
                origin_config: config::ServerConfig {
                    label: String::new(),
                    address: String::new(),
                    port: 0,
                    tls: false,
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
                    auto_reconnect: Some(false),
                    reconnect_delay: None,
                    reconnect_max_retries: None,
                    autosendcmd: None,
                    sasl_mechanism: None,
                    client_cert_path: None,
                },
                local_ip: None,
                enabled_caps: HashSet::new(),
                who_token_counter: 0,
                silent_who_channels: HashSet::new(),
                silent_banlist_channels: HashSet::new(),
            });

            let buffers = {
                let db = log_db.db.lock().expect("log db poisoned");
                crate::storage::query::list_buffers_for_network(&db, net)
                    .map_err(|e| eyre!("list_buffers_for_network: {e}"))?
            };
            for buf in buffers {
                let buf_id = make_buffer_id(&conn_id, &buf);
                if first_buffer_id.is_none() {
                    first_buffer_id = Some(buf_id.clone());
                }
                self.state.add_buffer(Buffer {
                    id: buf_id,
                    connection_id: conn_id.clone(),
                    buffer_type: BufferType::Log,
                    name: buf.clone(),
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
                });
            }
        }

        if let Some(id) = first_buffer_id {
            self.state.set_active_buffer(&id);
        }
        Ok(())
    }

    /// Load the most recent messages for `buffer_id` into its in-memory
    /// `messages` deque. Idempotent: a buffer that already has messages
    /// is skipped, so this can be called every tick from the main loop
    /// while a log buffer is active.
    ///
    /// Also caches `(line_count, oldest_ts, newest_ts)` on the buffer so
    /// the topic-bar render doesn't requery on every frame.
    pub fn load_initial_messages(&mut self, buffer_id: &str) {
        const INITIAL_LIMIT: usize = 200;
        let already_loaded = self
            .state
            .buffers
            .get(buffer_id)
            .is_some_and(|b| !b.messages.is_empty());
        if already_loaded {
            return;
        }
        let Some((net, buf)) = self.split_log_buffer_id(buffer_id) else {
            return;
        };

        // Cache stats first, even when there are zero messages — the
        // topic bar should show 0/range= empty for an empty buffer.
        let stats_result = self
            .log_db
            .as_ref()
            .and_then(|d| d.db.lock().ok().map(|db| crate::storage::query::buffer_stats(&db, &net, &buf)));
        if let Some(Ok(Some((count, oldest, newest)))) = stats_result
            && let Some(buffer) = self.state.buffers.get_mut(buffer_id)
        {
            buffer.log_total_lines = Some(count);
            buffer.log_oldest_ts = Some(oldest);
            buffer.log_newest_ts = Some(newest);
        }

        let Some(log_db) = &self.log_db else { return };
        let rows_result = {
            let Ok(db) = log_db.db.lock() else { return };
            crate::storage::query::get_messages(
                &db,
                &net,
                &buf,
                None,
                INITIAL_LIMIT,
                log_db.crypto_key.is_some(),
                log_db.crypto_key.as_ref(),
            )
        };
        match rows_result {
            Ok(rows) => {
                let exhausted = rows.len() < INITIAL_LIMIT;
                if let Some(buffer) = self.state.buffers.get_mut(buffer_id) {
                    for stored in rows {
                        buffer.messages.push_back(stored_to_message(&stored));
                    }
                    buffer.history_exhausted = exhausted;
                }
            }
            Err(e) => tracing::warn!(%buffer_id, "log load_initial failed: {e}"),
        }
    }

    /// Prepend up to `PAGE_LIMIT` messages older than the oldest currently
    /// loaded message. Sets `history_exhausted` when fewer rows are
    /// returned than requested. No-op when the buffer is already
    /// exhausted; falls back to `load_initial_messages` when called on
    /// an empty buffer.
    pub fn load_older_messages(&mut self, buffer_id: &str) {
        const PAGE_LIMIT: usize = 200;
        let Some(buffer) = self.state.buffers.get(buffer_id) else {
            return;
        };
        if buffer.history_exhausted {
            return;
        }
        let Some(oldest_msg) = buffer.messages.front() else {
            self.load_initial_messages(buffer_id);
            return;
        };
        let oldest_ts = oldest_msg.timestamp.timestamp();
        let Some((net, buf)) = self.split_log_buffer_id(buffer_id) else {
            return;
        };
        let Some(log_db) = &self.log_db else { return };
        let rows_result = {
            let Ok(db) = log_db.db.lock() else { return };
            crate::storage::query::get_messages(
                &db,
                &net,
                &buf,
                Some(oldest_ts),
                PAGE_LIMIT,
                log_db.crypto_key.is_some(),
                log_db.crypto_key.as_ref(),
            )
        };
        match rows_result {
            Ok(rows) => {
                let exhausted = rows.len() < PAGE_LIMIT;
                if let Some(buffer) = self.state.buffers.get_mut(buffer_id) {
                    // get_messages returns chronological ascending — push
                    // them onto the front in reverse order so the buffer
                    // stays sorted.
                    for stored in rows.into_iter().rev() {
                        buffer.messages.push_front(stored_to_message(&stored));
                    }
                    if exhausted {
                        buffer.history_exhausted = true;
                    }
                }
            }
            Err(e) => tracing::warn!(%buffer_id, "log load_older failed: {e}"),
        }
    }

    /// Split a `BufferType::Log` buffer id (`_log_<network>/<buffer>`)
    /// into `(network, buffer_name)`. Returns `None` for non-log
    /// buffers — the connection_id prefix is the discriminator.
    pub(crate) fn split_log_buffer_id(&self, buffer_id: &str) -> Option<(String, String)> {
        let buffer = self.state.buffers.get(buffer_id)?;
        let net = buffer
            .connection_id
            .strip_prefix(Self::LOG_CONN_PREFIX)?
            .to_string();
        Some((net, buffer.name.clone()))
    }

    /// Trigger `load_older_messages` for the active log buffer if the
    /// user has scrolled close enough to the top to want more history.
    /// Called from the scroll-up code paths (PageUp, mouse wheel) so the
    /// log paginates incrementally without an explicit "fetch more"
    /// gesture.
    ///
    /// Threshold: trigger when `scroll_offset` is within 50 lines of the
    /// loaded top — i.e. the next handful of PageUps would otherwise hit
    /// the boundary. Idempotent: `load_older_messages` already early-
    /// returns when `history_exhausted` is set.
    pub(crate) fn maybe_paginate_log_buffer(&mut self) {
        let Some(active_id) = self.state.active_buffer_id.clone() else {
            return;
        };
        let Some(buf) = self.state.buffers.get(&active_id) else {
            return;
        };
        if buf.buffer_type != BufferType::Log || buf.history_exhausted {
            return;
        }
        let messages_len = buf.messages.len();
        if self.scroll_offset.saturating_add(50) >= messages_len {
            self.load_older_messages(&active_id);
        }
    }
}

/// Convert a row read from `messages` into the in-memory `Message`
/// struct used by the buffer pipeline. Mirrors `app::backlog`'s
/// conversion but lives here because the log browser doesn't share
/// `App.storage` (which is `None` in log mode).
fn stored_to_message(stored: &crate::storage::StoredMessage) -> crate::state::buffer::Message {
    use crate::state::buffer::{Message, MessageType};
    let ts = chrono::DateTime::<Utc>::from_timestamp(stored.timestamp, 0).unwrap_or_else(Utc::now);
    let msg_type = match stored.msg_type.as_str() {
        "action" => MessageType::Action,
        "notice" => MessageType::Notice,
        "event" => MessageType::Event,
        _ => MessageType::Message,
    };
    Message {
        id: 0,
        timestamp: ts,
        message_type: msg_type,
        nick: stored.nick.clone(),
        nick_mode: None,
        text: stored.text.clone(),
        highlight: stored.highlight,
        event_key: stored.event_key.clone(),
        event_params: None,
        log_msg_id: None,
        log_ref_id: None,
        tags: None,
    }
}

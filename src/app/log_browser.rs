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
    /// No IRC, no scripts, no web server, no socket listener, no
    /// `Storage::init` (which would open the DB writeable and run
    /// retention purge) — just a read-only `SQLite` connection backing
    /// a sidebar built from the message log.
    pub fn new_log_browser() -> Result<Self> {
        let mut app = Self::new_with_mode(true)?;
        // `new_with_mode(true)` already sets `log_browser_mode = true`.

        let log_db = crate::storage::load_log_db(&app.config.logging)
            .map_err(|e| eyre!("{e}"))?;
        app.log_db = Some(log_db);

        // `new_with_mode(true)` does not create any default buffers,
        // but `state.connections` may still contain whatever the
        // chat-mode default-init leaves behind. Clear before catalog
        // build so the sidebar reflects the SQLite history only.
        app.state.connections.clear();
        app.state.buffers.clear();
        app.state.active_buffer_id = None;
        app.state.previous_buffer_id = None;

        app.build_log_catalog()?;
        // Eager-load the initial buffer's history so the user sees logs
        // even if they type a slash command before the first 1-Hz tick.
        // The tick still calls this — `log_initial_loaded` makes it a
        // no-op the second time around.
        if let Some(active_id) = app.state.active_buffer_id.clone() {
            app.load_initial_messages(&active_id);
        }
        Ok(app)
    }

    /// Populate `state.connections` and `state.buffers` from the distinct
    /// `(network, buffer)` pairs in the log database. Each network
    /// becomes a synthetic `Connection`, each buffer a `BufferType::Log`
    /// placeholder with empty `messages` (filled lazily when first
    /// activated).
    #[expect(
        clippy::too_many_lines,
        reason = "flat sidebar bootstrap; splitting helps no one"
    )]
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
            let net_label = label_for(net);
            self.state.add_connection(Connection {
                id: conn_id.clone(),
                label: net_label.clone(),
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

            // Server-type header buffer for the pseudo-network. Buffer-list
            // rendering treats `BufferType::Server` specially — it shows
            // the connection label and applies the `item_server` theme
            // format, which is what makes the "libera" / "ircnet" / "oftc"
            // headers visually pop in the sidebar instead of blending
            // into the channel list. Mirrors the Shell synthetic
            // connection pattern in `app::shell::ensure_shell_connection`.
            let header_id = make_buffer_id(&conn_id, &net_label);
            self.state.add_buffer(Buffer {
                id: header_id,
                connection_id: conn_id.clone(),
                buffer_type: BufferType::Server,
                name: net_label.clone(),
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
                    log_initial_loaded: false,
                });
            }
        }

        if let Some(id) = first_buffer_id {
            self.state.set_active_buffer(&id);
        }
        Ok(())
    }

    /// Load the most recent messages for `buffer_id` into its in-memory
    /// `messages` deque. Idempotent: gated on the explicit
    /// `log_initial_loaded` flag (NOT on `messages.is_empty()`) — the
    /// buffer may already contain `add_local_event` rows from `/help`,
    /// `/search` errors, or the slash-command rejection hint, which
    /// would otherwise hide the real history forever.
    ///
    /// Also caches `(line_count, oldest_ts, newest_ts)` on the buffer so
    /// the topic-bar render doesn't requery on every frame.
    pub fn load_initial_messages(&mut self, buffer_id: &str) {
        const INITIAL_LIMIT: usize = 200;
        let already_loaded = self
            .state
            .buffers
            .get(buffer_id)
            .is_some_and(|b| b.log_initial_loaded);
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
            crate::storage::query::get_messages_paginated(
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
                // Build messages first (mutable state borrow), then
                // push into the buffer (separate mutable borrow).
                let messages = rows_to_buffer_messages(&mut self.state, &rows, None);
                if let Some(buffer) = self.state.buffers.get_mut(buffer_id) {
                    if buffer.messages.is_empty() {
                        for msg in messages {
                            buffer.messages.push_back(msg);
                        }
                    } else {
                        // Buffer already contains local events — `/help`
                        // output, `/search` errors, or the slash-only
                        // hint were emitted before this first lazy
                        // load fired. Prepend the actual log so
                        // history reads chronologically and the
                        // command output stays at the bottom.
                        for msg in messages.into_iter().rev() {
                            buffer.messages.push_front(msg);
                        }
                    }
                    buffer.history_exhausted = exhausted;
                    buffer.log_initial_loaded = true;
                }
            }
            Err(e) => {
                tracing::warn!(%buffer_id, "log load_initial failed: {e}");
                // Mark loaded anyway — retrying on every tick would
                // hammer the DB with the same broken query.
                if let Some(buffer) = self.state.buffers.get_mut(buffer_id) {
                    buffer.log_initial_loaded = true;
                }
            }
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
        // Skip synthetic rows — day separators (`event_key =
        // "date_separator"`) carry `log_msg_id: None` because they
        // aren't real DB rows. Using one as the pagination anchor
        // would collapse the cursor to `None`, fetching the *newest*
        // page again and producing duplicate prepends. Walk forward
        // until we find a row that came from `stored_to_message`.
        let Some(oldest_msg) = buffer
            .messages
            .iter()
            .find(|m| m.log_msg_id.is_some())
        else {
            self.load_initial_messages(buffer_id);
            return;
        };
        let oldest_ts = oldest_msg.timestamp.timestamp();
        // `log_msg_id` is the SQLite row id (set in `stored_to_message`).
        // Composite cursor `(timestamp, id)` lets us page through rows
        // that share a second without losing any.
        let Some(oldest_id) = oldest_msg
            .log_msg_id
            .as_ref()
            .and_then(|s| s.parse::<i64>().ok())
        else {
            // Defensive: every `stored_to_message` row sets a numeric
            // `log_msg_id`. If parsing fails we'd fetch the newest page
            // again, so bail.
            return;
        };
        let cursor = Some((oldest_ts, oldest_id));
        let Some((net, buf)) = self.split_log_buffer_id(buffer_id) else {
            return;
        };
        let Some(log_db) = &self.log_db else { return };
        let rows_result = {
            let Ok(db) = log_db.db.lock() else { return };
            crate::storage::query::get_messages_paginated(
                &db,
                &net,
                &buf,
                cursor,
                PAGE_LIMIT,
                log_db.crypto_key.is_some(),
                log_db.crypto_key.as_ref(),
            )
        };
        match rows_result {
            Ok(rows) => {
                let exhausted = rows.len() < PAGE_LIMIT;
                // If the newest row in this batch shares its local date
                // with the existing buffer head separator, drop that
                // separator before prepending. The new batch will emit
                // its own separator for that date in the correct
                // position (above the new oldest message), avoiding the
                // earlier mistake of leaving messages above their
                // separator.
                let last_new_date = rows.last().map(|r| {
                    use chrono::TimeZone as _;
                    let ts = chrono::DateTime::<Utc>::from_timestamp(r.timestamp, 0)
                        .unwrap_or_else(Utc::now);
                    chrono::Local
                        .from_utc_datetime(&ts.naive_utc())
                        .date_naive()
                });
                if let Some(date) = last_new_date
                    && let Some(buffer) = self.state.buffers.get_mut(buffer_id)
                    && let Some(first) = buffer.messages.front()
                    && first.event_key.as_deref() == Some("date_separator")
                {
                    use chrono::TimeZone as _;
                    let first_date = chrono::Local
                        .from_utc_datetime(&first.timestamp.naive_utc())
                        .date_naive();
                    if first_date == date {
                        buffer.messages.pop_front();
                    }
                }
                let messages = rows_to_buffer_messages(&mut self.state, &rows, None);
                if let Some(buffer) = self.state.buffers.get_mut(buffer_id) {
                    for msg in messages.into_iter().rev() {
                        buffer.messages.push_front(msg);
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
    /// into `(network, buffer_name)`. Returns `None` for any buffer
    /// that isn't a log — the pseudo-network header rendered as a
    /// `BufferType::Server` row shares the `_log_` `connection_id`
    /// prefix but isn't a real log buffer; treating it as one would
    /// fire `/search` against a buffer name that's actually the
    /// network's friendly label and return zero hits, plus tip the
    /// status line into "0/0 lines" instead of leaving it alone.
    pub(crate) fn split_log_buffer_id(&self, buffer_id: &str) -> Option<(String, String)> {
        let buffer = self.state.buffers.get(buffer_id)?;
        if buffer.buffer_type != BufferType::Log {
            return None;
        }
        let net = buffer
            .connection_id
            .strip_prefix(Self::LOG_CONN_PREFIX)?
            .to_string();
        Some((net, buffer.name.clone()))
    }

    /// Trigger `load_older_messages` for the active log buffer if the
    /// user has scrolled close enough to the top to want more history.
    /// Called from the scroll-up code paths (`PageUp`, mouse wheel) so
    /// the log paginates incrementally without an explicit "fetch more"
    /// gesture.
    ///
    /// Threshold: trigger when `scroll_offset` is within 50 lines of
    /// the loaded top — i.e. the next handful of `PageUp`s would
    /// otherwise hit the boundary. Idempotent: `load_older_messages`
    /// already early-returns when `history_exhausted` is set.
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
///
/// `id` is taken from the live `state.message_counter` — never `0`,
/// which would collide with web-snapshot / session diffing on the
/// next message. `log_msg_id` carries the `SQLite` row id as a
/// decimal string so `load_older_messages` can build a `(timestamp,
/// id)` composite cursor without losing same-second rows.
/// Convert a chronological slice of stored rows into in-memory
/// `Message`s with day-change separators interleaved. Mirrors the UX
/// of `app::backlog::build_backlog_messages`: when consecutive rows
/// fall on different local-time dates, a `MessageType::Event` row
/// styled like irssi/weechat day separators (`─── Mon, 12 Aug 2024
/// ───`) is inserted between them. Ids come from `state.message_counter`.
///
/// The caller is responsible for stripping any leading separator from
/// the existing buffer that would duplicate the first separator
/// produced here — see `load_older_messages` for the prepend dance.
fn rows_to_buffer_messages(
    state: &mut crate::state::AppState,
    rows: &[crate::storage::StoredMessage],
    _unused: Option<chrono::NaiveDate>,
) -> Vec<crate::state::buffer::Message> {
    use crate::state::buffer::{Message, MessageType};
    use chrono::{Local, TimeZone};
    let mut out: Vec<Message> = Vec::with_capacity(rows.len() + 4);
    let mut last_date: Option<chrono::NaiveDate> = None;
    for stored in rows {
        let ts = chrono::DateTime::<Utc>::from_timestamp(stored.timestamp, 0)
            .unwrap_or_else(Utc::now);
        let local_date = Local.from_utc_datetime(&ts.naive_utc()).date_naive();
        if last_date.is_none_or(|d| d != local_date) {
            let sep_text = crate::app::backlog::format_date_separator(local_date);
            out.push(Message {
                id: state.next_message_id(),
                timestamp: ts,
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: sep_text.clone(),
                highlight: false,
                event_key: Some("date_separator".to_string()),
                event_params: Some(vec![sep_text]),
                log_msg_id: None,
                log_ref_id: None,
                tags: None,
                shortenings: Vec::new(),
            });
        }
        last_date = Some(local_date);
        out.push(stored_to_message(state, stored));
    }
    out
}

fn stored_to_message(
    state: &mut crate::state::AppState,
    stored: &crate::storage::StoredMessage,
) -> crate::state::buffer::Message {
    use crate::state::buffer::{Message, MessageType};
    let ts = chrono::DateTime::<Utc>::from_timestamp(stored.timestamp, 0).unwrap_or_else(Utc::now);
    let msg_type = match stored.msg_type.as_str() {
        "action" => MessageType::Action,
        "notice" => MessageType::Notice,
        "event" => MessageType::Event,
        "mention_log" => MessageType::MentionLog,
        _ => MessageType::Message,
    };
    // Suppress `event_key` for log rows. The theme's event templates
    // expand `$0..$N` from `event_params`, but `event_params` is not
    // persisted to SQLite — only the rendered `text` is. If we passed
    // `event_key` through, `render_event` would pick the template,
    // substitute `$0..$N` against an empty params slice, and produce
    // stripped lines like `(@) has joined`, `sets mode on`, or
    // `is now known as` (with the actual nicks/channels missing).
    // Setting `event_key = None` makes the renderer fall through to
    // `parse_format_string(&msg.text, &[])`, which prints the
    // original text that was logged at write time.
    Message {
        id: state.next_message_id(),
        timestamp: ts,
        message_type: msg_type,
        nick: stored.nick.clone(),
        nick_mode: None,
        text: stored.text.clone(),
        highlight: stored.highlight,
        event_key: None,
        event_params: None,
        log_msg_id: Some(stored.id.to_string()),
        log_ref_id: None,
        tags: None,
        shortenings: Vec::new(),
    }
}

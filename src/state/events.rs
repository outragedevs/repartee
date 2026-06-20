use tokio::sync::mpsc::error::TrySendError;

use crate::state::AppState;
use crate::state::buffer::{ActivityLevel, Buffer, Message, MessageType, NickEntry};
use crate::state::connection::{Connection, ConnectionStatus};
use crate::state::sorting::sort_buffers;
use crate::storage::LogRow;

impl AppState {
    pub fn new() -> Self {
        Self {
            connections: std::collections::HashMap::new(),
            buffers: indexmap::IndexMap::new(),
            active_buffer_id: None,
            previous_buffer_id: None,
            message_counter: 0,
            flood_state: crate::irc::flood::FloodState::new(),
            netsplit_state: crate::irc::netsplit::NetsplitState::new(),
            flood_protection: true,
            flood_exemptions: Vec::new(),
            ignores: Vec::new(),
            log_tx: None,
            shrink_incoming_tx: None,
            shrink_incoming_active: false,
            shrink_min_url_length: 50,
            log_exclude_types: Vec::new(),
            scrollback_limit: 2000,
            pending_web_events: Vec::new(),
            pending_e2e_sends: Vec::new(),
            pending_userhost_requests: Vec::new(),
            nick_color_sat: 0.65,
            nick_color_lit: 0.65,
            e2e_manager: None,
            suppress_event_display: false,
            web_preview_extractor: None,
        }
    }

    pub const fn next_message_id(&mut self) -> u64 {
        self.message_counter += 1;
        self.message_counter
    }

    // === Connection management ===

    pub fn add_connection(&mut self, conn: Connection) {
        self.connections.insert(conn.id.clone(), conn);
    }

    #[expect(
        dead_code,
        reason = "reserved for future reconnect/disconnect commands"
    )]
    pub fn remove_connection(&mut self, id: &str) {
        self.connections.remove(id);
    }

    pub fn update_connection_status(&mut self, id: &str, status: ConnectionStatus) {
        if let Some(conn) = self.connections.get_mut(id) {
            conn.status = status;
        }
    }

    // === Buffer management ===

    pub fn add_buffer(&mut self, buffer: Buffer) {
        let meta = crate::web::protocol::BufferMeta {
            id: buffer.id.clone(),
            connection_id: buffer.connection_id.clone(),
            name: buffer.name.clone(),
            buffer_type: crate::web::snapshot::buffer_type_str(&buffer.buffer_type).to_string(),
            topic: buffer.topic.clone(),
            unread_count: buffer.unread_count,
            activity: buffer.activity as u8,
            nick_count: u32::try_from(buffer.users.len()).unwrap_or(u32::MAX),
            modes: buffer.modes.clone(),
        };
        self.buffers.insert(buffer.id.clone(), buffer);
        self.pending_web_events
            .push(crate::web::protocol::WebEvent::BufferCreated { buffer: meta });
    }

    pub fn remove_buffer(&mut self, id: &str) {
        // Idempotent: callers may invoke this twice for the same buffer
        // — e.g. `/wc` removes the channel buffer immediately for an
        // instant UI close, then the server's PART echo runs
        // `handle_part` which also calls remove_buffer. Without this
        // guard the web frontend would receive a second `BufferClosed`
        // for an already-gone buffer.
        if !self.buffers.contains_key(id) {
            return;
        }
        let was_active = self.active_buffer_id.as_deref() == Some(id);
        self.pending_web_events
            .push(crate::web::protocol::WebEvent::BufferClosed {
                buffer_id: id.to_string(),
            });
        self.buffers.shift_remove(id);
        // Clean up per-buffer flood tracking to prevent unbounded map growth.
        self.flood_state.remove_buffer(id);

        if was_active {
            // Try to fall back to previous buffer
            if let Some(prev_id) = &self.previous_buffer_id
                && self.buffers.contains_key(prev_id.as_str())
            {
                self.active_buffer_id = Some(prev_id.clone());
                self.previous_buffer_id = None;
                return;
            }
            // Fall back to first buffer in sorted order
            let sorted = self.sorted_buffer_ids();
            self.active_buffer_id = sorted.into_iter().next();
            self.previous_buffer_id = None;
        }
    }

    /// Collapse a buffer's on-demand backlog window: if it's pinned (the user
    /// scrolled it up into loaded history), unpin it, clear `history_exhausted`
    /// so a later scroll-up reloads, and trim back to the normal
    /// `scrollback_limit` — freeing the loaded backlog. No-op if not pinned.
    pub(crate) fn collapse_buffer_backlog(&mut self, buffer_id: &str) {
        let limit = self.scrollback_limit;
        if let Some(buf) = self.buffers.get_mut(buffer_id)
            && buf.pin_backlog
        {
            buf.pin_backlog = false;
            buf.history_exhausted = false;
            if limit > 0 && buf.messages.len() > limit {
                let excess = buf.messages.len() - limit;
                buf.messages.drain(..excess);
                buf.messages.shrink_to(limit);
            }
        }
    }

    pub fn set_active_buffer(&mut self, id: &str) {
        if !self.buffers.contains_key(id) {
            return;
        }
        let changed = self.active_buffer_id.as_deref() != Some(id);
        // Save current as previous
        if changed {
            // Collapse the outgoing buffer's backlog window — otherwise a buffer
            // left while scrolled up stays pinned forever (exempt from trimming,
            // capped at PINNED_BACKLOG_CAP), leaking memory. This is the common
            // chokepoint for every buffer switch (Alt+arrows, Alt+N, click, …).
            if let Some(old) = self.active_buffer_id.clone() {
                self.collapse_buffer_backlog(&old);
            }
            self.previous_buffer_id = self.active_buffer_id.clone();
        }
        self.active_buffer_id = Some(id.to_string());

        // Reset activity on the newly active buffer
        if let Some(buf) = self.buffers.get_mut(id) {
            buf.activity = ActivityLevel::None;
            buf.unread_count = 0;
        }

        // Broadcast to web clients so TUI ↔ Web stay in sync.
        if changed {
            self.pending_web_events
                .push(crate::web::protocol::WebEvent::ActiveBufferChanged {
                    buffer_id: id.to_string(),
                });
        }
    }

    // === Messages ===

    pub fn add_message(&mut self, buffer_id: &str, message: Message) {
        // Honour script-driven event display suppression. State mutation runs
        // up the call chain before this point; this gate only hides the JOIN/
        // PART/QUIT/etc. event line so scripts that returned Suppress for a
        // state-mutating command keep their "hide noise" behaviour without
        // leaving the nicklist out of sync. Non-Event messages (PRIVMSG, etc.)
        // are not affected — those scripts use the early-return path in
        // App::handle_irc_event.
        if self.suppress_event_display && message.message_type == MessageType::Event {
            return;
        }
        // Incoming-shrink dispatch for NOTICEs from a real user (not
        // server-origin events, not echoes of our own outgoing).
        // PRIVMSG/ACTION go through add_message_with_activity which
        // has its own dispatch; this hook covers the remaining
        // live-chat path. Server notices (nick = None) skip — they
        // often carry one-shot tokens we shouldn't ship to a
        // third-party shortener. Self-echoes (msg.nick == our nick
        // for the buffer's connection) skip because /notice never
        // went through outgoing shrink in the first place; the wire
        // peers saw is unshrunk, so shortening on our local view
        // would diverge from theirs (and would apply the
        // incoming-only `[host]` hint to our own message).
        if self.shrink_incoming_active
            && message.message_type == MessageType::Notice
            && message.nick.as_deref().is_some_and(|n| !n.is_empty())
            && let Some(ref tx) = self.shrink_incoming_tx
        {
            let urls =
                crate::shrink::find_long_urls(&message.text, self.shrink_min_url_length as usize);
            let our_nick = self
                .buffers
                .get(buffer_id)
                .and_then(|b| self.connections.get(&b.connection_id))
                .map(|c| c.nick.as_str());
            // RFC 2812 §2.2: nicknames are case-insensitive. Compare with
            // `eq_ignore_ascii_case` so a server-echoed NOTICE whose nick
            // casing differs from our stored Connection.nick still matches.
            let is_own = match (our_nick, message.nick.as_deref()) {
                (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                _ => false,
            };
            if !urls.is_empty() && !is_own {
                let pending = crate::app::shrink::PendingIncoming {
                    buffer_id: buffer_id.to_string(),
                    message,
                    activity_level: ActivityLevel::None,
                    urls,
                    push_to_mentions: false,
                };
                match tx.try_send(pending) {
                    Ok(()) => return,
                    Err(TrySendError::Full(p)) => {
                        tracing::warn!("shrink: NOTICE queue full, delivering unshrunk");
                        return self.add_message_unshrunk(buffer_id, p.message);
                    }
                    Err(TrySendError::Closed(p)) => {
                        tracing::error!("shrink: incoming worker dead, delivering unshrunk");
                        return self.add_message_unshrunk(buffer_id, p.message);
                    }
                }
            }
        }
        self.add_message_unshrunk(buffer_id, message);
    }

    /// Inline-only path for `add_message`: skips suppress + shrink
    /// gates. Used by the deferred shrink deliver and by the
    /// queue-full fallback inside `add_message`.
    ///
    /// Guards on buffer existence at entry: when the user parts /
    /// closes the buffer between shrink dispatch and worker
    /// deliver, we must NOT write to `SQLite` or broadcast web events
    /// for a buffer the client side no longer knows about. Logging
    /// would also persist the substituted text under a buffer that
    /// no longer maps to it, making `/search` for the original URL
    /// return nothing.
    pub fn add_message_unshrunk(&mut self, buffer_id: &str, message: Message) {
        if !self.buffers.contains_key(buffer_id) {
            return;
        }
        self.maybe_log(buffer_id, &message);
        // Queue web event for broadcast.
        let wire =
            crate::web::snapshot::message_to_wire(&message, self.web_preview_extractor.as_deref());
        if message.highlight {
            self.pending_web_events
                .push(crate::web::protocol::WebEvent::MentionAlert {
                    buffer_id: buffer_id.to_string(),
                    message: wire.clone(),
                });
        }
        self.pending_web_events
            .push(crate::web::protocol::WebEvent::NewMessage {
                buffer_id: buffer_id.to_string(),
                message: wire,
            });
        if let Some(buf) = self.buffers.get_mut(buffer_id) {
            track_speaker(buf, &message);
            buf.messages.push_back(message);
            enforce_scrollback(buf, self.scrollback_limit);
        }
    }

    /// Add a message to a buffer WITHOUT logging it to the database.
    /// Used for local UI events (command output, status messages) that
    /// should appear on screen but not be persisted — but still broadcast
    /// to web clients so command output is visible on the web UI.
    pub fn add_local_message(&mut self, buffer_id: &str, message: Message) {
        self.pending_web_events
            .push(crate::web::protocol::WebEvent::NewMessage {
                buffer_id: buffer_id.to_string(),
                message: crate::web::snapshot::message_to_wire(
                    &message,
                    self.web_preview_extractor.as_deref(),
                ),
            });
        if let Some(buf) = self.buffers.get_mut(buffer_id) {
            buf.messages.push_back(message);
            enforce_scrollback(buf, self.scrollback_limit);
        }
    }

    /// Add a mention message to the `_mentions` buffer.
    ///
    /// Unlike `add_message_with_activity`, this:
    /// - Does NOT log to the messages DB (mention is already in the mentions table)
    /// - Does NOT push a `MentionAlert` web event (avoids double-counting the badge)
    /// - DOES push `NewMessage` for web clients
    /// - DOES set `ActivityLevel::Mention` on the buffer
    pub fn add_mention_to_buffer(&mut self, message: Message) {
        let buffer_id = "_mentions";
        // Guard buffer existence BEFORE doing any work — avoids
        // broadcasting orphaned web events when display.mentions_buffer
        // is disabled and the buffer doesn't exist.
        if !self.buffers.contains_key(buffer_id) {
            return;
        }
        let wire =
            crate::web::snapshot::message_to_wire(&message, self.web_preview_extractor.as_deref());
        self.pending_web_events
            .push(crate::web::protocol::WebEvent::NewMessage {
                buffer_id: buffer_id.to_string(),
                message: wire,
            });
        let Some(buf) = self.buffers.get_mut(buffer_id) else {
            return;
        };
        buf.messages.push_back(message);
        // Hard cap at 1000 messages — matches the DB LIMIT.
        // Uses drain + shrink_to to release peak VecDeque capacity.
        if buf.messages.len() > 1000 {
            let excess = buf.messages.len() - 1000;
            buf.messages.drain(..excess);
            buf.messages.shrink_to(1000);
        }
        // Always increment unread_count for non-active buffer — every
        // mention matters, not just the first one. Activity level is only
        // escalated once (it's already Mention after the first).
        let is_active = self.active_buffer_id.as_deref() == Some(buffer_id);
        if !is_active {
            buf.activity = ActivityLevel::Mention;
            buf.unread_count += 1;
            self.pending_web_events
                .push(crate::web::protocol::WebEvent::ActivityChanged {
                    buffer_id: buffer_id.to_string(),
                    activity: ActivityLevel::Mention as u8,
                    unread_count: buf.unread_count,
                });
        }
    }

    pub fn add_message_with_activity(
        &mut self,
        buffer_id: &str,
        message: Message,
        level: ActivityLevel,
    ) {
        // Incoming shrink: if the message text has URL(s) above the
        // configured threshold and shrink-incoming is wired up, hand
        // the message off to the background worker. The worker
        // substitutes the URLs (with `[host]` hint), then posts a
        // `ShrinkDeliver::Incoming` back to the main loop which
        // re-calls this same method (with `shrink_incoming_tx` set
        // to `None` on the substituted-message Message in the
        // deliver path) so we don't loop forever.
        if self.shrink_incoming_active
            && let Some(ref tx) = self.shrink_incoming_tx
        {
            let urls =
                crate::shrink::find_long_urls(&message.text, self.shrink_min_url_length as usize);
            if !urls.is_empty() {
                let pending = crate::app::shrink::PendingIncoming {
                    buffer_id: buffer_id.to_string(),
                    message,
                    activity_level: level,
                    urls,
                    // `push_to_mentions` is unused on the deferred
                    // path — the mentions buffer push is run inline
                    // by the call site (handle_privmsg) with original
                    // text; chat-buffer text uses the shortened form.
                    push_to_mentions: false,
                };
                match tx.try_send(pending) {
                    Ok(()) => return,
                    Err(TrySendError::Full(p)) => {
                        tracing::warn!("shrink: incoming queue full, delivering unshrunk");
                        self.add_message_with_activity_unshrunk(
                            buffer_id,
                            p.message,
                            p.activity_level,
                        );
                        return;
                    }
                    Err(TrySendError::Closed(p)) => {
                        tracing::error!("shrink: incoming worker dead, delivering unshrunk");
                        self.add_message_with_activity_unshrunk(
                            buffer_id,
                            p.message,
                            p.activity_level,
                        );
                        return;
                    }
                }
            }
        }
        self.add_message_with_activity_unshrunk(buffer_id, message, level);
    }

    /// Same as `add_message_with_activity`, but bypasses the shrink
    /// dispatch. Used by the deferred deliver path which has already
    /// substituted URLs and would otherwise loop forever.
    ///
    /// Same buffer-existence guard as `add_message_unshrunk` — when
    /// the user parted the channel during the shrink wait, dropping
    /// the delivery entirely is the only correct option (otherwise
    /// `SQLite` would log the substituted text orphaned from any
    /// visible buffer, and web clients would get a `NewMessage` for a
    /// buffer they no longer have).
    pub fn add_message_with_activity_unshrunk(
        &mut self,
        buffer_id: &str,
        message: Message,
        level: ActivityLevel,
    ) {
        if !self.buffers.contains_key(buffer_id) {
            return;
        }
        self.maybe_log(buffer_id, &message);
        // Queue web events for broadcast.
        let wire =
            crate::web::snapshot::message_to_wire(&message, self.web_preview_extractor.as_deref());
        if message.highlight {
            self.pending_web_events
                .push(crate::web::protocol::WebEvent::MentionAlert {
                    buffer_id: buffer_id.to_string(),
                    message: wire.clone(),
                });
        }
        self.pending_web_events
            .push(crate::web::protocol::WebEvent::NewMessage {
                buffer_id: buffer_id.to_string(),
                message: wire,
            });
        if let Some(buf) = self.buffers.get_mut(buffer_id) {
            track_speaker(buf, &message);
            buf.messages.push_back(message);
            enforce_scrollback(buf, self.scrollback_limit);
            // Only escalate activity if this is not the active buffer
            let is_active = self.active_buffer_id.as_deref() == Some(buffer_id);
            if !is_active && level > buf.activity {
                buf.activity = level;
                buf.unread_count += 1;
                self.pending_web_events
                    .push(crate::web::protocol::WebEvent::ActivityChanged {
                        buffer_id: buffer_id.to_string(),
                        activity: level as u8,
                        unread_count: buf.unread_count,
                    });
            }
        }
    }

    /// Send a message to the storage writer if logging is enabled.
    fn maybe_log(&self, buffer_id: &str, message: &Message) {
        let Some(tx) = &self.log_tx else { return };

        // Check exclude_types filter (e.g. "event" skips quit/join/nick fan-out)
        let type_str = message.message_type.as_str();
        if self
            .log_exclude_types
            .iter()
            .any(|t| t.eq_ignore_ascii_case(type_str))
        {
            return;
        }

        // buffer_id format: "connection_id/buffer_name"
        let Some((conn_id, buf_name)) = buffer_id.split_once('/') else {
            return;
        };

        // Use the connection label as network name (falls back to conn_id)
        let network = self
            .connections
            .get(conn_id)
            .map_or_else(|| conn_id.to_string(), |c| c.label.clone());

        let is_ref = message.log_ref_id.is_some();
        let tags_json = message
            .tags
            .as_ref()
            .and_then(|t| serde_json::to_string(t).ok());
        // Choose the stored row's `msg_id`:
        // 1. An explicit `log_msg_id` wins — it's a primary id that fan-out
        //    reference rows point at via `ref_id`, and must be preserved verbatim.
        // 2. A reference row (`log_ref_id` set) gets a fresh UUID so siblings
        //    sharing the same server `@msgid` tag don't collide on it and get
        //    dropped by the unique index.
        // 3. A plain conversational row is keyed by the server `@msgid` (carried
        //    in `tags`) when present, so a live message and its later CHATHISTORY
        //    replay collapse to one row. Otherwise a generated UUID.
        let msg_id = match (message.log_msg_id.clone(), message.log_ref_id.is_some()) {
            (Some(explicit), _) => explicit,
            (None, true) => uuid::Uuid::new_v4().to_string(),
            (None, false) => message
                .tags
                .as_ref()
                .and_then(|t| t.get("msgid"))
                .filter(|m| !m.is_empty())
                .cloned()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        };
        let row = LogRow {
            msg_id,
            network,
            buffer: buf_name.to_string(),
            timestamp: message.timestamp.timestamp(),
            msg_type: message.message_type.clone(),
            nick: message.nick.clone(),
            text: if is_ref {
                String::new()
            } else {
                message.text.clone()
            },
            highlight: message.highlight,
            ref_id: message.log_ref_id.clone(),
            tags: tags_json,
            event_key: message.event_key.clone(),
        };

        if let Err(e) = tx.try_send(row) {
            tracing::warn!("log queue full, dropping message: {e}");
        }
    }

    /// Persist a `draft/chathistory` message to the log store WITHOUT
    /// displaying it, mutating buffers/nicklists, or emitting notifications.
    ///
    /// chathistory is a background backlog filler: rows are written here and
    /// the UI surfaces them later through normal `SQLite` pagination
    /// (`get_messages_paginated`). Deduplication is handled by the unique
    /// `msg_id` index on the messages table, so re-ingesting an
    /// already-stored message is a no-op at the database layer.
    pub fn ingest_history_message(&self, buffer_id: &str, message: &Message) {
        self.maybe_log(buffer_id, message);
    }

    /// Splice reconnect gap-fill rows (from an `AFTER`/`LATEST` CHATHISTORY
    /// fetch) into a live buffer in timestamp order, skipping any already
    /// present. Scroll-up pagination only pulls rows OLDER than the current
    /// oldest message, so these gap rows — which sit between the pre-disconnect
    /// tail and post-reconnect live messages — would otherwise never appear
    /// until a restart or log-browser reload. Each spliced row is assigned a
    /// fresh in-memory id; rows are inserted before the first existing message
    /// with a strictly greater timestamp so ordering is preserved.
    pub(crate) fn surface_history_rows(&mut self, buffer_id: &str, rows: Vec<Message>) {
        for mut msg in rows {
            let already_present = match self.buffers.get(buffer_id) {
                Some(buf) => buffer_contains_history_row(buf, &msg),
                None => return,
            };
            if already_present {
                continue;
            }
            msg.id = self.next_message_id();
            if let Some(buf) = self.buffers.get_mut(buffer_id) {
                let pos = buf
                    .messages
                    .iter()
                    .position(|m| m.timestamp > msg.timestamp)
                    .unwrap_or(buf.messages.len());
                buf.messages.insert(pos, msg);
            }
        }
    }

    #[allow(dead_code, reason = "reserved for scripting API; used in tests")]
    pub fn set_activity(&mut self, buffer_id: &str, level: ActivityLevel) {
        if let Some(buf) = self.buffers.get_mut(buffer_id)
            && level > buf.activity
        {
            buf.activity = level;
        }
    }

    // === Topic ===

    pub fn set_topic(&mut self, buffer_id: &str, topic: String, set_by: Option<String>) {
        if let Some(buf) = self.buffers.get_mut(buffer_id) {
            buf.topic = Some(topic);
            buf.topic_set_by = set_by;
        }
    }

    // === Nick management ===

    pub fn add_nick(&mut self, buffer_id: &str, entry: NickEntry) {
        if let Some(buf) = self.buffers.get_mut(buffer_id) {
            let key = entry.nick.to_lowercase();
            buf.users.insert(key, entry);
        }
    }

    pub fn remove_nick(&mut self, buffer_id: &str, nick: &str) {
        if let Some(buf) = self.buffers.get_mut(buffer_id) {
            buf.users.remove(&nick.to_lowercase());
        }
    }

    pub fn update_nick(&mut self, buffer_id: &str, old_nick: &str, new_nick: &str) {
        if let Some(buf) = self.buffers.get_mut(buffer_id)
            && let Some(mut entry) = buf.users.remove(&old_nick.to_lowercase())
        {
            new_nick.clone_into(&mut entry.nick);
            buf.users.insert(new_nick.to_lowercase(), entry);
        }
    }

    // === Active buffer accessors ===

    pub fn active_buffer(&self) -> Option<&Buffer> {
        self.active_buffer_id
            .as_ref()
            .and_then(|id| self.buffers.get(id))
    }

    pub fn active_buffer_mut(&mut self) -> Option<&mut Buffer> {
        let id = self.active_buffer_id.as_deref()?;
        self.buffers.get_mut(id)
    }

    /// Look up the highest channel mode prefix for a nick in a buffer.
    ///
    /// Returns `Some('@')` for ops, `Some('+')` for voice, etc.
    pub fn nick_prefix(&self, buffer_id: &str, nick: &str) -> Option<char> {
        let buf = self.buffers.get(buffer_id)?;
        let entry = buf.users.get(&nick.to_lowercase())?;
        entry.prefix.chars().next()
    }

    // === Navigation ===

    pub fn sorted_buffer_ids(&self) -> Vec<String> {
        let buf_refs: Vec<&Buffer> = self.buffers.values().collect();
        let sorted = sort_buffers(&buf_refs, |conn_id| {
            self.connections
                .get(conn_id)
                .map_or_else(|| conn_id.to_string(), |c| c.label.clone())
        });
        sorted.into_iter().map(|b| b.id.clone()).collect()
    }

    pub fn next_buffer(&mut self) {
        let sorted = self.sorted_buffer_ids();
        if sorted.is_empty() {
            return;
        }
        let current_idx = self
            .active_buffer_id
            .as_ref()
            .and_then(|id| sorted.iter().position(|s| s == id));
        let next_idx = current_idx.map_or(0, |idx| (idx + 1) % sorted.len());
        let next_id = sorted[next_idx].clone();
        self.set_active_buffer(&next_id);
    }

    pub fn prev_buffer(&mut self) {
        let sorted = self.sorted_buffer_ids();
        if sorted.is_empty() {
            return;
        }
        let current_idx = self
            .active_buffer_id
            .as_ref()
            .and_then(|id| sorted.iter().position(|s| s == id));
        let prev_idx = match current_idx {
            Some(0) => sorted.len() - 1,
            Some(idx) => idx - 1,
            None => 0,
        };
        let prev_id = sorted[prev_idx].clone();
        self.set_active_buffer(&prev_id);
    }
}

/// Track a speaker for tab completion recency ordering.
/// Only tracks user messages (PRIVMSG, ACTION, NOTICE) — not system events.
fn track_speaker(buf: &mut Buffer, message: &Message) {
    if let Some(ref nick) = message.nick {
        match message.message_type {
            MessageType::Message | MessageType::Action | MessageType::Notice => {
                buf.touch_speaker(nick);
            }
            MessageType::Event | MessageType::MentionLog => {}
        }
    }
}

/// The scrollback limit actually applied to a buffer, accounting for the pin.
/// A pinned buffer (the user has scrolled it up into on-demand-loaded backlog)
/// keeps up to [`crate::app::backlog::PINNED_BACKLOG_CAP`] (or `limit` if the
/// user configured an even larger scrollback) so a live message can't drop the
/// loaded history; an unpinned buffer keeps `limit`. `limit == 0` means
/// unlimited and is preserved in both states.
const fn effective_scrollback_limit(pinned: bool, limit: usize) -> usize {
    if !pinned || limit == 0 {
        // Unpinned uses the plain limit; `0` (unlimited) is preserved in both
        // states so a user who opted out of trimming keeps everything.
        return limit;
    }
    let cap = crate::app::backlog::PINNED_BACKLOG_CAP;
    if limit > cap { limit } else { cap }
}

/// Whether `buf` already contains the same logical message as `candidate`.
/// Used to dedup CHATHISTORY gap-fill rows against messages already shown live.
/// Prefers IRC `@msgid` equality (authoritative when both carry one), falling
/// back to matching timestamp, nick, type and text.
fn buffer_contains_history_row(buf: &Buffer, candidate: &Message) -> bool {
    let candidate_msgid = candidate.tags.as_ref().and_then(|t| t.get("msgid"));
    buf.messages.iter().any(|m| {
        if let Some(cid) = candidate_msgid
            && let Some(mid) = m.tags.as_ref().and_then(|t| t.get("msgid"))
        {
            return cid == mid;
        }
        m.timestamp == candidate.timestamp
            && m.nick == candidate.nick
            && m.message_type == candidate.message_type
            && m.text == candidate.text
    })
}

/// Trim oldest messages from the buffer if it exceeds the (pin-aware) scrollback
/// limit. Uses `VecDeque::drain` which is O(n) on the drained range only.
fn enforce_scrollback(buf: &mut Buffer, limit: usize) {
    let limit = effective_scrollback_limit(buf.pin_backlog, limit);
    if limit > 0 && buf.messages.len() > limit {
        let excess = buf.messages.len() - limit;
        buf.messages.drain(..excess);
        // Release the ring-buffer capacity retained from the peak.
        // Without this, a burst of 10K messages → drain to 2000 still
        // holds 10K slots of heap allocation.
        buf.messages.shrink_to(limit);
        // We just dropped the oldest in-memory rows, so there is now older
        // history below the in-memory head (it still lives in the log DB).
        // Clear any stale `history_exhausted` set by an earlier short backlog
        // load, otherwise `maybe_load_older_chat_backlog` would refuse to page.
        buf.history_exhausted = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::buffer::*;
    use crate::state::connection::*;
    use chrono::Utc;
    use std::collections::{HashMap, VecDeque};

    fn make_test_connection() -> Connection {
        Connection {
            id: "libera".to_string(),
            label: "Libera".to_string(),
            status: ConnectionStatus::Connected,
            nick: "testuser".to_string(),
            user_modes: String::new(),
            isupport: HashMap::new(),
            isupport_parsed: crate::irc::isupport::Isupport::new(),
            error: None,
            lag: None,
            lag_pending: false,
            reconnect_attempts: 0,

            reconnect_delay_secs: 30,
            next_reconnect: None,
            should_reconnect: true,
            joined_channels: Vec::new(),
            origin_config: crate::config::ServerConfig {
                label: "Libera".to_string(),
                address: "irc.libera.chat".to_string(),
                port: 6697,
                tls: true,
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
                auto_reconnect: Some(true),
                reconnect_delay: None,
                reconnect_max_retries: None,
                autosendcmd: None,
                sasl_mechanism: None,
                client_cert_path: None,
            },
            local_ip: None,
            enabled_caps: std::collections::HashSet::new(),
            chathistory: crate::irc::chathistory::HistoryState::new(),
            who_token_counter: 0,
            silent_who_channels: std::collections::HashSet::new(),
            silent_banlist_channels: std::collections::HashSet::new(),
        }
    }

    fn make_test_buffer(conn_id: &str, btype: BufferType, name: &str) -> Buffer {
        Buffer {
            id: make_buffer_id(conn_id, name),
            connection_id: conn_id.to_string(),
            buffer_type: btype,
            name: name.to_string(),
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
        }
    }

    fn make_test_message(state: &mut AppState, text: &str) -> Message {
        Message {
            id: state.next_message_id(),
            timestamp: Utc::now(),
            message_type: MessageType::Message,
            nick: Some("someone".to_string()),
            nick_mode: None,
            text: text.to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
        }
    }

    fn make_test_state() -> AppState {
        let mut state = AppState::new();
        state.add_connection(make_test_connection());
        state.add_buffer(make_test_buffer("libera", BufferType::Server, "libera"));
        state.add_buffer(make_test_buffer("libera", BufferType::Channel, "#rust"));
        state.add_buffer(make_test_buffer("libera", BufferType::Channel, "#linux"));
        state
    }

    #[test]
    fn add_buffer_and_set_active() {
        let mut state = make_test_state();
        assert!(state.active_buffer().is_none());

        state.set_active_buffer("libera/#rust");
        assert_eq!(state.active_buffer().unwrap().name, "#rust");
    }

    #[test]
    fn add_message_to_buffer() {
        let mut state = make_test_state();
        let msg = make_test_message(&mut state, "hello world");
        state.add_message("libera/#rust", msg);

        let buf = state.buffers.get("libera/#rust").unwrap();
        assert_eq!(buf.messages.len(), 1);
        assert_eq!(buf.messages[0].text, "hello world");
    }

    #[test]
    fn suppress_event_display_drops_event_messages_only() {
        // Script suppress for state-mutating commands sets
        // suppress_event_display before handle_irc_message runs and clears
        // it after. While set, MessageType::Event lines (the JOIN/PART/QUIT
        // event display) are dropped from the buffer — but PRIVMSG/Action
        // lines pass through unchanged because those go through the
        // non-state-mutating early-return path in app/irc.rs and never
        // reach add_message with the flag set.
        let mut state = make_test_state();
        state.suppress_event_display = true;

        let event_msg = Message {
            id: state.next_message_id(),
            timestamp: Utc::now(),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text: "alice has joined #rust".to_string(),
            highlight: false,
            event_key: Some("join".to_string()),
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
        };
        state.add_message("libera/#rust", event_msg);
        assert!(
            state
                .buffers
                .get("libera/#rust")
                .unwrap()
                .messages
                .is_empty(),
            "Event display must be dropped while suppress_event_display is set"
        );

        let chat_msg = make_test_message(&mut state, "regular chat");
        state.add_message("libera/#rust", chat_msg);
        assert_eq!(
            state.buffers.get("libera/#rust").unwrap().messages.len(),
            1,
            "MessageType::Message must NOT be suppressed by the event-only gate"
        );

        state.suppress_event_display = false;
        let event_msg2 = Message {
            id: state.next_message_id(),
            timestamp: Utc::now(),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text: "bob has parted".to_string(),
            highlight: false,
            event_key: Some("part".to_string()),
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
        };
        state.add_message("libera/#rust", event_msg2);
        assert_eq!(
            state.buffers.get("libera/#rust").unwrap().messages.len(),
            2,
            "Event display must resume after the flag is cleared"
        );
    }

    #[test]
    fn activity_only_escalates() {
        let mut state = make_test_state();
        state.set_activity("libera/#rust", ActivityLevel::Events);
        assert_eq!(
            state.buffers.get("libera/#rust").unwrap().activity,
            ActivityLevel::Events
        );

        // Escalate to Mention
        state.set_activity("libera/#rust", ActivityLevel::Mention);
        assert_eq!(
            state.buffers.get("libera/#rust").unwrap().activity,
            ActivityLevel::Mention
        );

        // Should NOT downgrade
        state.set_activity("libera/#rust", ActivityLevel::Events);
        assert_eq!(
            state.buffers.get("libera/#rust").unwrap().activity,
            ActivityLevel::Mention
        );
    }

    #[test]
    fn activation_resets_activity() {
        let mut state = make_test_state();
        state.set_activity("libera/#rust", ActivityLevel::Mention);
        assert_eq!(
            state.buffers.get("libera/#rust").unwrap().activity,
            ActivityLevel::Mention
        );

        state.set_active_buffer("libera/#rust");
        assert_eq!(
            state.buffers.get("libera/#rust").unwrap().activity,
            ActivityLevel::None
        );
    }

    #[test]
    fn remove_buffer_falls_back_to_previous() {
        let mut state = make_test_state();
        state.set_active_buffer("libera/libera");
        state.set_active_buffer("libera/#rust");

        assert_eq!(state.active_buffer_id.as_deref(), Some("libera/#rust"));
        assert_eq!(state.previous_buffer_id.as_deref(), Some("libera/libera"));

        // Remove the active buffer; should fall back to previous
        state.remove_buffer("libera/#rust");
        assert_eq!(state.active_buffer_id.as_deref(), Some("libera/libera"));
    }

    #[test]
    fn next_prev_buffer_cycles() {
        let mut state = make_test_state();
        // Sorted order: libera/libera (server=1), libera/#linux (chan=2), libera/#rust (chan=2)
        let sorted = state.sorted_buffer_ids();
        assert_eq!(
            sorted,
            vec!["libera/libera", "libera/#linux", "libera/#rust"]
        );

        state.set_active_buffer("libera/libera");

        state.next_buffer();
        assert_eq!(state.active_buffer_id.as_deref(), Some("libera/#linux"));

        state.next_buffer();
        assert_eq!(state.active_buffer_id.as_deref(), Some("libera/#rust"));

        // Wrap around
        state.next_buffer();
        assert_eq!(state.active_buffer_id.as_deref(), Some("libera/libera"));

        // Prev wraps the other way
        state.prev_buffer();
        assert_eq!(state.active_buffer_id.as_deref(), Some("libera/#rust"));
    }

    #[test]
    fn add_message_with_activity_skips_active_buffer() {
        let mut state = make_test_state();
        state.set_active_buffer("libera/#rust");

        // Adding a message with activity to the *active* buffer should not escalate
        let msg = make_test_message(&mut state, "test");
        state.add_message_with_activity("libera/#rust", msg, ActivityLevel::Mention);
        assert_eq!(
            state.buffers.get("libera/#rust").unwrap().activity,
            ActivityLevel::None
        );

        // Adding to an inactive buffer should escalate
        let msg2 = make_test_message(&mut state, "test2");
        state.add_message_with_activity("libera/#linux", msg2, ActivityLevel::Mention);
        assert_eq!(
            state.buffers.get("libera/#linux").unwrap().activity,
            ActivityLevel::Mention
        );
    }

    #[test]
    fn nick_management() {
        let mut state = make_test_state();
        let entry = NickEntry {
            nick: "alice".to_string(),
            prefix: "@".to_string(),
            modes: "o".to_string(),
            away: false,
            account: None,
            ident: None,
            host: None,
        };
        state.add_nick("libera/#rust", entry);
        assert!(
            state
                .buffers
                .get("libera/#rust")
                .unwrap()
                .users
                .contains_key("alice")
        );

        state.update_nick("libera/#rust", "alice", "alice_");
        assert!(
            !state
                .buffers
                .get("libera/#rust")
                .unwrap()
                .users
                .contains_key("alice")
        );
        assert!(
            state
                .buffers
                .get("libera/#rust")
                .unwrap()
                .users
                .contains_key("alice_")
        );

        state.remove_nick("libera/#rust", "alice_");
        assert!(state.buffers.get("libera/#rust").unwrap().users.is_empty());
    }

    #[test]
    fn maybe_log_uses_server_msgid_from_tags_for_dedup() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let mut state = make_test_state();
        state.log_tx = Some(tx);

        // A live PRIVMSG carries the server @msgid in its tags but no
        // log_msg_id (only DB-loaded rows set that). It must be stored under
        // the @msgid so a later CHATHISTORY replay of the same message dedups
        // via the unique msg_id index instead of inserting a second row.
        let mut tags = std::collections::HashMap::new();
        tags.insert("msgid".to_string(), "server-msgid-xyz".to_string());
        let msg = Message {
            id: state.next_message_id(),
            timestamp: Utc::now(),
            message_type: MessageType::Message,
            nick: Some("bob".to_string()),
            nick_mode: None,
            text: "hello".to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: Some(tags),
        };
        state.add_message("libera/#rust", msg);

        let row = rx.try_recv().expect("logged row");
        assert_eq!(
            row.msg_id, "server-msgid-xyz",
            "live row must be keyed by the server @msgid for dedup"
        );
    }

    #[test]
    fn maybe_log_preserves_explicit_id_over_server_msgid_for_fanout() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let mut state = make_test_state();
        state.log_tx = Some(tx);

        let mut tags = std::collections::HashMap::new();
        tags.insert("msgid".to_string(), "server-M".to_string());

        // Fan-out primary (e.g. a QUIT across channels): carries its own
        // generated id that reference rows point at, *plus* a server @msgid
        // tag. The explicit id must win — otherwise reference rows, which share
        // the same @msgid, would all collide on it and be dropped by the unique
        // index, and `ref_id` would point at a primary stored under a different id.
        let primary = Message {
            id: state.next_message_id(),
            timestamp: Utc::now(),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text: "alice has quit".to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: Some("primary-gen-id".to_string()),
            log_ref_id: None,
            tags: Some(tags.clone()),
        };
        state.add_message("libera/#rust", primary);

        let reference = Message {
            id: state.next_message_id(),
            timestamp: Utc::now(),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text: "alice has quit".to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: Some("primary-gen-id".to_string()),
            tags: Some(tags),
        };
        state.add_message("libera/#linux", reference);

        let row1 = rx.try_recv().expect("primary row");
        assert_eq!(
            row1.msg_id, "primary-gen-id",
            "explicit primary id must be preserved over the @msgid tag"
        );
        let row2 = rx.try_recv().expect("reference row");
        assert_eq!(row2.ref_id, Some("primary-gen-id".to_string()));
        assert_ne!(
            row2.msg_id, "server-M",
            "reference rows must not collide on the shared @msgid"
        );
        assert_ne!(row2.msg_id, "primary-gen-id");
    }

    #[test]
    fn maybe_log_sends_ref_id_with_empty_text() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let mut state = make_test_state();
        state.log_tx = Some(tx);

        let primary_id = "primary-uuid-123".to_string();

        // Primary row: full text, log_msg_id set, no ref_id
        let msg1 = Message {
            id: state.next_message_id(),
            timestamp: Utc::now(),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text: "alice has quit (Quit: bye)".to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: Some(primary_id.clone()),
            log_ref_id: None,
            tags: None,
        };
        state.add_message("libera/#rust", msg1);

        // Reference row: same text in UI, but ref_id set
        let msg2 = Message {
            id: state.next_message_id(),
            timestamp: Utc::now(),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text: "alice has quit (Quit: bye)".to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: Some(primary_id.clone()),
            tags: None,
        };
        state.add_message("libera/#linux", msg2);

        // Check primary row
        let row1 = rx.try_recv().unwrap();
        assert_eq!(row1.msg_id, primary_id);
        assert_eq!(row1.text, "alice has quit (Quit: bye)");
        assert!(row1.ref_id.is_none());

        // Check reference row
        let row2 = rx.try_recv().unwrap();
        assert!(row2.text.is_empty(), "reference row should have empty text");
        assert_eq!(row2.ref_id, Some(primary_id));
    }

    #[test]
    fn effective_limit_unpinned_is_the_plain_limit() {
        assert_eq!(super::effective_scrollback_limit(false, 2000), 2000);
        assert_eq!(super::effective_scrollback_limit(false, 0), 0);
    }

    #[test]
    fn effective_limit_pinned_raises_to_backlog_cap() {
        assert_eq!(
            super::effective_scrollback_limit(true, 2000),
            crate::app::backlog::PINNED_BACKLOG_CAP
        );
    }

    #[test]
    fn effective_limit_pinned_keeps_unlimited() {
        // limit == 0 means unlimited and must stay unlimited even when pinned.
        assert_eq!(super::effective_scrollback_limit(true, 0), 0);
    }

    #[test]
    fn effective_limit_pinned_keeps_a_larger_configured_limit() {
        let bigger = crate::app::backlog::PINNED_BACKLOG_CAP + 5000;
        assert_eq!(super::effective_scrollback_limit(true, bigger), bigger);
    }

    #[test]
    fn pinned_buffer_is_exempt_from_trimming() {
        // With a tiny limit, a pinned buffer keeps all its messages (the loaded
        // backlog) instead of being trimmed down to the limit.
        let mut state = make_test_state();
        state.scrollback_limit = 3;
        // Seed one message so the buffer exists, then pin it.
        let seed = make_test_message(&mut state, "seed");
        state.add_message("libera/#rust", seed);
        state
            .buffers
            .get_mut("libera/#rust")
            .expect("buffer created by add_message")
            .pin_backlog = true;
        for i in 0..6 {
            let msg = make_test_message(&mut state, &format!("msg{i}"));
            state.add_message("libera/#rust", msg);
        }
        let buf = state.buffers.get("libera/#rust").unwrap();
        assert_eq!(buf.messages.len(), 7, "pinned buffer must not trim to limit");
    }

    #[test]
    fn trimming_clears_stale_history_exhausted() {
        // A buffer that loaded its whole (short) history is marked exhausted.
        // Once live traffic trims the oldest rows, older history exists below the
        // in-memory head again, so the flag must clear or scroll-up would refuse.
        let mut state = make_test_state();
        state.scrollback_limit = 3;
        let seed = make_test_message(&mut state, "seed");
        state.add_message("libera/#rust", seed);
        state
            .buffers
            .get_mut("libera/#rust")
            .unwrap()
            .history_exhausted = true;
        // Add enough to force a trim (unpinned → trims to limit 3).
        for i in 0..5 {
            let msg = make_test_message(&mut state, &format!("m{i}"));
            state.add_message("libera/#rust", msg);
        }
        let buf = state.buffers.get("libera/#rust").unwrap();
        assert_eq!(buf.messages.len(), 3, "trimmed to limit");
        assert!(
            !buf.history_exhausted,
            "trimming must clear the stale exhausted flag"
        );
    }

    #[test]
    fn switching_away_collapses_a_pinned_buffer() {
        // A buffer scrolled up into backlog (pinned, holding more than the limit)
        // must collapse — unpin + trim — when the user switches to another buffer,
        // so it can't stay pinned (and memory-bloated) forever.
        let mut state = make_test_state();
        state.scrollback_limit = 3;
        // Build #rust with 6 messages, pinned (simulating loaded backlog).
        for i in 0..6 {
            let msg = make_test_message(&mut state, &format!("r{i}"));
            state.add_message("libera/#rust", msg);
        }
        state.set_active_buffer("libera/#rust");
        state
            .buffers
            .get_mut("libera/#rust")
            .unwrap()
            .pin_backlog = true;
        // Re-add so the pinned buffer holds >limit again (it was trimmed to 3
        // before pinning; push it back up to 6 while pinned).
        for i in 6..9 {
            let msg = make_test_message(&mut state, &format!("r{i}"));
            state.add_message("libera/#rust", msg);
        }
        assert_eq!(state.buffers.get("libera/#rust").unwrap().messages.len(), 6);

        // Switch to another buffer → outgoing #rust collapses.
        let other = make_test_message(&mut state, "l0");
        state.add_message("libera/#linux", other);
        state.set_active_buffer("libera/#linux");

        let rust = state.buffers.get("libera/#rust").unwrap();
        assert!(!rust.pin_backlog, "outgoing buffer must be unpinned");
        assert!(!rust.history_exhausted, "exhausted flag cleared on collapse");
        assert_eq!(rust.messages.len(), 3, "collapsed back to scrollback_limit");
    }

    #[test]
    fn scrollback_limit_evicts_oldest() {
        let mut state = make_test_state();
        state.scrollback_limit = 3;

        for i in 0..5 {
            let msg = make_test_message(&mut state, &format!("msg{i}"));
            state.add_message("libera/#rust", msg);
        }

        let buf = state.buffers.get("libera/#rust").unwrap();
        assert_eq!(buf.messages.len(), 3);
        assert_eq!(buf.messages[0].text, "msg2");
        assert_eq!(buf.messages[2].text, "msg4");
    }

    #[test]
    fn scrollback_limit_zero_means_unlimited() {
        let mut state = make_test_state();
        state.scrollback_limit = 0;

        for i in 0..100 {
            let msg = make_test_message(&mut state, &format!("msg{i}"));
            state.add_message("libera/#rust", msg);
        }

        let buf = state.buffers.get("libera/#rust").unwrap();
        assert_eq!(buf.messages.len(), 100);
    }

    #[test]
    fn scrollback_limit_with_activity() {
        let mut state = make_test_state();
        state.scrollback_limit = 2;
        state.set_active_buffer("libera/#rust");

        for i in 0..5 {
            let msg = make_test_message(&mut state, &format!("msg{i}"));
            state.add_message_with_activity("libera/#linux", msg, ActivityLevel::Activity);
        }

        let buf = state.buffers.get("libera/#linux").unwrap();
        assert_eq!(buf.messages.len(), 2);
        assert_eq!(buf.messages[0].text, "msg3");
    }

    #[test]
    fn add_local_message_does_not_log_to_storage() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let mut state = make_test_state();
        state.log_tx = Some(tx);

        let msg = make_test_message(&mut state, "local UI output");
        state.add_local_message("libera/#rust", msg);

        // Nothing should have been sent to the log channel.
        assert!(
            rx.try_recv().is_err(),
            "add_local_message must not send to log_tx"
        );

        // But the message should still appear in the buffer.
        let buf = state.buffers.get("libera/#rust").unwrap();
        assert_eq!(buf.messages.back().unwrap().text, "local UI output");
    }

    #[test]
    fn add_message_does_log_to_storage() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let mut state = make_test_state();
        state.log_tx = Some(tx);

        let msg = make_test_message(&mut state, "IRC message");
        state.add_message("libera/#rust", msg);

        // add_message SHOULD send to log channel.
        assert!(rx.try_recv().is_ok(), "add_message must send to log_tx");
    }
}

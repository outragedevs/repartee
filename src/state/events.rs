use tokio::sync::mpsc::error::TrySendError;

use crate::state::{AppState, OwnEchoDecoration};
use crate::state::buffer::{ActivityLevel, Buffer, Message, MessageType, NickEntry};
use crate::state::connection::{Connection, ConnectionStatus};
use crate::state::sorting::sort_buffers;
use crate::storage::LogRow;

/// Which arrival gate's shrink rules apply to a row parked for ordering —
/// see [`AppState::offer_parked_row_to_shrink`].
#[derive(Clone, Copy)]
enum ParkedShrinkRules {
    /// [`AppState::add_message`]'s gate: NOTICEs from a real user only.
    Notice,
    /// [`AppState::add_message_with_activity`]'s gate: any chat row.
    Chat,
}

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
            translate_incoming_tx: None,
            translate_queues: std::collections::HashMap::new(),
            translate_active: false,
            translate_buffers: std::collections::HashMap::new(),
            translate_my_lang: "en".to_string(),
            translate_show_original_in: true,
            translate_max_queue: 200,
            translate_tally: crate::state::TranslateTally::default(),
            outgoing_in_flight: std::collections::HashMap::new(),
            own_echo_decorations: std::collections::HashMap::new(),
            buffer_redirects: std::collections::HashMap::new(),
            query_departures: std::collections::HashMap::new(),
            pending_buffer_rekeys: Vec::new(),
            log_exclude_types: Vec::new(),
            scrollback_limit: 2000,
            pending_web_events: Vec::new(),
            pending_e2e_sends: Vec::new(),
            pending_e2e_gapfills: Vec::new(),
            pending_userhost_requests: Vec::new(),
            typing: crate::state::typing::TypingTracker::default(),
            typing_show: true,
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
        let mut meta = crate::web::protocol::BufferMeta {
            id: buffer.id.clone(),
            connection_id: buffer.connection_id.clone(),
            name: buffer.name.clone(),
            buffer_type: crate::web::snapshot::buffer_type_str(&buffer.buffer_type).to_string(),
            topic: buffer.topic.clone(),
            unread_count: buffer.unread_count,
            activity: buffer.activity as u8,
            nick_count: u32::try_from(buffer.users.len()).unwrap_or(u32::MAX),
            modes: buffer.modes.clone(),
            e2e_enabled: false,
        };
        self.buffers.insert(buffer.id.clone(), buffer);
        meta.e2e_enabled = matches!(
            self.buffers[&meta.id].buffer_type,
            crate::state::buffer::BufferType::Channel | crate::state::buffer::BufferType::Query
        ) && self.e2e_enabled_for_target(&meta.connection_id, &meta.name);
        self.pending_web_events
            .push(crate::web::protocol::WebEvent::BufferCreated { buffer: meta });
    }

    pub(crate) fn push_buffer_e2e_status(&mut self, buffer_id: &str) {
        let Some(buffer) = self.buffers.get(buffer_id) else {
            return;
        };
        let connection_id = buffer.connection_id.clone();
        let name = buffer.name.clone();
        let enabled = matches!(
            buffer.buffer_type,
            crate::state::buffer::BufferType::Channel | crate::state::buffer::BufferType::Query
        ) && self.e2e_enabled_for_target(&connection_id, &name);
        self.pending_web_events
            .push(crate::web::protocol::WebEvent::BufferE2eChanged {
                buffer_id: buffer_id.to_string(),
                enabled,
            });
    }

    pub(crate) fn push_all_buffer_e2e_statuses(&mut self) {
        let buffer_ids: Vec<String> = self
            .buffers
            .values()
            .filter(|buffer| {
                matches!(
                    buffer.buffer_type,
                    crate::state::buffer::BufferType::Channel
                        | crate::state::buffer::BufferType::Query
                )
            })
            .map(|buffer| buffer.id.clone())
            .collect();
        for buffer_id in buffer_ids {
            self.push_buffer_e2e_status(&buffer_id);
        }
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
        // Release whatever is still waiting on a translation BEFORE the
        // buffer goes, and while it still exists to receive them.
        //
        // These lines already arrived from IRC — the queue governs only when
        // they are allowed on screen, not whether they were received — so
        // dropping them loses messages the user was sent and never writes
        // them to storage. Ordering matters and is the whole fix: after
        // `shift_remove` the buffer-existence guard in
        // `add_message_with_activity_unshrunk` silently refuses every one of
        // them, which is what "dropping is fine, delivery would be refused
        // anyway" used to describe. The refusal was the bug.
        self.flush_translate_queue(id);
        self.own_echo_decorations.remove(id);
        // `outgoing_in_flight` is deliberately NOT dropped here. It is the
        // only remaining record that a send is still out for this
        // conversation, and closing the window does not recall it: the peer
        // may yet change nick, and without this the rename goes unrecorded
        // and the message is addressed to a nick somebody else may hold. The
        // entry ages out on its own.
        self.pending_web_events
            .push(crate::web::protocol::WebEvent::BufferClosed {
                buffer_id: id.to_string(),
            });
        self.buffers.shift_remove(id);
        self.typing.remove_buffer(id);
        // Clean up per-buffer flood tracking to prevent unbounded map growth.
        self.flood_state.remove_buffer(id);
        // Nothing should be left, but a queue must never outlive its buffer:
        // it would grow unbounded across a long session of joins and parts.
        self.translate_queues.remove(id);

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
        // Same translation routing as `add_message_with_activity`. It matters
        // here mostly for the queue-parking half: a JOIN or a notice arriving
        // while lines are still translating must take its place in the queue
        // rather than render ahead of them.
        let Some(message) = self.route_through_translation(
            buffer_id,
            message,
            ActivityLevel::None,
            ParkedShrinkRules::Notice,
        ) else {
            return;
        };
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
    /// [`Self::add_local_message`] for a row that belongs in the TIMELINE — a
    /// day separator — rather than being a response to something the user
    /// just did.
    ///
    /// Command output keeps using `add_local_message` and appears at once:
    /// holding `/help` behind a pending translation would read as a hung
    /// client. A separator is different — it dates the lines around it, so
    /// rendering it above one that arrived before midnight is simply wrong.
    pub fn add_local_message_in_order(&mut self, buffer_id: &str, message: Message) {
        if let Some(queue) = self.translate_queues.get_mut(buffer_id) {
            let id = message.id;
            queue.push_resolved_as(
                id,
                message,
                ActivityLevel::None,
                crate::translate::queue::ReadyDelivery::Local,
            );
            self.enforce_translate_ceiling(buffer_id);
            return;
        }
        self.add_local_message(buffer_id, message);
    }

    /// [`Self::add_local_message`] with the queue already consulted.
    fn deliver_local_now(&mut self, buffer_id: &str, message: Message) {
        self.add_local_message(buffer_id, message);
    }

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

    /// Returns `true` when the row was taken over by translation and will be
    /// delivered later, so the caller must not act as though it is on screen.
    ///
    /// The mention fan-out is the caller that cares: building it now would
    /// aggregate the ORIGINAL text while the channel goes on to show the
    /// translation, and the two would never agree again. It is done at
    /// release instead, in `deliver_ready`.
    pub fn add_message_with_activity(
        &mut self,
        buffer_id: &str,
        message: Message,
        level: ActivityLevel,
    ) -> bool {
        // Translation dispatch runs BEFORE shrink: the two are mutually
        // exclusive per line and translation wins. Two external round-trips
        // on one line is worse than losing shrink on a translated buffer.
        let Some(message) =
            self.route_through_translation(buffer_id, message, level, ParkedShrinkRules::Chat)
        else {
            return true;
        };
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
                // `false` on every arm: shrink defers the CHAT row but its
                // mention is pushed inline by the call site, deliberately
                // with the original text (see `push_to_mentions` above). Only
                // translation moves the fan-out to release time.
                match tx.try_send(pending) {
                    Ok(()) => return false,
                    Err(TrySendError::Full(p)) => {
                        tracing::warn!("shrink: incoming queue full, delivering unshrunk");
                        self.add_message_with_activity_unshrunk(
                            buffer_id,
                            p.message,
                            p.activity_level,
                        );
                        return false;
                    }
                    Err(TrySendError::Closed(p)) => {
                        tracing::error!("shrink: incoming worker dead, delivering unshrunk");
                        self.add_message_with_activity_unshrunk(
                            buffer_id,
                            p.message,
                            p.activity_level,
                        );
                        return false;
                    }
                }
            }
        }
        self.add_message_with_activity_unshrunk(buffer_id, message, level);
        false
    }

    /// Copy a highlighted channel line into the `_mentions` aggregate.
    ///
    /// One formatter for all three callers — the two inline pushes in
    /// `handle_privmsg` and the deferred one in [`Self::deliver_ready`] —
    /// because a mention that reads differently from the line it aggregates
    /// is the whole defect this exists to avoid.
    pub fn fan_out_mention(
        &mut self,
        conn_id: &str,
        target: &str,
        nick: &str,
        body: &str,
        ts: chrono::DateTime<chrono::Utc>,
    ) {
        if !self.buffers.contains_key("_mentions") {
            return;
        }
        let conn_label = self
            .connections
            .get(conn_id)
            .map_or(conn_id, |c| c.label.as_str());
        let datetime = ts
            .with_timezone(&chrono::Local)
            .format("%Y/%m/%d %H:%M:%S")
            .to_string();
        let mention_text = crate::state::mention_format::format_mention_line(
            &datetime,
            conn_label,
            target,
            nick,
            body,
            self.nick_color_sat,
            self.nick_color_lit,
        );
        let mention_msg = Message {
            id: self.next_message_id(),
            timestamp: ts,
            message_type: MessageType::MentionLog,
            nick: None,
            nick_mode: None,
            text: mention_text,
            highlight: true,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
            wire_origin: None,
            log_key: None,
        };
        self.add_mention_to_buffer(mention_msg);
    }

    /// [`Self::fan_out_mention`] for a row released from the reorder queue,
    /// so the aggregate carries the text the channel actually shows.
    fn fan_out_mention_for_row(&mut self, buffer_id: &str, message: &Message) {
        if !message.highlight {
            return;
        }
        let Some((conn_id, target)) = buffer_id.split_once('/') else {
            return;
        };
        if !crate::e2e::is_channel_target(target) {
            return;
        }
        let nick = message.nick.clone().unwrap_or_default();
        let body = if message.message_type == MessageType::Action {
            format!("* {nick} {}", message.text)
        } else {
            message.text.clone()
        };
        let (conn_id, target) = (conn_id.to_string(), target.to_string());
        self.fan_out_mention(&conn_id, &target, &nick, &body, message.timestamp);
    }

    /// Hold this buffer's queue position for an outgoing echo that is still
    /// being translated.
    ///
    /// Creates the queue if there is none: the barrier has to exist for the
    /// whole wait. Reserving only an id is not enough — a later incoming line
    /// can create a queue, resolve, drain and have it pruned before the echo
    /// arrives, and the echo would then be appended after it.
    pub fn reserve_echo_slot(&mut self, buffer_id: &str, id: u64) {
        self.translate_queues
            .entry(buffer_id.to_string())
            .or_default()
            .reserve(id);
        self.enforce_translate_ceiling(buffer_id);
    }

    /// How long a filed decoration stays matchable. Generous — the round
    /// trip is one server hop — but finite, so a reflection lost to a
    /// netsplit does not sit there waiting to decorate an unrelated line the
    /// user retypes verbatim much later.
    const OWN_ECHO_TTL: std::time::Duration = std::time::Duration::from_secs(30);
    /// Cap per buffer. A user cannot outrun their own echoes by this much;
    /// the bound exists so a server that stops echoing entirely cannot grow
    /// this without limit.
    pub(crate) const OWN_ECHO_MAX: usize = 32;

    /// Record how `echo-message`'s reflection of one wire line should be
    /// rendered when it comes back.
    ///
    /// See [`AppState::own_echo_decorations`] for why the reflection is
    /// decorated rather than replaced by a locally-written row.
    pub fn decorate_own_echo(&mut self, buffer_id: &str, decoration: OwnEchoDecoration) {
        let now = std::time::Instant::now();
        let mut dropped: Vec<u64> = Vec::new();
        {
            let entries = self
                .own_echo_decorations
                .entry(buffer_id.to_string())
                .or_default();
            for expired in entries
                .iter()
                .filter(|d| now.duration_since(d.filed_at) >= Self::OWN_ECHO_TTL)
                .map(|d| d.echo_id)
            {
                if !dropped.contains(&expired) {
                    dropped.push(expired);
                }
            }
            // Counted in RECORDS, dropped in MESSAGES: a group is several
            // records, so counting groups would evict far more than the cap
            // asks for.
            let mut surviving = entries
                .iter()
                .filter(|d| !dropped.contains(&d.echo_id))
                .count();
            while surviving >= Self::OWN_ECHO_MAX {
                let Some(oldest) = entries
                    .iter()
                    .find(|d| !dropped.contains(&d.echo_id))
                    .map(|d| d.echo_id)
                else {
                    break;
                };
                surviving -= entries.iter().filter(|d| d.echo_id == oldest).count();
                dropped.push(oldest);
            }
            // BY MESSAGE, never by record. One message files a record per
            // wire line, all sharing the reserved id, and dropping some while
            // keeping others is worse than dropping all: the reflections that
            // lost their record are appended behind the reservation, the one
            // that kept its record then FILLS that reservation, and the
            // message renders with its last chunk first.
            entries.retain(|d| !dropped.contains(&d.echo_id));
            entries.push_back(decoration);
        }
        self.abandon_reflections(buffer_id, dropped);
    }

    /// Give back the places held for reflections that can no longer arrive.
    ///
    /// A record is what lets a reflection find the slot reserved when the
    /// user pressed Enter. Dropping the record without the reservation leaves
    /// a barrier nothing will ever fill: the buffer stalls behind it until
    /// the queue's expiry, and every line that finished translating meanwhile
    /// is held back and then released ahead of the echo they were replies to.
    ///
    /// Reachable well short of the 32-record cap, because one translation
    /// long enough to split files a record per wire line and several sends
    /// can resolve in a burst before any of their reflections arrive.
    fn abandon_reflections(&mut self, buffer_id: &str, dropped: Vec<u64>) {
        if dropped.is_empty() {
            return;
        }
        for echo_id in dropped {
            self.abandon_own_reflection(buffer_id, echo_id);
        }
    }

    /// Give back the place held for one reflection that will not be shown.
    ///
    /// Two ways that happens, and they look identical to the queue. The
    /// record can go missing — evicted at the cap or timed out — so an
    /// arriving reflection has nothing to match. Or the reflection can arrive
    /// and be DROPPED on purpose: an ignore rule that matches our own
    /// hostmask, or a script that ate the PRIVMSG before the handler ran.
    ///
    /// Either way the reservation is a barrier nothing will ever fill, and
    /// leaving it up stalls the whole conversation until the queue's expiry —
    /// lines that finished translating meanwhile are held back and then
    /// released in a burst, ahead of the echo they were replies to.
    pub fn abandon_own_reflection(&mut self, buffer_id: &str, echo_id: u64) {
        tracing::debug!(
            buffer_id,
            echo_id,
            "translate: reflection will not be shown; releasing the place held for it"
        );
        self.release_echo_slot(buffer_id, echo_id);
        // Releasing at the head makes it and everything resolved behind it
        // deliverable, and nothing else on this path would revisit the queue.
        self.drain_translate_ready(buffer_id);
    }

    /// Drop the records kept for reservations that have just timed out.
    ///
    /// The mirror of [`Self::abandon_own_reflection`], and the half that was
    /// missing: that one gives a place back when its record dies, this one
    /// throws a record away when its place does. The two lifetimes are one
    /// thing — a record exists only to let a reflection find its slot — and
    /// letting either outlive the other breaks the pairing in a different
    /// direction.
    ///
    /// This direction is the sharper one, because a record left behind is not
    /// inert: [`Self::take_own_echo_decoration`] matches by wire text, oldest
    /// first, so a stale record is consumed by the NEXT reflection carrying
    /// the same text. With the default 5-second budget against the record's
    /// 30-second TTL there is a 25-second window in which the natural reaction
    /// to a message that never appeared — retyping it — reflects into the dead
    /// record: the row is decorated with the FIRST send's original, and the
    /// second send's reservation, which the reflection should have filled, is
    /// left barricading the buffer for another full timeout.
    ///
    /// Only timeouts. The ceiling (§3.8) also ends reservations, but it is not
    /// evidence the reflection is lost — it takes the POSITION back while the
    /// send is still in flight — so the record stays and the reflection still
    /// renders with its original, merely out of place.
    pub fn forget_own_echo_records(&mut self, buffer_id: &str, ids: &[u64]) {
        if ids.is_empty() {
            return;
        }
        let Some(entries) = self.own_echo_decorations.get_mut(buffer_id) else {
            return;
        };
        let before = entries.len();
        entries.retain(|d| !ids.contains(&d.echo_id));
        if entries.len() != before {
            tracing::debug!(
                buffer_id,
                dropped = before - entries.len(),
                "translate: reservation timed out; forgetting the record kept for it"
            );
        }
        if entries.is_empty() {
            self.own_echo_decorations.remove(buffer_id);
        }
    }

    /// Build a reflection record, stamped now.
    pub fn own_echo_decoration(
        wire_text: String,
        echo_id: u64,
        display: Option<(String, crate::state::buffer::WireOrigin)>,
        is_last: bool,
    ) -> OwnEchoDecoration {
        OwnEchoDecoration {
            wire_text,
            echo_id,
            display,
            is_last,
            filed_at: std::time::Instant::now(),
        }
    }

    /// Consume the decoration matching this reflected line, if there is one.
    ///
    /// Consuming rather than peeking is what makes sending the same text
    /// twice work: the first reflection takes the first record, the second
    /// takes the second. Matching the OLDEST record first keeps that in send
    /// order.
    pub fn take_own_echo_decoration(
        &mut self,
        buffer_id: &str,
        wire_text: &str,
    ) -> Option<OwnEchoDecoration> {
        let now = std::time::Instant::now();
        let mut dropped: Vec<u64> = Vec::new();
        let entries = self.own_echo_decorations.get_mut(buffer_id)?;
        // Expiry is by MESSAGE too — see `decorate_own_echo`.
        for expired in entries
            .iter()
            .filter(|d| now.duration_since(d.filed_at) >= Self::OWN_ECHO_TTL)
            .map(|d| d.echo_id)
        {
            if !dropped.contains(&expired) {
                dropped.push(expired);
            }
        }
        entries.retain(|d| !dropped.contains(&d.echo_id));
        let found = entries
            .iter()
            .position(|d| d.wire_text == wire_text)
            .map(|pos| entries.remove(pos).expect("position just found it"));
        // The record we just took is about to fill its reservation, so an
        // expired SIBLING of the same message must not release it first.
        if let Some(taken) = found.as_ref() {
            dropped.retain(|id| *id != taken.echo_id);
        }
        if entries.is_empty() {
            self.own_echo_decorations.remove(buffer_id);
        }
        // A record that timed out leaves the same orphaned barrier as one
        // evicted at the cap.
        self.abandon_reflections(buffer_id, dropped);
        found
    }

    /// How long a re-keyed buffer keeps answering to its old id.
    ///
    /// Only in-flight translation work needs this, so the window is the one
    /// that matters: comfortably longer than any request, short enough that a
    /// stranger who later claims the abandoned nick does not inherit a
    /// redirect meant for its previous owner.
    const REDIRECT_TTL: std::time::Duration = std::time::Duration::from_secs(300);

    /// [`Self::REDIRECT_TTL`], for callers outside this module that need the
    /// same window — a nick change has to be recorded for as long as a
    /// redirect could still be consulted.
    #[must_use]
    pub const fn redirect_ttl() -> std::time::Duration {
        Self::REDIRECT_TTL
    }

    /// How many occupancies of one buffer id are remembered at a time.
    ///
    /// A nick passed around inside the TTL would otherwise grow this list
    /// without bound. Dropping the OLDEST is safe because every era carries
    /// its own window: work that fell off resolves to no redirect at all,
    /// which the delivery path treats as "the conversation is gone" and
    /// refuses — the fail-closed answer.
    const REDIRECT_ERAS_MAX: usize = 16;

    /// Where work dispatched against `buffer_id` at `dispatched_at` should go
    /// now, if that buffer has since been re-keyed.
    ///
    /// The decision is made on TIME, not on whether a buffer still exists
    /// under the old id. Both questions have to be answered at once and only
    /// the timestamp answers them:
    ///
    /// - Work dispatched BEFORE the rename belongs to the conversation that
    ///   moved, so it follows — even if somebody has since claimed the
    ///   abandoned nick and opened a fresh query under the same id.
    /// - Work dispatched AFTER belongs to whoever holds that nick now, so it
    ///   stays put.
    ///
    /// An earlier version keyed on "is there a live buffer under the old id",
    /// which gets the second case right and the first case catastrophically
    /// wrong: it silently addresses the stale name, so a pending private
    /// message is delivered to the stranger who took the nick.
    ///
    /// The answer comes from the ERA that held the id at `dispatched_at`, not
    /// from the most recent rename off it. A query id is a nick, and a nick
    /// passes from one conversation to the next: keeping only the newest
    /// mapping hands work dispatched under the first occupant to whoever
    /// occupied it last.
    #[must_use]
    pub fn redirected_buffer_id(
        &self,
        buffer_id: &str,
        dispatched_at: std::time::Instant,
    ) -> crate::state::BufferRedirect<'_> {
        use crate::state::BufferRedirect;
        let Some(eras) = self.buffer_redirects.get(buffer_id) else {
            // Nothing on record. Either this id never moved, or every era
            // aged out — and an era that aged out ended at least
            // `REDIRECT_TTL` ago, so it cannot have covered work dispatched
            // inside that window. Older than the window, we cannot say.
            return if dispatched_at.elapsed() >= Self::REDIRECT_TTL {
                BufferRedirect::Unknown
            } else {
                BufferRedirect::Stays
            };
        };
        if let Some(era) = eras.iter().find(|era| {
            era.ended_at.elapsed() < Self::REDIRECT_TTL
                && era.started_at.is_none_or(|from| from <= dispatched_at)
                && dispatched_at <= era.ended_at
        }) {
            return BufferRedirect::MovedTo(era.target.as_str());
        }
        // No era covers it. That is an answer only if we can see far enough
        // back: past the TTL, or before the earliest era we still hold, a
        // rename we have dropped may have covered this work.
        let floor = eras.first().and_then(|era| era.started_at);
        if dispatched_at.elapsed() >= Self::REDIRECT_TTL
            || floor.is_some_and(|from| dispatched_at < from)
        {
            BufferRedirect::Unknown
        } else {
            BufferRedirect::Stays
        }
    }

    /// Note that the peer of a query LEFT the network.
    ///
    /// See [`AppState::query_departures`]. Recorded whatever the display
    /// rules say about the QUIT itself — an ignored quit and a netsplit quit
    /// free the nick exactly as loudly as any other, and this is a fact about
    /// identity, not a message.
    pub fn note_query_departure(&mut self, buffer_id: &str) {
        let now = std::time::Instant::now();
        self.query_departures
            .retain(|_, at| now.duration_since(*at) < Self::REDIRECT_TTL);
        self.query_departures.insert(buffer_id.to_string(), now);
    }

    /// Whether the peer of `buffer_id` left after work was dispatched against
    /// it — the window in which a translation, and only a translation, is
    /// still holding the user's text.
    ///
    /// A departure recorded BEFORE the dispatch is deliberately not a refusal:
    /// text typed into a query whose peer had already gone is the same gamble
    /// with or without translation, and turning that into a refusal would be a
    /// policy for the whole client rather than for this mechanism.
    #[must_use]
    pub fn query_departed_since(
        &self,
        buffer_id: &str,
        dispatched_at: std::time::Instant,
    ) -> bool {
        self.query_departures
            .get(buffer_id)
            .is_some_and(|at| *at >= dispatched_at)
    }

    /// Drop the reflection records held for an id whose conversation is being
    /// replaced, releasing whatever they were holding.
    ///
    /// A record says "when the server reflects THIS wire text back, render it
    /// like so" — and the only thing it is matched on is that text. It is
    /// therefore meaningful only inside the conversation that filed it: once
    /// the id belongs to somebody else, the next identical line consumes a
    /// record meant for a different person.
    ///
    /// The reservations go with them, because a record that will never be
    /// matched leaves a barrier nothing can fill — the same orphan an expired
    /// or evicted record leaves, and released the same way.
    pub fn retire_own_echo_decorations(&mut self, buffer_id: &str) {
        let Some(entries) = self.own_echo_decorations.remove(buffer_id) else {
            return;
        };
        let mut ids: Vec<u64> = Vec::new();
        for d in &entries {
            if !ids.contains(&d.echo_id) {
                ids.push(d.echo_id);
            }
        }
        tracing::debug!(
            buffer_id,
            count = ids.len(),
            "translate: the nick changed hands; dropping its pending reflections"
        );
        self.abandon_reflections(buffer_id, ids);
    }

    /// Take the queue away from an id whose CONVERSATION is being replaced,
    /// writing what it held to the log and showing none of it.
    ///
    /// The rows belong to whoever held this nick until a moment ago, and the
    /// window that could have shown them is being handed to somebody else in
    /// the same breath — so delivering them normally would print one person's
    /// private lines inside another's conversation, log them there, and
    /// broadcast them to every open web tab under that id.
    ///
    /// Dropping them outright is not the answer either: the network delivered
    /// them and the log is the record of what was said. The log is keyed by
    /// the NAME, which both occupants share, so a row written there is
    /// indistinguishable from any other line that nick sent — which is the
    /// truth of the situation rather than a compromise.
    pub fn retire_translate_queue(&mut self, buffer_id: &str) {
        let Some(mut queue) = self.translate_queues.remove(buffer_id) else {
            return;
        };
        let ready = queue.flush_all();
        if ready.is_empty() {
            return;
        }
        tracing::warn!(
            buffer_id,
            count = ready.len(),
            "translate: the nick was taken over; logging its queued lines \
             without showing them in the new conversation"
        );
        for entry in ready {
            self.translate_tally.record(&entry.origin);
            // Only rows that would have been logged on the way out. A
            // transient placeholder or a local notice is display-only by
            // contract, and a contract does not change because the window
            // did.
            if matches!(
                entry.delivery,
                crate::translate::queue::ReadyDelivery::Logged
            ) {
                let _stored = self.maybe_log(buffer_id, &entry.message);
            }
        }
    }

    /// Move every buffer-id-keyed map from `old_id` to `new_id`.
    ///
    /// Called when a query buffer is re-keyed because the peer changed nick.
    /// The buffer itself moves in `rename_query_buffers`; these are the side
    /// tables that would otherwise be orphaned under a key nothing looks up
    /// again.
    ///
    /// `translate_buffers` is a mirror of `config.translate.buffers`, so the
    /// config key is migrated too (by the App, which owns it) — otherwise the
    /// next `sync_translate_from_config` would re-derive this map from the
    /// stale config and undo the move.
    pub fn rekey_buffer_state(&mut self, old_id: &str, new_id: &str) {
        if old_id == new_id {
            return;
        }
        if let Some(queue) = self.translate_queues.remove(old_id) {
            // The new id may still hold its PREVIOUS occupant's queue — a
            // stale query under the very nick this conversation is renaming
            // onto, with lines the network already delivered still waiting in
            // it. Installing ours over it would drop them unseen and
            // unlogged; release them into the buffer first, exactly as a
            // buffer close would have.
            if self.translate_queues.contains_key(new_id) {
                self.flush_translate_queue(new_id);
            }
            self.translate_queues.insert(new_id.to_string(), queue);
        }
        if let Some(cfg) = self.translate_buffers.remove(old_id) {
            self.translate_buffers.insert(new_id.to_string(), cfg);
        }
        // Records under the NEW id belong to whoever held that nick until
        // now, and a reflection is matched by its wire TEXT alone — nothing
        // in it says which conversation it came from. Left in place, the
        // arriving conversation's "ok" consumes the departed occupant's
        // "ok": the reflection is decorated with a stranger's original, and
        // its own reservation stays up as a barrier nothing will ever fill,
        // holding the buffer until the queue expires.
        //
        // Retired rather than replaced, because the moving buffer may have no
        // records of its own — and then a plain insert leaves the old ones
        // exactly where they do harm.
        self.retire_own_echo_decorations(new_id);
        if let Some(echoes) = self.own_echo_decorations.remove(old_id) {
            self.own_echo_decorations.insert(new_id.to_string(), echoes);
        }
        if let Some(in_flight) = self.outgoing_in_flight.remove(old_id) {
            // MERGED, never inserted over: markers may already sit under the
            // new id (the previous occupant's send still at the provider),
            // and replacing them would let this buffer's next ordinary send
            // skip the in-flight refusal and overtake it on the wire. The
            // markers are ages, not identities, so combining the two queues
            // keeps both counts honest and they age out individually.
            self.outgoing_in_flight
                .entry(new_id.to_string())
                .or_default()
                .extend(in_flight);
        }
        // Work already dispatched still carries the old id, so it needs a way
        // to find its way here. Existing redirects INTO the old id are
        // repointed rather than chained, so a peer who renames twice while
        // one line is in flight still resolves in a single hop.
        let now = std::time::Instant::now();
        for eras in self.buffer_redirects.values_mut() {
            for era in eras.iter_mut().filter(|e| e.target == old_id) {
                // Repoint WITHOUT touching the window. The window answers
                // "which work does this apply to", and that was settled by
                // the rename that created the era — a second rename changes
                // only where the conversation went.
                //
                // Widening it would cover work dispatched between the two
                // renames, which may belong to somebody who claimed the
                // abandoned nick in the meantime. Their private message would
                // then be redirected to the original peer.
                //
                // Only eras still pointing AT `old_id` are touched, and those
                // are by construction the conversation that is renaming now:
                // any earlier occupant's era was repointed away when IT
                // renamed.
                era.target = new_id.to_string();
            }
        }
        let eras = self.buffer_redirects.entry(old_id.to_string()).or_default();
        // This era began where the previous one ended. The id may have been
        // handed from conversation to conversation, and each hand-off is the
        // boundary that keeps one occupant's work from resolving to another's
        // window.
        let started_at = eras.last().map(|prev| prev.ended_at);
        eras.push(crate::state::RedirectEra {
            target: new_id.to_string(),
            started_at,
            ended_at: now,
        });
        // Oldest first, so a nick passed around quickly cannot grow this. The
        // bound is on the FRONT because each era carries its own window: an
        // expired one can be dropped without widening the one behind it.
        while eras.len() > Self::REDIRECT_ERAS_MAX {
            eras.remove(0);
        }
        for eras in self.buffer_redirects.values_mut() {
            eras.retain(|era| era.ended_at.elapsed() < Self::REDIRECT_TTL);
        }
        self.buffer_redirects.retain(|_, eras| !eras.is_empty());
        self.pending_buffer_rekeys
            .push((old_id.to_string(), new_id.to_string()));
    }

    /// Note that an outgoing send for this buffer has entered the translator.
    pub fn note_outgoing_dispatch(&mut self, buffer_id: &str) {
        self.outgoing_in_flight
            .entry(buffer_id.to_string())
            .or_default()
            .push_back(std::time::Instant::now());
    }

    /// Clear one of this buffer's in-flight markers — the send came back,
    /// whatever the outcome.
    pub fn clear_outgoing_dispatch(&mut self, buffer_id: &str) {
        if let Some(times) = self.outgoing_in_flight.get_mut(buffer_id) {
            times.pop_front();
            if times.is_empty() {
                self.outgoing_in_flight.remove(buffer_id);
            }
        }
    }

    /// Outgoing sends still at the provider, per buffer, for `/translate
    /// status`. Counted from the markers rather than from reservations: the
    /// queue ceiling can take a reservation back while its send runs on.
    #[must_use]
    pub fn outgoing_in_flight_counts(
        &self,
        max_age: std::time::Duration,
    ) -> Vec<(String, usize)> {
        let mut rows: Vec<(String, usize)> = self
            .outgoing_in_flight
            .iter()
            .map(|(id, times)| {
                (
                    id.clone(),
                    times.iter().filter(|at| at.elapsed() < max_age).count(),
                )
            })
            .filter(|(_, live)| *live > 0)
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    }

    /// Whether one of this buffer's own messages is still in the translator.
    ///
    /// `max_age` bounds how long a marker is believed. Every delivery clears
    /// one, but a path that somehow does not must not leave this buffer's
    /// ordinary sends refused forever — the send it stands for cannot outlive
    /// its own budget either way.
    #[must_use]
    pub fn has_outgoing_in_flight(&self, buffer_id: &str, max_age: std::time::Duration) -> bool {
        self.outgoing_in_flight
            .get(buffer_id)
            .is_some_and(|times| times.iter().any(|at| at.elapsed() < max_age))
    }

    /// Give up a reservation whose message will never arrive.
    ///
    /// Prunes the queue when that emptied it. A queue's mere EXISTENCE is
    /// what routes every later row through it (`route_through_translation`
    /// gates on `contains_key`), so an empty queue left behind by a
    /// dispatch-side refusal parks the buffer's next lines — incoming chat,
    /// JOINs, their mention alerts — until the one-second tick prunes it,
    /// and on an outgoing-only buffer no incoming outcome will ever arrive
    /// to drain them sooner.
    pub fn release_echo_slot(&mut self, buffer_id: &str, id: u64) {
        if let Some(queue) = self.translate_queues.get_mut(buffer_id) {
            queue.release_reserved(id);
            if queue.is_empty() {
                self.translate_queues.remove(buffer_id);
            }
        }
    }

    /// [`Self::release_echo_slot`] when the buffer id the reservation was
    /// filed under can no longer be resolved — a redirect that outlived its
    /// history. Message ids are globally unique, so scanning every queue for
    /// the one holding this reservation is exact; returns the buffer id it
    /// was found under so the caller can drain what the release unblocked.
    pub fn release_echo_slot_anywhere(&mut self, id: u64) -> Option<String> {
        let holder = self
            .translate_queues
            .iter()
            .find_map(|(bid, q)| q.holds_reservation(id).then(|| bid.clone()))?;
        self.release_echo_slot(&holder, id);
        Some(holder)
    }

    /// Deliver a line we authored ourselves — a deferred local echo.
    ///
    /// It must never be sent for translation: we wrote it, and an outgoing
    /// translated message has already been through the pipeline. It must
    /// still take its place in the buffer's queue, so it cannot render ahead
    /// of lines queued before it.
    ///
    /// This exists because the nick check in `translate_should_dispatch` is
    /// not sufficient for a DEFERRED echo. Those carry the nick captured at
    /// dispatch time — deliberately, so the echo matches what peers saw — so
    /// a `/nick` during the wait leaves the echo's nick different from the
    /// connection's, and the "is this ours" test would wrongly say no and
    /// translate our own message a second time.
    pub fn add_own_message(&mut self, buffer_id: &str, order_key: u64, message: Message) {
        self.add_own_message_chunks(buffer_id, order_key, vec![message]);
    }

    /// Park one row of a split own message at the place reserved for it,
    /// leaving the barrier up.
    ///
    /// A translated send long enough to split comes back as several
    /// reflections, one IRC message at a time. Treating each as a completed
    /// echo closes the reservation on the first: the barrier lifts, every
    /// line that finished translating behind it drains, and the remaining
    /// chunks of the user's own sentence render after the replies to it. So
    /// all but the last are held, and only the last calls
    /// [`Self::add_own_message`].
    ///
    /// Falls back to delivering the row when there is no reservation to hold
    /// it — timed out, flushed, or the buffer was closed. Holding a chunk of
    /// a message the server has already sent us is a positioning device, and
    /// it must never become a way to lose one.
    pub fn hold_own_message_chunk(&mut self, buffer_id: &str, order_key: u64, message: Message) {
        let unheld = match self.translate_queues.get_mut(buffer_id) {
            Some(queue) => queue.hold_in_reserved(order_key, message, ActivityLevel::None),
            None => Some(message),
        };
        if let Some(message) = unheld {
            self.add_own_message_chunks(buffer_id, order_key, vec![message]);
        }
    }

    /// [`Self::add_own_message`] for a message that became several rows.
    ///
    /// `order_key` is the id reserved at submission and is what places these
    /// rows in the queue — all of them, together, in the ONE place held for
    /// them. It is deliberately NOT each row's `Message::id`: those are
    /// transport identities and must stay distinct, because the web client
    /// treats two live rows sharing an id as the same message and drops the
    /// second. Conflating the two silently swallowed every chunk after the
    /// first — usually including the one carrying ` [original]`.
    pub fn add_own_message_chunks(
        &mut self,
        buffer_id: &str,
        order_key: u64,
        chunks: Vec<Message>,
    ) {
        if chunks.is_empty() {
            return;
        }
        let first_id = order_key;
        if let Some(queue) = self.translate_queues.get_mut(buffer_id) {
            let rows: Vec<(Message, ActivityLevel)> = chunks
                .iter()
                .cloned()
                .map(|m| (m, ActivityLevel::None))
                .collect();
            // Fill the place held at submission when there is one. Failing
            // that, insert by id — a deferred echo's id was allocated when
            // the user pressed Enter, so appending would render their own
            // message after the replies to it.
            if !queue.fill_reserved_with(first_id, rows) {
                for message in chunks {
                    queue.insert_resolved_in_order(first_id, message, ActivityLevel::None);
                }
            }
            // A split echo adds rows beyond the one reserved place.
            self.enforce_translate_ceiling(buffer_id);
            // Filling a reservation lifts a barrier, so this row and
            // everything resolved behind it are deliverable now. The
            // outgoing arm drains for itself, but a reflection arriving on
            // the IRC path has nothing else that would.
            self.drain_translate_ready(buffer_id);
            return;
        }
        for message in chunks {
            self.add_message_unshrunk(buffer_id, message);
        }
    }

    /// Decide what happens to a message on a translation-enabled buffer.
    ///
    /// Returns `Some(message)` when the caller should deliver it normally,
    /// and `None` when the message has been taken over — either dispatched
    /// for translation or parked in the buffer's reorder queue.
    ///
    /// The E2E check uses the FAIL-CLOSED [`AppState::e2e_possible_for_target`],
    /// never the advisory `e2e_enabled_for_target`. The advisory resolves a
    /// keyring read error, an unresolved DM handle, and a pre-migration
    /// multi-network row all to `false` — states the send gate still REFUSES
    /// as E2E-enabled. Translating on any of them would ship the plaintext
    /// of an end-to-end-protected conversation to a third-party provider
    /// before the refusal ever ran. This is the same reasoning, and the same
    /// predicate, as the outgoing shrink gate in `src/app/input.rs`, which
    /// was written for the identical problem: an external service that
    /// receives cleartext before the send gate runs may only be used when
    /// E2E is definitively ruled out.
    ///
    /// Placing the check here rather than only in `/translate addin` is what
    /// makes enabling order irrelevant: `/e2e on` after `/translate addin`
    /// simply stops translation from the next line, with no state to keep in
    /// sync.
    fn route_through_translation(
        &mut self,
        buffer_id: &str,
        message: Message,
        level: ActivityLevel,
        parked_shrink: ParkedShrinkRules,
    ) -> Option<Message> {
        // A buffer with a non-empty queue takes EVERYTHING through the
        // queue, translatable or not. Otherwise a JOIN renders before the
        // lines queued ahead of it and the timeline reorders silently — the
        // same failure the queue exists to prevent, from the other side.
        if !self.translate_should_dispatch(buffer_id, &message) {
            if self.translate_queues.contains_key(buffer_id) {
                // A row parked only for ORDERING still gets its shrink. The
                // park used to swallow the shrink dispatch outright, so the
                // same NOTICE rendered shortened or raw depending on whether
                // a translation happened to be in flight.
                let Some(message) =
                    self.offer_parked_row_to_shrink(buffer_id, message, level, parked_shrink)
                else {
                    // Shrink took it, and it is in the queue as a RESERVATION
                    // — the same room a parked row takes, on the same ceiling.
                    // Without this a burst of long-URL lines grows the queue
                    // to the shrink CHANNEL's capacity instead of
                    // `translate.max_queue`, and the bound the user configured
                    // only reasserts itself on the next maintenance tick.
                    self.enforce_translate_ceiling(buffer_id);
                    return None;
                };
                let id = message.id;
                let queue = self.translate_queues.get_mut(buffer_id)?;
                queue.push_resolved(id, message, level);
                // JOINs, notices and events land in the same queue and take
                // up the same room. A busy channel is exactly where the
                // ceiling is supposed to bite.
                self.enforce_translate_ceiling(buffer_id);
                return None;
            }
            return Some(message);
        }
        // A reassembled `draft/multiline` message carries newlines. The
        // contract is one line per request, and handing the whole thing over
        // makes a backend collapse and reorder words ACROSS line boundaries —
        // the shipped stub does exactly that. It is eligible but not
        // translatable, so it is a GAP: delivered marked, the same as the
        // outgoing side refusing multiline rather than mangling it.
        if message.text.contains('\n') {
            return self.deliver_untranslated_in_order(
                buffer_id,
                message,
                level,
                &crate::translate::UntranslatedReason::Error(
                    "multi-line message".to_string(),
                ),
            );
        }
        self.dispatch_for_translation(buffer_id, message, level)
    }

    /// Offer a row that is about to be PARKED for ordering to the incoming
    /// shrink worker, mirroring the gate it would have passed on an
    /// unqueued buffer. Returns the message back when shrink is not taking
    /// it.
    ///
    /// Order still has to hold, so the row's place is RESERVED before
    /// dispatch and the deferred deliver fills it
    /// ([`Self::deliver_shrunk_in_order`]); on a full or dead worker the
    /// reservation is taken back and the caller parks the row raw, exactly
    /// as before.
    fn offer_parked_row_to_shrink(
        &mut self,
        buffer_id: &str,
        message: Message,
        level: ActivityLevel,
        rules: ParkedShrinkRules,
    ) -> Option<Message> {
        if !self.shrink_incoming_active {
            return Some(message);
        }
        // Mirrors the two arrival gates precisely — the shrink blocks in
        // `add_message` (NOTICE rules) and `add_message_with_activity`.
        let eligible = match rules {
            ParkedShrinkRules::Notice => {
                message.message_type == MessageType::Notice
                    && message.nick.as_deref().is_some_and(|n| !n.is_empty())
                    && {
                        let our_nick = self
                            .buffers
                            .get(buffer_id)
                            .and_then(|b| self.connections.get(&b.connection_id))
                            .map(|c| c.nick.as_str());
                        !match (our_nick, message.nick.as_deref()) {
                            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                            _ => false,
                        }
                    }
            }
            ParkedShrinkRules::Chat => true,
        };
        if !eligible {
            return Some(message);
        }
        let urls =
            crate::shrink::find_long_urls(&message.text, self.shrink_min_url_length as usize);
        if urls.is_empty() {
            return Some(message);
        }
        let Some(tx) = self.shrink_incoming_tx.clone() else {
            return Some(message);
        };
        // Reserve BEFORE dispatching, so the shrunk text comes back exactly
        // where the raw row would have been.
        let id = message.id;
        let Some(queue) = self.translate_queues.get_mut(buffer_id) else {
            return Some(message);
        };
        queue.reserve(id);
        let pending = crate::app::shrink::PendingIncoming {
            buffer_id: buffer_id.to_string(),
            message,
            activity_level: level,
            urls,
            push_to_mentions: false,
        };
        match tx.try_send(pending) {
            Ok(()) => None,
            Err(TrySendError::Full(p) | TrySendError::Closed(p)) => {
                tracing::warn!("shrink: incoming queue unavailable, parking the row raw");
                if let Some(queue) = self.translate_queues.get_mut(buffer_id) {
                    queue.release_reserved(id);
                }
                Some(p.message)
            }
        }
    }

    /// Deliver a shrink-worker result, filling the place reserved for it
    /// when the row was parked for ordering at dispatch.
    ///
    /// The reservation may be gone — expired, flushed, or the buffer closed
    /// — in which case the row is inserted by its id when a queue still
    /// exists (the id was allocated at arrival, so this keeps it ahead of
    /// anything newer) and delivered directly otherwise.
    pub(crate) fn deliver_shrunk_in_order(
        &mut self,
        buffer_id: &str,
        message: Message,
        level: ActivityLevel,
    ) {
        let id = message.id;
        if let Some(queue) = self.translate_queues.get_mut(buffer_id) {
            if queue.holds_reservation(id) {
                let filled = queue.fill_reserved_with(id, vec![(message, level)]);
                debug_assert!(filled, "the reservation was checked just above");
                self.drain_translate_ready(buffer_id);
                return;
            }
            queue.insert_resolved_in_order(id, message, level);
            self.drain_translate_ready(buffer_id);
            return;
        }
        self.add_message_with_activity_unshrunk(buffer_id, message, level);
    }

    /// Deliver a line that will not be translated after all, without letting
    /// it overtake lines already queued for this buffer.
    ///
    /// Returning it to the caller for immediate display would put it on
    /// screen ahead of older lines still waiting on their translations —
    /// breaking the one guarantee the reorder queue exists to provide, and
    /// doing so exactly when the channel is busiest, which is when a full
    /// worker queue happens.
    fn deliver_untranslated_in_order(
        &mut self,
        buffer_id: &str,
        mut message: Message,
        level: ActivityLevel,
        reason: &crate::translate::UntranslatedReason,
    ) -> Option<Message> {
        // Mark it. A line that failed to even reach the worker is a GAP, and
        // delivering it unchanged makes it indistinguishable from one the
        // broker correctly decided to leave alone — the same invisible
        // failure the marker exists to prevent, just from the dispatch side.
        let (text, offset) = crate::translate::mark_untranslated(&message.text, reason);
        let original = std::mem::replace(&mut message.text, text);
        message.wire_origin = Some(crate::state::buffer::WireOrigin {
            text: original,
            suffix_at: offset,
        });

        if let Some(queue) = self.translate_queues.get_mut(buffer_id) {
            let id = message.id;
            queue.push_untranslated(id, message, level, reason.clone());
            // A fallback row lands in the same queue and counts against the
            // same ceiling — and these arrive exactly when the provider is
            // already in trouble, which is when the bound matters.
            self.enforce_translate_ceiling(buffer_id);
            return None;
        }
        // No queue to carry the reason, so it is counted here instead. This
        // is the same line either way and must appear in the tally exactly
        // once, whichever branch it took.
        self.translate_tally
            .record(&crate::translate::queue::ReadyOrigin::Untranslated(
                reason.clone(),
            ));
        // Deliver it here rather than handing it back. Returning `Some` puts
        // it back into `add_message_with_activity`, which then offers it to
        // incoming SHRINK — so a line already marked `[untranslated: …]`
        // would be sent to a second external service, come back rewritten,
        // and have its `wire_origin` overwritten: the marker offset stops
        // pointing at the marker and the wire text stops being the wire text.
        // Translation and shrink are mutually exclusive per line by design,
        // and that has to hold on the failure path too.
        //
        // The fan-out happens HERE because this branch is the delivery: the
        // caller is told the row was taken over, so it skips its inline
        // mention push, and nothing releases from a queue that was never
        // created — the mention would simply be lost. `deliver_ready` does
        // the same job for every row that really does go through a queue.
        self.fan_out_mention_for_row(buffer_id, &message);
        self.add_message_with_activity_unshrunk(buffer_id, message, level);
        None
    }

    /// Whether this message on this buffer should be sent for translation.
    fn translate_should_dispatch(&self, buffer_id: &str, message: &Message) -> bool {
        if !self.translate_active || self.translate_incoming_tx.is_none() {
            return false;
        }
        // Only real chat lines are translated. Events, notices from the
        // server, and pre-formatted mention rows are structure, not prose.
        if !matches!(message.message_type, MessageType::Message | MessageType::Action) {
            return false;
        }
        if !self
            .translate_buffers
            .get(buffer_id)
            .is_some_and(|c| c.incoming)
        {
            return false;
        }
        let Some(buffer) = self.buffers.get(buffer_id) else {
            return false;
        };
        // Our own echo is already in the language we typed it in.
        if let Some(conn) = self.connections.get(&buffer.connection_id)
            && message
                .nick
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(&conn.nick))
        {
            return false;
        }
        let conn_id = buffer.connection_id.clone();
        let target = buffer.name.clone();
        !self.e2e_possible_for_target(&conn_id, &target)
    }

    /// Reserve the line's place in the queue and hand it to the worker.
    ///
    /// On a full or dead worker queue the message is delivered untranslated
    /// rather than dropped — the same fallback shrink uses.
    fn dispatch_for_translation(
        &mut self,
        buffer_id: &str,
        message: Message,
        level: ActivityLevel,
    ) -> Option<Message> {
        let buffer = self.buffers.get(buffer_id)?;
        let (network, casemapping) = self
            .connections
            .get(&buffer.connection_id)
            .map_or_else(
                || (String::new(), "rfc1459".to_string()),
                |connection| {
                    (
                        connection.label.clone(),
                        connection.isupport_parsed.casemapping().to_string(),
                    )
                },
            );
        let target = buffer.name.clone();
        let known_nicks: Vec<String> = buffer.users.keys().cloned().collect();
        // One resolver for both directions — see `resolve_langs`.
        let (source_lang, target_lang) = crate::translate::resolve_langs(
            self.translate_buffers.get(buffer_id),
            &self.translate_my_lang,
        )
        .incoming();

        let id = message.id;
        let original = message.text.clone();
        let req = crate::translate::TranslateRequest {
            id,
            direction: crate::translate::Direction::Incoming,
            network,
            target,
            nick: message.nick.clone().unwrap_or_default(),
            text: original.clone(),
            source_lang,
            target_lang,
            deadline: None,
            casemapping,
            known_nicks,
        };

        let Some(tx) = self.translate_incoming_tx.as_ref() else {
            return self.deliver_untranslated_in_order(
                buffer_id,
                message,
                level,
                &crate::translate::UntranslatedReason::NoProvider,
            );
        };
        match tx.try_send(crate::app::translate::PendingTranslate {
            buffer_id: buffer_id.to_string(),
            req,
            submitted_at: std::time::Instant::now(),
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                tracing::warn!("translate: incoming queue full, delivering untranslated");
                return self.deliver_untranslated_in_order(
                    buffer_id,
                    message,
                    level,
                    &crate::translate::UntranslatedReason::Error(
                        "translation queue full".to_string(),
                    ),
                );
            }
            Err(TrySendError::Closed(_)) => {
                tracing::error!("translate: incoming worker dead, delivering untranslated");
                return self.deliver_untranslated_in_order(
                    buffer_id,
                    message,
                    level,
                    &crate::translate::UntranslatedReason::NoProvider,
                );
            }
        }

        let show_original = self.translate_show_original_in;
        self.translate_queues
            .entry(buffer_id.to_string())
            .or_default()
            .push_pending(
                id,
                original,
                crate::translate::queue::PendingPayload {
                    message,
                    activity: level,
                    show_original,
                },
            );
        self.enforce_translate_ceiling(buffer_id);
        None
    }

    /// The one way a line leaves the reorder queue for a buffer.
    ///
    /// Every release path funnels through here — the ordinary drain, the
    /// flush on close or disconnect, and the ceiling — so the running tally
    /// cannot fall behind by someone adding a fourth. Counting at each call
    /// site instead is how a mirror goes stale, which this file has already
    /// paid for once.
    pub(crate) fn deliver_ready(&mut self, buffer_id: &str, ready: Vec<crate::translate::queue::ReadyEntry>) {
        for entry in ready {
            self.translate_tally.record(&entry.origin);
            if let Some(reason) = entry.origin.gap() {
                tracing::debug!(
                    buffer_id,
                    id = entry.id,
                    reason = %reason.label(),
                    "translate: line delivered untranslated"
                );
            }
            // Each row leaves the way it would have arrived. A day separator
            // or an E2E placeholder took its place in the queue so it could
            // not render ahead of the lines queued before it, but it must not
            // pick up logging on the way out.
            match entry.delivery {
                crate::translate::queue::ReadyDelivery::Logged => {
                    // Now, not when it was queued: the aggregate has to carry
                    // the text the channel ends up showing, marker and all.
                    self.fan_out_mention_for_row(buffer_id, &entry.message);
                    self.add_message_with_activity_unshrunk(
                        buffer_id,
                        entry.message,
                        entry.activity,
                    );
                }
                crate::translate::queue::ReadyDelivery::Transient => {
                    self.deliver_transient_now(buffer_id, entry.message, entry.activity);
                }
                crate::translate::queue::ReadyDelivery::Local => {
                    self.deliver_local_now(buffer_id, entry.message);
                }
            }
        }
    }

    /// Release everything this buffer's queue has ready, and drop the queue
    /// if that emptied it.
    ///
    /// Lives here rather than only on `App` because filling a reservation
    /// happens from the IRC path — an `echo-message` reflection taking the
    /// place held for it — and that path has no way back up to the App to
    /// ask for a drain.
    pub fn drain_translate_ready(&mut self, buffer_id: &str) {
        let ready = {
            let Some(queue) = self.translate_queues.get_mut(buffer_id) else {
                return;
            };
            queue.drain_ready()
        };
        self.deliver_ready(buffer_id, ready);
        if self
            .translate_queues
            .get(buffer_id)
            .is_some_and(crate::translate::queue::TranslateQueue::is_empty)
        {
            self.translate_queues.remove(buffer_id);
        }
    }

    /// Release every queued line for one buffer, untranslated where it is
    /// still pending, and drop the queue.
    ///
    /// Called on buffer close, `/part`, `/kick`, disconnect, quit and detach.
    /// Pending lines are released rather than dropped: the network already
    /// delivered them, and losing them silently is worse than showing them
    /// untranslated.
    ///
    /// Lives here and not only on `App` because `remove_buffer` is reached
    /// from the IRC event path, which has no way back up to the App to ask
    /// for a flush first.
    pub fn flush_translate_queue(&mut self, buffer_id: &str) {
        let Some(mut queue) = self.translate_queues.remove(buffer_id) else {
            return;
        };
        let ready = queue.flush_all();
        if !ready.is_empty() {
            tracing::debug!(
                buffer_id,
                count = ready.len(),
                "translate: flushed queued lines"
            );
        }
        self.deliver_ready(buffer_id, ready);
    }

    /// Hold one buffer's queue to `max_queue`, releasing whatever that forces
    /// out.
    ///
    /// Run on every insertion, not only on the maintenance tick. A stalled
    /// provider and a busy channel can put hundreds of lines in a queue
    /// between two one-second ticks, and `max_queue` is documented as a bound
    /// on memory AND on how far behind the display is allowed to fall — a
    /// bound checked once a second is neither.
    fn enforce_translate_ceiling(&mut self, buffer_id: &str) {
        let max_queue = self.translate_max_queue.max(1);
        let ready = {
            let Some(queue) = self.translate_queues.get_mut(buffer_id) else {
                return;
            };
            if queue.len() <= max_queue {
                return;
            }
            let forced = queue.enforce_ceiling(max_queue);
            if forced.forced > 0 {
                tracing::debug!(
                    buffer_id,
                    forced = forced.forced,
                    "translate: queue ceiling reached, releasing oldest untranslated"
                );
            }
            if forced.barriers_lifted > 0 {
                // Louder than the rest, and separate: this one says an
                // outgoing message lost the place held for it, so it will
                // render after replies that arrived while it was being
                // translated. See `TranslateQueue::enforce_ceiling` for why
                // that is still the least-bad option at the ceiling.
                tracing::warn!(
                    buffer_id,
                    barriers = forced.barriers_lifted,
                    "translate: queue ceiling lifted an outgoing message's \
                     reserved position; its echo will render out of order"
                );
            }
            queue.drain_ready()
        };
        self.deliver_ready(buffer_id, ready);
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
        self.deliver_message_to_buffer(buffer_id, message, level);
    }

    /// Add a TRANSIENT message: delivered to the in-memory buffer and broadcast
    /// to web clients (with activity escalation), but NEVER persisted to storage.
    ///
    /// For session-only notices that must not occupy a `(network, @msgid)` row —
    /// notably the "awaiting our own identity" E2E placeholder. That placeholder's
    /// real ciphertext is re-fetched and decrypted via CHATHISTORY once our own
    /// handle is learned (see `regapfill_queries_after_own_handle`);
    /// persisting the placeholder under the server `@msgid` would make the
    /// decryptable replay collapse into `INSERT OR IGNORE` on the unique
    /// `(network, msg_id)` index and be lost forever.
    pub fn add_transient_message_with_activity(
        &mut self,
        buffer_id: &str,
        message: Message,
        level: ActivityLevel,
    ) {
        // Through the queue when there is one. A placeholder is a
        // chronological row like any other: appended directly it renders
        // above the lines queued before it, which is the reordering the queue
        // exists to prevent, arriving from yet another direction.
        if let Some(queue) = self.translate_queues.get_mut(buffer_id) {
            let id = message.id;
            queue.push_resolved_as(
                id,
                message,
                level,
                crate::translate::queue::ReadyDelivery::Transient,
            );
            self.enforce_translate_ceiling(buffer_id);
            return;
        }
        self.deliver_transient_now(buffer_id, message, level);
    }

    /// [`Self::add_transient_message_with_activity`] with the queue already
    /// consulted. Never persisted, by contract.
    fn deliver_transient_now(&mut self, buffer_id: &str, message: Message, level: ActivityLevel) {
        if !self.buffers.contains_key(buffer_id) {
            return;
        }
        self.deliver_message_to_buffer(buffer_id, message, level);
    }

    /// Shared tail of [`AppState::add_message_with_activity_unshrunk`] and
    /// [`AppState::add_transient_message_with_activity`]: queue web events,
    /// append to the in-memory buffer, and escalate activity. Does NOT log to
    /// storage — the caller decides whether to `maybe_log` first.
    fn deliver_message_to_buffer(
        &mut self,
        buffer_id: &str,
        message: Message,
        level: ActivityLevel,
    ) {
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
    ///
    /// Returns `true` only when the row was actually enqueued for storage, and
    /// `false` when it was dropped — logging disabled, the type is filtered by
    /// `log_exclude_types`, a malformed `buffer_id`, or a full log queue. Callers
    /// that track storage progress (CHATHISTORY ingest) must not count a dropped
    /// row as stored.
    fn maybe_log(&self, buffer_id: &str, message: &Message) -> bool {
        let Some(tx) = &self.log_tx else { return false };

        // Check exclude_types filter (e.g. "event" skips quit/join/nick fan-out)
        let type_str = message.message_type.as_str();
        if self
            .log_exclude_types
            .iter()
            .any(|t| t.eq_ignore_ascii_case(type_str))
        {
            return false;
        }

        // buffer_id format: "connection_id/buffer_name"
        let Some((conn_id, buf_name)) = buffer_id.split_once('/') else {
            return false;
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
            (None, false) => storage_identity(&network, buf_name, message),
        };
        let row = LogRow {
            msg_id,
            network,
            buffer: buf_name.to_string(),
            timestamp: message.timestamp.timestamp(),
            ts_ms: message.timestamp.timestamp_millis(),
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
            return false;
        }
        true
    }

    /// Persist a `draft/chathistory` message to the log store WITHOUT
    /// displaying it, mutating buffers/nicklists, or emitting notifications.
    ///
    /// chathistory is a background backlog filler: rows are written here and
    /// the UI surfaces them later through normal `SQLite` pagination
    /// (`get_messages_paginated`). Deduplication is handled by the unique
    /// `msg_id` index on the messages table, so re-ingesting an
    /// already-stored message is a no-op at the database layer.
    ///
    /// Returns whether the row was actually enqueued for storage (see
    /// [`Self::maybe_log`]); a dropped row (filtered type, full queue, logging
    /// off) must not be counted toward CHATHISTORY storage progress.
    #[must_use]
    pub fn ingest_history_message(&self, buffer_id: &str, message: &Message) -> bool {
        self.maybe_log(buffer_id, message)
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
        // Timestamps of rows actually spliced in — used to clear any matching
        // "[E2E: awaiting our own identity]" placeholder afterwards. The
        // placeholder is transient (no @msgid, deliberately un-dedupable so
        // the decrypted replay is never skipped), so nothing else ever removes
        // it; without this sweep the user sees BOTH the placeholder and the
        // decrypted line for the rest of the session. Matching on the exact
        // timestamp (both lines carry the same server @time) keeps unrelated
        // placeholders — ones whose ciphertext was NOT part of this replay —
        // intact.
        let mut spliced_ts: Vec<chrono::DateTime<chrono::Utc>> = Vec::new();
        // What this replay would be STORED as, so a row already on screen that
        // came back from the log — and therefore carries only its stored key,
        // its wire text long since replaced by the display text — can still be
        // recognised as the same message.
        let stored_as = buffer_id.split_once('/').map(|(conn_id, buf_name)| {
            let network = self
                .connections
                .get(conn_id)
                .map_or_else(|| conn_id.to_string(), |c| c.label.clone());
            (network, buf_name.to_string())
        });
        // Our nick on this connection, so a replay of something WE sent can be
        // recognised as the local echo already on screen — the one row in the
        // buffer whose timestamp is our own clock rather than the server's.
        // See `own_echo_matches_replay`.
        let own_nick = buffer_id
            .split_once('/')
            .and_then(|(conn_id, _)| self.connections.get(conn_id))
            .map(|c| c.nick.clone());
        for mut msg in rows {
            let candidate_key = stored_as
                .as_ref()
                .map(|(network, buf_name)| storage_identity(network, buf_name, &msg));
            let already_present = match self.buffers.get(buffer_id) {
                // The buffer AND the reorder queue: a live copy of this line
                // can sit in the queue for the whole translation wait, and a
                // scan of the buffer alone splices the same message in a
                // second time — raw, ahead of its still-translating twin.
                Some(buf) => {
                    buffer_contains_history_row(
                        buf,
                        &msg,
                        candidate_key.as_deref(),
                        own_nick.as_deref(),
                    ) || self.translate_queues.get(buffer_id).is_some_and(|q| {
                        q.any_message(|m| {
                            history_row_matches(
                                m,
                                &msg,
                                candidate_key.as_deref(),
                                own_nick.as_deref(),
                            )
                        })
                    })
                }
                None => return,
            };
            if already_present {
                continue;
            }
            spliced_ts.push(msg.timestamp);
            msg.id = self.next_message_id();
            // Splicing into buf.messages bypasses add_message's web-event queue,
            // so broadcast the row ourselves — these gap-fill rows are not
            // reachable by the web client's older-only pagination, so without
            // this they stay invisible there until a full resync. Use
            // `InsertMessage` (sorted insert), NOT `NewMessage` (append): a gap
            // row can be older than already-displayed post-reconnect live
            // messages, so appending would put the web timeline out of order.
            let wire = crate::web::snapshot::message_to_wire(
                &msg,
                self.web_preview_extractor.as_deref(),
            );
            self.pending_web_events
                .push(crate::web::protocol::WebEvent::InsertMessage {
                    buffer_id: buffer_id.to_string(),
                    message: wire,
                });
            if let Some(buf) = self.buffers.get_mut(buffer_id) {
                let pos = buf
                    .messages
                    .iter()
                    .position(|m| m.timestamp > msg.timestamp)
                    .unwrap_or(buf.messages.len());
                buf.messages.insert(pos, msg);
            }
        }
        // Splicing bypasses add_message, so apply the same scrollback trim live
        // messages get: a reconnect gap spanning many CHATHISTORY AFTER pages can
        // splice far more rows than the configured cap, growing the in-memory deque
        // without bound. enforce_scrollback drops the oldest rows down to the
        // (pin-aware) limit — and if the user has scrolled this buffer up, the
        // raised pinned limit preserves the loaded backlog, exactly as for live
        // traffic.
        let limit = self.scrollback_limit;
        // A decrypted replay and its placeholder share the same server @time,
        // and placeholders are tagless (no @msgid) with no other correlator, so
        // the sweep matches by timestamp. But remove at most ONE placeholder
        // PER spliced row at a given timestamp: two transient placeholders can
        // collide on the same millisecond @time, and if only one of their
        // ciphertexts was replayed here, removing every placeholder at that
        // timestamp would hide a still-undecryptable message from both the TUI
        // and web clients. Budget the sweep by how many rows actually spliced at
        // each timestamp.
        let mut sweep_budget: std::collections::HashMap<chrono::DateTime<chrono::Utc>, usize> =
            std::collections::HashMap::new();
        for ts in &spliced_ts {
            *sweep_budget.entry(*ts).or_insert(0) += 1;
        }
        // In-memory ids of the placeholders swept below. The placeholder was
        // broadcast to live web clients via `NewMessage` when it was delivered,
        // so a server-side retain alone leaves the client showing BOTH the
        // placeholder and the decrypted `InsertMessage` until a full resync.
        // Collect the ids and emit a `DeleteMessages` event so the client drops
        // the stale line in place.
        let mut swept_ids: Vec<u64> = Vec::new();
        if let Some(buf) = self.buffers.get_mut(buffer_id) {
            // Sweep placeholders whose real (decrypted) line just surfaced.
            if !sweep_budget.is_empty() {
                buf.messages.retain(|m| {
                    let is_e2e_placeholder = m.text
                        == crate::e2e::AWAITING_OWN_IDENTITY_PLACEHOLDER
                        || m.text
                            .starts_with(crate::e2e::AWAITING_SESSION_PLACEHOLDER_PREFIX);
                    if is_e2e_placeholder
                        && let Some(budget) = sweep_budget.get_mut(&m.timestamp)
                        && *budget > 0
                    {
                        *budget -= 1;
                        swept_ids.push(m.id);
                        return false;
                    }
                    true
                });
            }
            enforce_scrollback(buf, limit);
        }
        if !swept_ids.is_empty() {
            self.pending_web_events
                .push(crate::web::protocol::WebEvent::DeleteMessages {
                    buffer_id: buffer_id.to_string(),
                    message_ids: swept_ids,
                });
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

    /// Buffer IDs in sidebar order, excluding the app-level default Status
    /// buffer. Index `n - 1` is the window number `n` printed next to each
    /// name in the buffer list.
    ///
    /// Single source of truth for window numbering — the sidebar renderer,
    /// the statusbar activity list, Alt+N switching, buffer-list clicks and
    /// `/wc <n>` all resolve numbers through here so they cannot drift apart.
    pub fn numbered_buffer_ids(&self) -> Vec<String> {
        self.sorted_buffer_ids()
            .into_iter()
            .filter(|id| {
                self.buffers
                    .get(id.as_str())
                    .is_some_and(|b| b.connection_id != crate::app::App::DEFAULT_CONN_ID)
            })
            .collect()
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
/// Deterministic fallback `msg_id` for a conversational row with no server
/// `@msgid`, derived from its identifying content + full-millisecond time.
///
/// Replaces a random UUID so a live message and its later CHATHISTORY replay —
/// which both flow through [`AppState::maybe_log`] and, on a server without
/// `@msgid`, would otherwise each mint a distinct UUID — collapse on the unique
/// `(network, msg_id)` index instead of being stored (and paginated) twice.
///
/// Uses FNV-1a, which is stable across processes and Rust versions (unlike
/// `DefaultHasher`), so the two copies key identically even across restarts. The
/// trade-off is that two genuinely distinct messages with the same network,
/// buffer, millisecond, nick, type and text collapse to one — indistinguishable
/// anyway without a server `@msgid`, and vanishingly rare.
/// The key a conversational row is stored under: the server `@msgid` when
/// there is one, otherwise a deterministic hash of its content and time.
///
/// One function for the writer and for the in-memory `CHATHISTORY` dedup,
/// because the two answering differently is precisely how a replay ends up
/// spliced beside the row it duplicates. The hash covers the WIRE text — for
/// a translated line, not what is displayed or stored — since a replay
/// bypasses translation and would otherwise hash to something else entirely.
fn storage_identity(network: &str, buf_name: &str, message: &Message) -> String {
    message
        .tags
        .as_ref()
        .and_then(|t| t.get("msgid"))
        .filter(|m| !m.is_empty())
        .cloned()
        .unwrap_or_else(|| {
            synthetic_msg_id(
                network,
                buf_name,
                message.timestamp.timestamp_millis(),
                message.nick.as_deref(),
                message.message_type.as_str(),
                dedup_text(message),
            )
        })
}

fn synthetic_msg_id(
    network: &str,
    buffer: &str,
    ts_ms: i64,
    nick: Option<&str>,
    type_str: &str,
    text: &str,
) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let ts_bytes = ts_ms.to_le_bytes();
    let mut h = OFFSET;
    for field in [
        network.as_bytes(),
        buffer.as_bytes(),
        ts_bytes.as_slice(),
        nick.unwrap_or("").as_bytes(),
        type_str.as_bytes(),
        text.as_bytes(),
    ] {
        for &b in field {
            h ^= u64::from(b);
            h = h.wrapping_mul(PRIME);
        }
        // Mix a separator between fields so ("ab","c") and ("a","bc") differ.
        h ^= 0x00ff;
        h = h.wrapping_mul(PRIME);
    }
    format!("synth:{h:016x}")
}

/// The text a row is IDENTIFIED by, which is not always the text it shows.
///
/// A translated row displays the translation (with the original in brackets
/// when configured) while the wire carried something else, so the two sides
/// of a dedup comparison only line up here. Everything else is its own wire
/// text.
fn dedup_text(message: &Message) -> &str {
    message
        .wire_origin
        .as_ref()
        .map_or(message.text.as_str(), |o| o.text.as_str())
}

/// How far a row WE wrote may sit from the server's stamp on the same
/// message before the two stop being recognisable as one.
///
/// The gap is a round trip plus whatever the two clocks disagree by, so it is
/// generous: the alternative to a wide window is a duplicate on screen, while
/// the cost of one is confined to our OWN nick repeating itself verbatim
/// inside a minute — and every line we send while connected is echoed
/// locally, so both copies are on screen already and it is the redundant
/// replays that get skipped.
const OWN_ECHO_REPLAY_SKEW: chrono::TimeDelta = chrono::TimeDelta::seconds(60);

/// Whether `m` is a row we WROTE ourselves and `candidate` is the server's
/// replay of that same message.
///
/// A local echo — the row written when `echo-message` is unavailable — never
/// carried a wire identity. It has no `@msgid` to compare, and its timestamp
/// is OUR clock at the moment of the send while the replay carries the
/// server's `@time`, so neither exact test in [`history_row_matches`] can
/// ever match the pair and a reconnect gap-fill splices our own line in a
/// second time. For a translated row the duplicate is not even a lookalike:
/// one copy shows the translation with ` [original]`, the other the bare wire
/// text.
///
/// Bounded so it cannot swallow anything else: the row has to be ours, has to
/// be one we WROTE rather than one that arrived (anything off the wire that
/// carried tags is excluded), and has to agree on type and on wire text.
fn own_echo_matches_replay(m: &Message, candidate: &Message, own_nick: Option<&str>) -> bool {
    let Some(own) = own_nick else { return false };
    if m.tags.is_some() {
        return false;
    }
    if !m
        .nick
        .as_deref()
        .is_some_and(|n| n.eq_ignore_ascii_case(own))
        || !candidate
            .nick
            .as_deref()
            .is_some_and(|n| n.eq_ignore_ascii_case(own))
    {
        return false;
    }
    m.message_type == candidate.message_type
        && dedup_text(m) == dedup_text(candidate)
        && (m.timestamp - candidate.timestamp).abs() <= OWN_ECHO_REPLAY_SKEW
}

/// Whether `m` is the same logical message as `candidate` — the one
/// comparison for the buffer scan AND the reorder-queue scan, because the
/// two answering differently is precisely how a replay ends up spliced
/// beside the row it duplicates.
fn history_row_matches(
    m: &Message,
    candidate: &Message,
    candidate_key: Option<&str>,
    own_nick: Option<&str>,
) -> bool {
    if let Some(cid) = candidate.tags.as_ref().and_then(|t| t.get("msgid"))
        && let Some(mid) = m.tags.as_ref().and_then(|t| t.get("msgid"))
    {
        return cid == mid;
    }
    // A row read back from the log kept the key it was stored under and
    // nothing else that identifies the wire: its text is the DISPLAY text,
    // which for a translated line is not what a replay carries. Comparing
    // storage keys is the only match left, and it is the same key the
    // unique (network, msg_id) index already collapses them on.
    if let (Some(stored), Some(key)) = (m.log_key.as_deref(), candidate_key) {
        return stored == key;
    }
    if m.timestamp == candidate.timestamp
        && m.nick == candidate.nick
        && m.message_type == candidate.message_type
        // Both sides through `dedup_text`: the in-memory row may be a
        // translation of the very line being replayed, and comparing the
        // display texts would never match, splicing a duplicate in.
        && dedup_text(m) == dedup_text(candidate)
    {
        return true;
    }
    // Exact timestamps are the wrong test for the one row the client wrote
    // itself, and only for that row.
    own_echo_matches_replay(m, candidate, own_nick)
}

fn buffer_contains_history_row(
    buf: &Buffer,
    candidate: &Message,
    candidate_key: Option<&str>,
    own_nick: Option<&str>,
) -> bool {
    buf.messages
        .iter()
        .any(|m| history_row_matches(m, candidate, candidate_key, own_nick))
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
pub mod tests {
    use super::*;
    use crate::state::buffer::*;
    use crate::state::connection::*;
    use chrono::Utc;
    use std::collections::{HashMap, VecDeque};

    pub fn make_test_connection() -> Connection {
        Connection {
            id: "libera".to_string(),
            label: "Libera".to_string(),
            status: ConnectionStatus::Connected,
            own_handle: None,
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
                sasl_key_path: None,
            },
            local_ip: None,
            enabled_caps: std::collections::HashSet::new(),
            chathistory: crate::irc::chathistory::HistoryState::new(),
            who_token_counter: 0,
            multiline: None,
            batch_ref_counter: 0,
            silent_who_channels: std::collections::HashSet::new(),
            silent_banlist_channels: std::collections::HashSet::new(),
        }
    }

    pub fn make_test_buffer(conn_id: &str, btype: BufferType, name: &str) -> Buffer {
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

    pub fn make_test_message(state: &mut AppState, text: &str) -> Message {
        Message {
            log_key: None,
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
            wire_origin: None,
        }
    }

    pub fn make_test_state() -> AppState {
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
            log_key: None,
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
            wire_origin: None,
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
            log_key: None,
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
            wire_origin: None,
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
    fn numbered_buffer_ids_match_the_sidebar() {
        // Window numbers printed in the sidebar are 1-based positions in this
        // list. Two invariants matter: the app-level default Status buffer is
        // never numbered, and Mentions sorts to the front — so `/wc 1` lands on
        // Mentions whenever it exists, which is exactly what the guard assumes.
        let mut state = make_test_state();
        state.add_buffer(make_test_buffer(
            crate::app::App::DEFAULT_CONN_ID,
            BufferType::Server,
            "Status",
        ));
        state.add_buffer(make_test_buffer("", BufferType::Mentions, "Mentions"));

        let numbered = state.numbered_buffer_ids();
        let names: Vec<&str> = numbered
            .iter()
            .map(|id| state.buffers.get(id.as_str()).unwrap().name.as_str())
            .collect();
        assert_eq!(names, vec!["Mentions", "libera", "#linux", "#rust"]);

        // The default Status buffer exists but is unnumbered.
        assert_eq!(state.sorted_buffer_ids().len(), numbered.len() + 1);
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
            log_key: None,
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
            wire_origin: None,
        };
        state.add_message("libera/#rust", msg);

        let row = rx.try_recv().expect("logged row");
        assert_eq!(
            row.msg_id, "server-msgid-xyz",
            "live row must be keyed by the server @msgid for dedup"
        );
    }

    #[test]
    fn ingest_history_message_reports_whether_row_was_stored() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let mut state = make_test_state();
        state.log_tx = Some(tx);

        let msg = Message {
            log_key: None,
            id: 0,
            timestamp: Utc::now(),
            message_type: MessageType::Message,
            nick: Some("dora".to_string()),
            nick_mode: None,
            text: "hi".to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
            wire_origin: None,
        };

        // Normal config: the row is queued, so ingest reports success.
        assert!(state.ingest_history_message("libera/#rust", &msg));
        rx.try_recv().expect("row queued for storage");

        // log_exclude_types filtering a conversational type drops the row in
        // maybe_log — ingest must report failure so the caller does not count it
        // as stored (which would advance the oldest_ingested watermark and clear
        // history_exhausted for a row that never reaches SQLite).
        state.log_exclude_types = vec!["message".to_string()];
        assert!(!state.ingest_history_message("libera/#rust", &msg));
        assert!(rx.try_recv().is_err(), "excluded row must not be queued");
    }

    #[test]
    fn ingest_history_message_reports_failure_on_full_queue() {
        // A full log queue drops the row; ingest must report failure so a busy
        // CHATHISTORY backfill doesn't book unstored rows as stored.
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let mut state = make_test_state();
        state.log_tx = Some(tx);
        let msg = Message {
            log_key: None,
            id: 0,
            timestamp: Utc::now(),
            message_type: MessageType::Message,
            nick: Some("dora".to_string()),
            nick_mode: None,
            text: "hi".to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
            wire_origin: None,
        };
        // First fills the single slot (kept unread via _rx), second overflows.
        assert!(state.ingest_history_message("libera/#rust", &msg));
        assert!(
            !state.ingest_history_message("libera/#rust", &msg),
            "a full queue must report the row as not stored"
        );
    }

    #[test]
    fn maybe_log_dedups_msgid_less_live_and_history_via_deterministic_key() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let mut state = make_test_state();
        state.log_tx = Some(tx);

        // A server that supports draft/chathistory but sends NO @msgid: the live
        // message and its later CHATHISTORY replay both lack a stable server key.
        // Both go through maybe_log and must derive the SAME deterministic msg_id
        // from their content+time, so INSERT OR IGNORE on (network, msg_id)
        // collapses them. With a random UUID per call they would get two distinct
        // keys and the same message would be stored — and paginated — twice.
        let ts = chrono::Utc::now();
        let build = || Message {
            log_key: None,
            id: 0,
            timestamp: ts,
            message_type: MessageType::Message,
            nick: Some("carol".to_string()),
            nick_mode: None,
            text: "no msgid here".to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
            wire_origin: None,
        };

        state.add_message("libera/#rust", build());
        let live = rx.try_recv().expect("live row logged");
        assert!(state.ingest_history_message("libera/#rust", &build()), "history row stored");
        let history = rx.try_recv().expect("history row logged");

        assert_eq!(
            live.msg_id, history.msg_id,
            "msgid-less live and history copies must share a deterministic key"
        );
        assert!(!live.msg_id.is_empty(), "synthetic key must be non-empty");
        // Determinism guard: a random UUID would differ between the two calls.
        assert_ne!(
            live.msg_id,
            uuid::Uuid::new_v4().to_string(),
            "key must be content-derived, not a random UUID"
        );

        // A genuinely different message (different text) gets a different key, so
        // distinct messages are never collapsed.
        let mut other = build();
        other.text = "something else".to_string();
        assert!(state.ingest_history_message("libera/#rust", &other), "other row stored");
        let other_row = rx.try_recv().expect("other row logged");
        assert_ne!(
            other_row.msg_id, live.msg_id,
            "distinct content must not collide on the synthetic key"
        );
    }

    #[test]
    fn a_translated_row_keys_on_the_wire_text_not_the_translation() {
        // The live row displays and stores the translation; its CHATHISTORY
        // replay carries what the peer actually sent and never goes near the
        // translator. Keying on the displayed text gives the two different
        // synthetic ids, and on a msgid-less server the unique index stops
        // collapsing them — the same line stored twice after a gap-fill.
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let mut state = make_test_state();
        state.log_tx = Some(tx);

        let ts = Utc::now();
        let wire = || Message {
            log_key: None,
            id: 0,
            timestamp: ts,
            message_type: MessageType::Message,
            nick: Some("carol".to_string()),
            nick_mode: None,
            text: "dzien dobry".to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
            wire_origin: None,
        };
        // What the queue produces: display text replaced, wire text recorded.
        let mut translated = wire();
        translated.text = "good morning [dzien dobry]".to_string();
        translated.wire_origin = Some(WireOrigin {
            text: "dzien dobry".to_string(),
            suffix_at: Some("good morning".len()),
        });

        state.add_message("libera/#rust", translated);
        let live = rx.try_recv().expect("live row logged");
        assert!(
            state.ingest_history_message("libera/#rust", &wire()),
            "history row stored"
        );
        let history = rx.try_recv().expect("history row logged");

        assert_eq!(
            live.msg_id, history.msg_id,
            "a translated line and its own replay are ONE message"
        );
        assert_eq!(
            live.text, "good morning [dzien dobry]",
            "and what is stored is still what was on screen"
        );
    }

    #[test]
    fn a_translated_row_is_not_spliced_again_by_its_own_history_replay() {
        // The in-memory half of the same problem: `surface_history_rows`
        // compares the candidate against what is on screen, and on screen is
        // the translation.
        let mut state = make_test_state();
        state.add_buffer(Buffer::for_test("libera", BufferType::Channel, "#rust"));
        let ts = Utc::now();
        let wire = Message {
            log_key: None,
            id: 1,
            timestamp: ts,
            message_type: MessageType::Message,
            nick: Some("carol".to_string()),
            nick_mode: None,
            text: "dzien dobry".to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
            wire_origin: None,
        };
        let mut translated = wire.clone();
        translated.text = "good morning".to_string();
        // `show_original_in = false`: the text is replaced with no suffix at
        // all, which is the case a suffix-only marker would miss entirely.
        translated.wire_origin = Some(WireOrigin {
            text: "dzien dobry".to_string(),
            suffix_at: None,
        });
        state.add_message_unshrunk("libera/#rust", translated);

        state.surface_history_rows("libera/#rust", vec![wire]);

        let texts: Vec<String> = state.buffers["libera/#rust"]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect();
        assert_eq!(
            texts,
            vec!["good morning".to_string()],
            "the gap-fill row is the line already on screen, translated"
        );
    }

    #[test]
    fn our_own_local_echo_is_not_spliced_again_by_its_server_replay() {
        // Without `echo-message` the client WRITES its own row, and that row
        // is the only one in the buffer whose timestamp is our clock rather
        // than the server's. It carries no `@msgid` either — nothing about it
        // came off the wire. So both exact tests miss, and a reconnect
        // gap-fill splices the user's own line in a second time: for a
        // translated send, once as the translation with ` [original]` and once
        // as the bare text the network carried.
        let mut state = make_test_state();
        let sent_at = Utc::now();
        let mut echo = Message {
            log_key: None,
            id: 1,
            timestamp: sent_at,
            message_type: MessageType::Message,
            nick: Some("testuser".to_string()),
            nick_mode: None,
            text: "guten morgen [dzien dobry]".to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
            wire_origin: Some(WireOrigin {
                text: "guten morgen".to_string(),
                suffix_at: Some("guten morgen".len()),
            }),
        };
        state.add_message_unshrunk("libera/#rust", echo.clone());

        // The server's replay of that same message: its own @time, its own
        // @msgid, and the text the NETWORK carried — our translation, without
        // the original we kept for ourselves.
        let mut tags = std::collections::HashMap::new();
        tags.insert("msgid".to_string(), "server-1".to_string());
        let replay = Message {
            id: 2,
            timestamp: sent_at + chrono::TimeDelta::milliseconds(420),
            text: "guten morgen".to_string(),
            tags: Some(tags),
            wire_origin: None,
            ..echo.clone()
        };
        state.surface_history_rows("libera/#rust", vec![replay]);

        let texts: Vec<String> = state.buffers["libera/#rust"]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect();
        assert_eq!(
            texts,
            vec!["guten morgen [dzien dobry]".to_string()],
            "the replay is the line already on screen, not a second copy"
        );

        // The window is not a licence to collapse anything that looks alike:
        // a row from somebody ELSE at a different second is a different
        // message, and on a server with no msgid the exact timestamp is the
        // only thing telling two identical lines apart.
        echo.nick = Some("carol".to_string());
        echo.text = "no to co".to_string();
        echo.wire_origin = None;
        state.add_message_unshrunk("libera/#rust", echo.clone());
        let mut again = echo;
        again.timestamp = sent_at + chrono::TimeDelta::seconds(2);
        state.surface_history_rows("libera/#rust", vec![again]);
        assert_eq!(
            state.buffers["libera/#rust"]
                .messages
                .iter()
                .filter(|m| m.text == "no to co")
                .count(),
            2,
            "carol said it twice and both rows stand"
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
            log_key: None,
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
            wire_origin: None,
        };
        state.add_message("libera/#rust", primary);

        let reference = Message {
            log_key: None,
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
            wire_origin: None,
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
            log_key: None,
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
            wire_origin: None,
        };
        state.add_message("libera/#rust", msg1);

        // Reference row: same text in UI, but ref_id set
        let msg2 = Message {
            log_key: None,
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
            wire_origin: None,
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
    fn spliced_gap_fill_rows_are_trimmed_to_scrollback() {
        // A reconnect gap spanning many CHATHISTORY AFTER pages splices rows
        // straight into buf.messages. Like live messages they must honor the
        // scrollback cap — otherwise a long active-buffer gap grows the deque
        // without bound.
        let mut state = make_test_state();
        state.scrollback_limit = 5;
        let rows: Vec<Message> = (0..20)
            .map(|i| {
                let mut m = make_test_message(&mut state, &format!("gap{i}"));
                // Distinct increasing timestamps so none dedup against another and
                // each splices as a separate row.
                m.timestamp = Utc::now() + chrono::Duration::milliseconds(i);
                m
            })
            .collect();

        state.surface_history_rows("libera/#rust", rows);

        let buf = state.buffers.get("libera/#rust").unwrap();
        assert_eq!(
            buf.messages.len(),
            5,
            "spliced gap rows must be trimmed to the scrollback limit"
        );
        // Trimming keeps the most recent rows (drops the oldest).
        assert_eq!(buf.messages.back().unwrap().text, "gap19");
        assert_eq!(buf.messages.front().unwrap().text, "gap15");
    }

    #[test]
    fn spliced_gap_fill_rows_survive_when_buffer_pinned() {
        // If the user has scrolled the buffer up (pinned), the raised limit must
        // preserve the spliced backlog just as it does for live messages.
        let mut state = make_test_state();
        state.scrollback_limit = 5;
        state
            .buffers
            .get_mut("libera/#rust")
            .unwrap()
            .pin_backlog = true;
        let rows: Vec<Message> = (0..20)
            .map(|i| {
                let mut m = make_test_message(&mut state, &format!("gap{i}"));
                m.timestamp = Utc::now() + chrono::Duration::milliseconds(i);
                m
            })
            .collect();

        state.surface_history_rows("libera/#rust", rows);

        let buf = state.buffers.get("libera/#rust").unwrap();
        assert_eq!(
            buf.messages.len(),
            20,
            "a pinned buffer keeps all spliced rows (loaded backlog is exempt)"
        );
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

    #[test]
    fn add_transient_message_with_activity_does_not_log_but_escalates() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let mut state = make_test_state();
        state.log_tx = Some(tx);
        state.set_active_buffer("libera/#linux");

        let msg = make_test_message(&mut state, "transient placeholder");
        state.add_transient_message_with_activity(
            "libera/#rust",
            msg,
            ActivityLevel::Mention,
        );

        // Transient: never persisted — it must not occupy a (network, @msgid)
        // row that a later decryptable CHATHISTORY replay needs.
        assert!(
            rx.try_recv().is_err(),
            "add_transient_message_with_activity must not send to log_tx"
        );
        // Delivered to the buffer, and (unlike add_local_message) escalates
        // activity on the inactive buffer so the user is still notified.
        let buf = state.buffers.get("libera/#rust").unwrap();
        assert_eq!(buf.messages.back().unwrap().text, "transient placeholder");
        assert_eq!(buf.activity, ActivityLevel::Mention);
        assert_eq!(buf.unread_count, 1);
    }

    #[test]
    fn decrypted_replay_surfaces_past_a_tagless_placeholder() {
        // The awaiting-own-identity placeholder carries NO @msgid (handle_privmsg
        // strips it). The later decrypted CHATHISTORY replay carries the server
        // @msgid; buffer_contains_history_row must NOT dedup it against the
        // tagless placeholder, so the real message surfaces instead of staying
        // hidden behind "[E2E: awaiting our own identity]".
        let mut state = make_test_state();
        let mut placeholder = make_test_message(&mut state, "[E2E: awaiting our own identity]");
        placeholder.tags = None; // as handle_privmsg now builds it
        let placeholder_ts = placeholder.timestamp;
        state.add_transient_message_with_activity(
            "libera/#rust",
            placeholder,
            ActivityLevel::Mention,
        );

        let mut decrypted = make_test_message(&mut state, "secret plaintext");
        // The replay carries the SAME server @time as the placeholder — both
        // originate from the one wire line.
        decrypted.timestamp = placeholder_ts;
        decrypted.tags = Some(HashMap::from([("msgid".to_string(), "abc".to_string())]));
        state.surface_history_rows("libera/#rust", vec![decrypted]);

        let buf = state.buffers.get("libera/#rust").unwrap();
        assert!(
            buf.messages.iter().any(|m| m.text == "secret plaintext"),
            "decrypted replay must surface — a tagless placeholder must not dedup it"
        );
        // And the placeholder must be swept once its real line surfaced —
        // otherwise the user sees both for the rest of the session (the
        // placeholder is transient, so nothing else ever removes it).
        assert!(
            !buf.messages
                .iter()
                .any(|m| m.text == crate::e2e::AWAITING_OWN_IDENTITY_PLACEHOLDER),
            "placeholder must be removed when its decrypted replay surfaces"
        );
    }

    #[test]
    fn session_placeholder_swept_by_replay_splice() {
        // The awaiting-SESSION placeholder ("[E2E: awaiting session with …]",
        // shown for the DM that triggered a handshake) follows the same
        // lifecycle as the awaiting-own-identity one: tagless + transient, and
        // swept when the decrypted replay of its wire line splices in.
        let mut state = make_test_state();
        let mut placeholder =
            make_test_message(&mut state, "[E2E: awaiting session with ~bob@b.host]");
        placeholder.tags = None;
        let placeholder_ts = placeholder.timestamp;
        state.add_transient_message_with_activity(
            "libera/#rust",
            placeholder,
            ActivityLevel::Mention,
        );

        let mut decrypted = make_test_message(&mut state, "the lost first message");
        decrypted.timestamp = placeholder_ts;
        decrypted.tags = Some(HashMap::from([("msgid".to_string(), "abc".to_string())]));
        state.surface_history_rows("libera/#rust", vec![decrypted]);

        let buf = state.buffers.get("libera/#rust").unwrap();
        assert!(
            buf.messages
                .iter()
                .any(|m| m.text == "the lost first message"),
            "decrypted replay must surface"
        );
        assert!(
            !buf.messages
                .iter()
                .any(|m| m.text.starts_with("[E2E: awaiting session with")),
            "session placeholder must be removed when its decrypted replay surfaces"
        );
    }

    #[test]
    fn swept_placeholder_emits_delete_web_event() {
        // A live web client received the placeholder via a NewMessage event when
        // it was delivered; sweeping it server-side must ALSO emit a
        // DeleteMessages event so the client drops the stale line instead of
        // showing both it and the decrypted replay until a full resync.
        let mut state = make_test_state();
        let mut placeholder =
            make_test_message(&mut state, crate::e2e::AWAITING_OWN_IDENTITY_PLACEHOLDER);
        placeholder.tags = None;
        let placeholder_id = placeholder.id;
        let placeholder_ts = placeholder.timestamp;
        state.add_transient_message_with_activity(
            "libera/#rust",
            placeholder,
            ActivityLevel::Mention,
        );
        // Drop the NewMessage/activity events queued by delivery so we assert
        // only on what surfacing the decrypted replay emits.
        state.pending_web_events.clear();

        let mut decrypted = make_test_message(&mut state, "the lost first message");
        decrypted.timestamp = placeholder_ts;
        decrypted.tags = Some(HashMap::from([("msgid".to_string(), "abc".to_string())]));
        state.surface_history_rows("libera/#rust", vec![decrypted]);

        let delete = state.pending_web_events.iter().find_map(|e| match e {
            crate::web::protocol::WebEvent::DeleteMessages {
                buffer_id,
                message_ids,
            } => Some((buffer_id.clone(), message_ids.clone())),
            _ => None,
        });
        let (buffer_id, ids) =
            delete.expect("sweeping a placeholder must emit a DeleteMessages web event");
        assert_eq!(buffer_id, "libera/#rust");
        assert_eq!(ids, vec![placeholder_id]);
    }

    #[test]
    fn unrelated_placeholder_survives_a_replay_of_other_lines() {
        // The sweep matches on the wire line's timestamp: a placeholder whose
        // ciphertext was NOT part of this replay (still undecryptable) must
        // stay visible — it is the only hint the user has that a message is
        // pending.
        let mut state = make_test_state();
        let placeholder =
            make_test_message(&mut state, crate::e2e::AWAITING_OWN_IDENTITY_PLACEHOLDER);
        state.add_transient_message_with_activity(
            "libera/#rust",
            placeholder,
            ActivityLevel::Mention,
        );

        // A gap-fill replay of some OTHER line (different timestamp).
        let mut other = make_test_message(&mut state, "unrelated backlog line");
        other.tags = Some(HashMap::from([("msgid".to_string(), "zzz".to_string())]));
        state.surface_history_rows("libera/#rust", vec![other]);

        let buf = state.buffers.get("libera/#rust").unwrap();
        assert!(
            buf.messages
                .iter()
                .any(|m| m.text == crate::e2e::AWAITING_OWN_IDENTITY_PLACEHOLDER),
            "a placeholder for a still-pending ciphertext must not be swept"
        );
        assert!(
            !state.pending_web_events.iter().any(|e| matches!(
                e,
                crate::web::protocol::WebEvent::DeleteMessages { .. }
            )),
            "no DeleteMessages must be emitted when nothing is swept"
        );
    }

    #[test]
    fn only_one_placeholder_swept_per_replay_at_shared_timestamp() {
        // Two transient placeholders colliding on the same @time: if only ONE
        // decryptable replay splices at that timestamp, exactly ONE placeholder
        // must be swept. Removing every placeholder at the timestamp (the old
        // `spliced_ts.contains` behavior) would hide the second, still-
        // undecryptable message from both the TUI and web clients.
        let mut state = make_test_state();
        let shared_ts = Utc::now();

        let mut p1 =
            make_test_message(&mut state, crate::e2e::AWAITING_OWN_IDENTITY_PLACEHOLDER);
        p1.timestamp = shared_ts;
        p1.tags = None;
        state.add_transient_message_with_activity("libera/#rust", p1, ActivityLevel::Mention);

        let mut p2 =
            make_test_message(&mut state, crate::e2e::AWAITING_OWN_IDENTITY_PLACEHOLDER);
        p2.timestamp = shared_ts;
        p2.tags = None;
        state.add_transient_message_with_activity("libera/#rust", p2, ActivityLevel::Mention);
        state.pending_web_events.clear();

        // A single decrypted replay lands on the shared timestamp.
        let mut decrypted = make_test_message(&mut state, "decrypted line");
        decrypted.timestamp = shared_ts;
        decrypted.tags = Some(HashMap::from([("msgid".to_string(), "abc".to_string())]));
        state.surface_history_rows("libera/#rust", vec![decrypted]);

        let buf = state.buffers.get("libera/#rust").unwrap();
        let remaining = buf
            .messages
            .iter()
            .filter(|m| m.text == crate::e2e::AWAITING_OWN_IDENTITY_PLACEHOLDER)
            .count();
        assert_eq!(
            remaining, 1,
            "one placeholder must survive when only one of two collided replays surfaced"
        );
        assert!(
            buf.messages.iter().any(|m| m.text == "decrypted line"),
            "the decrypted replay must surface"
        );
        let deleted: usize = state
            .pending_web_events
            .iter()
            .filter_map(|e| match e {
                crate::web::protocol::WebEvent::DeleteMessages { message_ids, .. } => {
                    Some(message_ids.len())
                }
                _ => None,
            })
            .sum();
        assert_eq!(
            deleted, 1,
            "web clients must be told to drop exactly the one swept placeholder"
        );
    }

    #[test]
    fn replay_is_still_deduped_when_placeholder_kept_the_msgid() {
        // Guard the regression direction: if the placeholder HAD kept the server
        // @msgid, the same-@msgid replay would be deduped and the real message
        // lost. This documents exactly why handle_privmsg must strip the tags.
        let mut state = make_test_state();
        let mut placeholder = make_test_message(&mut state, "[E2E: awaiting our own identity]");
        placeholder.tags = Some(HashMap::from([("msgid".to_string(), "abc".to_string())]));
        state.add_transient_message_with_activity(
            "libera/#rust",
            placeholder,
            ActivityLevel::Mention,
        );

        let mut decrypted = make_test_message(&mut state, "secret plaintext");
        decrypted.tags = Some(HashMap::from([("msgid".to_string(), "abc".to_string())]));
        state.surface_history_rows("libera/#rust", vec![decrypted]);

        let buf = state.buffers.get("libera/#rust").unwrap();
        assert!(
            !buf.messages.iter().any(|m| m.text == "secret plaintext"),
            "same-@msgid replay is deduped — proves the tag-strip in handle_privmsg is required"
        );
    }
}

#[cfg(test)]
mod translate_gate_tests {
    use super::tests::{make_test_message, make_test_state};
    use crate::config::TranslateBufferConfig;
    use crate::state::AppState;
    use crate::state::buffer::{ActivityLevel, MessageType};
    use tokio::sync::mpsc;

    const BUF: &str = "libera/#rust";

    /// State with translation live for `BUF`, plus the receiving end of the
    /// worker channel so tests can assert on what was dispatched.
    fn state_with_translation() -> (
        AppState,
        mpsc::Receiver<crate::app::translate::PendingTranslate>,
    ) {
        state_with_translation_capacity(16)
    }

    #[test]
    fn a_rename_onto_an_id_with_a_stale_queue_releases_its_lines_first() {
        // The peer renames onto a nick whose PREVIOUS conversation still has
        // lines in flight — somebody who held the nick until moments ago.
        // Installing the moved queue over the stale one dropped those lines
        // unseen and unlogged, though the network had already delivered them.
        let mut state = make_test_state();
        state.add_buffer(crate::state::buffer::Buffer::for_test(
            "libera",
            crate::state::buffer::BufferType::Query,
            "frankie",
        ));

        let mut leftover = crate::translate::queue::TranslateQueue::new();
        let line = make_test_message(&mut state, "wiadomosc starego frankie");
        leftover.push_pending(
            line.id,
            line.text.clone(),
            crate::translate::queue::PendingPayload {
                message: line,
                activity: ActivityLevel::Activity,
                show_original: false,
            },
        );
        state
            .translate_queues
            .insert("libera/frankie".to_string(), leftover);

        let mut moved = crate::translate::queue::TranslateQueue::new();
        moved.reserve(99);
        state
            .translate_queues
            .insert("libera/frank".to_string(), moved);

        state.rekey_buffer_state("libera/frank", "libera/frankie");

        let shown: Vec<String> = state.buffers["libera/frankie"]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect();
        assert_eq!(
            shown,
            vec!["wiadomosc starego frankie [untranslated: timeout]".to_string()],
            "the stale queue's line is released into the buffer, not dropped"
        );
        assert!(
            state.translate_queues["libera/frankie"].has_reservation(),
            "and the moved queue is installed with its reservation intact"
        );
    }

    #[test]
    fn a_taken_over_nick_does_not_pour_the_old_conversation_into_the_new_one() {
        // The other half of the test above, and the half that decides whose
        // private lines a user reads. `rename_query_buffers` REPLACES the
        // buffer at the new id before any of the queue handling runs, so by
        // the time the stale queue is released the window under it belongs to
        // somebody else — and releasing it normally prints the previous
        // occupant's private lines in the new person's conversation, logs
        // them there, and broadcasts them to every open web tab.
        //
        // They are not dropped either: the network delivered them and the log
        // is the record of what was said. The log is keyed by the NAME, which
        // both occupants share, so a row written there is indistinguishable
        // from any other line that nick sent.
        let (log_tx, mut log_rx) = mpsc::channel(16);
        let mut state = make_test_state();
        state.log_tx = Some(log_tx);
        state.add_buffer(crate::state::buffer::Buffer::for_test(
            "libera",
            crate::state::buffer::BufferType::Query,
            "frank",
        ));
        state.add_buffer(crate::state::buffer::Buffer::for_test(
            "libera",
            crate::state::buffer::BufferType::Query,
            "bob",
        ));

        // Old frank's line, still waiting on its translation.
        let mut leftover = crate::translate::queue::TranslateQueue::new();
        let line = make_test_message(&mut state, "sekret starego franka");
        leftover.push_pending(
            line.id,
            line.text.clone(),
            crate::translate::queue::PendingPayload {
                message: line,
                activity: ActivityLevel::Activity,
                show_original: false,
            },
        );
        state
            .translate_queues
            .insert("libera/frank".to_string(), leftover);

        // bob renames onto the nick frank left behind.
        crate::irc::events::rename_query_buffers_for_test(
            &mut state,
            "libera",
            "bob",
            "frank",
            &["libera/bob".to_string()],
        );

        let shown: Vec<String> = state.buffers["libera/frank"]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect();
        assert!(
            shown.is_empty(),
            "the new occupant's window must not be handed the old one's \
             private lines: {shown:?}"
        );
        assert!(
            !state
                .pending_web_events
                .iter()
                .any(|e| matches!(e, crate::web::protocol::WebEvent::NewMessage { .. })),
            "and no web tab may be sent them either"
        );
        let logged = log_rx.try_recv().expect("the line still reaches the log");
        assert_eq!(logged.text, "sekret starego franka [untranslated: timeout]");
    }

    #[test]
    fn a_taken_over_nick_does_not_inherit_the_old_conversation_s_reflections() {
        // A reflection record says "when the server sends THIS wire text
        // back, render it like so", and the wire text is the only thing it is
        // matched on — nothing in it says which conversation filed it.
        //
        // So a record left behind by the nick's previous occupant is taken by
        // the new one's next identical line: "ok" comes back decorated with a
        // stranger's original, and the reservation the new line filed stays up
        // as a barrier nothing will fill, holding the conversation until the
        // queue expires. The old occupant's own records may be the only ones
        // there — the arriving buffer need not have any — so nothing else
        // clears them.
        let mut state = make_test_state();
        state.add_buffer(crate::state::buffer::Buffer::for_test(
            "libera",
            crate::state::buffer::BufferType::Query,
            "frank",
        ));
        state.add_buffer(crate::state::buffer::Buffer::for_test(
            "libera",
            crate::state::buffer::BufferType::Query,
            "bob",
        ));
        state.decorate_own_echo(
            "libera/frank",
            AppState::own_echo_decoration(
                "ok".to_string(),
                7,
                Some((
                    "ok [dobra]".to_string(),
                    crate::state::buffer::WireOrigin {
                        text: "ok".to_string(),
                        suffix_at: Some(2),
                    },
                )),
                true,
            ),
        );

        crate::irc::events::rename_query_buffers_for_test(
            &mut state,
            "libera",
            "bob",
            "frank",
            &["libera/bob".to_string()],
        );

        assert!(
            state
                .take_own_echo_decoration("libera/frank", "ok")
                .is_none(),
            "the new occupant's line must not consume a record filed by the \
             person who held the nick before them"
        );
    }

    #[test]
    fn a_rename_merges_in_flight_markers_rather_than_replacing_them() {
        // A marker may already sit under the new id — the previous
        // occupant's send still at the provider. Replacing the entry would
        // erase it, and the next ordinary send to that conversation would
        // skip the in-flight refusal and overtake the translated one on the
        // wire.
        let mut state = make_test_state();
        state.note_outgoing_dispatch("libera/frankie");
        state.note_outgoing_dispatch("libera/frank");

        state.rekey_buffer_state("libera/frank", "libera/frankie");

        assert!(
            !state.outgoing_in_flight.contains_key("libera/frank"),
            "nothing is left under the abandoned id"
        );
        assert_eq!(
            state.outgoing_in_flight["libera/frankie"].len(),
            2,
            "both sends stay on record until they come back or age out"
        );
    }

    /// [`state_with_translation`] with a chosen worker-queue depth, so a test
    /// about the DISPLAY ceiling is not confounded by the dispatch channel
    /// filling up and turning later lines into ready rows.
    /// [`state_with_translation`] with the storage writer wired up, so a test
    /// can assert what does and does not reach the log.
    fn state_with_translation_and_log() -> (
        AppState,
        mpsc::Receiver<crate::storage::LogRow>,
    ) {
        let (mut state, rx) = state_with_translation();
        // The dispatch receiver has to stay alive or the gate falls back to
        // delivering untranslated.
        std::mem::forget(rx);
        let (log_tx, log_rx) = mpsc::channel(32);
        state.log_tx = Some(log_tx);
        (state, log_rx)
    }

    fn state_with_translation_capacity(
        capacity: usize,
    ) -> (
        AppState,
        mpsc::Receiver<crate::app::translate::PendingTranslate>,
    ) {
        let mut state = make_test_state();
        let (tx, rx) = mpsc::channel(capacity);
        state.translate_incoming_tx = Some(tx);
        state.translate_active = true;
        state.translate_my_lang = "pl".to_string();
        state.translate_buffers.insert(
            BUF.to_string(),
            TranslateBufferConfig {
                incoming: true,
                outgoing: false,
                lang: Some("de".to_string()),
                my_lang: None,
            },
        );
        (state, rx)
    }

    fn shown(state: &AppState, buffer_id: &str) -> usize {
        state.buffers[buffer_id].messages.len()
    }

    #[test]
    fn releasing_the_only_reservation_prunes_the_queue_it_created() {
        // A queue's mere existence routes every later row through it, so an
        // empty one left behind by a dispatch-side refusal parks the
        // buffer's next lines until the tick prunes it — and on an
        // outgoing-only buffer no incoming outcome ever arrives to drain
        // them sooner.
        let mut state = make_test_state();
        state.reserve_echo_slot(BUF, 7);
        assert!(state.translate_queues.contains_key(BUF), "precondition");
        state.release_echo_slot(BUF, 7);
        assert!(
            !state.translate_queues.contains_key(BUF),
            "an emptied queue must not linger to park the next lines"
        );
    }

    #[test]
    fn a_gap_fill_is_deduped_against_a_line_still_in_the_queue() {
        // A live line sits in the reorder queue for the whole translation
        // wait, invisible to a scan of the buffer. A reconnect gap-fill
        // overlapping it would splice the same message in a second time —
        // raw, ahead of its still-translating twin — and log it under two
        // keys on a msgid-less server.
        let (mut state, rx) = state_with_translation();
        // Keep the dispatch channel open, or the gate delivers untranslated.
        std::mem::forget(rx);
        let mut live = make_test_message(&mut state, "hola que tal");
        live.tags = Some(std::collections::HashMap::from([(
            "msgid".to_string(),
            "m1".to_string(),
        )]));
        state.add_message_with_activity(BUF, live, ActivityLevel::Activity);
        assert!(
            state.translate_queues.contains_key(BUF),
            "precondition: the live copy is pending translation"
        );
        assert_eq!(shown(&state, BUF), 0);

        let mut replay = make_test_message(&mut state, "hola que tal");
        replay.tags = Some(std::collections::HashMap::from([(
            "msgid".to_string(),
            "m1".to_string(),
        )]));
        state.surface_history_rows(BUF, vec![replay]);

        assert_eq!(
            shown(&state, BUF),
            0,
            "the replay's live copy is still translating; splicing it in \
             would show the message twice"
        );
    }

    #[test]
    fn a_notice_parked_for_ordering_still_goes_through_shrink_in_place() {
        // A non-empty reorder queue used to skip incoming shrink entirely
        // for rows parked only for ordering: the same NOTICE rendered
        // shortened or raw depending on whether a translation happened to
        // be in flight. The row's place is reserved while it is at the
        // worker, so the shrunk text comes back exactly where the raw row
        // would have been.
        let (mut state, t_rx) = state_with_translation();
        std::mem::forget(t_rx);
        let (shrink_tx, mut shrink_rx) = mpsc::channel(4);
        state.shrink_incoming_tx = Some(shrink_tx);
        state.shrink_incoming_active = true;
        state.shrink_min_url_length = 10;

        // A translation in flight parks everything behind it.
        let pending_line = make_test_message(&mut state, "hola que tal");
        let pending_id = pending_line.id;
        state.add_message_with_activity(BUF, pending_line, ActivityLevel::Activity);

        // A user NOTICE with a long URL arrives while the queue is
        // non-empty.
        let mut notice =
            make_test_message(&mut state, "zobacz http://example.com/bardzo-dlugi-link");
        notice.message_type = MessageType::Notice;
        let notice_id = notice.id;
        state.add_message(BUF, notice);

        let handed = shrink_rx
            .try_recv()
            .expect("the parked NOTICE still reaches the shrink worker");
        assert_eq!(handed.message.id, notice_id);
        assert!(
            state.translate_queues[BUF].holds_reservation(notice_id),
            "and its place is held while it is away"
        );

        // The worker returns the substituted text; the row takes its held
        // place — after the still-translating line, never before it.
        let mut shrunk = handed.message;
        shrunk.text = "zobacz [example.com]".to_string();
        state.deliver_shrunk_in_order(BUF, shrunk, handed.activity_level);
        assert_eq!(
            shown(&state, BUF),
            0,
            "still behind the pending head — order holds"
        );

        state
            .translate_queues
            .get_mut(BUF)
            .expect("queue")
            .resolve(pending_id, Ok("czesc".to_string()));
        state.drain_translate_ready(BUF);
        let texts: Vec<String> = state.buffers[BUF]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect();
        assert_eq!(
            texts,
            vec![
                // `show_original_in` is on by default, so the resolved line
                // carries its original in brackets.
                "czesc [hola que tal]".to_string(),
                "zobacz [example.com]".to_string(),
            ],
            "the shrunk NOTICE lands in its reserved place, in order"
        );
    }

    #[test]
    fn rows_away_at_the_shrink_worker_still_count_against_the_ceiling() {
        // A row dispatched to shrink holds a RESERVATION in the queue — the
        // same room a raw parked row takes. Counting it only once it comes
        // back lets a burst of long-URL lines grow the queue to the shrink
        // CHANNEL's capacity instead of `translate.max_queue`, which is the
        // memory and display-backlog bound the user actually configured.
        let (mut state, t_rx) = state_with_translation();
        std::mem::forget(t_rx);
        let (shrink_tx, shrink_rx) = mpsc::channel(64);
        std::mem::forget(shrink_rx);
        state.shrink_incoming_tx = Some(shrink_tx);
        state.shrink_incoming_active = true;
        state.shrink_min_url_length = 10;
        state.translate_max_queue = 4;

        // One translation in flight parks everything behind it.
        let pending = make_test_message(&mut state, "hola que tal");
        state.add_message_with_activity(BUF, pending, ActivityLevel::Activity);

        // …then a burst of NOTICEs that are all eligible for shrink and all
        // ineligible for translation.
        for i in 0..20 {
            let mut notice = make_test_message(
                &mut state,
                &format!("zobacz http://example.com/bardzo-dlugi-link-{i}"),
            );
            notice.message_type = MessageType::Notice;
            state.add_message(BUF, notice);
        }

        assert!(
            state.translate_queues[BUF].len() <= 4,
            "the ceiling has to hold while the rows are away: {}",
            state.translate_queues[BUF].len()
        );
    }

    #[test]
    fn an_enabled_buffer_queues_the_line_instead_of_showing_it() {
        let (mut state, mut rx) = state_with_translation();
        let msg = make_test_message(&mut state, "hola que tal");
        state.add_message_with_activity(BUF, msg, ActivityLevel::Activity);

        assert_eq!(shown(&state, BUF), 0, "the line waits for its translation");
        let dispatched = rx.try_recv().expect("a request was dispatched");
        assert_eq!(dispatched.buffer_id, BUF);
        assert_eq!(dispatched.req.text, "hola que tal");
        assert_eq!(dispatched.req.target_lang, "pl");
        assert_eq!(dispatched.req.source_lang.as_deref(), Some("de"));
        assert_eq!(state.translate_queues[BUF].pending_len(), 1);
    }

    #[test]
    fn an_incoming_request_carries_the_channels_language_as_its_source() {
        let (mut state, mut rx) = state_with_translation();
        let msg = make_test_message(&mut state, "hola que tal");
        state.add_message_with_activity(BUF, msg, ActivityLevel::Activity);
        let req = rx.try_recv().expect("dispatched").req;
        assert_eq!(
            req.source_lang.as_deref(),
            Some("de"),
            "incoming starts in the CHANNEL's language"
        );
        assert_eq!(req.target_lang, "pl", "and lands in ours");
    }

    #[test]
    fn a_per_buffer_override_wins_for_incoming() {
        let (mut state, mut rx) = state_with_translation();
        state
            .translate_buffers
            .get_mut(BUF)
            .expect("configured above")
            .my_lang = Some("en".to_string());
        let msg = make_test_message(&mut state, "hola que tal");
        state.add_message_with_activity(BUF, msg, ActivityLevel::Activity);
        let req = rx.try_recv().expect("dispatched").req;
        assert_eq!(req.source_lang.as_deref(), Some("de"));
        assert_eq!(
            req.target_lang, "en",
            "this buffer is read in English while the global stays Polish"
        );
    }

    #[test]
    fn incoming_without_a_channel_language_lets_the_broker_detect_it() {
        let (mut state, mut rx) = state_with_translation();
        state
            .translate_buffers
            .get_mut(BUF)
            .expect("configured above")
            .lang = None;
        let msg = make_test_message(&mut state, "hola que tal");
        state.add_message_with_activity(BUF, msg, ActivityLevel::Activity);
        let req = rx.try_recv().expect("dispatched").req;
        assert_eq!(req.source_lang, None, "autodetect is valid for incoming");
        assert_eq!(req.target_lang, "pl");
    }

    #[test]
    fn a_buffer_without_the_incoming_flag_is_untouched() {
        let (mut state, mut rx) = state_with_translation();
        let msg = make_test_message(&mut state, "hola que tal");
        state.add_message_with_activity("libera/#linux", msg, ActivityLevel::Activity);

        assert_eq!(shown(&state, "libera/#linux"), 1, "delivered immediately");
        assert!(rx.try_recv().is_err(), "nothing was dispatched");
    }

    #[test]
    fn the_master_switch_being_off_disables_every_buffer() {
        let (mut state, mut rx) = state_with_translation();
        state.translate_active = false;
        let msg = make_test_message(&mut state, "hola que tal");
        state.add_message_with_activity(BUF, msg, ActivityLevel::Activity);

        assert_eq!(shown(&state, BUF), 1);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn our_own_echo_is_not_translated() {
        // It is already in the language we typed it in.
        let (mut state, mut rx) = state_with_translation();
        let our_nick = state.connections["libera"].nick.clone();
        let mut msg = make_test_message(&mut state, "hola que tal");
        msg.nick = Some(our_nick);
        state.add_message_with_activity(BUF, msg, ActivityLevel::Activity);

        assert_eq!(shown(&state, BUF), 1);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn events_are_not_translated_but_do_take_their_place_in_the_queue() {
        let (mut state, mut rx) = state_with_translation();
        let chat = make_test_message(&mut state, "hola que tal");
        state.add_message_with_activity(BUF, chat, ActivityLevel::Activity);
        rx.try_recv().expect("the chat line dispatched");

        let mut join = make_test_message(&mut state, "bob has joined");
        join.message_type = MessageType::Event;
        join.nick = None;
        state.add_message(BUF, join);

        assert!(rx.try_recv().is_err(), "an event is never sent to translate");
        assert_eq!(
            shown(&state, BUF),
            0,
            "but it must not overtake the line queued ahead of it"
        );
        assert_eq!(state.translate_queues[BUF].len(), 2);
    }

    #[test]
    fn a_dead_worker_delivers_untranslated_rather_than_dropping_the_line() {
        let (mut state, rx) = state_with_translation();
        drop(rx);
        let msg = make_test_message(&mut state, "hola que tal");
        state.add_message_with_activity(BUF, msg, ActivityLevel::Activity);

        assert_eq!(shown(&state, BUF), 1, "the line is never lost");
        assert!(!state.translate_queues.contains_key(BUF));
    }

    /// Give `state` a real E2E manager and turn E2E on for `#rust`.
    fn enable_e2e_on_rust(state: &mut AppState) {
        let db = crate::storage::db::open_database(false).unwrap();
        let keyring =
            crate::e2e::keyring::Keyring::new(std::sync::Arc::new(std::sync::Mutex::new(db)));
        let mgr = crate::e2e::manager::E2eManager::load_or_init(keyring).unwrap();
        mgr.keyring()
            .set_channel_config(&crate::e2e::keyring::ChannelConfig {
                channel: crate::e2e::scoped_context("Libera", "#rust"),
                enabled: true,
                mode: crate::e2e::keyring::ChannelMode::Normal,
            })
            .unwrap();
        state.e2e_manager = Some(std::sync::Arc::new(mgr));
    }

    #[test]
    fn an_e2e_conversation_is_never_translated() {
        // Translating would hand the plaintext of an end-to-end-protected
        // conversation to a third-party provider, destroying the guarantee
        // E2E exists to provide. There is no opt-in for this.
        let (mut state, mut rx) = state_with_translation();
        enable_e2e_on_rust(&mut state);

        let msg = make_test_message(&mut state, "hola que tal");
        state.add_message_with_activity(BUF, msg, ActivityLevel::Activity);

        assert!(
            rx.try_recv().is_err(),
            "no request may be built for an E2E conversation"
        );
        assert_eq!(
            shown(&state, BUF),
            1,
            "the line is delivered untranslated, in place"
        );
        assert!(!state.translate_queues.contains_key(BUF));
    }

    #[test]
    fn enabling_e2e_after_translation_stops_it_from_the_next_line() {
        // The gate lives at request-build time, not only in `/translate
        // addin`, so enabling order cannot be used to get around it.
        let (mut state, mut rx) = state_with_translation();
        let first = make_test_message(&mut state, "hola que tal");
        state.add_message_with_activity(BUF, first, ActivityLevel::Activity);
        rx.try_recv().expect("translated while E2E was off");

        enable_e2e_on_rust(&mut state);
        let second = make_test_message(&mut state, "y ahora que");
        state.add_message_with_activity(BUF, second, ActivityLevel::Activity);
        assert!(
            rx.try_recv().is_err(),
            "the next line must not be dispatched"
        );
    }

    #[test]
    fn a_deferred_echo_is_never_translated_even_after_a_nick_change() {
        // A deferred local echo carries the nick captured at dispatch, so
        // after a /nick during the wait it no longer matches the connection
        // nick. Routing it through `add_own_message` is what stops the gate
        // treating our own message as someone else's and translating it a
        // second time.
        let (mut state, mut rx) = state_with_translation();
        let mut echo = make_test_message(&mut state, "moje zdanie");
        echo.nick = Some("old_nick".to_string());
        state.add_own_message(BUF, echo.id, echo);

        assert!(
            rx.try_recv().is_err(),
            "our own echo must never be sent for translation"
        );
        assert_eq!(shown(&state, BUF), 1, "and it is shown as written");
    }

    #[test]
    fn a_deferred_echo_takes_its_place_in_a_live_queue() {
        let (mut state, mut rx) = state_with_translation();
        let incoming = make_test_message(&mut state, "hola que tal");
        state.add_message_with_activity(BUF, incoming, ActivityLevel::Activity);
        rx.try_recv().expect("the incoming line dispatched");

        let mut echo = make_test_message(&mut state, "moje zdanie");
        echo.nick = Some("old_nick".to_string());
        state.add_own_message(BUF, echo.id, echo);

        assert_eq!(
            shown(&state, BUF),
            0,
            "the echo must not render ahead of the line queued before it"
        );
        assert_eq!(state.translate_queues[BUF].len(), 2);
    }

    #[test]
    fn a_line_that_never_reached_the_worker_is_tallied_as_a_failure() {
        // These arrive exactly when the provider is in trouble, which is
        // when `/translate status` is asked whether the provider is in
        // trouble. Counting them as successful translations would have the
        // command report health precisely when there is none.
        let (mut state, _rx) = state_with_translation_capacity(1);
        let first = make_test_message(&mut state, "hola");
        let first_id = first.id;
        state.add_message_with_activity(BUF, first, ActivityLevel::Activity);
        // The worker channel now holds its one slot, so this one cannot be
        // dispatched at all and falls back to an untranslated row.
        let second = make_test_message(&mut state, "que tal");
        state.add_message_with_activity(BUF, second, ActivityLevel::Activity);

        // Counting happens where a line is DELIVERED, so release them both.
        state
            .translate_queues
            .get_mut(BUF)
            .expect("queued")
            .resolve(first_id, Ok("czesc".to_string()));
        state.drain_translate_ready(BUF);
        assert_eq!(shown(&state, BUF), 2, "precondition: both were delivered");

        assert_eq!(
            state.translate_tally.translated, 1,
            "only the one that actually came back from a broker"
        );
        let failures = state.translate_tally.timeout
            + state.translate_tally.provider
            + state.translate_tally.refused;
        assert_eq!(failures, 1, "the undispatched line is counted, once");
    }

    #[test]
    fn the_tally_separates_filtered_from_failed() {
        // Both leave the line in its original language on screen. Telling
        // them apart is the whole reason the counters are split by reason.
        let (mut state, _rx) = state_with_translation();
        let msg = make_test_message(&mut state, "hola que tal");
        let id = msg.id;
        state.add_message_with_activity(BUF, msg, ActivityLevel::Activity);
        state
            .translate_queues
            .get_mut(BUF)
            .expect("queued")
            .resolve(id, Err(crate::translate::UntranslatedReason::Filtered));
        state.drain_translate_ready(BUF);

        assert_eq!(state.translate_tally.filtered, 1);
        assert_eq!(state.translate_tally.translated, 0);
        assert_eq!(
            state.translate_tally.timeout + state.translate_tally.provider,
            0,
            "a filtered line is the broker working, not a failure"
        );
    }

    #[test]
    fn rows_that_were_never_candidates_stay_out_of_the_tally() {
        // JOINs, notices and our own echoes pass through the same queue in
        // bulk. Folding them into the translated total would report a
        // healthy provider that has not answered once.
        let (mut state, _rx) = state_with_translation();
        state.reserve_echo_slot(BUF, 100);
        let mut echo = make_test_message(&mut state, "moje zdanie");
        echo.nick = Some("me".to_string());
        state.add_own_message(BUF, 100, echo);

        assert_eq!(
            state.translate_tally,
            crate::state::TranslateTally::default(),
            "our own echo says nothing about the translator"
        );
    }

    #[test]
    fn evicting_a_reflection_record_gives_back_the_place_it_held() {
        // The record is what lets a reflection find the slot reserved when
        // the user pressed Enter. Dropping it at the cap without the
        // reservation leaves a barrier nothing can ever fill: the buffer
        // stalls behind it until the queue expiry, and the replies that
        // finished translating meanwhile are then released AHEAD of the
        // message they were replies to.
        let (mut state, _rx) = state_with_translation();
        state.reserve_echo_slot(BUF, 1);
        let reply = make_test_message(&mut state, "reply");
        let reply_id = reply.id;
        state
            .translate_queues
            .get_mut(BUF)
            .expect("reserved above")
            .push_resolved(reply_id, reply, ActivityLevel::Activity);
        state.decorate_own_echo(
            BUF,
            AppState::own_echo_decoration("moje zdanie".to_string(), 1, None, true),
        );
        assert_eq!(shown(&state, BUF), 0, "precondition: the barrier holds");

        // Enough later sends to push that record off the front.
        for i in 0..AppState::OWN_ECHO_MAX {
            let echo_id = 100 + i as u64;
            state.reserve_echo_slot(BUF, echo_id);
            state.decorate_own_echo(
                BUF,
                AppState::own_echo_decoration(format!("linia {i}"), echo_id, None, true),
            );
        }

        assert_eq!(
            shown(&state, BUF),
            1,
            "the orphaned barrier is lifted, so the reply is not held behind \
             a reflection that can no longer be placed"
        );
        assert!(
            state.translate_queues[BUF].has_reservation(),
            "and only the orphaned one goes — the later sends still hold theirs"
        );
    }

    #[test]
    fn a_split_message_loses_all_its_records_together_or_none() {
        // Evicting one chunk's record while a sibling survives is worse than
        // evicting both. The reflection that lost its record is appended
        // BEHIND the reservation; the one that kept its record then fills
        // that reservation and renders first, so the message arrives with its
        // last chunk at the top.
        let (mut state, _rx) = state_with_translation();
        state.reserve_echo_slot(BUF, 1);
        state.decorate_own_echo(
            BUF,
            AppState::own_echo_decoration("pierwsza".to_string(), 1, None, false),
        );
        state.decorate_own_echo(
            BUF,
            AppState::own_echo_decoration("druga".to_string(), 1, None, true),
        );

        // Fill to exactly the cap, so the NEXT record forces one eviction and
        // no more. Checking after a long run of sends would prove nothing:
        // dropping a group one record at a time converges on the same set
        // eventually, and it is the moment of the eviction that matters.
        let fill = AppState::OWN_ECHO_MAX - state.own_echo_decorations[BUF].len();
        for i in 0..fill {
            let echo_id = 100 + i as u64;
            state.reserve_echo_slot(BUF, echo_id);
            state.decorate_own_echo(
                BUF,
                AppState::own_echo_decoration(format!("linia {i}"), echo_id, None, true),
            );
        }
        assert_eq!(
            state.own_echo_decorations[BUF].len(),
            AppState::OWN_ECHO_MAX,
            "precondition: exactly at the cap, nothing evicted yet"
        );

        state.reserve_echo_slot(BUF, 999);
        state.decorate_own_echo(
            BUF,
            AppState::own_echo_decoration("jeszcze".to_string(), 999, None, true),
        );

        let left: Vec<u64> = state.own_echo_decorations[BUF]
            .iter()
            .map(|d| d.echo_id)
            .collect();
        assert!(
            !left.contains(&1),
            "neither chunk's record may be left behind on its own: {left:?}"
        );
        assert!(
            state.take_own_echo_decoration(BUF, "druga").is_none(),
            "including the one that would have filled the reservation"
        );
    }

    #[test]
    fn a_record_still_expected_by_a_sibling_chunk_keeps_its_reservation() {
        // One message files a record per wire line, all sharing the reserved
        // id. Losing one chunk's record must not release a reservation
        // another chunk is still coming for.
        let (mut state, _rx) = state_with_translation();
        state.reserve_echo_slot(BUF, 7);
        state.decorate_own_echo(
            BUF,
            AppState::own_echo_decoration("pierwsza".to_string(), 7, None, false),
        );
        state.decorate_own_echo(
            BUF,
            AppState::own_echo_decoration("druga".to_string(), 7, None, true),
        );

        // Take the first chunk's record, as its reflection would.
        let taken = state
            .take_own_echo_decoration(BUF, "pierwsza")
            .expect("the record is there");
        assert!(!taken.is_last);
        assert!(
            state.translate_queues[BUF].has_reservation(),
            "the place stays held for the chunk still to come"
        );
    }

    #[test]
    fn a_reloaded_translated_row_still_dedups_its_own_replay() {
        // The log stores the DISPLAY text, so a translated row read back from
        // SQLite has lost the wire text a CHATHISTORY replay carries. On a
        // server without @msgid there is then nothing left to match on, and
        // the replay is spliced in beside the translation, untranslated.
        //
        // What survives the round trip is the key the row was STORED under —
        // for a msgid-less server, a hash of its wire text — and that is the
        // same key the unique (network, msg_id) index collapses them on.
        let mut state = make_test_state();
        let ts = chrono::Utc::now();
        let network = state.connections["libera"].label.clone();

        // The live row, as it looked before it was logged: display text is
        // the translation, identity is the wire text.
        let mut live = make_test_message(&mut state, "czesc");
        live.timestamp = ts;
        live.nick = Some("alice".to_string());
        live.wire_origin = Some(crate::state::buffer::WireOrigin {
            text: "hola".to_string(),
            suffix_at: None,
        });
        let stored_key = super::storage_identity(&network, "#rust", &live);

        // Read back from the log: display text only, plus the stored key.
        let mut reloaded = live;
        reloaded.wire_origin = None;
        reloaded.log_key = Some(stored_key);
        state.buffers[BUF].messages.clear();
        state
            .buffers
            .get_mut(BUF)
            .expect("buffer")
            .messages
            .push_back(reloaded);

        // The replay: same line, untranslated, no @msgid.
        let mut replay = make_test_message(&mut state, "hola");
        replay.timestamp = ts;
        replay.nick = Some("alice".to_string());
        let before = state.buffers[BUF].messages.len();

        state.surface_history_rows(BUF, vec![replay]);

        assert_eq!(
            state.buffers[BUF].messages.len(),
            before,
            "the replay is recognised as the row already on screen"
        );
    }

    #[test]
    fn a_day_separator_waits_its_turn_and_is_still_not_logged() {
        // Appended directly it renders ABOVE a line that arrived before
        // midnight and is still being translated — dating the lines around it
        // wrongly, which is the one thing a separator exists to do. It has to
        // take its place in the queue, and come out of it without picking up
        // logging on the way.
        let (mut state, mut log_rx) = state_with_translation_and_log();
        let msg = make_test_message(&mut state, "hola");
        let id = msg.id;
        state.add_message_with_activity(BUF, msg, ActivityLevel::Activity);

        let mut separator = make_test_message(&mut state, "--- day changed ---");
        separator.message_type = MessageType::Event;
        state.add_local_message_in_order(BUF, separator);
        assert_eq!(
            shown(&state, BUF),
            0,
            "the separator does not jump ahead of the line queued before it"
        );

        state
            .translate_queues
            .get_mut(BUF)
            .expect("queued")
            .resolve(id, Ok("czesc".to_string()));
        state.drain_translate_ready(BUF);

        let rows: Vec<&str> = state.buffers[BUF]
            .messages
            .iter()
            .map(|m| m.text.as_str())
            .collect();
        assert_eq!(
            rows,
            vec!["czesc [hola]", "--- day changed ---"],
            "and lands after it, where it belongs"
        );
        assert!(
            log_rx.try_recv().is_ok(),
            "the chat line is logged, as always"
        );
        assert!(
            log_rx.try_recv().is_err(),
            "the separator is not — going through the queue must not make a \
             local row start being persisted"
        );
    }

    #[test]
    fn a_transient_placeholder_waits_its_turn_and_is_never_logged() {
        // Same for an E2E placeholder, whose whole contract is that it is
        // shown and never persisted.
        let (mut state, mut log_rx) = state_with_translation_and_log();
        let msg = make_test_message(&mut state, "hola");
        let id = msg.id;
        state.add_message_with_activity(BUF, msg, ActivityLevel::Activity);

        let placeholder = make_test_message(&mut state, "[E2E: awaiting our own identity]");
        state.add_transient_message_with_activity(BUF, placeholder, ActivityLevel::Activity);
        assert_eq!(shown(&state, BUF), 0, "queued, not appended");

        state
            .translate_queues
            .get_mut(BUF)
            .expect("queued")
            .resolve(id, Ok("czesc".to_string()));
        state.drain_translate_ready(BUF);

        assert_eq!(shown(&state, BUF), 2);
        assert!(log_rx.try_recv().is_ok(), "the chat line is logged");
        assert!(
            log_rx.try_recv().is_err(),
            "the placeholder is not, queue or no queue"
        );
    }

    #[test]
    fn a_mention_survives_a_line_that_never_reached_the_translator() {
        // A multi-line message, or a dead worker, is delivered straight away
        // with no queue behind it — but the caller was still told the row was
        // taken over, so it skipped its own mention push. Nothing releases
        // from a queue that was never created, so without a fan-out here the
        // mention is simply gone.
        let (mut state, _rx) = state_with_translation();
        let mut mentions = crate::state::events::tests::make_test_buffer(
            "libera",
            crate::state::buffer::BufferType::Mentions,
            "Mentions",
        );
        mentions.id = "_mentions".to_string();
        state.buffers.insert("_mentions".to_string(), mentions);

        // Multi-line: eligible for translation, but not translatable.
        let mut msg = make_test_message(&mut state, "hola kofany\nsegunda linea");
        msg.highlight = true;
        let taken = state.add_message_with_activity(BUF, msg, ActivityLevel::Mention);

        assert!(taken, "precondition: the caller is told not to push inline");
        assert!(
            !state.translate_queues.contains_key(BUF),
            "precondition: no queue was created, so nothing will release it"
        );
        assert_eq!(
            state.buffers["_mentions"].messages.len(),
            1,
            "the mention is aggregated by the branch that delivered it"
        );
    }

    #[test]
    fn a_mention_carries_the_text_the_channel_ends_up_showing() {
        // The channel shows the translation; the `_mentions` aggregate was
        // built the instant the line arrived, from the ORIGINAL. The two then
        // disagree permanently — and the aggregate is exactly where someone
        // looks when they were away and cannot re-read the channel.
        let (mut state, _rx) = state_with_translation();
        let mut mentions = crate::state::events::tests::make_test_buffer(
            "libera",
            crate::state::buffer::BufferType::Mentions,
            "Mentions",
        );
        mentions.id = "_mentions".to_string();
        state.buffers.insert("_mentions".to_string(), mentions);
        let mut msg = make_test_message(&mut state, "hola kofany");
        msg.highlight = true;
        let id = msg.id;
        state.add_message_with_activity(BUF, msg, ActivityLevel::Mention);

        assert_eq!(
            state.buffers["_mentions"].messages.len(),
            0,
            "nothing is aggregated while the line is still being translated"
        );

        state
            .translate_queues
            .get_mut(BUF)
            .expect("queued")
            .resolve(id, Ok("czesc kofany".to_string()));
        state.drain_translate_ready(BUF);

        let aggregated = state.buffers["_mentions"]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            aggregated.contains("czesc kofany"),
            "the aggregate shows what the channel shows: {aggregated}"
        );
    }

    #[test]
    fn an_untranslated_mention_is_still_aggregated_once() {
        // The deferral must not become a way to LOSE a mention: a line that
        // goes through the queue without being translated — the queue was
        // busy, or the provider failed — is aggregated on release, exactly
        // once.
        let (mut state, _rx) = state_with_translation();
        let mut mentions = crate::state::events::tests::make_test_buffer(
            "libera",
            crate::state::buffer::BufferType::Mentions,
            "Mentions",
        );
        mentions.id = "_mentions".to_string();
        state.buffers.insert("_mentions".to_string(), mentions);
        let mut msg = make_test_message(&mut state, "hola kofany");
        msg.highlight = true;
        let id = msg.id;
        state.add_message_with_activity(BUF, msg, ActivityLevel::Mention);
        state
            .translate_queues
            .get_mut(BUF)
            .expect("queued")
            .resolve(id, Err(crate::translate::UntranslatedReason::Timeout));
        state.drain_translate_ready(BUF);

        assert_eq!(
            state.buffers["_mentions"].messages.len(),
            1,
            "aggregated once, not zero times and not twice"
        );
    }

    #[test]
    fn a_split_reflection_holds_the_barrier_until_its_last_chunk() {
        // An `echo-message` server reflects a split translated send one wire
        // line at a time. Treating the first as a completed echo closes the
        // reservation, so the reply that arrived DURING the translation
        // drains between the two halves of the user's own sentence:
        //
        //   me> pierwsza polowa
        //   alice> reply
        //   me> druga polowa      <- wrong
        //
        // The earlier chunks are therefore parked AT the reservation and
        // only the last one closes it.
        let (mut state, _rx) = state_with_translation();
        state.reserve_echo_slot(BUF, 100);
        let reply = make_test_message(&mut state, "reply");
        let reply_id = reply.id;
        state
            .translate_queues
            .get_mut(BUF)
            .expect("reserved above")
            .push_resolved(reply_id, reply, ActivityLevel::Activity);

        let mut first = make_test_message(&mut state, "pierwsza polowa");
        first.nick = Some("me".to_string());
        state.hold_own_message_chunk(BUF, 100, first);
        assert_eq!(
            shown(&state, BUF),
            0,
            "the first chunk must not lift the barrier — the reply is behind it"
        );

        let mut second = make_test_message(&mut state, "druga polowa");
        second.nick = Some("me".to_string());
        state.add_own_message(BUF, 100, second);

        let rows: Vec<&str> = state.buffers[BUF]
            .messages
            .iter()
            .map(|m| m.text.as_str())
            .collect();
        assert_eq!(
            rows,
            vec!["pierwsza polowa", "druga polowa", "reply"],
            "both halves stay together, ahead of the reply that arrived \
             while they were being translated"
        );
    }

    #[test]
    fn chunks_parked_at_a_reservation_survive_a_flush() {
        // The parked rows are messages the server already sent us. If the
        // rest of the split never comes back — a disconnect between two
        // reflections — the flush has to RELEASE what arrived, not drop it
        // with the barrier.
        let (mut state, _rx) = state_with_translation();
        state.reserve_echo_slot(BUF, 100);
        let mut first = make_test_message(&mut state, "pierwsza polowa");
        first.nick = Some("me".to_string());
        state.hold_own_message_chunk(BUF, 100, first);
        assert_eq!(shown(&state, BUF), 0, "held, not shown");

        state.flush_translate_queue(BUF);

        let rows: Vec<&str> = state.buffers[BUF]
            .messages
            .iter()
            .map(|m| m.text.as_str())
            .collect();
        assert_eq!(
            rows,
            vec!["pierwsza polowa"],
            "the half we did receive is not lost with the reservation"
        );
    }

    #[test]
    fn closing_a_buffer_releases_its_queued_lines_instead_of_dropping_them() {
        // `/close`, a self-PART and a kick all reach `remove_buffer` while
        // lines are still waiting on their translations. Those lines already
        // arrived from IRC — the queue governs only WHEN they are allowed on
        // screen — so dropping them loses received messages and never writes
        // them to storage.
        let (mut state, mut rx) = state_with_translation();
        let msg = make_test_message(&mut state, "hola que tal");
        state.add_message_with_activity(BUF, msg, ActivityLevel::Activity);
        rx.try_recv().expect("dispatched");
        assert_eq!(shown(&state, BUF), 0, "precondition: still queued");

        state.remove_buffer(BUF);

        let shown_before_close = state
            .pending_web_events
            .iter()
            .take_while(|e| {
                !matches!(
                    e,
                    crate::web::protocol::WebEvent::BufferClosed { .. }
                )
            })
            .filter(|e| matches!(e, crate::web::protocol::WebEvent::NewMessage { .. }))
            .count();
        assert_eq!(
            shown_before_close, 1,
            "the queued line is released while its buffer still exists, so it \
             is displayed and logged — and only then does the buffer close"
        );
    }

    #[test]
    fn surfaced_history_is_never_sent_for_translation() {
        // Scroll-back and CHATHISTORY splice straight into `buf.messages`,
        // bypassing `add_message` and therefore the dispatch gate. That is
        // load-bearing: routing history through `add_message` would fire a
        // translation request per historical line every time the user
        // scrolls up, and re-translate text that was already translated
        // when it was live. This pins the property.
        let (mut state, mut rx) = state_with_translation();
        let rows = vec![
            make_test_message(&mut state, "erste zeile"),
            make_test_message(&mut state, "zweite zeile"),
        ];
        state.surface_history_rows(BUF, rows);

        assert!(
            rx.try_recv().is_err(),
            "history must never reach the translation worker"
        );
        assert_eq!(shown(&state, BUF), 2, "and it is spliced in as stored");
        assert!(!state.translate_queues.contains_key(BUF));
    }

    #[test]
    fn a_full_worker_queue_delivers_untranslated() {
        let mut state = make_test_state();
        let (tx, _rx) = mpsc::channel(1);
        state.translate_incoming_tx = Some(tx);
        state.translate_active = true;
        state.translate_buffers.insert(
            BUF.to_string(),
            TranslateBufferConfig {
                incoming: true,
                outgoing: false,
                lang: None,
                my_lang: None,
            },
        );
        // Fill the single slot, then send one more.
        let first = make_test_message(&mut state, "line one");
        state.add_message_with_activity(BUF, first, ActivityLevel::Activity);
        let second = make_test_message(&mut state, "line two");
        state.add_message_with_activity(BUF, second, ActivityLevel::Activity);

        assert_eq!(
            state.translate_queues[BUF].pending_len(),
            1,
            "only the line that fit is pending"
        );
        assert_eq!(
            shown(&state, BUF),
            0,
            "the overflow line must NOT jump ahead of the line still translating"
        );
        assert_eq!(
            state.translate_queues[BUF].len(),
            2,
            "it takes its place in the queue instead"
        );
    }

    #[test]
    fn a_multiline_message_is_delivered_marked_not_mangled() {
        // The contract is one line per request; handing the whole thing over
        // makes a backend collapse and reorder words ACROSS line boundaries.
        let (mut state, mut rx) = state_with_translation();
        let mut msg = make_test_message(&mut state, "pierwsza linia");
        msg.text = "pierwsza linia\ndruga linia".to_string();
        state.add_message_with_activity(BUF, msg, ActivityLevel::Activity);

        assert!(
            rx.try_recv().is_err(),
            "a multi-line body must never become one request"
        );
        let shown = state.buffers[BUF].messages.back().expect("delivered").text.clone();
        assert!(shown.starts_with("pierwsza linia\ndruga linia"), "intact: {shown:?}");
        assert!(
            shown.contains("[untranslated: error: multi-line message]"),
            "and marked, so it is not mistaken for a clean pass: {shown:?}"
        );
    }

    #[test]
    fn the_ceiling_holds_during_a_burst_not_only_on_the_tick() {
        // A stalled provider and a busy channel put far more than max_queue
        // in a queue between two one-second ticks. `max_queue` is documented
        // as a bound on memory AND on how far behind the display may fall —
        // a bound checked once a second is neither.
        // A deep worker channel, so every line really is dispatched and the
        // only thing bounding the queue is the ceiling under test.
        let (mut state, mut rx) = state_with_translation_capacity(64);
        state.translate_max_queue = 4;

        for i in 0..20 {
            let msg = make_test_message(&mut state, &format!("linia {i}"));
            state.add_message_with_activity(BUF, msg, ActivityLevel::Activity);
        }

        assert_eq!(
            state.translate_queues[BUF].len(),
            4,
            "the queue is bounded the whole time, with no tick in sight"
        );
        // Nothing is lost: what the ceiling forced out is on screen.
        let shown = state.buffers[BUF].messages.len();
        assert_eq!(shown, 16, "the oldest sixteen were released: {shown}");
        assert!(
            state.buffers[BUF]
                .messages
                .iter()
                .all(|m| m.text.contains("[untranslated:")),
            "and released marked, so the gap is visible"
        );
        // Every line still reached the worker — the ceiling governs display,
        // not dispatch.
        let mut dispatched = 0;
        while rx.try_recv().is_ok() {
            dispatched += 1;
        }
        assert_eq!(dispatched, 20);
    }

    #[test]
    fn the_ceiling_counts_rows_that_are_not_translations() {
        // JOINs, notices and events go into the same queue to keep the
        // timeline honest, so they take up the same room. A busy channel
        // behind a stalled provider is exactly the case the bound is for.
        let (mut state, _rx) = state_with_translation_capacity(64);
        state.translate_max_queue = 4;

        // One line in flight, so everything after it has to queue.
        let msg = make_test_message(&mut state, "guten tag");
        state.add_message_with_activity(BUF, msg, ActivityLevel::Activity);

        for i in 0..30 {
            let id = state.next_message_id();
            let mut ev = make_test_message(&mut state, &format!("* someone joined {i}"));
            ev.id = id;
            ev.message_type = MessageType::Event;
            state.add_message_with_activity(BUF, ev, ActivityLevel::None);
        }

        assert!(
            state.translate_queues[BUF].len() <= 4,
            "non-translatable rows count against the ceiling too: {}",
            state.translate_queues[BUF].len()
        );
    }

    #[test]
    fn an_untranslated_fallback_is_never_handed_to_shrink() {
        // Translation and shrink are mutually exclusive per line. On the
        // FAILURE path that has to hold too: a line already marked
        // `[untranslated: …]` must not be shipped to a second external
        // service, come back rewritten, and have its `wire_origin`
        // overwritten — the marker offset would stop pointing at the marker
        // and the recorded wire text would stop being the wire text.
        let (mut state, mut rx) = state_with_translation();
        let (shrink_tx, mut shrink_rx) = mpsc::channel(8);
        state.shrink_incoming_tx = Some(shrink_tx);
        state.shrink_incoming_active = true;
        state.shrink_min_url_length = 10;

        // Multi-line, so translation refuses it, AND carrying a long URL, so
        // shrink would take it if it were offered the chance.
        let mut msg = make_test_message(&mut state, "x");
        msg.text = "look https://example.com/a/very/long/path
and a second line".to_string();
        state.add_message_with_activity(BUF, msg, ActivityLevel::Activity);

        assert!(
            rx.try_recv().is_err(),
            "a multi-line body must never become a translation request"
        );
        assert!(
            shrink_rx.try_recv().is_err(),
            "nor a shrink request — it is already marked and final"
        );
        let shown = state.buffers[BUF]
            .messages
            .back()
            .expect("delivered inline")
            .clone();
        assert!(
            shown.text.contains("https://example.com/a/very/long/path"),
            "the URL is untouched: {:?}",
            shown.text
        );
        let origin = shown.wire_origin.expect("the marker is recorded");
        let at = origin.suffix_at.expect("and its offset");
        assert!(
            shown.text[at..].starts_with(" [untranslated:"),
            "the offset still points at the marker: {:?}",
            &shown.text[at..]
        );
    }

    #[test]
    fn a_line_that_never_reached_the_worker_is_marked() {
        // Otherwise a dispatch failure looks exactly like a line the broker
        // correctly decided to leave alone.
        let (mut state, rx) = state_with_translation();
        drop(rx);
        let msg = make_test_message(&mut state, "hola que tal");
        state.add_message_with_activity(BUF, msg, ActivityLevel::Activity);
        let shown_text = state.buffers[BUF]
            .messages
            .back()
            .expect("delivered")
            .text
            .clone();
        assert!(
            shown_text.contains("[untranslated:"),
            "the gap must be visible: {shown_text}"
        );
    }

    #[test]
    fn an_overflow_line_still_renders_in_arrival_order() {
        // End of the same story: once the line ahead resolves, both surface,
        // oldest first — the untranslated fallback included.
        let mut state = make_test_state();
        let (tx, _rx) = mpsc::channel(1);
        state.translate_incoming_tx = Some(tx);
        state.translate_active = true;
        state.translate_buffers.insert(
            BUF.to_string(),
            TranslateBufferConfig {
                incoming: true,
                outgoing: false,
                lang: None,
                my_lang: None,
            },
        );
        let first = make_test_message(&mut state, "line one");
        let first_id = first.id;
        state.add_message_with_activity(BUF, first, ActivityLevel::Activity);
        let second = make_test_message(&mut state, "line two");
        state.add_message_with_activity(BUF, second, ActivityLevel::Activity);

        let ready = {
            let queue = state.translate_queues.get_mut(BUF).expect("queue exists");
            queue.resolve(first_id, Ok("translated one".to_string()));
            queue.drain_ready()
        };
        let texts: Vec<String> = ready.iter().map(|e| e.message.text.clone()).collect();
        assert_eq!(
            texts,
            vec![
                // show_original_in defaults on, hence the bracket.
                "translated one [line one]".to_string(),
                // The overflow line never reached the worker, so it is
                // marked rather than looking like a clean pass-through.
                "line two [untranslated: error: translation queue full]".to_string(),
            ],
            "arrival order holds across the fallback"
        );
    }

    #[test]
    fn a_dead_worker_with_no_queue_still_delivers_immediately() {
        // With nothing pending there is no order to protect, so the line
        // must not be parked in a queue nobody will ever drain.
        let (mut state, rx) = state_with_translation();
        drop(rx);
        let msg = make_test_message(&mut state, "hola que tal");
        state.add_message_with_activity(BUF, msg, ActivityLevel::Activity);
        assert_eq!(shown(&state, BUF), 1);
        assert!(!state.translate_queues.contains_key(BUF));
    }
}

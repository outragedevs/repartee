use std::collections::VecDeque;

use chrono::{Local, TimeZone, Utc};

use crate::state::buffer::{BufferType, Message, MessageType};

use super::App;

/// Hard ceiling on a *pinned* live-chat buffer holding on-demand-loaded backlog,
/// so a long scroll-up session (or a live burst while scrolled up) stays
/// memory-bounded. The single source of truth: `state::events::enforce_scrollback`
/// trims a pinned buffer to this, and the loader below refuses to page past it.
/// Lives here (a `pub(crate) mod`) so `state::events` can reference it without
/// tripping `redundant_pub_crate` from a privately-nested module.
pub(crate) const PINNED_BACKLOG_CAP: usize = 10_000;

impl App {
    /// Load recent chat history from the log database into a newly created buffer.
    ///
    /// Messages are **prepended** before any messages already in the buffer
    /// (e.g. the triggering PRIVMSG that caused a query buffer to be created).
    /// Date separators are inserted between messages from different days.
    pub(crate) fn load_backlog(&mut self, buffer_id: &str) {
        let limit = self.config.display.backlog_lines;
        if limit == 0 {
            return;
        }

        let Some(storage) = self.storage.as_ref() else {
            return;
        };

        let Some(buf) = self.state.buffers.get(buffer_id) else {
            return;
        };

        // Only chat buffers get backlog — skip server/special
        if !matches!(
            buf.buffer_type,
            BufferType::Channel | BufferType::Query | BufferType::DccChat
        ) {
            return;
        }

        let network = self
            .state
            .connections
            .get(&buf.connection_id)
            .map_or_else(|| buf.connection_id.clone(), |c| c.label.clone());
        let buf_name = buf.name.clone();

        let encrypt = storage.encrypt;
        let key = storage.crypto_key;
        let rows = {
            let Ok(db) = storage.db.lock() else {
                return;
            };
            // Subsecond-paginated (composite-cursor) query, `None` = newest page.
            // Returns rows tagged with their SQLite id via
            // `rows_to_buffer_messages` -> `stored_to_message`, so the first
            // scroll-up can build a lossless `(millis, id)` cursor. Passing the
            // read key lets encrypted logs decrypt (previously `None` => ciphertext).
            crate::storage::query::get_messages_paginated_subsecond(
                &db,
                &network,
                &buf_name,
                None,
                limit,
                encrypt,
                key.as_ref(),
            )
        };

        let Ok(rows) = rows else {
            return;
        };

        if rows.is_empty() {
            return;
        }

        let count = rows.len();
        let exhausted = rows.len() < limit;
        let mut backlog: VecDeque<Message> =
            crate::app::log_browser::rows_to_buffer_messages(&mut self.state, &rows, None).into();

        // Add "end of backlog" separator.
        let sep_id = self.state.next_message_id();
        backlog.push_back(make_separator(
            sep_id,
            Utc::now(),
            format!("─── End of backlog ({count} lines) ───"),
            "backlog_end",
        ));

        // Prepend backlog before any existing messages (e.g. the triggering
        // PRIVMSG that created a query buffer arrives before load_backlog runs).
        if let Some(buf) = self.state.buffers.get_mut(buffer_id) {
            let existing = std::mem::take(&mut buf.messages);
            backlog.extend(existing);
            buf.messages = backlog;
            // Fewer rows than asked => the whole channel history is loaded.
            buf.history_exhausted = exhausted;
        }
    }
}

/// Page size for an on-demand older-history load in live chat mode.
pub(crate) const CHAT_BACKLOG_PAGE: usize = 200;

/// The `(unix_millis, id)` pagination cursor for fetching messages OLDER than a
/// buffer's current oldest real message. Synthetic date separators (and the
/// end-of-backlog marker) carry no `log_msg_id`, so they're skipped.
///
/// The timestamp is full milliseconds (the in-memory row is reconstructed from
/// `ts_ms` in `stored_to_message`), so [`get_messages_paginated_subsecond`]
/// orders same-second rows by real `@time` — a backfilled older row with a
/// larger autoincrement id is not skipped.
///
/// - Oldest message came from the DB (`log_msg_id` set): use the real
///   `(millis, id)` keyset cursor — lossless, never re-fetches the cursor row.
/// - Oldest message is a *live* message (`log_msg_id` None): there's no DB id, so
///   use `(millis, 0)`, which the `< OR (= AND id < 0)` predicate collapses to
///   "strictly older millisecond". This may skip rows in the exact same
///   millisecond at the live/DB boundary, but never duplicates the boundary
///   message — preferred, since a visible duplicate is worse than a rare dropped
///   same-millisecond line (far rarer than the old same-second drop).
///
/// Returns `None` when there is no real (non-separator) message.
///
/// [`get_messages_paginated_subsecond`]: crate::storage::query::get_messages_paginated_subsecond
pub(crate) fn oldest_backlog_cursor(messages: &VecDeque<Message>) -> Option<(i64, i64)> {
    let oldest = messages.iter().find(|m| {
        m.event_key.as_deref() != Some("date_separator")
            && m.event_key.as_deref() != Some("backlog_end")
    })?;
    let ts = oldest.timestamp.timestamp_millis();
    let id = oldest
        .log_msg_id
        .as_ref()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    Some((ts, id))
}

/// Build a first-`BEFORE` CHATHISTORY anchor `(msgid?, unix_millis)` from the
/// oldest **in-memory** real message, skipping synthetic separators. Used as a
/// fallback when `SQLite` has no stored row for the target yet (a freshly joined
/// buffer, or live rows still queued in the async log writer) but the buffer
/// already holds live messages and the server may have older history.
///
/// The timestamp is full-precision (millis) and the `@msgid` (from the message's
/// tags) is a verified server reference. Returns `None` when the buffer has no
/// real (non-separator) message to anchor on.
pub(crate) fn in_memory_oldest_anchor(messages: &VecDeque<Message>) -> Option<(Option<String>, i64)> {
    let oldest = messages.iter().find(|m| {
        m.event_key.as_deref() != Some("date_separator")
            && m.event_key.as_deref() != Some("backlog_end")
    })?;
    let millis = oldest.timestamp.timestamp_millis();
    let msgid = oldest
        .tags
        .as_ref()
        .and_then(|t| t.get("msgid"))
        .filter(|m| !m.is_empty())
        .cloned();
    Some((msgid, millis))
}

/// Whether scroll-back pagination has reached the oldest row chathistory
/// fetched from the server — its `BEFORE` watermark (`watermark_ms`) vs the
/// buffer's oldest in-memory message (`buffer_oldest_ms`).
///
/// Returns `false` only when a server fetch exists and the buffer's oldest is
/// still strictly newer than the watermark — meaning a just-fetched final page
/// is queued in the async log writer but not yet paginated into the buffer. In
/// that case the buffer must NOT be marked `history_exhausted`, or those rows
/// stay hidden once they flush to `SQLite`.
const fn reached_history_watermark(watermark_ms: Option<i64>, buffer_oldest_ms: Option<i64>) -> bool {
    match (watermark_ms, buffer_oldest_ms) {
        // No server fetch yet, or no in-memory rows — local exhaustion is final.
        (None, _) | (Some(_), None) => true,
        // Displayed down to (or past) the oldest the server gave us.
        (Some(watermark), Some(oldest)) => oldest <= watermark,
    }
}

/// Remove the synthetic "End of backlog (N lines)" marker from a buffer.
///
/// `load_backlog` plants it at the startup-snapshot boundary (just above the
/// live session) with a fixed line count. Once the user scrolls up and pages
/// older rows in above it, that count is stale and the marker no longer denotes
/// a meaningful point — so it's dropped on the first older-page prepend.
pub(crate) fn remove_backlog_end_marker(messages: &mut VecDeque<Message>) {
    messages.retain(|m| m.event_key.as_deref() != Some("backlog_end"));
}

impl App {
    /// If the active live-chat buffer is scrolled close to the top of its loaded
    /// messages, load an older page from the log DB and prepend it — the live-mode
    /// analogue of `maybe_paginate_log_buffer`. Pins the buffer first so the
    /// loaded history survives `enforce_scrollback` on the next live message.
    pub(crate) fn maybe_load_older_chat_backlog(&mut self) {
        if self.log_browser_mode {
            return; // log browser has its own paginator
        }
        let Some(active_id) = self.state.active_buffer_id.clone() else {
            return;
        };
        let Some(buf) = self.state.buffers.get(&active_id) else {
            return;
        };
        if !matches!(
            buf.buffer_type,
            BufferType::Channel | BufferType::Query | BufferType::DccChat
        ) {
            return;
        }
        // A server CHATHISTORY request is in flight: nothing can grow until its
        // batch lands, so skip both the per-tick paginated DB query and a
        // duplicate server request.
        if self
            .state
            .connections
            .get(&buf.connection_id)
            .is_some_and(|c| c.chathistory.any_in_flight(&buf.name))
        {
            return;
        }
        // Memory bound: once the window is full, stop paging deeper this session.
        if buf.messages.len() >= PINNED_BACKLOG_CAP {
            return;
        }
        // Only act when scrolled within 50 lines of the loaded top (mirrors log mode).
        if self.scroll_offset.saturating_add(50) < buf.messages.len() {
            return;
        }
        if buf.history_exhausted {
            // Local SQLite is drained for this buffer (often just a few recent
            // lines loaded at startup), but the server may still hold older
            // history. Ask via `draft/chathistory` BEFORE — the batch ingests
            // older rows into SQLite and clears `history_exhausted`
            // (`process_completed_batch`), so a later tick re-paginates them
            // into the buffer. A no-op when the cap is absent or the server
            // already reported BEFORE exhausted, so a truly exhausted buffer
            // stays quiet.
            self.fetch_older_via_chathistory(&active_id);
        } else {
            self.load_older_chat_backlog(&active_id);
        }
    }

    /// Fetch one older page from the log DB for a live-chat buffer and prepend it,
    /// marking the buffer pinned and (when the page is short) history-exhausted.
    fn load_older_chat_backlog(&mut self, buffer_id: &str) {
        let Some(storage) = self.storage.as_ref() else {
            return;
        };
        let Some(buf) = self.state.buffers.get(buffer_id) else {
            return;
        };
        let cursor = oldest_backlog_cursor(&buf.messages);
        let network = self
            .state
            .connections
            .get(&buf.connection_id)
            .map_or_else(|| buf.connection_id.clone(), |c| c.label.clone());
        let buf_name = buf.name.clone();
        let encrypt = storage.encrypt;
        let key = storage.crypto_key;

        let rows = {
            let Ok(db) = storage.db.lock() else {
                return;
            };
            crate::storage::query::get_messages_paginated_subsecond(
                &db,
                &network,
                &buf_name,
                cursor,
                CHAT_BACKLOG_PAGE,
                encrypt,
                key.as_ref(),
            )
        };
        let Ok(rows) = rows else {
            return;
        };
        let local_short = rows.len() < CHAT_BACKLOG_PAGE;

        // If the batch's newest local date matches the buffer's current head
        // date-separator, drop that separator — the new batch re-emits it above
        // its own messages (same prepend dance as the log browser).
        if let Some(last) = rows.last() {
            let date = Local
                .from_utc_datetime(
                    &chrono::DateTime::<Utc>::from_timestamp(last.timestamp, 0)
                        .unwrap_or_else(Utc::now)
                        .naive_utc(),
                )
                .date_naive();
            if let Some(buf) = self.state.buffers.get_mut(buffer_id)
                && let Some(first) = buf.messages.front()
                && first.event_key.as_deref() == Some("date_separator")
                && Local
                    .from_utc_datetime(&first.timestamp.naive_utc())
                    .date_naive()
                    == date
            {
                buf.messages.pop_front();
            }
        }

        let messages = crate::app::log_browser::rows_to_buffer_messages(&mut self.state, &rows, None);
        if let Some(buf) = self.state.buffers.get_mut(buffer_id) {
            if !messages.is_empty() {
                // Older rows are about to land above the startup backlog, so the
                // "End of backlog (N lines)" marker — pinned at the snapshot
                // boundary with a now-stale count — no longer marks a meaningful
                // point. Drop it before prepending. [P3 review]
                remove_backlog_end_marker(&mut buf.messages);
            }
            for msg in messages.into_iter().rev() {
                buf.messages.push_front(msg);
            }
            buf.pin_backlog = true;
        }

        // Local SQLite is drained for this page. Before declaring the buffer
        // history-exhausted, try to pull older messages from the server via
        // `draft/chathistory`; only mark exhausted when that isn't possible
        // (cap absent, or the server already reported no more history). When a
        // request is in flight or freshly sent, leave `history_exhausted`
        // false so the next scroll tick re-paginates the newly ingested rows.
        //
        // Also defer exhaustion while a just-completed final BEFORE batch is
        // still queued in the async log writer: marking it now (before the rows
        // reach SQLite) would hide them, since the exhausted path stops
        // paginating once they flush. `history_fully_paginated` guards this.
        if local_short
            && !self.fetch_older_via_chathistory(buffer_id)
            && self.history_fully_paginated(buffer_id)
            && let Some(buf) = self.state.buffers.get_mut(buffer_id)
        {
            buf.history_exhausted = true;
        }
    }

    /// Whether the buffer has paginated down to every row chathistory actually
    /// **ingested** — so it's safe to mark the buffer `history_exhausted`. False
    /// while a final short BEFORE batch's stored rows are queued in the async log
    /// writer but not yet paginated in.
    ///
    /// Compares against the *ingested* watermark, NOT `oldest_fetched`: the latter
    /// advances past skipped event-playback lines (for the next BEFORE anchor),
    /// but those never become `SQLite` rows, so the buffer's oldest displayed
    /// message could never reach them — a skipped-only final batch would loop
    /// forever re-querying an empty local page. The ingested watermark only moves
    /// for rows that will actually surface.
    fn history_fully_paginated(&self, buffer_id: &str) -> bool {
        let Some(buf) = self.state.buffers.get(buffer_id) else {
            return true;
        };
        let watermark_ms = self
            .state
            .connections
            .get(&buf.connection_id)
            .and_then(|c| c.chathistory.oldest_ingested(&buf.name));
        let buffer_oldest_ms = in_memory_oldest_anchor(&buf.messages).map(|(_, ms)| ms);
        reached_history_watermark(watermark_ms, buffer_oldest_ms)
    }

    /// Attempt to fetch older history for `buffer_id` from the server via
    /// `draft/chathistory` (`BEFORE`). Returns `true` when chathistory is
    /// handling it — a request was sent or one is already in flight — so the
    /// caller should keep the buffer non-exhausted and wait. Returns `false`
    /// when chathistory can't help (cap absent, server history exhausted, no
    /// anchor, or no connection handle), so the caller marks the buffer
    /// exhausted as before.
    fn fetch_older_via_chathistory(&mut self, buffer_id: &str) -> bool {
        use crate::irc::chathistory::Direction;

        let Some(buf) = self.state.buffers.get(buffer_id) else {
            return false;
        };
        // DCC chats are peer-to-peer; no IRC server can serve their history, so
        // keep them on local log pagination only (never issue CHATHISTORY for a
        // DCC target — it would fail and leave the target stuck in-flight).
        if !matches!(buf.buffer_type, BufferType::Channel | BufferType::Query) {
            return false;
        }
        let conn_id = buf.connection_id.clone();
        let target = buf.name.clone();
        // Fallback anchor if SQLite has nothing stored yet (see below).
        let in_memory_anchor = in_memory_oldest_anchor(&buf.messages);

        let (cap, exhausted, in_flight, watermark, network) = {
            let Some(conn) = self.state.connections.get(&conn_id) else {
                return false;
            };
            (
                conn.enabled_caps.contains("draft/chathistory"),
                conn.chathistory.is_before_exhausted(&target),
                conn.chathistory.any_in_flight(&target),
                conn.chathistory.oldest_fetched(&target),
                conn.label.clone(),
            )
        };

        if !cap || exhausted {
            return false;
        }
        // A BEFORE fetch is store-only: the batch is ingested via the log writer
        // (`log_tx`) and surfaced through SQLite pagination, never spliced into
        // the live buffer. With logging/storage unavailable (disabled or failed
        // to initialize) those rows would be silently dropped and scrollback
        // could keep issuing requests without ever growing — so don't ask.
        if self.state.log_tx.is_none() {
            return false;
        }
        if in_flight {
            // A CHATHISTORY request (BEFORE, or a reconnect AFTER/LATEST) is
            // already outstanding for this target. Requests are serialized, so
            // wait for it to complete rather than marking the buffer exhausted;
            // the next scroll tick re-evaluates.
            return true;
        }

        // Anchor at the oldest point we have already pulled. The per-target
        // watermark is authoritative once set: it carries a full-precision
        // (millis) timestamp plus the exact IRC @msgid when a chathistory batch
        // gave us one (a *verified* server id), so each BEFORE strictly
        // advances backwards — even across windows of only event-playback lines
        // — and never skips messages within the boundary second.
        //
        // On the very first request (no watermark yet) we anchor from the oldest
        // stored row's IRCv3 `tags` — its `@time` (full millisecond, so BEFORE
        // doesn't floor to `.000Z` and skip same-second history) and its `@msgid`
        // (a *verified* server reference; the `msg_id` column is never used, as
        // it may be a locally-minted UUID). `oldest_anchor` returns that
        // `(millis, msgid?)` pair directly.
        //
        // If SQLite has no row for this target yet — a freshly joined buffer, or
        // live rows still queued in the async log writer — fall back to the
        // oldest in-memory message so scroll-up can still reach older server
        // history instead of giving up (which would mark the buffer exhausted).
        let anchor: (Option<String>, i64) = if let Some((ms, msgid)) = watermark {
            (msgid, ms)
        } else {
            let sqlite_anchor = self.storage.as_ref().and_then(|storage| {
                storage.db.lock().ok().and_then(|db| {
                    crate::storage::query::oldest_anchor(&db, &network, &target)
                        .ok()
                        .flatten()
                        .map(|(millis, msgid)| (msgid, millis))
                })
            });
            let Some(resolved) = sqlite_anchor.or(in_memory_anchor) else {
                return false;
            };
            resolved
        };

        self.request_chathistory(&conn_id, &target, Direction::Before, Some(anchor))
    }

    /// Issue a `CHATHISTORY` request for `target` on `conn_id` in `dir`.
    ///
    /// `anchor` is `(msgid?, unix_millis)` for `BEFORE`/`AFTER` (the msgid
    /// reference is used only when the server advertises it via `MSGREFTYPES`
    /// and one is available; otherwise a full-precision timestamp reference is
    /// used), or `None` for `LATEST`. The timestamp is in **milliseconds** so
    /// the anchor never floors to the second and skips same-second messages.
    /// The page size is clamped to the server's `CHATHISTORY` limit.
    /// Returns `true` if a request was sent, recording it as in-flight.
    pub(crate) fn request_chathistory(
        &mut self,
        conn_id: &str,
        target: &str,
        dir: crate::irc::chathistory::Direction,
        anchor: Option<(Option<String>, i64)>,
    ) -> bool {
        use crate::irc::chathistory::{self, HistoryRef, RefKind};

        let (limit, history_ref) = {
            let Some(conn) = self.state.connections.get(conn_id) else {
                return false;
            };
            let cap = conn.enabled_caps.contains("draft/chathistory");
            if !conn.chathistory.should_request(target, dir, cap) {
                return false;
            }
            let limit = chathistory::clamp_limit(
                CHAT_BACKLOG_PAGE,
                conn.isupport_parsed.chathistory_max(),
            );
            let history_ref = match anchor {
                None => HistoryRef::Latest,
                Some((msgid, ts)) => {
                    let use_msgid = matches!(
                        chathistory::pick_ref_type(&conn.isupport_parsed.msgreftypes()),
                        RefKind::MsgId
                    ) && msgid.as_ref().is_some_and(|m| !m.is_empty());
                    if use_msgid {
                        HistoryRef::MsgId(msgid.unwrap_or_default())
                    } else {
                        HistoryRef::Timestamp(chathistory::rfc3339_millis(ts))
                    }
                }
            };
            (limit, history_ref)
        };

        let line = chathistory::build_command(dir.subcommand(), target, &history_ref, limit);
        let Some(handle) = self.irc_handles.get(conn_id) else {
            return false;
        };
        if handle
            .sender
            .send(::irc::proto::Command::Raw(line.clone(), vec![]))
            .is_err()
        {
            return false;
        }
        tracing::debug!(conn_id, %line, "chathistory: request sent");
        if let Some(conn) = self.state.connections.get_mut(conn_id) {
            conn.chathistory.mark_in_flight(target, dir, limit);
        }
        true
    }

    /// On (re)connect, request a `CHATHISTORY` gap-fill for the **active**
    /// buffer when it is a **query** (PM) belonging to `conn_id`. Fired at
    /// end-of-MOTD.
    ///
    /// Channels are deliberately excluded here: their gap-fill runs from the
    /// end-of-NAMES path ([`Self::gapfill_active_channel_on_join`]) instead.
    /// Config-channel auto-joins are sent by the irc crate at end-of-MOTD, so a
    /// channel `CHATHISTORY` issued here would race the JOIN and be rejected for
    /// non-membership by servers that gate history on channel membership, with no
    /// retry. A query needs no membership, so MOTD timing is correct for it.
    pub(crate) fn gapfill_active_buffer_on_connect(&mut self, conn_id: &str) {
        let Some(active_id) = self.state.active_buffer_id.clone() else {
            return;
        };
        let Some(buf) = self.state.buffers.get(&active_id) else {
            return;
        };
        if buf.connection_id != conn_id || !matches!(buf.buffer_type, BufferType::Query) {
            return;
        }
        let target = buf.name.clone();
        self.request_connect_gapfill(conn_id, &target);
    }

    /// Re-run every query buffer's reconnect gap-fill once our own handle (the
    /// recipient DM context) is learned. The end-of-MOTD gap-fill may have run
    /// before the self-`USERHOST` reply and skipped encrypted DM backlog, yet
    /// still claimed the one-shot via `request_connect_gapfill` — so we must
    /// RELEASE that claim first, otherwise the retry is suppressed and the
    /// skipped backlog is never re-fetched. `CHATHISTORY` dedups, so re-issuing
    /// is safe.
    ///
    /// All query buffers on the connection are re-fetched, not just the active
    /// one: an encrypted DM that arrived in a background query before the handle
    /// was known otherwise stays stuck on the "awaiting our own identity"
    /// placeholder (which is transient, so backlog reload won't heal it either).
    pub(crate) fn regapfill_queries_after_own_handle(&mut self, conn_id: &str) {
        let targets: Vec<String> = self
            .state
            .buffers
            .values()
            .filter(|b| b.connection_id == conn_id && matches!(b.buffer_type, BufferType::Query))
            .map(|b| b.name.clone())
            .collect();
        for target in targets {
            if let Some(conn) = self.state.connections.get_mut(conn_id) {
                conn.chathistory.clear_connect_gapfilled(&target);
            }
            self.request_connect_gapfill(conn_id, &target);
        }
    }

    /// On a channel's NAMES completion after (re)connect, gap-fill that channel's
    /// history **if it is the active buffer**. Running here (rather than at
    /// end-of-MOTD) guarantees the server has acknowledged our JOIN — NAMES is
    /// only sent to members — so a membership-gated `CHATHISTORY` is not rejected.
    /// Other channels fill lazily when focused (follow-up).
    pub(crate) fn gapfill_active_channel_on_join(&mut self, conn_id: &str, channel: &str) {
        let Some(active_id) = self.state.active_buffer_id.clone() else {
            return;
        };
        let Some(buf) = self.state.buffers.get(&active_id) else {
            return;
        };
        if buf.connection_id != conn_id
            || !matches!(buf.buffer_type, BufferType::Channel)
            || !buf.name.eq_ignore_ascii_case(channel)
        {
            return;
        }
        let target = buf.name.clone();
        self.request_connect_gapfill(conn_id, &target);
    }

    /// Shared gap-fill request: anchor `AFTER` the newest stored row (so we pull
    /// only what we missed while disconnected), or `LATEST` when the buffer has
    /// no stored history yet. No-op unless the connection negotiated
    /// `draft/chathistory`.
    fn request_connect_gapfill(&mut self, conn_id: &str, target: &str) {
        use crate::irc::chathistory::Direction;

        // Gate once per target per connection. The channel path runs on
        // RPL_ENDOFNAMES, which recurs (manual /names, refresh, part/rejoin); each
        // recurrence would otherwise re-anchor at the original connect cutoff and
        // refetch the same window. Only CHECK the claim here — it is set after the
        // request is confirmed sent (below), so a recurrence retries if this
        // attempt is suppressed by an in-flight request or a failed send.
        let (network, cutoff) = {
            let Some(conn) = self.state.connections.get(conn_id) else {
                return;
            };
            if !conn.enabled_caps.contains("draft/chathistory") {
                return;
            }
            if conn.chathistory.is_connect_gapfilled(target) {
                return;
            }
            // Exclude reconnect-time rows (JOIN echo, traffic logged during a slow
            // NAMES) so the AFTER anchor stays on the pre-disconnect tail and the
            // gap-fill targets the actual disconnected gap.
            (conn.label.clone(), conn.chathistory.gapfill_cutoff())
        };

        // Newest stored row (older than the reconnect cutoff) → AFTER anchor; if
        // the buffer has no such row, ask for the LATEST page instead.
        let anchor = self.storage.as_ref().and_then(|storage| {
            let db = storage.db.lock().ok()?;
            crate::storage::query::newest_anchor(&db, &network, target, cutoff)
                .ok()
                .flatten()
        });

        let issued = match anchor {
            Some((anchor_ms, anchor_msgid)) => {
                // `newest_anchor` returns the full-millisecond `@time` plus the
                // row's *verified* `@msgid` (from its tags). request_chathistory
                // anchors by msgid when the server advertises MSGREFTYPES=msgid —
                // `AFTER timestamp=...` starts strictly after the millisecond, so a
                // msgid reference avoids skipping rows in the anchor's millisecond.
                // The timestamp fallback is still full-precision (not floored to
                // `.000`), so it never refetches only the boundary second either.
                self.request_chathistory(
                    conn_id,
                    target,
                    Direction::After,
                    Some((anchor_msgid, anchor_ms)),
                )
            }
            None => self.request_chathistory(conn_id, target, Direction::Latest, None),
        };

        // Claim the one-shot ONLY now that the request actually went out. If it was
        // suppressed (another CHATHISTORY in flight for the target — e.g. a BEFORE
        // scroll-up, or a stale marker after reconnect) or the send failed, leave
        // the target un-claimed so a later NAMES/end-of-MOTD trigger retries the
        // gap-fill for this connection.
        if issued
            && let Some(conn) = self.state.connections.get_mut(conn_id)
        {
            conn.chathistory.mark_connect_gapfilled(target);
        }
    }

    /// Pin the active live-chat buffer so loaded backlog survives trimming. Called
    /// when the user scrolls up. No-op in log mode or for non-chat buffers.
    pub(crate) fn pin_active_backlog(&mut self) {
        if self.log_browser_mode {
            return;
        }
        if let Some(id) = self.state.active_buffer_id.clone()
            && let Some(buf) = self.state.buffers.get_mut(&id)
            && matches!(
                buf.buffer_type,
                BufferType::Channel | BufferType::Query | BufferType::DccChat
            )
        {
            buf.pin_backlog = true;
        }
    }

    /// When the user has returned to the live bottom (`scroll_offset == 0`),
    /// collapse a pinned buffer: drop the loaded backlog (keep the newest
    /// `scrollback_limit`), unpin, and clear `history_exhausted` so a later
    /// scroll-up reloads fresh. This is the "free what we're not displaying" step.
    pub(crate) fn collapse_backlog_if_at_bottom(&mut self) {
        if self.scroll_offset != 0 || self.log_browser_mode {
            return;
        }
        if let Some(id) = self.state.active_buffer_id.clone() {
            self.state.collapse_buffer_backlog(&id);
        }
    }
}

/// Create a separator/event message (date header, end-of-backlog, etc.).
fn make_separator(
    id: u64,
    timestamp: chrono::DateTime<Utc>,
    text: String,
    event_key: &str,
) -> Message {
    let event_param = text.clone();
    Message {
        id,
        timestamp,
        message_type: MessageType::Event,
        nick: None,
        nick_mode: None,
        text,
        highlight: false,
        event_key: Some(event_key.to_string()),
        event_params: Some(vec![event_param]),
        log_msg_id: None,
        log_ref_id: None,
        tags: None,
    }
}

/// Format a date separator line like irssi/weechat.
///
/// Example: `─── Mon, 29 Mar 2026 ───`
pub fn format_date_separator(date: chrono::NaiveDate) -> String {
    let formatted = date.format("%a, %d %b %Y");
    format!("─── {formatted} ───")
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use chrono::Utc;

    use super::{
        in_memory_oldest_anchor, make_separator, oldest_backlog_cursor, reached_history_watermark,
        remove_backlog_end_marker,
    };
    use crate::state::buffer::{Message, MessageType};

    fn msg(ts: i64, log_id: Option<&str>, event_key: Option<&str>) -> Message {
        Message {
            id: 1,
            timestamp: chrono::DateTime::from_timestamp(ts, 0).unwrap(),
            message_type: event_key.map_or(MessageType::Message, |_| MessageType::Event),
            nick: Some("bob".into()),
            nick_mode: None,
            text: "hi".into(),
            highlight: false,
            event_key: event_key.map(str::to_owned),
            event_params: None,
            log_msg_id: log_id.map(str::to_owned),
            log_ref_id: None,
            tags: None,
        }
    }

    #[test]
    fn in_memory_anchor_uses_oldest_real_message_time_and_msgid() {
        // When SQLite has no stored row yet, the first BEFORE anchors on the
        // oldest in-memory message: full-precision millis from its timestamp and
        // the verified @msgid from its tags. Separators are skipped.
        let mut q: VecDeque<Message> = VecDeque::new();
        q.push_back(msg(1000, None, Some("date_separator")));
        let mut tags = std::collections::HashMap::new();
        tags.insert("msgid".to_string(), "M1".to_string());
        let mut oldest = msg(1500, None, None);
        oldest.tags = Some(tags);
        q.push_back(oldest);
        q.push_back(msg(2000, None, None));

        assert_eq!(
            in_memory_oldest_anchor(&q),
            Some((Some("M1".to_string()), 1_500_000))
        );
    }

    #[test]
    fn in_memory_anchor_without_msgid_tag_is_timestamp_only() {
        let mut q: VecDeque<Message> = VecDeque::new();
        q.push_back(msg(1500, None, None));
        assert_eq!(in_memory_oldest_anchor(&q), Some((None, 1_500_000)));
    }

    #[test]
    fn reached_history_watermark_gates_exhaustion() {
        // No server fetch yet → local exhaustion is authoritative.
        assert!(reached_history_watermark(None, Some(1000)));
        // No in-memory rows → nothing pending.
        assert!(reached_history_watermark(Some(5000), None));
        // Buffer oldest still NEWER than the watermark → a just-fetched final
        // page is queued in the writer but not yet paginated; don't exhaust.
        assert!(!reached_history_watermark(Some(5000), Some(9000)));
        // Buffer oldest reached/passed the watermark → fetched rows displayed.
        assert!(reached_history_watermark(Some(5000), Some(5000)));
        assert!(reached_history_watermark(Some(5000), Some(3000)));
    }

    #[test]
    fn in_memory_anchor_none_without_real_messages() {
        let mut q: VecDeque<Message> = VecDeque::new();
        q.push_back(msg(100, None, Some("date_separator")));
        q.push_back(msg(100, None, Some("backlog_end")));
        assert_eq!(in_memory_oldest_anchor(&q), None);
    }

    #[test]
    fn cursor_is_none_without_real_messages() {
        let mut q = VecDeque::new();
        q.push_back(msg(100, None, Some("date_separator")));
        q.push_back(msg(100, None, Some("backlog_end")));
        assert_eq!(oldest_backlog_cursor(&q), None);
    }

    #[test]
    fn cursor_uses_oldest_db_row_keyset_skipping_separators() {
        // [date_sep, db row id=42 @1000s, live @2000s] -> cursor is (millis, 42).
        let mut q = VecDeque::new();
        q.push_back(msg(1000, None, Some("date_separator")));
        q.push_back(msg(1000, Some("42"), None));
        q.push_back(msg(2000, None, None));
        assert_eq!(oldest_backlog_cursor(&q), Some((1_000_000, 42)));
    }

    #[test]
    fn cursor_for_live_oldest_uses_id_zero() {
        // Oldest real message is live (no log_msg_id) -> id 0 => strictly-older-ms.
        let mut q = VecDeque::new();
        q.push_back(msg(1500, None, None));
        q.push_back(msg(2500, None, None));
        assert_eq!(oldest_backlog_cursor(&q), Some((1_500_000, 0)));
    }

    #[test]
    fn remove_backlog_end_marker_drops_only_that_marker() {
        // [history, backlog_end, live] -> the marker is removed, the real
        // messages and other separators are preserved in order.
        let mut q = VecDeque::new();
        q.push_back(msg(1000, Some("10"), None)); // history (DB row)
        q.push_back(msg(1500, None, Some("date_separator")));
        q.push_back(msg(2000, None, Some("backlog_end")));
        q.push_back(msg(2500, None, None)); // live

        remove_backlog_end_marker(&mut q);

        assert_eq!(q.len(), 3, "only the backlog_end marker is removed");
        assert!(
            !q.iter()
                .any(|m| m.event_key.as_deref() == Some("backlog_end")),
            "no backlog_end marker remains"
        );
        // Surrounding rows keep their order and identity.
        assert_eq!(q[0].log_msg_id.as_deref(), Some("10"));
        assert_eq!(q[1].event_key.as_deref(), Some("date_separator"));
        assert_eq!(q[2].timestamp.timestamp(), 2500);
    }

    #[test]
    fn remove_backlog_end_marker_is_a_noop_without_one() {
        let mut q = VecDeque::new();
        q.push_back(msg(1000, Some("10"), None));
        q.push_back(msg(2000, None, None));
        remove_backlog_end_marker(&mut q);
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn separator_event_carries_text_in_event_params() {
        let message = make_separator(
            1,
            Utc::now(),
            "─── Mon, 29 Mar 2026 ───".to_string(),
            "date_separator",
        );

        assert_eq!(message.event_key.as_deref(), Some("date_separator"));
        assert_eq!(
            message.event_params.as_deref(),
            Some(&["─── Mon, 29 Mar 2026 ───".to_string()][..])
        );
    }
}

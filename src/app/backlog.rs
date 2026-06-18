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
            // Paginated (composite-cursor) query, `None` = newest page. Unlike the
            // old `get_messages`, this returns rows tagged with their SQLite id via
            // `rows_to_buffer_messages` -> `stored_to_message`, so the first
            // scroll-up can build a lossless `(timestamp, id)` cursor. Passing the
            // read key lets encrypted logs decrypt (previously `None` => ciphertext).
            crate::storage::query::get_messages_paginated(
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

/// The `(timestamp, id)` pagination cursor for fetching messages OLDER than a
/// buffer's current oldest real message. Synthetic date separators (and the
/// end-of-backlog marker) carry no `log_msg_id`, so they're skipped.
///
/// - Oldest message came from the DB (`log_msg_id` set): use the real
///   `(timestamp, id)` keyset cursor — lossless, never re-fetches the cursor row.
/// - Oldest message is a *live* message (`log_msg_id` None): there's no DB id, so
///   use `(timestamp, 0)`, which the `< OR (= AND id < 0)` predicate collapses to
///   "strictly older timestamp". This may skip same-second rows exactly at the
///   live/DB boundary, but never duplicates the boundary message — preferred,
///   since a visible duplicate is worse than a rare dropped same-second line.
///
/// Returns `None` when there is no real (non-separator) message.
pub(crate) fn oldest_backlog_cursor(messages: &VecDeque<Message>) -> Option<(i64, i64)> {
    let oldest = messages.iter().find(|m| {
        m.event_key.as_deref() != Some("date_separator")
            && m.event_key.as_deref() != Some("backlog_end")
    })?;
    let ts = oldest.timestamp.timestamp();
    let id = oldest
        .log_msg_id
        .as_ref()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    Some((ts, id))
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
        ) || buf.history_exhausted
        {
            return;
        }
        // Memory bound: once the window is full, stop paging deeper this session.
        if buf.messages.len() >= PINNED_BACKLOG_CAP {
            return;
        }
        // Trigger when within 50 lines of the loaded top (mirrors log mode).
        if self.scroll_offset.saturating_add(50) >= buf.messages.len() {
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
            crate::storage::query::get_messages_paginated(
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
        if local_short
            && !self.fetch_older_via_chathistory(buffer_id)
            && let Some(buf) = self.state.buffers.get_mut(buffer_id)
        {
            buf.history_exhausted = true;
        }
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
        let conn_id = buf.connection_id.clone();
        let target = buf.name.clone();
        // Anchor by timestamp: the in-memory `log_msg_id` is a SQLite rowid,
        // not an IRC `@msgid`, so we cannot use a msgid reference here.
        let Some((oldest_ts, _)) = oldest_backlog_cursor(&buf.messages) else {
            return false;
        };

        let (cap, exhausted, in_flight) = {
            let Some(conn) = self.state.connections.get(&conn_id) else {
                return false;
            };
            let cap = conn.enabled_caps.contains("draft/chathistory");
            let exhausted = conn.chathistory.is_before_exhausted(&target);
            // should_request is false when in flight (cap true, not exhausted).
            let in_flight = cap
                && !exhausted
                && !conn
                    .chathistory
                    .should_request(&target, Direction::Before, cap);
            (cap, exhausted, in_flight)
        };

        if !cap || exhausted {
            return false;
        }
        if in_flight {
            return true; // already waiting on a BEFORE batch
        }
        self.request_chathistory(&conn_id, &target, Direction::Before, Some((None, oldest_ts)))
    }

    /// Issue a `CHATHISTORY` request for `target` on `conn_id` in `dir`.
    ///
    /// `anchor` is `(msgid?, unix_ts)` for `BEFORE`/`AFTER` (the msgid reference
    /// is used only when the server advertises it via `MSGREFTYPES` and one is
    /// available; otherwise a timestamp reference is used), or `None` for
    /// `LATEST`. The page size is clamped to the server's `CHATHISTORY` limit.
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
                        HistoryRef::Timestamp(rfc3339_millis(ts))
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

/// Format a Unix timestamp as an `IRCv3` `server-time` reference
/// (`YYYY-MM-DDTHH:MM:SS.sssZ`) for a `CHATHISTORY` timestamp anchor.
fn rfc3339_millis(unix: i64) -> String {
    chrono::DateTime::<Utc>::from_timestamp(unix, 0)
        .unwrap_or_else(Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
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

    use super::{make_separator, oldest_backlog_cursor, remove_backlog_end_marker};
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
    fn cursor_is_none_without_real_messages() {
        let mut q = VecDeque::new();
        q.push_back(msg(100, None, Some("date_separator")));
        q.push_back(msg(100, None, Some("backlog_end")));
        assert_eq!(oldest_backlog_cursor(&q), None);
    }

    #[test]
    fn cursor_uses_oldest_db_row_keyset_skipping_separators() {
        // [date_sep, db row id=42 @1000, live @2000] -> cursor is (1000, 42).
        let mut q = VecDeque::new();
        q.push_back(msg(1000, None, Some("date_separator")));
        q.push_back(msg(1000, Some("42"), None));
        q.push_back(msg(2000, None, None));
        assert_eq!(oldest_backlog_cursor(&q), Some((1000, 42)));
    }

    #[test]
    fn cursor_for_live_oldest_uses_id_zero() {
        // Oldest real message is live (no log_msg_id) -> id 0 => strictly-older-ts.
        let mut q = VecDeque::new();
        q.push_back(msg(1500, None, None));
        q.push_back(msg(2500, None, None));
        assert_eq!(oldest_backlog_cursor(&q), Some((1500, 0)));
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

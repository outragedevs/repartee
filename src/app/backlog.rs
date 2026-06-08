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
        let exhausted = rows.len() < CHAT_BACKLOG_PAGE;

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
            for msg in messages.into_iter().rev() {
                buf.messages.push_front(msg);
            }
            buf.pin_backlog = true;
            if exhausted {
                buf.history_exhausted = true;
            }
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

    use super::{make_separator, oldest_backlog_cursor};
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

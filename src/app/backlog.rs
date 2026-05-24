use std::collections::VecDeque;

use chrono::{Local, TimeZone, Utc};

use crate::state::buffer::{BufferType, Message, MessageType};
use crate::storage::StoredMessage;

use super::App;

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

        let messages = {
            let Ok(db) = storage.db.lock() else {
                return;
            };
            crate::storage::query::get_messages(
                &db,
                &network,
                &buf_name,
                None,
                limit,
                storage.encrypt,
                None,
            )
        };

        let Ok(messages) = messages else {
            return;
        };

        if messages.is_empty() {
            return;
        }

        let count = messages.len();
        let mut backlog = build_backlog_messages(&messages, &mut self.state);

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
        }
    }
}

/// Build backlog messages with date separators between days.
fn build_backlog_messages(
    messages: &[StoredMessage],
    state: &mut crate::state::AppState,
) -> VecDeque<Message> {
    let mut backlog = VecDeque::with_capacity(messages.len() + 10);
    let mut last_date: Option<chrono::NaiveDate> = None;

    for stored in messages {
        let ts = chrono::DateTime::from_timestamp(stored.timestamp, 0).unwrap_or_else(Utc::now);
        let local_date = Local.from_utc_datetime(&ts.naive_utc()).date_naive();

        // Insert date separator when the day changes.
        if last_date.is_some_and(|d| d != local_date) || last_date.is_none() {
            let sep_id = state.next_message_id();
            backlog.push_back(make_separator(
                sep_id,
                ts,
                format_date_separator(local_date),
                "date_separator",
            ));
        }
        last_date = Some(local_date);

        let msg_type = match stored.msg_type.as_str() {
            "action" => MessageType::Action,
            "notice" => MessageType::Notice,
            "event" => MessageType::Event,
            _ => MessageType::Message,
        };

        let id = state.next_message_id();
        backlog.push_back(Message {
            id,
            timestamp: ts,
            message_type: msg_type,
            nick: stored.nick.clone(),
            nick_mode: None,
            text: stored.text.clone(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
        });
    }

    backlog
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
    use chrono::Utc;

    use super::make_separator;

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

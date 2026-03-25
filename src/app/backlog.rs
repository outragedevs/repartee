use chrono::Utc;

use crate::state::buffer::{BufferType, Message, MessageType};

use super::App;

impl App {
    /// Load recent chat history from the log database into a newly created buffer.
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

        for stored in &messages {
            let msg_type = match stored.msg_type.as_str() {
                "action" => MessageType::Action,
                "notice" => MessageType::Notice,
                "event" => MessageType::Event,
                _ => MessageType::Message,
            };

            let id = self.state.next_message_id();
            let ts = chrono::DateTime::from_timestamp(stored.timestamp, 0).unwrap_or_else(Utc::now);

            if let Some(buf) = self.state.buffers.get_mut(buffer_id) {
                buf.messages.push_back(Message {
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
        }

        // Add separator after backlog
        let sep_id = self.state.next_message_id();
        if let Some(buf) = self.state.buffers.get_mut(buffer_id) {
            buf.messages.push_back(Message {
                id: sep_id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: format!("─── End of backlog ({count} lines) ───"),
                highlight: false,
                event_key: Some("backlog_end".to_string()),
                event_params: None,
                log_msg_id: None,
                log_ref_id: None,
                tags: None,
            });
        }
    }
}

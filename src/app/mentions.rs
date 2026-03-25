use chrono::Utc;

use crate::state::buffer::{
    ActivityLevel, Buffer, BufferType, Message, MessageType,
};

use super::App;

impl App {
    /// Buffer ID for the mentions aggregation buffer.
    pub const MENTIONS_BUFFER_ID: &'static str = "_mentions";

    /// Create the mentions buffer if it doesn't already exist.
    pub(crate) fn create_mentions_buffer(&mut self) {
        if self.state.buffers.contains_key(Self::MENTIONS_BUFFER_ID) {
            return;
        }
        let buf = Buffer {
            id: Self::MENTIONS_BUFFER_ID.to_string(),
            connection_id: String::new(),
            buffer_type: BufferType::Mentions,
            name: "Mentions".to_string(),
            messages: std::collections::VecDeque::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: Utc::now(),
            topic: None,
            topic_set_by: None,
            users: std::collections::HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: std::collections::HashMap::new(),
            last_speakers: Vec::new(),
        };
        self.state
            .buffers
            .insert(Self::MENTIONS_BUFFER_ID.to_string(), buf);
        self.load_mentions_history();
    }

    /// Load recent mentions from DB into the mentions buffer (7 days, max 1000).
    pub(crate) fn load_mentions_history(&mut self) {
        let Some(storage) = &self.storage else { return };
        let Ok(db) = storage.db.lock() else { return };
        let seven_days_ago = chrono::Utc::now().timestamp() - 7 * 24 * 3600;
        let Ok(rows) =
            crate::storage::query::load_recent_mentions(&db, seven_days_ago, 1000)
        else {
            return;
        };
        drop(db);
        // Pre-allocate message IDs before borrowing buffers mutably.
        let base_id = self.state.message_counter + 1;
        self.state.message_counter += rows.len() as u64;
        let Some(buf) = self.state.buffers.get_mut(Self::MENTIONS_BUFFER_ID) else {
            return;
        };
        for (i, row) in rows.iter().enumerate() {
            buf.messages
                .push_back(Self::mention_row_to_message(
                    row,
                    base_id + i as u64,
                    self.config.display.nick_color_saturation,
                    self.config.display.nick_color_lightness,
                ));
        }
    }

    /// Convert a `MentionRow` to a `Message` for the mentions buffer.
    pub(crate) fn mention_row_to_message(
        row: &crate::storage::types::MentionRow,
        id: u64,
        nick_sat: f32,
        nick_lit: f32,
    ) -> Message {
        let ts = chrono::DateTime::from_timestamp(row.timestamp, 0)
            .unwrap_or_else(chrono::Utc::now);
        let datetime = ts
            .with_timezone(&chrono::Local)
            .format("%Y/%m/%d %H:%M:%S")
            .to_string();
        let text = crate::ui::format_mention_line(
            &datetime,
            &row.network,
            &row.channel,
            &row.nick,
            &row.text,
            nick_sat,
            nick_lit,
        );
        Message {
            id,
            timestamp: ts,
            message_type: MessageType::MentionLog,
            nick: None,
            nick_mode: None,
            text,
            highlight: true,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
        }
    }
}

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// === Buffer Type ===

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BufferType {
    Server,
    Channel,
    Query,
    Special,
}

impl BufferType {
    pub const fn sort_group(&self) -> u8 {
        match self {
            Self::Server => 1,
            Self::Channel => 2,
            Self::Query => 3,
            Self::Special => 4,
        }
    }
}

// === Activity Level ===

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ActivityLevel {
    None = 0,
    Events = 1,
    Highlight = 2,
    Activity = 3,
    Mention = 4,
}

// === Message Type ===

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageType {
    Message,
    Action,
    Event,
    Notice,
    Ctcp,
}

// === Message ===

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Message {
    pub id: u64,
    pub timestamp: DateTime<Utc>,
    #[expect(clippy::struct_field_names, reason = "message_type is the canonical IRC term")]
    pub message_type: MessageType,
    pub nick: Option<String>,
    pub nick_mode: Option<String>,
    pub text: String,
    pub highlight: bool,
    pub event_key: Option<String>,
    pub event_params: Option<Vec<String>>,
    /// For fan-out events: if set, used as the log row's `msg_id` (instead of auto-generating).
    pub log_msg_id: Option<String>,
    /// For fan-out events (quit/nick): reference rows point to the primary row's `msg_id`.
    pub log_ref_id: Option<String>,
    /// `IRCv3` message tags extracted from the incoming IRC message.
    pub tags: HashMap<String, String>,
}

// === NickEntry ===

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct NickEntry {
    pub nick: String,
    pub prefix: String,
    pub modes: String,
    pub away: bool,
    pub account: Option<String>,
}

// === ListEntry ===

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ListEntry {
    pub mask: String,
    pub set_by: String,
    pub set_at: i64,
}

// === Buffer ===

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Buffer {
    pub id: String,
    pub connection_id: String,
    #[expect(clippy::struct_field_names, reason = "buffer_type clarifies the field vs the type enum")]
    pub buffer_type: BufferType,
    pub name: String,
    pub messages: Vec<Message>,
    pub activity: ActivityLevel,
    pub unread_count: u32,
    pub last_read: DateTime<Utc>,
    pub topic: Option<String>,
    pub topic_set_by: Option<String>,
    pub users: HashMap<String, NickEntry>,
    pub modes: Option<String>,
    pub mode_params: Option<HashMap<String, String>>,
    pub list_modes: HashMap<String, Vec<ListEntry>>,
}

// === Helpers ===

pub fn make_buffer_id(connection_id: &str, name: &str) -> String {
    format!("{}/{}", connection_id, name.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_buffer_id_lowercases() {
        assert_eq!(make_buffer_id("libera", "#Rust"), "libera/#rust");
    }

    #[test]
    fn activity_level_ordering() {
        assert!(ActivityLevel::Mention > ActivityLevel::Activity);
        assert!(ActivityLevel::Activity > ActivityLevel::Highlight);
        assert!(ActivityLevel::Highlight > ActivityLevel::Events);
        assert!(ActivityLevel::Events > ActivityLevel::None);
    }

    #[test]
    fn buffer_type_sort_group() {
        assert!(BufferType::Server.sort_group() < BufferType::Channel.sort_group());
        assert!(BufferType::Channel.sort_group() < BufferType::Query.sort_group());
        assert!(BufferType::Query.sort_group() < BufferType::Special.sort_group());
    }
}

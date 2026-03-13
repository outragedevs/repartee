use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// === Buffer Type ===

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BufferType {
    Server,
    Channel,
    Query,
    /// Direct Client Connection chat — a 1:1 peer-to-peer chat buffer.
    DccChat,
    Special,
}

impl BufferType {
    pub const fn sort_group(&self) -> u8 {
        match self {
            Self::Server => 1,
            Self::Channel => 2,
            Self::Query => 3,
            Self::DccChat => 4,
            Self::Special => 5,
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

impl MessageType {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Action => "action",
            Self::Event => "event",
            Self::Notice => "notice",
            Self::Ctcp => "ctcp",
        }
    }
}

// === Message ===

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Message {
    pub id: u64,
    pub timestamp: DateTime<Utc>,
    #[expect(
        clippy::struct_field_names,
        reason = "message_type is the canonical IRC term"
    )]
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
    /// Ident (username) from `userhost-in-names` — `nick!ident@host`.
    pub ident: Option<String>,
    /// Hostname from `userhost-in-names` — `nick!ident@host`.
    pub host: Option<String>,
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

/// Maximum number of recent speakers to track for tab completion.
const LAST_SPEAKERS_CAP: usize = 50;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Buffer {
    pub id: String,
    pub connection_id: String,
    #[expect(
        clippy::struct_field_names,
        reason = "buffer_type clarifies the field vs the type enum"
    )]
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
    /// Recent speakers for tab completion, most recent first.
    /// Capped at [`LAST_SPEAKERS_CAP`]. Updated on PRIVMSG/NOTICE/ACTION.
    pub last_speakers: Vec<String>,
}

impl Buffer {
    /// Record a nick as having spoken in this buffer.
    /// Moves them to the front of `last_speakers` (most recent first).
    pub fn touch_speaker(&mut self, nick: &str) {
        // Remove if already present (case-insensitive).
        let nick_lower = nick.to_lowercase();
        self.last_speakers
            .retain(|n| n.to_lowercase() != nick_lower);
        // Prepend.
        self.last_speakers.insert(0, nick.to_string());
        // Cap.
        self.last_speakers.truncate(LAST_SPEAKERS_CAP);
    }
}

// === Helpers ===

pub fn make_buffer_id(connection_id: &str, name: &str) -> String {
    format!("{}/{}", connection_id, name.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_speaker_adds_to_front() {
        let mut buf = Buffer {
            id: "test/chan".to_string(),
            connection_id: "test".to_string(),
            buffer_type: BufferType::Channel,
            name: "#chan".to_string(),
            messages: Vec::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: chrono::Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
            last_speakers: Vec::new(),
        };

        buf.touch_speaker("alice");
        buf.touch_speaker("bob");
        buf.touch_speaker("charlie");
        assert_eq!(buf.last_speakers, vec!["charlie", "bob", "alice"]);

        // Touching an existing nick moves it to front.
        buf.touch_speaker("alice");
        assert_eq!(buf.last_speakers, vec!["alice", "charlie", "bob"]);
    }

    #[test]
    fn touch_speaker_case_insensitive_dedup() {
        let mut buf = Buffer {
            id: "test/chan".to_string(),
            connection_id: "test".to_string(),
            buffer_type: BufferType::Channel,
            name: "#chan".to_string(),
            messages: Vec::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: chrono::Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
            last_speakers: Vec::new(),
        };

        buf.touch_speaker("Alice");
        buf.touch_speaker("alice"); // same nick different case
        assert_eq!(buf.last_speakers.len(), 1);
        assert_eq!(buf.last_speakers[0], "alice"); // uses the latest casing
    }

    #[test]
    fn touch_speaker_respects_cap() {
        let mut buf = Buffer {
            id: "test/chan".to_string(),
            connection_id: "test".to_string(),
            buffer_type: BufferType::Channel,
            name: "#chan".to_string(),
            messages: Vec::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: chrono::Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
            last_speakers: Vec::new(),
        };

        for i in 0..60 {
            buf.touch_speaker(&format!("user{i}"));
        }
        assert_eq!(buf.last_speakers.len(), LAST_SPEAKERS_CAP);
        assert_eq!(buf.last_speakers[0], "user59"); // most recent
    }

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
        assert!(BufferType::Query.sort_group() < BufferType::DccChat.sort_group());
        assert!(BufferType::DccChat.sort_group() < BufferType::Special.sort_group());
    }
}

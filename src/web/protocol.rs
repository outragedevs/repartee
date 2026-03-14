use serde::{Deserialize, Serialize};

/// Server → Client events (JSON over WSS).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WebEvent {
    /// Initial state sync on WebSocket connect.
    SyncInit {
        buffers: Vec<BufferMeta>,
        connections: Vec<ConnectionMeta>,
        mention_count: u32,
    },
    /// A new message was received in a buffer.
    NewMessage {
        buffer_id: String,
        message: WireMessage,
    },
    /// Channel topic changed.
    TopicChanged {
        buffer_id: String,
        topic: Option<String>,
        set_by: Option<String>,
    },
    /// Nick-related event (join, part, quit, nick change, mode, away).
    NickEvent {
        buffer_id: String,
        kind: NickEventKind,
        nick: String,
        new_nick: Option<String>,
        prefix: Option<String>,
        modes: Option<String>,
        away: Option<bool>,
        message: Option<String>,
    },
    /// A new buffer was created.
    BufferCreated { buffer: BufferMeta },
    /// A buffer was closed.
    BufferClosed { buffer_id: String },
    /// Buffer activity level or unread count changed.
    ActivityChanged {
        buffer_id: String,
        activity: u8,
        unread_count: u32,
    },
    /// Connection status changed.
    ConnectionStatus {
        conn_id: String,
        label: String,
        connected: bool,
        nick: String,
    },
    /// A highlight mention was received (for badge updates).
    MentionAlert {
        buffer_id: String,
        message: WireMessage,
    },
    /// Response to `FetchMessages`.
    Messages {
        buffer_id: String,
        messages: Vec<WireMessage>,
        has_more: bool,
    },
    /// Response to `FetchNickList`.
    NickList {
        buffer_id: String,
        nicks: Vec<WireNick>,
    },
    /// Response to `FetchMentions`.
    MentionsList { mentions: Vec<WireMention> },
    /// Server-side error.
    Error { message: String },
}

/// Client → Server commands (JSON over WSS).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WebCommand {
    /// Send a message to a buffer (plain text or /command).
    SendMessage { buffer_id: String, text: String },
    /// Switch the session-local active buffer (does NOT affect terminal).
    SwitchBuffer { buffer_id: String },
    /// Mark messages as read up to a timestamp.
    MarkRead { buffer_id: String, up_to: i64 },
    /// Fetch message history with cursor-based pagination.
    FetchMessages {
        buffer_id: String,
        limit: u32,
        before: Option<i64>,
    },
    /// Request full nick list for a channel buffer.
    FetchNickList { buffer_id: String },
    /// Request unread mentions.
    FetchMentions,
    /// Execute a command in the context of a specific buffer.
    RunCommand { buffer_id: String, text: String },
}

/// Buffer metadata sent in `SyncInit` and `BufferCreated`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferMeta {
    pub id: String,
    pub connection_id: String,
    pub name: String,
    pub buffer_type: String,
    pub topic: Option<String>,
    pub unread_count: u32,
    pub activity: u8,
    pub nick_count: u32,
}

/// Connection metadata sent in `SyncInit`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionMeta {
    pub id: String,
    pub label: String,
    pub nick: String,
    pub connected: bool,
}

/// Wire-format message for transport over WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireMessage {
    pub id: u64,
    pub timestamp: i64,
    pub msg_type: String,
    pub nick: Option<String>,
    pub nick_mode: Option<String>,
    pub text: String,
    pub highlight: bool,
}

/// Wire-format nick entry for transport over WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireNick {
    pub nick: String,
    pub prefix: String,
    pub modes: String,
    pub away: bool,
}

/// Wire-format mention entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireMention {
    pub id: i64,
    pub timestamp: i64,
    pub buffer_id: String,
    pub channel: String,
    pub nick: String,
    pub text: String,
}

/// Kinds of nick events broadcast to web clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NickEventKind {
    Join,
    Part,
    Quit,
    NickChange,
    ModeChange,
    AwayChange,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_event_serializes_with_type_tag() {
        let event = WebEvent::BufferClosed {
            buffer_id: "libera/#rust".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"BufferClosed"#));
        assert!(json.contains(r#""buffer_id":"libera/#rust"#));
    }

    #[test]
    fn web_command_deserializes_from_json() {
        let json = r#"{"type":"SendMessage","buffer_id":"libera/#rust","text":"hello"}"#;
        let cmd: WebCommand = serde_json::from_str(json).unwrap();
        assert!(matches!(cmd, WebCommand::SendMessage { .. }));
    }

    #[test]
    fn wire_message_roundtrip() {
        let msg = WireMessage {
            id: 42,
            timestamp: 1_710_000_000,
            msg_type: "message".into(),
            nick: Some("ferris".into()),
            nick_mode: Some("@".into()),
            text: "hello 🚀".into(),
            highlight: false,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: WireMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.id, 42);
        assert_eq!(decoded.nick.as_deref(), Some("ferris"));
        assert_eq!(decoded.text, "hello 🚀");
    }

    #[test]
    fn fetch_messages_with_null_before() {
        let json = r#"{"type":"FetchMessages","buffer_id":"x","limit":50,"before":null}"#;
        let cmd: WebCommand = serde_json::from_str(json).unwrap();
        assert!(matches!(
            cmd,
            WebCommand::FetchMessages { before: None, .. }
        ));
    }
}

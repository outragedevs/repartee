/// Shared protocol types — mirrors `src/web/protocol.rs` on the server.
/// Duplicated here because the WASM crate can't depend on the main crate.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WebEvent {
    SyncInit {
        buffers: Vec<BufferMeta>,
        connections: Vec<ConnectionMeta>,
        mention_count: u32,
    },
    NewMessage {
        buffer_id: String,
        message: WireMessage,
    },
    TopicChanged {
        buffer_id: String,
        topic: Option<String>,
        set_by: Option<String>,
    },
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
    BufferCreated {
        buffer: BufferMeta,
    },
    BufferClosed {
        buffer_id: String,
    },
    ActivityChanged {
        buffer_id: String,
        activity: u8,
        unread_count: u32,
    },
    ConnectionStatus {
        conn_id: String,
        label: String,
        connected: bool,
        nick: String,
    },
    MentionAlert {
        buffer_id: String,
        message: WireMessage,
    },
    Messages {
        buffer_id: String,
        messages: Vec<WireMessage>,
        has_more: bool,
    },
    NickList {
        buffer_id: String,
        nicks: Vec<WireNick>,
    },
    MentionsList {
        mentions: Vec<WireMention>,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WebCommand {
    SendMessage { buffer_id: String, text: String },
    SwitchBuffer { buffer_id: String },
    MarkRead { buffer_id: String, up_to: i64 },
    FetchMessages { buffer_id: String, limit: u32, before: Option<i64> },
    FetchNickList { buffer_id: String },
    FetchMentions,
    RunCommand { buffer_id: String, text: String },
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionMeta {
    pub id: String,
    pub label: String,
    pub nick: String,
    pub connected: bool,
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireNick {
    pub nick: String,
    pub prefix: String,
    pub modes: String,
    pub away: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireMention {
    pub id: i64,
    pub timestamp: i64,
    pub buffer_id: String,
    pub channel: String,
    pub nick: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NickEventKind {
    Join,
    Part,
    Quit,
    NickChange,
    ModeChange,
    AwayChange,
}

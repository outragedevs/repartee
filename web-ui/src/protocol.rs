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
        #[serde(default)]
        active_buffer_id: Option<String>,
        #[serde(default)]
        timestamp_format: Option<String>,
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
        #[serde(default)]
        session_id: Option<String>,
    },
    NickList {
        buffer_id: String,
        nicks: Vec<WireNick>,
        #[serde(default)]
        session_id: Option<String>,
    },
    MentionsList {
        mentions: Vec<WireMention>,
        #[serde(default)]
        session_id: Option<String>,
    },
    ActiveBufferChanged {
        buffer_id: String,
    },
    SettingsChanged {
        timestamp_format: String,
        line_height: f32,
        theme: String,
        #[serde(default)]
        nick_column_width: u32,
        #[serde(default)]
        nick_max_length: u32,
    },
    Error {
        message: String,
    },
    ShellScreen {
        buffer_id: String,
        rows: Vec<ShellScreenRow>,
        cursor_row: u16,
        cursor_col: u16,
        cursor_visible: bool,
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
    ShellInput { buffer_id: String, data: String },
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
    #[serde(default)]
    pub modes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionMeta {
    pub id: String,
    pub label: String,
    pub nick: String,
    pub connected: bool,
    #[serde(default)]
    pub user_modes: String,
    #[serde(default)]
    pub lag: Option<u64>,
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

/// Complete shell screen state for rendering in the web frontend.
#[derive(Debug, Clone)]
pub struct ShellScreenData {
    pub buffer_id: String,
    pub rows: Vec<ShellScreenRow>,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub cursor_visible: bool,
}

/// A row of styled text spans for shell screen rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellScreenRow {
    pub spans: Vec<ShellSpan>,
}

/// A run of characters sharing the same style in a shell screen row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellSpan {
    pub text: String,
    #[serde(default)]
    pub fg: String,
    #[serde(default)]
    pub bg: String,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub underline: bool,
    #[serde(default)]
    pub inverse: bool,
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

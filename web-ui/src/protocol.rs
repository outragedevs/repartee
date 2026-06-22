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
        #[serde(default = "default_true")]
        emotes_enabled: bool,
    },
    NewMessage {
        buffer_id: String,
        message: WireMessage,
    },
    /// A historical gap-fill row (reconnect CHATHISTORY) — inserted by
    /// `(timestamp, id)` rather than appended, since it may be older than
    /// already-displayed live messages.
    InsertMessage {
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
        #[serde(default = "default_true")]
        nick_colors: bool,
        #[serde(default = "default_true")]
        nick_colors_in_nicklist: bool,
        #[serde(default = "default_saturation")]
        nick_color_saturation: f32,
        #[serde(default = "default_lightness")]
        nick_color_lightness: f32,
        #[serde(default = "default_true")]
        emotes_enabled: bool,
    },
    Error {
        message: String,
        #[serde(default)]
        session_id: Option<String>,
    },
    ShellScreen {
        buffer_id: String,
        cols: u16,
        rows: Vec<ShellScreenRow>,
        cursor_row: u16,
        cursor_col: u16,
        cursor_visible: bool,
        #[serde(default)]
        session_id: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WebCommand {
    SendMessage {
        buffer_id: String,
        text: String,
    },
    SwitchBuffer {
        buffer_id: String,
    },
    MarkRead {
        buffer_id: String,
        up_to: i64,
    },
    FetchMessages {
        buffer_id: String,
        limit: u32,
        before: Option<i64>,
        /// Oldest loaded message's id (DB rowid for log-sourced rows), forming a
        /// `(before, before_id)` keyset cursor so same-second rows aren't dropped.
        before_id: Option<i64>,
    },
    FetchNickList {
        buffer_id: String,
    },
    FetchMentions,
    RunCommand {
        buffer_id: String,
        text: String,
    },
    ShellInput {
        buffer_id: String,
        data: String,
    },
    ShellResize {
        buffer_id: String,
        cols: u16,
        rows: u16,
    },
    /// Add or edit a server from the web wizard (mirrors the server-side
    /// variant). Boxed because the payload dwarfs the other variants.
    SaveServer(Box<SaveServerCmd>),
}

/// Payload of [`WebCommand::SaveServer`]. `id` None = add (id derived from
/// `network`); Some = edit. Credentials: None = unchanged, Some("") = clear,
/// Some(v) = set.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "flat wire DTO mirroring the wizard's boolean fields"
)]
pub struct SaveServerCmd {
    #[serde(default)]
    pub id: Option<String>,
    pub network: String,
    pub address: String,
    #[serde(default)]
    pub port: Option<u16>,
    pub tls: bool,
    pub tls_verify: bool,
    pub autoconnect: bool,
    #[serde(default)]
    pub channels: String,
    #[serde(default)]
    pub nick: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub realname: String,
    #[serde(default)]
    pub bind_ip: String,
    #[serde(default)]
    pub encoding: String,
    #[serde(default)]
    pub sasl_user: String,
    #[serde(default)]
    pub sasl_mechanism: String,
    #[serde(default)]
    pub autosendcmd: String,
    #[serde(default)]
    pub client_cert_path: String,
    #[serde(default)]
    pub auto_reconnect: bool,
    #[serde(default)]
    pub reconnect_delay: String,
    #[serde(default)]
    pub reconnect_max_retries: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub sasl_pass: Option<String>,
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
    /// Full-millisecond `@time`; the ordering key for sorted gap-fill inserts
    /// (whole-second `timestamp` is too coarse). `0` when the server omitted it,
    /// in which case ordering falls back to `timestamp * 1000`.
    #[serde(default)]
    pub ts_ms: i64,
    pub msg_type: String,
    pub nick: Option<String>,
    pub nick_mode: Option<String>,
    pub text: String,
    pub highlight: bool,
    /// SQLite log rowid for log-sourced messages (`None` for live in-memory
    /// rows). Echoed back as `FetchMessages.before_id` for a lossless keyset
    /// scroll-back cursor.
    #[serde(default)]
    pub log_id: Option<i64>,
    #[serde(default)]
    pub event_key: Option<String>,
    /// Server-extracted link previews. Empty when image previews are
    /// disabled or the message contains no eligible URLs.
    #[serde(default)]
    pub previews: Vec<LinkPreview>,
}

/// Mirror of `src/web/preview::LinkPreview`. Kept in sync manually because
/// the WASM crate cannot depend on the main crate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkPreview {
    pub link: String,
    pub kind: LinkPreviewKind,
    pub thumb_url: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LinkPreviewKind {
    /// Browser fetches `thumb_url` straight from the third-party host.
    ClientDirect,
    /// Browser fetches `thumb_url` from `/api/preview` on our server.
    ServerProxy,
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

fn default_true() -> bool {
    true
}
fn default_saturation() -> f32 {
    0.65
}
fn default_lightness() -> f32 {
    0.65
}

/// Complete shell screen state for rendering in the web frontend.
#[derive(Debug, Clone)]
pub struct ShellScreenData {
    pub cols: u16,
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

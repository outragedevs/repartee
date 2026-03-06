use crate::state::buffer::MessageType;

/// A row to be written to the log database.
#[derive(Debug, Clone)]
pub struct LogRow {
    pub msg_id: String,
    pub network: String,
    pub buffer: String,
    pub timestamp: i64,
    pub msg_type: MessageType,
    pub nick: Option<String>,
    pub text: String,
    pub highlight: bool,
    /// For fan-out events (quit/nick): points to the primary row's `msg_id`.
    /// Reference rows store empty text; the web frontend JOINs to get the full text.
    pub ref_id: Option<String>,
    /// JSON-serialized `IRCv3` message tags (`None` if empty).
    pub tags: Option<String>,
}

/// A message read back from the database.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct StoredMessage {
    pub id: i64,
    pub msg_id: String,
    pub network: String,
    pub buffer: String,
    pub timestamp: i64,
    pub msg_type: String,
    pub nick: Option<String>,
    pub text: String,
    pub highlight: bool,
    pub ref_id: Option<String>,
    /// JSON-serialized `IRCv3` message tags (`None` if empty).
    pub tags: Option<String>,
}

/// Per-client read position for a buffer.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ReadMarker {
    pub network: String,
    pub buffer: String,
    pub client: String,
    pub last_read: i64,
}

/// Stats about the log database.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct StorageStats {
    pub message_count: u64,
    pub db_size_bytes: u64,
}

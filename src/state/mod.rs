use indexmap::IndexMap;
use std::collections::HashMap;

use tokio::sync::mpsc;

pub mod buffer;
pub mod connection;
pub mod events;
pub mod sorting;

use buffer::Buffer;
use connection::Connection;

use crate::config::IgnoreEntry;
use crate::irc::flood::FloodState;
use crate::irc::netsplit::NetsplitState;
use crate::storage::LogRow;

pub struct AppState {
    pub connections: HashMap<String, Connection>,
    pub buffers: IndexMap<String, Buffer>,
    pub active_buffer_id: Option<String>,
    pub previous_buffer_id: Option<String>,
    pub message_counter: u64,
    /// Flood detection state (global, not per-connection).
    pub flood_state: FloodState,
    /// Netsplit detection state (global, not per-connection).
    pub netsplit_state: NetsplitState,
    /// Whether flood protection is enabled (from config).
    pub flood_protection: bool,
    /// Ignore rules (from config).
    pub ignores: Vec<IgnoreEntry>,
    /// Sender for the storage writer. When `Some`, messages are logged to `SQLite`.
    pub log_tx: Option<mpsc::UnboundedSender<LogRow>>,
    /// Message types excluded from logging (e.g. "event" to skip quit/join/nick fan-out).
    pub log_exclude_types: Vec<String>,
    /// Maximum messages per buffer (FIFO eviction). 0 = unlimited.
    pub scrollback_limit: usize,
}

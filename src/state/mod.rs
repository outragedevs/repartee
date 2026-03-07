use indexmap::IndexMap;
use std::collections::HashMap;

use tokio::sync::mpsc;

pub mod buffer;
pub mod connection;
pub mod events;
pub mod sorting;

use buffer::Buffer;
use connection::Connection;
use connection::ConnectionStatus;

use crate::config::IgnoreEntry;
use crate::irc::flood::FloodState;
use crate::irc::netsplit::NetsplitState;
use crate::scripting::engine::{
    BufferInfo, ConnectionInfo, NickInfo, ScriptStateSnapshot,
};
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

impl AppState {
    /// Build a lightweight snapshot of the current state for script callbacks.
    pub fn script_snapshot(&self) -> ScriptStateSnapshot {
        let connections: Vec<ConnectionInfo> = self
            .connections
            .values()
            .map(|c| ConnectionInfo {
                id: c.id.clone(),
                label: c.label.clone(),
                nick: c.nick.clone(),
                connected: c.status == ConnectionStatus::Connected,
                user_modes: c.user_modes.clone(),
            })
            .collect();

        let buffers: Vec<BufferInfo> = self
            .buffers
            .values()
            .map(|b| {
                let bt = match b.buffer_type {
                    buffer::BufferType::Server => "server",
                    buffer::BufferType::Channel => "channel",
                    buffer::BufferType::Query => "query",
                    buffer::BufferType::Special => "special",
                };
                BufferInfo {
                    id: b.id.clone(),
                    connection_id: b.connection_id.clone(),
                    name: b.name.clone(),
                    buffer_type: bt.to_string(),
                    topic: b.topic.clone(),
                    unread_count: b.unread_count,
                }
            })
            .collect();

        let mut buffer_nicks: HashMap<String, Vec<NickInfo>> = HashMap::new();
        for (buf_id, buf) in &self.buffers {
            if !buf.users.is_empty() {
                let nicks = buf
                    .users
                    .values()
                    .map(|e| NickInfo {
                        nick: e.nick.clone(),
                        prefix: e.prefix.clone(),
                        modes: e.modes.clone(),
                        away: e.away,
                    })
                    .collect();
                buffer_nicks.insert(buf_id.clone(), nicks);
            }
        }

        ScriptStateSnapshot {
            active_buffer_id: self.active_buffer_id.clone(),
            connections,
            buffers,
            buffer_nicks,
            script_config: HashMap::new(),
            app_config_toml: None,
        }
    }
}

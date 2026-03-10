use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::config::ServerConfig;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    Disconnected,
    Error,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Connection {
    pub id: String,
    pub label: String,
    pub status: ConnectionStatus,
    pub nick: String,
    pub user_modes: String,
    pub isupport: HashMap<String, String>,
    pub isupport_parsed: crate::irc::isupport::Isupport,
    pub error: Option<String>,
    pub lag: Option<u64>,
    /// Whether a PING has been sent and we're still waiting for PONG.
    pub lag_pending: bool,
    /// Number of reconnect attempts made so far.
    pub reconnect_attempts: u32,
    /// Base delay in seconds between reconnect attempts.
    pub reconnect_delay_secs: u64,
    /// When the next reconnect attempt should be made.
    pub next_reconnect: Option<std::time::Instant>,
    /// Whether auto-reconnect is enabled. Set to false when user explicitly /disconnects.
    pub should_reconnect: bool,
    /// Channels that were joined before disconnect, for auto-rejoin on reconnect.
    pub joined_channels: Vec<String>,
    /// The server config used to establish this connection.
    /// Stored so ad-hoc connections (from `/connect address`) can reconnect
    /// without requiring a matching entry in the config file.
    pub origin_config: ServerConfig,
    /// `IRCv3` capabilities that were successfully negotiated with the server.
    pub enabled_caps: HashSet<String>,
    /// Counter for WHOX tokens. Each `WHO %fields,TOKEN` request gets
    /// a unique numeric token so we can match 354 replies.
    pub who_token_counter: u32,
    /// Channels with a pending auto-WHO (e.g. on join).
    /// WHO/WHOX replies for these channels update state silently
    /// without display. Removed on `RPL_ENDOFWHO` (315).
    pub silent_who_channels: HashSet<String>,
}

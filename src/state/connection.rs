use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

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
    /// Local IP address of the IRC TCP socket (for DCC own-IP fallback).
    pub local_ip: Option<IpAddr>,
    /// `IRCv3` capabilities that were successfully negotiated with the server.
    pub enabled_caps: HashSet<String>,
    /// Client-side `draft/chathistory` request state (in-flight tracking and
    /// per-target server-history exhaustion).
    pub chathistory: crate::irc::chathistory::HistoryState,
    /// Counter for WHOX tokens. Each `WHO %fields,TOKEN` request gets
    /// a unique numeric token so we can match 354 replies.
    pub who_token_counter: u32,
    /// Channels with a pending auto-WHO (e.g. on join).
    /// WHO/WHOX replies for these channels update state silently
    /// without display. Removed on `RPL_ENDOFWHO` (315).
    pub silent_who_channels: HashSet<String>,
    /// Channels with a pending auto ban-list sync (e.g. on join).
    /// `367/368` replies for these channels update state silently
    /// without display. Removed on `RPL_ENDOFBANLIST` (368).
    pub silent_banlist_channels: HashSet<String>,
    /// Negotiated `draft/multiline` limits, or `None` when the cap is not active.
    pub multiline: Option<crate::irc::multiline::MultilineLimits>,
    /// Monotonic counter for outbound multiline BATCH reference tags.
    pub batch_ref_counter: u64,
}

impl Connection {
    /// Allocate a unique outbound BATCH reference tag for this connection.
    // TEMPORARY allow: wired into the outbound multiline path in Phase 4.
    #[allow(dead_code)]
    pub fn next_batch_ref(&mut self) -> String {
        self.batch_ref_counter = self.batch_ref_counter.wrapping_add(1);
        format!("ml{}", self.batch_ref_counter)
    }
}

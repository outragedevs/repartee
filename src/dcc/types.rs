use std::net::IpAddr;
use std::time::Instant;

/// DCC sub-protocol type. Currently only Chat; Send can be added later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DccType {
    Chat,
}

/// State machine for a DCC connection lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DccState {
    /// Incoming request received, waiting for user to accept.
    WaitingUser,
    /// Our TCP listener is open, waiting for peer to connect.
    Listening,
    /// Outgoing TCP `connect()` in progress.
    Connecting,
    /// TCP connected, actively exchanging chat lines.
    Connected,
}

/// A single DCC connection record.
#[derive(Debug, Clone)]
pub struct DccRecord {
    /// Unique ID: nick, or nick2/nick3 if multiple DCC to same nick.
    pub id: String,
    /// DCC sub-protocol type — reserved for future DCC SEND support.
    #[allow(dead_code)]
    pub dcc_type: DccType,
    /// Remote user's current nick.
    pub nick: String,
    /// IRC connection ID this DCC was initiated from.
    pub conn_id: String,
    /// Remote IP address (fake 1.1.1.1 for outgoing passive).
    pub addr: IpAddr,
    /// Remote port (0 = passive DCC).
    pub port: u16,
    pub state: DccState,
    /// Token for passive/reverse DCC matching.
    pub passive_token: Option<u32>,
    /// When this record was created (for timeout).
    pub created: Instant,
    /// When the TCP connection was established.
    pub started: Option<Instant>,
    /// Total bytes transferred over this connection.
    pub bytes_transferred: u64,
    /// Whether remote uses mIRC CTCP style (default true, auto-detected).
    #[allow(dead_code)]
    pub mirc_ctcp: bool,
    /// Remote ident (from original CTCP request).
    #[allow(dead_code)]
    pub ident: String,
    /// Remote hostname (from original CTCP request).
    #[allow(dead_code)]
    pub host: String,
}

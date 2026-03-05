use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    Disconnected,
    Error,
}

#[derive(Debug, Clone)]
pub struct Connection {
    pub id: String,
    pub label: String,
    pub status: ConnectionStatus,
    pub nick: String,
    pub user_modes: String,
    pub isupport: HashMap<String, String>,
    pub error: Option<String>,
    pub lag: Option<u64>,
}

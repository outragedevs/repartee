use indexmap::IndexMap;
use std::collections::HashMap;

pub mod buffer;
pub mod connection;
pub mod events;
pub mod sorting;

use buffer::Buffer;
use connection::Connection;

pub struct AppState {
    pub connections: HashMap<String, Connection>,
    pub buffers: IndexMap<String, Buffer>,
    pub active_buffer_id: Option<String>,
    pub previous_buffer_id: Option<String>,
    pub message_counter: u64,
}

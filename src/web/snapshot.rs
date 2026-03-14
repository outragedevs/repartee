use crate::state::AppState;
use crate::state::buffer::{BufferType, Message};
use crate::state::connection::ConnectionStatus;
use crate::web::protocol::{BufferMeta, ConnectionMeta, WebEvent, WireMessage, WireNick};

/// Build a `SyncInit` event from the current `AppState`.
pub fn build_sync_init(state: &AppState, mention_count: u32) -> WebEvent {
    let buffers: Vec<BufferMeta> = state
        .buffers
        .values()
        .map(|b| BufferMeta {
            id: b.id.clone(),
            connection_id: b.connection_id.clone(),
            name: b.name.clone(),
            buffer_type: buffer_type_str(&b.buffer_type).to_string(),
            topic: b.topic.clone(),
            unread_count: b.unread_count,
            activity: b.activity as u8,
            nick_count: u32::try_from(b.users.len()).unwrap_or(u32::MAX),
        })
        .collect();

    let connections: Vec<ConnectionMeta> = state
        .connections
        .values()
        .map(|c| ConnectionMeta {
            id: c.id.clone(),
            label: c.label.clone(),
            nick: c.nick.clone(),
            connected: c.status == ConnectionStatus::Connected,
        })
        .collect();

    WebEvent::SyncInit {
        buffers,
        connections,
        mention_count,
    }
}

/// Build a `NickList` event for a specific buffer.
pub fn build_nick_list(state: &AppState, buffer_id: &str) -> Option<WebEvent> {
    let buf = state.buffers.get(buffer_id)?;
    let nicks: Vec<WireNick> = buf
        .users
        .values()
        .map(|n| WireNick {
            nick: n.nick.clone(),
            prefix: n.prefix.clone(),
            modes: n.modes.clone(),
            away: n.away,
        })
        .collect();
    Some(WebEvent::NickList {
        buffer_id: buffer_id.to_string(),
        nicks,
    })
}

/// Convert a state `Message` to a `WireMessage` for transport.
pub fn message_to_wire(msg: &Message) -> WireMessage {
    WireMessage {
        id: msg.id,
        timestamp: msg.timestamp.timestamp(),
        msg_type: msg.message_type.as_str().to_string(),
        nick: msg.nick.clone(),
        nick_mode: msg.nick_mode.clone(),
        text: msg.text.clone(),
        highlight: msg.highlight,
    }
}

/// Convert a `StoredMessage` (from `SQLite`) to a `WireMessage`.
pub fn stored_to_wire(msg: &crate::storage::types::StoredMessage) -> WireMessage {
    WireMessage {
        id: u64::try_from(msg.id).unwrap_or(0),
        timestamp: msg.timestamp,
        msg_type: msg.msg_type.clone(),
        nick: msg.nick.clone(),
        nick_mode: None,
        text: msg.text.clone(),
        highlight: msg.highlight,
    }
}

const fn buffer_type_str(bt: &BufferType) -> &'static str {
    match bt {
        BufferType::Server => "server",
        BufferType::Channel => "channel",
        BufferType::Query => "query",
        BufferType::DccChat => "dcc_chat",
        BufferType::Special => "special",
    }
}

/// Split a `buffer_id` (`"connection_id/buffer_name"`) into `(network, buffer)`.
pub fn split_buffer_id(buffer_id: &str) -> (&str, &str) {
    buffer_id
        .split_once('/')
        .unwrap_or((buffer_id, buffer_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::buffer::{ActivityLevel, Buffer, BufferType, MessageType};
    use chrono::Utc;
    use std::collections::HashMap;

    fn make_test_state() -> AppState {
        let mut state = AppState::new();
        state.buffers.insert(
            "libera/#rust".to_string(),
            Buffer {
                id: "libera/#rust".to_string(),
                connection_id: "libera".to_string(),
                buffer_type: BufferType::Channel,
                name: "#rust".to_string(),
                messages: Vec::new(),
                activity: ActivityLevel::None,
                unread_count: 3,
                last_read: Utc::now(),
                topic: Some("Welcome to #rust".to_string()),
                topic_set_by: None,
                users: HashMap::new(),
                modes: None,
                mode_params: None,
                list_modes: HashMap::new(),
                last_speakers: Vec::new(),
            },
        );
        state
    }

    #[test]
    fn sync_init_includes_buffers() {
        let state = make_test_state();
        let event = build_sync_init(&state, 5);
        match event {
            WebEvent::SyncInit {
                buffers,
                mention_count,
                ..
            } => {
                assert_eq!(buffers.len(), 1);
                assert_eq!(buffers[0].name, "#rust");
                assert_eq!(buffers[0].unread_count, 3);
                assert_eq!(buffers[0].buffer_type, "channel");
                assert_eq!(mention_count, 5);
            }
            _ => panic!("expected SyncInit"),
        }
    }

    #[test]
    fn nick_list_returns_none_for_unknown_buffer() {
        let state = make_test_state();
        assert!(build_nick_list(&state, "nonexistent").is_none());
    }

    #[test]
    fn message_to_wire_converts_correctly() {
        let msg = crate::state::buffer::Message {
            id: 42,
            timestamp: Utc::now(),
            message_type: MessageType::Message,
            nick: Some("ferris".to_string()),
            nick_mode: Some("@".to_string()),
            text: "hello".to_string(),
            highlight: true,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: HashMap::new(),
        };
        let wire = message_to_wire(&msg);
        assert_eq!(wire.id, 42);
        assert_eq!(wire.nick.as_deref(), Some("ferris"));
        assert_eq!(wire.nick_mode.as_deref(), Some("@"));
        assert!(wire.highlight);
    }

    #[test]
    fn split_buffer_id_works() {
        assert_eq!(split_buffer_id("libera/#rust"), ("libera", "#rust"));
        assert_eq!(split_buffer_id("no_slash"), ("no_slash", "no_slash"));
    }
}

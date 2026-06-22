use crate::state::AppState;
use crate::state::buffer::{BufferType, Message};
use crate::state::connection::ConnectionStatus;
use crate::state::sorting::sort_buffers;
use crate::web::protocol::{BufferMeta, ConnectionMeta, WebEvent, WireMessage, WireNick};

/// Build a `SyncInit` event from the current `AppState`.
///
/// Buffers are sorted to match terminal order: connection label → `sort_group` → name.
/// `timestamp_format` and `emotes_enabled` come from config (not in `AppState`).
pub fn build_sync_init(
    state: &AppState,
    mention_count: u32,
    timestamp_format: &str,
    emotes_enabled: bool,
) -> WebEvent {
    // Sort buffers in the same order as the terminal sidebar.
    let buf_refs: Vec<_> = state.buffers.values().collect();
    let sorted = sort_buffers(&buf_refs, |conn_id| {
        state
            .connections
            .get(conn_id)
            .map_or_else(|| conn_id.to_string(), |c| c.label.clone())
    });

    let buffers: Vec<BufferMeta> = sorted
        .iter()
        .map(|b| BufferMeta {
            id: b.id.clone(),
            connection_id: b.connection_id.clone(),
            name: b.name.clone(),
            buffer_type: buffer_type_str(&b.buffer_type).to_string(),
            topic: b.topic.clone(),
            unread_count: b.unread_count,
            activity: b.activity as u8,
            nick_count: u32::try_from(b.users.len()).unwrap_or(u32::MAX),
            modes: b.modes.clone(),
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
            user_modes: c.user_modes.clone(),
            lag: c.lag,
        })
        .collect();

    WebEvent::SyncInit {
        buffers,
        connections,
        mention_count,
        active_buffer_id: state.active_buffer_id.clone(),
        timestamp_format: timestamp_format.to_string(),
        emotes_enabled,
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
        session_id: None,
    })
}

/// Convert a state `Message` to a `WireMessage` for transport.
///
/// `extractor` populates [`WireMessage::previews`] when web image previews
/// are enabled; pass `None` to leave it empty (the field is also skipped
/// from JSON when empty so old/disabled clients see no change).
pub fn message_to_wire(
    msg: &Message,
    extractor: Option<&crate::web::preview::WebPreviewExtractor>,
) -> WireMessage {
    WireMessage {
        id: msg.id,
        timestamp: msg.timestamp.timestamp(),
        msg_type: msg.message_type.as_str().to_string(),
        nick: msg.nick.clone(),
        nick_mode: msg.nick_mode.clone(),
        text: msg.text.clone(),
        highlight: msg.highlight,
        // Only set when this in-memory message was itself loaded from the log DB
        // (backlog); live messages have no rowid yet.
        log_id: msg.log_msg_id.as_ref().and_then(|s| s.parse::<i64>().ok()),
        event_key: msg.event_key.clone(),
        previews: extractor.map(|e| e.extract(&msg.text)).unwrap_or_default(),
    }
}

/// Convert a `StoredMessage` (from `SQLite`) to a `WireMessage`.
pub fn stored_to_wire(
    msg: &crate::storage::types::StoredMessage,
    extractor: Option<&crate::web::preview::WebPreviewExtractor>,
) -> WireMessage {
    WireMessage {
        id: u64::try_from(msg.id).unwrap_or(0),
        timestamp: msg.timestamp,
        msg_type: msg.msg_type.clone(),
        nick: msg.nick.clone(),
        nick_mode: None,
        text: msg.text.clone(),
        highlight: msg.highlight,
        // The SQLite rowid — the lossless scroll-back cursor key.
        log_id: Some(msg.id),
        event_key: msg.event_key.clone(),
        previews: extractor.map(|e| e.extract(&msg.text)).unwrap_or_default(),
    }
}

pub const fn buffer_type_str(bt: &BufferType) -> &'static str {
    match bt {
        BufferType::Mentions => "mentions",
        BufferType::Server => "server",
        BufferType::Channel => "channel",
        BufferType::Query => "query",
        BufferType::DccChat => "dcc_chat",
        BufferType::Special => "special",
        BufferType::Shell => "shell",
        BufferType::Log => "log",
    }
}

/// Split a `buffer_id` (`"connection_id/buffer_name"`) into `(network, buffer)`.
pub fn split_buffer_id(buffer_id: &str) -> (&str, &str) {
    buffer_id.split_once('/').unwrap_or((buffer_id, buffer_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::buffer::{ActivityLevel, Buffer, BufferType, MessageType};
    use chrono::Utc;
    use std::collections::{HashMap, VecDeque};

    fn make_test_state() -> AppState {
        let mut state = AppState::new();
        state.buffers.insert(
            "libera/#rust".to_string(),
            Buffer {
                id: "libera/#rust".to_string(),
                connection_id: "libera".to_string(),
                buffer_type: BufferType::Channel,
                name: "#rust".to_string(),
                messages: VecDeque::new(),
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
                peer_handle: None,
                log_total_lines: None,
                log_oldest_ts: None,
                log_newest_ts: None,
                history_exhausted: false,
                log_initial_loaded: false,
                pin_backlog: false,
            },
        );
        state
    }

    #[test]
    fn sync_init_includes_buffers() {
        let state = make_test_state();
        let event = build_sync_init(&state, 5, "%H:%M", false);
        match event {
            WebEvent::SyncInit {
                buffers,
                mention_count,
                emotes_enabled,
                ..
            } => {
                assert_eq!(buffers.len(), 1);
                assert_eq!(buffers[0].name, "#rust");
                assert_eq!(buffers[0].unread_count, 3);
                assert_eq!(buffers[0].buffer_type, "channel");
                assert_eq!(mention_count, 5);
                assert!(
                    !emotes_enabled,
                    "build_sync_init must carry the emotes flag"
                );
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
            tags: None,
        };
        let wire = message_to_wire(&msg, None);
        assert_eq!(wire.id, 42);
        assert_eq!(wire.nick.as_deref(), Some("ferris"));
        assert_eq!(wire.nick_mode.as_deref(), Some("@"));
        assert!(wire.highlight);
        assert!(wire.event_key.is_none());
        assert!(wire.previews.is_empty(), "no extractor → no previews");
    }

    #[test]
    fn message_to_wire_preserves_event_key() {
        let msg = crate::state::buffer::Message {
            id: 99,
            timestamp: Utc::now(),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text: "alice has joined #rust".to_string(),
            highlight: false,
            event_key: Some("join".to_string()),
            event_params: Some(vec!["alice".to_string(), "#rust".to_string()]),
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
        };
        let wire = message_to_wire(&msg, None);
        assert_eq!(wire.event_key.as_deref(), Some("join"));
    }

    #[test]
    fn message_to_wire_populates_previews_when_extractor_provided() {
        let extractor = crate::web::preview::WebPreviewExtractor::new(vec![0u8; 32], 4, 200);
        let msg = crate::state::buffer::Message {
            id: 1,
            timestamp: Utc::now(),
            message_type: MessageType::Message,
            nick: Some("alice".into()),
            nick_mode: None,
            text: "look at https://example.com/photo.jpg please".into(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
        };
        let wire = message_to_wire(&msg, Some(&extractor));
        assert_eq!(wire.previews.len(), 1);
        assert_eq!(
            wire.previews[0].kind,
            crate::web::preview::LinkPreviewKind::ServerProxy
        );
    }

    #[test]
    fn split_buffer_id_works() {
        assert_eq!(split_buffer_id("libera/#rust"), ("libera", "#rust"));
        assert_eq!(split_buffer_id("no_slash"), ("no_slash", "no_slash"));
    }

    #[test]
    fn stored_to_wire_preserves_event_key() {
        let stored = crate::storage::types::StoredMessage {
            id: 1,
            msg_id: "msg-1".to_string(),
            network: "Libera".to_string(),
            buffer: "#rust".to_string(),
            timestamp: 1_710_000_000,
            ts_ms: 1_710_000_000_000,
            msg_type: "event".to_string(),
            nick: None,
            text: "You were kicked from #rust by op (behave)".to_string(),
            highlight: true,
            ref_id: None,
            tags: None,
            event_key: Some("kicked".to_string()),
        };
        let wire = stored_to_wire(&stored, None);
        assert_eq!(wire.event_key.as_deref(), Some("kicked"));
        assert!(wire.highlight);
    }
}

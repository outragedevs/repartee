use crate::state::AppState;
use crate::state::buffer::{BufferType, Message};
use crate::state::connection::ConnectionStatus;
use crate::state::sorting::sort_buffers;
use crate::web::protocol::{BufferMeta, ConnectionMeta, WebEvent, WireMessage, WireNick};

/// Build a `SyncInit` event from the current `AppState`.
///
/// Buffers are sorted to match terminal order: connection label → `sort_group` → name.
/// `timestamp_format`, `emotes_enabled` and `statusbar` come from config (not in
/// `AppState`).
pub fn build_sync_init(
    state: &AppState,
    mention_count: u32,
    timestamp_format: &str,
    emotes_enabled: bool,
    statusbar: &crate::config::StatusbarConfig,
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
            e2e_enabled: matches!(b.buffer_type, BufferType::Channel | BufferType::Query)
                && state.e2e_enabled_for_target(&b.connection_id, &b.name),
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

    // Seed the client's typing indicators. Without this a tab that connects (or
    // lag-resyncs) while someone is mid-sentence shows nothing until the visible
    // set next changes: the sender's 3s refresh is not a change, and `paused`
    // is never resent at all.
    //
    // `typing.show = false` already keeps the tracker empty (ingestion is gated
    // in `handle_tagmsg`), but the snapshot must not depend on that — state left
    // over from before the setting was switched off must not leak to the client.
    let typing = if state.typing_show {
        state.typing.snapshot()
    } else {
        std::collections::HashMap::new()
    };

    WebEvent::SyncInit {
        buffers,
        connections,
        mention_count,
        active_buffer_id: state.active_buffer_id.clone(),
        timestamp_format: timestamp_format.to_string(),
        emotes_enabled,
        typing,
        statusbar_items: statusbar_item_names(statusbar),
        statusbar_enabled: statusbar.enabled,
    }
}

/// The status line's items as the wire names `/items` uses — the browser
/// renders straight from this list, so it must never grow a second naming
/// scheme (`parse_statusbar_item` is the inverse, and a test pins the trip).
pub fn statusbar_item_names(statusbar: &crate::config::StatusbarConfig) -> Vec<String> {
    statusbar
        .items
        .iter()
        .map(|item| crate::commands::handlers_ui::statusbar_item_name(item).to_string())
        .collect()
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
        ts_ms: msg.timestamp.timestamp_millis(),
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
        // Live-only, exactly as in the TUI: the log stores the flat text.
        orig_offset: msg.wire_origin.as_ref().and_then(|o| o.suffix_at),
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
        ts_ms: msg.ts_ms,
        msg_type: msg.msg_type.clone(),
        nick: msg.nick.clone(),
        nick_mode: None,
        text: msg.text.clone(),
        highlight: msg.highlight,
        // The SQLite rowid — the lossless scroll-back cursor key.
        log_id: Some(msg.id),
        event_key: msg.event_key.clone(),
        previews: extractor.map(|e| e.extract(&msg.text)).unwrap_or_default(),
        // A stored row's text is flat — the suffix is indistinguishable from
        // any other trailing brackets, and must not be guessed at.
        orig_offset: None,
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
    use crate::config::{StatusbarConfig, StatusbarItem};
    use crate::state::buffer::{ActivityLevel, Buffer, BufferType, MessageType};
    use chrono::Utc;
    use std::collections::{HashMap, VecDeque};
    use std::time::Instant;

    /// Every `build_sync_init` call in these tests that doesn't care about the
    /// statusbar passes this.
    fn statusbar() -> StatusbarConfig {
        StatusbarConfig::default()
    }

    fn sync_init_statusbar(event: &WebEvent) -> (&[String], bool) {
        match event {
            WebEvent::SyncInit {
                statusbar_items,
                statusbar_enabled,
                ..
            } => (statusbar_items, *statusbar_enabled),
            _ => panic!("expected SyncInit"),
        }
    }

    #[test]
    fn sync_init_carries_the_statusbar_items_in_config_order() {
        // The browser must render from the config, not from a hardcoded
        // sequence — otherwise `/items move`, `/items remove` and
        // `statusbar.enabled = false` are no-ops in the tab, and the two UIs
        // disagree the moment either is touched.
        let state = make_test_state();
        let event = build_sync_init(&state, 0, "%H:%M", false, &statusbar());
        let (items, enabled) = sync_init_statusbar(&event);
        assert!(enabled);
        assert_eq!(
            items,
            ["time", "nick_info", "channel_info", "typing", "lag", "active_windows"]
        );
    }

    #[test]
    fn sync_init_statusbar_names_round_trip_through_the_parser() {
        // The wire names are the ones `/items` speaks. A second naming scheme
        // would drift silently.
        let state = make_test_state();
        let event = build_sync_init(&state, 0, "%H:%M", false, &statusbar());
        let (items, _) = sync_init_statusbar(&event);
        let parsed: Vec<StatusbarItem> = items
            .iter()
            .map(|name| {
                crate::commands::handlers_ui::parse_statusbar_item(name)
                    .unwrap_or_else(|| panic!("{name} must round-trip"))
            })
            .collect();
        assert_eq!(parsed, StatusbarConfig::default().items);
    }

    #[test]
    fn sync_init_carries_a_customised_statusbar_verbatim() {
        let state = make_test_state();
        let custom = StatusbarConfig {
            enabled: false,
            items: vec![StatusbarItem::Lag, StatusbarItem::Time],
            ..StatusbarConfig::default()
        };
        let event = build_sync_init(&state, 0, "%H:%M", false, &custom);
        let (items, enabled) = sync_init_statusbar(&event);
        assert!(!enabled, "statusbar.enabled = false must reach the browser");
        assert_eq!(items, ["lag", "time"]);
    }

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
        let event = build_sync_init(&state, 5, "%H:%M", false, &statusbar());
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

    /// A second channel buffer, so "only buffers with typers appear" is a real
    /// assertion and not a tautology.
    fn add_channel(state: &mut AppState, id: &str, name: &str) {
        let mut buf = state.buffers["libera/#rust"].clone();
        buf.id = id.to_string();
        buf.name = name.to_string();
        state.buffers.insert(id.to_string(), buf);
    }

    fn sync_init_typing(event: &WebEvent) -> &std::collections::HashMap<String, Vec<String>> {
        match event {
            WebEvent::SyncInit { typing, .. } => typing,
            _ => panic!("expected SyncInit"),
        }
    }

    #[test]
    fn sync_init_seeds_typing_only_for_buffers_that_have_typers() {
        // A browser tab that connects mid-typing must be told who is already
        // typing: the sender's 3s refresh is not a visible change, so it pushes
        // nothing, and a `paused` peer is never resent at all.
        let mut state = make_test_state();
        add_channel(&mut state, "libera/#tokio", "#tokio");
        state.typing.set(
            "libera/#rust",
            "alice",
            crate::irc::typing::TypingState::Active,
            Instant::now(),
        );

        let event = build_sync_init(&state, 0, "%H:%M", false, &statusbar());
        let typing = sync_init_typing(&event);
        assert_eq!(typing.len(), 1, "#tokio has no typers, so it has no entry");
        assert_eq!(typing["libera/#rust"], vec!["alice"]);
    }

    #[test]
    fn sync_init_typing_matches_the_live_push_exactly() {
        // The snapshot seeds the client; the live `Typing` event replaces it.
        // If they can disagree on order or spelling, the indicator jumps as soon
        // as the first live event lands. Both go through `TypingTracker::nicks`.
        let mut state = make_test_state();
        let now = Instant::now();
        state.typing.set(
            "libera/#rust",
            "Bob",
            crate::irc::typing::TypingState::Active,
            now,
        );
        state.typing.set(
            "libera/#rust",
            "alice",
            crate::irc::typing::TypingState::Paused,
            now,
        );

        let event = build_sync_init(&state, 0, "%H:%M", false, &statusbar());
        let seeded = sync_init_typing(&event)["libera/#rust"].clone();

        crate::irc::events::push_typing_web_event(&mut state, "libera/#rust");
        let pushed = match state.pending_web_events.last() {
            Some(WebEvent::Typing { nicks, .. }) => nicks.clone(),
            _ => panic!("expected a Typing event"),
        };

        assert_eq!(seeded, pushed);
        assert_eq!(seeded, vec!["alice", "Bob"]);
    }

    #[test]
    fn sync_init_carries_no_typing_when_typing_display_is_off() {
        // `typing.show = false` gates ingestion, so the tracker is normally
        // empty here anyway — the gate is explicit so the snapshot cannot leak
        // state left over from before the setting was switched off.
        let mut state = make_test_state();
        state.typing_show = false;
        state.typing.set(
            "libera/#rust",
            "alice",
            crate::irc::typing::TypingState::Active,
            Instant::now(),
        );

        let event = build_sync_init(&state, 0, "%H:%M", false, &statusbar());
        assert!(sync_init_typing(&event).is_empty());
    }

    #[test]
    fn sync_init_typing_is_present_but_empty_when_nobody_is_typing() {
        // Pinned: the field is always emitted (`"typing":{}`), never omitted —
        // see the JSON test in `protocol.rs`.
        let state = make_test_state();
        let event = build_sync_init(&state, 0, "%H:%M", false, &statusbar());
        assert!(sync_init_typing(&event).is_empty());
    }

    #[test]
    fn nick_list_returns_none_for_unknown_buffer() {
        let state = make_test_state();
        assert!(build_nick_list(&state, "nonexistent").is_none());
    }

    #[test]
    fn message_to_wire_converts_correctly() {
        let msg = crate::state::buffer::Message {
            log_key: None,
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
            wire_origin: None,
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
            log_key: None,
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
            wire_origin: None,
        };
        let wire = message_to_wire(&msg, None);
        assert_eq!(wire.event_key.as_deref(), Some("join"));
    }

    #[test]
    fn message_to_wire_populates_previews_when_extractor_provided() {
        let extractor = crate::web::preview::WebPreviewExtractor::new(vec![0u8; 32], 4, 200);
        let msg = crate::state::buffer::Message {
            log_key: None,
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
            wire_origin: None,
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
        assert_eq!(
            wire.orig_offset, None,
            "a stored row's text is flat — the suffix must never be guessed"
        );
    }

    #[test]
    fn a_translated_row_carries_its_dim_boundary_to_the_browser() {
        // Without this the web client receives one undifferentiated string
        // and renders the appended original in full brightness, while the
        // TUI dims it — the documented display differing per frontend.
        let msg = Message {
            log_key: None,
            id: 1,
            timestamp: chrono::Utc::now(),
            message_type: crate::state::buffer::MessageType::Message,
            nick: Some("alice".to_string()),
            nick_mode: None,
            text: "good morning [dzien dobry]".to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
            wire_origin: Some(crate::state::buffer::WireOrigin {
                text: "dzien dobry".to_string(),
                suffix_at: Some("good morning".len()),
            }),
        };
        let wire = message_to_wire(&msg, None);
        assert_eq!(wire.orig_offset, Some("good morning".len()));
        assert_eq!(&wire.text[wire.orig_offset.unwrap()..], " [dzien dobry]");
    }

    #[test]
    fn a_rewritten_row_with_no_suffix_sends_no_boundary() {
        // `show_original_in = false` still replaces the text, but there is
        // no appended original — so nothing to dim.
        let msg = Message {
            log_key: None,
            id: 1,
            timestamp: chrono::Utc::now(),
            message_type: crate::state::buffer::MessageType::Message,
            nick: Some("alice".to_string()),
            nick_mode: None,
            text: "good morning".to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
            wire_origin: Some(crate::state::buffer::WireOrigin {
                text: "dzien dobry".to_string(),
                suffix_at: None,
            }),
        };
        assert_eq!(message_to_wire(&msg, None).orig_offset, None);
    }
}

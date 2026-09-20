use super::*;

fn state() -> AppState {
    let mut state = super::tests::make_test_state();
    let conn = state.connections.get_mut("test").unwrap();
    conn.origin_config.bouncer_network_id = Some("42".into());
    conn.enabled_caps.insert("message-tags".into());
    state
}

fn receive(state: &mut AppState, line: &str) {
    handle_irc_message(state, "test", &line.parse().unwrap());
}

fn batch(target: &str, line: &str) -> crate::irc::batch::BatchInfo {
    crate::irc::batch::BatchInfo {
        batch_type: "CHATHISTORY".into(),
        params: vec![target.into()],
        messages: vec![line.parse().unwrap()],
        message_order: vec![0],
        redaction_refs: vec![],
        dropped_messages: 0,
        started_at: std::time::Instant::now(),
        opener_tags: None,
    }
}

#[test]
fn private_context_routes_to_existing_channel_and_web_without_query() {
    for tag in ["+channel-context", "+draft/channel-context"] {
        for prefix in ["Alice!u@h", "Alice", "Alice@h"] {
            for command in ["PRIVMSG", "NOTICE"] {
                let mut state = state();
                receive(
                    &mut state,
                    &format!("@{tag}=#TEST;msgid=context :{prefix} {command} me :context message"),
                );
                let channel = &state.buffers["test/#test"];
                assert_eq!(channel.messages.back().unwrap().text, "context message");
                assert!(!state.buffers.contains_key("test/alice"));
                assert!(state.pending_web_events.iter().any(|event| matches!(event, crate::web::protocol::WebEvent::NewMessage { buffer_id, .. } if buffer_id == "test/#test")));
            }
        }
    }
}

#[test]
fn public_and_invalid_context_do_not_redirect_or_create_channels() {
    for context in ["#missing", "Alice", "#bad\\sroom", "#one,#two", ""] {
        let mut state = state();
        receive(
            &mut state,
            &format!("@+channel-context={context} :Alice!u@h PRIVMSG me :private message"),
        );
        assert_eq!(
            state.buffers["test/alice"].messages.back().unwrap().text,
            "private message"
        );
        assert!(state.buffers["test/#test"].messages.is_empty());
        assert!(!state.buffers.contains_key("test/#missing"));
    }
    let mut state = state();
    state.add_buffer(Buffer::for_test("test", BufferType::Channel, "#other"));
    receive(
        &mut state,
        "@+channel-context=#other :Alice!u@h PRIVMSG #test :public message",
    );
    assert_eq!(
        state.buffers["test/#test"].messages.back().unwrap().text,
        "public message"
    );
    assert!(state.buffers["test/#other"].messages.is_empty());
}

#[test]
fn context_alias_precedence_and_server_casemapping_are_consistent() {
    let mut state = state();
    state.add_buffer(Buffer::for_test("test", BufferType::Channel, "#Room["));
    receive(
        &mut state,
        "@+channel-context=#ROOM{;+draft/channel-context=#test :Alice!u@h PRIVMSG me :folded",
    );
    assert_eq!(
        state.buffers["test/#room["].messages.back().unwrap().text,
        "folded"
    );
    receive(
        &mut state,
        "@+channel-context=;+draft/channel-context=#test :Alice!u@h PRIVMSG me :invalid final tag",
    );
    assert_eq!(
        state.buffers["test/alice"].messages.back().unwrap().text,
        "invalid final tag"
    );
    assert!(state.buffers["test/#test"].messages.is_empty());
}

#[test]
fn history_context_uses_batch_ownership_without_current_membership() {
    for command in ["PRIVMSG", "NOTICE"] {
        let mut state = state();
        state.buffers.get_mut("test/#test").unwrap().users.clear();
        let line = format!(
            "@+draft/channel-context=#TEST;time=2026-09-20T10:00:00Z;msgid=historical :Departed!u@h {command} me :old context"
        );
        let outcome = ingest_chathistory_batch(&state, "test", &batch("#test", &line), true);
        assert_eq!(outcome.display_rows.len(), 1);
        assert_eq!(outcome.display_rows[0].0, "test/#test");
        assert_eq!(outcome.display_rows[0].1.text, "old context");
        let private = ingest_chathistory_batch(&state, "test", &batch("Departed", &line), true);
        assert_eq!(private.display_rows[0].0, "test/departed");
        let other = ingest_chathistory_batch(&state, "test", &batch("#other", &line), true);
        assert_ne!(other.display_rows[0].0, "test/#test");
    }
}

#[test]
fn context_preserves_private_ignores_and_blocked_channel_policy() {
    for ignored in [true, false] {
        let mut state = state();
        if ignored {
            state.ignores.push(crate::config::IgnoreEntry {
                mask: "Alice!*@*".into(),
                levels: vec![IgnoreLevel::Msgs],
                channels: None,
            });
        } else {
            state.set_metadata("test", "#test", crate::irc::metadata::Key::Blocked, true);
        }
        receive(
            &mut state,
            "@+channel-context=#test :Alice!u@h PRIVMSG me :hidden",
        );
        assert!(state.buffers["test/#test"].messages.is_empty());
        assert!(!state.buffers.contains_key("test/alice"));
    }
}

#[test]
fn context_is_network_local_and_preserves_server_history_ownership() {
    for bouncer in [false, true] {
        let mut state = state();
        state
            .connections
            .get_mut("test")
            .unwrap()
            .origin_config
            .bouncer_network_id = bouncer.then(|| "42".into());
        let mut other = state.connections["test"].clone();
        other.id = "other".into();
        other.label = "Other".into();
        state.add_connection(other);
        state.add_buffer(Buffer::for_test("other", BufferType::Channel, "#test"));
        state.set_active_buffer("other/#test");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        state.log_tx = Some(tx);
        receive(
            &mut state,
            "@+channel-context=#test;msgid=scoped :Alice!u@h PRIVMSG me :scoped message",
        );
        assert_eq!(
            state.buffers["test/#test"].messages.back().unwrap().text,
            "scoped message"
        );
        assert!(state.buffers["other/#test"].messages.is_empty());
        assert_eq!(rx.try_recv().is_err(), bouncer);
    }
    let mut state = state();
    state
        .connections
        .get_mut("test")
        .unwrap()
        .enabled_caps
        .clear();
    receive(
        &mut state,
        "@+channel-context=#test :Alice!u@h PRIVMSG me :unnegotiated",
    );
    assert_eq!(
        state.buffers["test/alice"].messages.back().unwrap().text,
        "unnegotiated"
    );
    assert!(state.buffers["test/#test"].messages.is_empty());
}

#[test]
fn context_keeps_private_recipient_key_for_live_and_history_decryption() {
    use crate::e2e::keyring::{ChannelConfig, ChannelMode, Keyring};
    use crate::e2e::manager::E2eManager;
    use std::sync::{Arc, Mutex};

    let manager = || {
        E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(
            crate::storage::db::open_database(false).unwrap(),
        ))))
        .unwrap()
    };
    let ours = Arc::new(manager());
    let peer = manager();
    let mut state = state();
    state.e2e_manager = Some(Arc::clone(&ours));
    state.connections.get_mut("test").unwrap().own_handle = Some("~me@host".into());
    let request = ours
        .build_keyreq(&crate::e2e::scoped_context("TestServer", "@~me@host"))
        .unwrap();
    peer.keyring()
        .set_channel_config(&ChannelConfig {
            channel: "@~me@host".into(),
            enabled: true,
            mode: ChannelMode::AutoAccept,
        })
        .unwrap();
    let mut response = peer
        .handle_keyreq_with_nick("~me@host", Some("me"), &request)
        .unwrap()
        .unwrap();
    response.channel = crate::e2e::scoped_context("TestServer", &response.channel);
    ours.handle_keyrsp("~alice@a.host", &response).unwrap();
    let live = peer
        .encrypt_outgoing("@~me@host", "live secret")
        .unwrap()
        .remove(0);
    receive(
        &mut state,
        &format!(
            "@+channel-context=#test;msgid=encrypted-live :Alice!~alice@a.host PRIVMSG me :{live}"
        ),
    );
    assert_eq!(
        state.buffers["test/#test"].messages.back().unwrap().text,
        "live secret"
    );
    let archived = peer
        .encrypt_outgoing("@~me@host", "history secret")
        .unwrap()
        .remove(0);
    let history = batch(
        "#test",
        &format!(
            "@+channel-context=#test;msgid=encrypted-history;time=2026-09-20T10:00:00Z :Alice!~alice@a.host PRIVMSG me :{archived}"
        ),
    );
    let result = ingest_chathistory_batch(&state, "test", &history, true);
    assert_eq!(result.display_rows.len(), 1);
    assert_eq!(result.display_rows[0].0, "test/#test");
    assert_eq!(result.display_rows[0].1.text, "history secret");
}

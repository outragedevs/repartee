use super::*;
use crate::irc::{IrcEvent, IrcHandle, IrcSender};
use crate::state::buffer::{Buffer, BufferType};

fn app() -> super::super::App {
    let mut app = crate::app::input::submit_typing_tests::test_app();
    for id in ["first", "second"] {
        let config = toml::from_str("label='fixture'\naddress='localhost'\nport=6667\ntls=false\nchannels=[]\nnick='me'\nbouncer_network_id='1'").unwrap();
        app.setup_connection(id, &config);
        let conn = app.state.connections.get_mut(id).unwrap();
        conn.status = ConnectionStatus::Connected;
        conn.network_scope = Some(format!("account/{id}"));
        conn.enabled_caps.extend([metadata::CAP.into(), "batch".into()]);
        app.irc_handles.insert(id.into(), IrcHandle::new(id.into(), IrcSender::capturing(0), None, None));
        app.state.add_buffer_with_focus(Buffer::empty(id, BufferType::Channel, "#Room"), false);
    }
    app.state.set_active_buffer("first/#room");
    app.tick_bouncer_metadata();
    app
}

fn receive(app: &mut super::super::App, wire: &str) {
    app.handle_irc_event(IrcEvent::Message("first".into(), Box::new(wire.parse().unwrap())));
}

#[tokio::test]
async fn updates_before_subscription_ack_are_scoped_and_seed_new_buffers() {
    let mut app = app();
    let (tx, mut logs) = tokio::sync::mpsc::channel(8);
    app.state.log_tx = Some(tx);
    receive(&mut app, "METADATA #room soju.im/pinned * 1");
    receive(&mut app, "METADATA Alice soju.im/muted * 1");
    assert!(app.state.buffers["first/#room"].metadata.pinned);
    assert!(!app.state.buffers["second/#room"].metadata.pinned);
    app.state.add_buffer_with_focus(Buffer::empty("first", BufferType::Query, "Alice"), false);
    assert!(app.state.buffers["first/alice"].metadata.muted);
    receive(&mut app, "METADATA Alice soju.im/muted *");
    assert!(!app.state.buffers["first/alice"].metadata.muted);
    receive(&mut app, ":Mallory!u@h METADATA Alice soju.im/blocked * 1");
    assert!(!app.state.buffers["first/alice"].metadata.blocked);
    for key in Key::ALL { receive(&mut app, &format!(":bouncer 770 * {}", key.wire())); }
    assert_eq!(app.bouncer_metadata["first"].acknowledged.len(), 3);
    assert!(logs.try_recv().is_err());
    let snapshot = crate::web::snapshot::build_sync_init(&app.state, 0, "%H:%M", false, false, &app.config.statusbar);
    let crate::web::protocol::WebEvent::SyncInit { buffers, .. } = snapshot else { panic!("missing snapshot"); };
    assert!(buffers.iter().find(|buffer| buffer.id == "first/#room").unwrap().pinned);
}

#[tokio::test]
async fn commands_wait_for_ack_and_capability_loss_rearms_subscription() {
    let mut app = app();
    let sender = app.irc_handles["first"].sender().clone();
    assert_eq!(sender.captured().len(), 1);
    command(&mut app, &["#Room".into(), "mute".into(), "on".into()]);
    assert_eq!(sender.captured().len(), 1);
    for key in Key::ALL { receive(&mut app, &format!(":bouncer 770 * {}", key.wire())); }
    command(&mut app, &["#Room".into(), "mute".into(), "on".into()]);
    assert!(sender.captured().iter().any(|message| message.to_string() == "METADATA #Room SET soju.im/muted 1\r\n"));
    assert!(!app.state.buffers["first/#room"].metadata.muted);
    receive(&mut app, ":bouncer 761 * #Room soju.im/muted * 1");
    assert!(app.state.buffers["first/#room"].metadata.muted);
    receive(&mut app, ":bouncer CAP me DEL :draft/metadata-2");
    assert!(!app.bouncer_metadata.contains_key("first"));
    receive(&mut app, ":bouncer CAP me ACK :draft/metadata-2");
    assert!(app.bouncer_metadata["first"].acknowledged.is_empty());
    assert!(sender.captured().last().unwrap().to_string().starts_with("METADATA * SUB "));
}

#[tokio::test]
async fn native_sort_keeps_server_first_and_promotes_pinned_queries() {
    let mut app = app();
    app.state.add_buffer_with_focus(Buffer::empty("first", BufferType::Query, "Alice"), false);
    app.state.add_buffer_with_focus(Buffer::empty("first", BufferType::Channel, "#Other"), false);
    receive(&mut app, "METADATA Alice soju.im/pinned * 1");
    receive(&mut app, "METADATA #Other soju.im/muted * 1");
    let buffers: Vec<_> = app.state.buffers.values().filter(|buffer| buffer.connection_id == "first").collect();
    let sorted = crate::state::sorting::sort_buffers(&buffers, str::to_string);
    let names: Vec<_> = sorted.iter().map(|buffer| buffer.name.as_str()).collect();
    assert_eq!(names, ["fixture", "Alice", "#Room", "#Other"]);
}

#[tokio::test]
async fn blocking_filters_live_rows_invites_and_typing_and_purges_existing_rows() {
    let mut app = app();
    app.state.typing_show = true;
    receive(&mut app, ":Alice!u@h PRIVMSG #Room :visible before blocking");
    receive(&mut app, "@+typing=active :Alice!u@h TAGMSG #Room");
    assert!(app.state.buffers["first/#room"].messages.iter().any(|row| row.text.contains("visible before blocking")));
    assert!(!app.state.typing.nicks("first/#room").is_empty());
    receive(&mut app, "METADATA Alice soju.im/blocked * 1");
    assert!(!app.state.buffers["first/#room"].messages.iter().any(|row| row.nick.as_deref() == Some("Alice")));
    assert!(app.state.typing.nicks("first/#room").is_empty());
    let count = app.state.buffers["first/#room"].messages.len();
    for wire in [
        ":Alice!u@h PRIVMSG #Room :blocked message",
        ":Alice!u@h NOTICE #Room :blocked notice",
        ":Alice!u@h PRIVMSG #Room :\u{1}ACTION blocked action\u{1}",
        ":Alice!u@h INVITE me #Room",
        "@+typing=active :Alice!u@h TAGMSG #Room",
    ] { receive(&mut app, wire); }
    assert_eq!(app.state.buffers["first/#room"].messages.len(), count);
    assert!(app.state.typing.nicks("first/#room").is_empty());
    receive(&mut app, ":Bob!u@h PRIVMSG #Room :unrelated sender");
    assert_eq!(app.state.buffers["first/#room"].messages.len(), count + 1);
    receive(&mut app, "METADATA Alice soju.im/blocked * 0");
    receive(&mut app, ":Alice!u@h PRIVMSG #Room :visible after unblock");
    assert_eq!(app.state.buffers["first/#room"].messages.len(), count + 2);
}

#[tokio::test]
async fn muting_retains_messages_without_highlights_or_mention_fanout() {
    let mut app = app();
    receive(&mut app, "METADATA Alice soju.im/muted * 1");
    receive(&mut app, ":Alice!u@h PRIVMSG #Room :me: muted mention");
    receive(&mut app, ":Alice!u@h INVITE me #Room");
    let rows = &app.state.buffers["first/#room"].messages;
    assert!(rows.iter().any(|row| row.text.contains("muted mention")));
    assert!(rows.iter().any(|row| row.text.contains("invites you")));
    assert!(rows.iter().all(|row| !row.highlight));
    assert!(app.volatile_mentions.is_empty());
}

#[tokio::test]
async fn blocking_removes_reconstructed_mentions_and_notifies_web_drawers() {
    let mut app = app();
    for nick in ["Alice", "Bob"] {
        receive(&mut app, &format!(":{nick}!u@h PRIVMSG #Room :stored mention from {nick}"));
    }
    assert_eq!(app.volatile_mentions.len(), 2);
    let blocked_id = app.volatile_mentions.front().unwrap().1.source_message_id;
    app.create_mentions_buffer();
    assert_eq!(app.state.buffers[super::super::App::MENTIONS_BUFFER_ID].messages.len(), 2);
    let mut receiver = app.web_broadcaster.subscribe();
    receive(&mut app, "METADATA Alice soju.im/blocked * 1");
    assert_eq!(app.volatile_mentions.len(), 1);
    assert_eq!(app.volatile_mentions.front().unwrap().1.nick, "Bob");
    let rows = &app.state.buffers[super::super::App::MENTIONS_BUFFER_ID].messages;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].text.contains("Bob"));
    let mut notified = false;
    while let Ok(event) = receiver.try_recv() {
        if let crate::web::protocol::WebEvent::MentionsRedacted { message_ids } = event {
            notified |= message_ids.contains(&blocked_id);
        }
    }
    assert!(notified);
    app.state.buffers.shift_remove(super::super::App::MENTIONS_BUFFER_ID);
    app.create_mentions_buffer();
    assert_eq!(app.state.buffers[super::super::App::MENTIONS_BUFFER_ID].messages.len(), 1);
}

#[tokio::test]
async fn blocked_history_advances_cursors_and_late_pages_cannot_resurrect_rows() {
    let mut app = app();
    let batch = crate::irc::batch::BatchInfo {
        batch_type: "chathistory".into(), params: vec!["#Room".into()],
        messages: vec!["@time=2026-09-20T10:00:00.000Z;msgid=old :Alice!u@h PRIVMSG #Room :old page".parse().unwrap()],
        message_order: vec![], redaction_refs: vec![], dropped_messages: 0,
        started_at: Instant::now(), opener_tags: None,
    };
    let before_block = crate::irc::events::ingest_chathistory_batch(&app.state, "first", &batch, true);
    assert_eq!(before_block.display_rows.len(), 1);
    receive(&mut app, "METADATA Alice soju.im/blocked * 1");
    let blocked = crate::irc::events::ingest_chathistory_batch(&app.state, "first", &batch, true);
    assert!(blocked.display_rows.is_empty());
    assert_eq!(blocked.ingested, 0);
    assert_eq!(blocked.oldest, before_block.oldest);
    assert_eq!(blocked.newest, before_block.newest);
    assert_eq!(blocked.oldest.as_ref().unwrap().1.as_deref(), Some("old"));
    app.state.surface_history_page("first/#room", before_block.display_rows.into_iter().map(|(_, row)| row).collect(), true);
    assert!(!app.state.buffers["first/#room"].messages.iter().any(|row| row.text == "old page"));
}

#[tokio::test]
async fn metadata_is_rechecked_when_translation_releases_each_delivery_kind() {
    use crate::translate::queue::{ReadyDelivery, ReadyEntry, ReadyOrigin};
    use crate::state::buffer::{ActivityLevel, MessageType};
    let mut app = app();
    app.create_mentions_buffer();
    let mut row = crate::state::events::tests::make_test_message(&mut app.state, "delayed translated mention");
    row.message_type = MessageType::Message;
    row.nick = Some("Alice".into());
    row.highlight = true;
    receive(&mut app, "METADATA Alice soju.im/blocked * 1");
    app.state.pending_web_events.clear();
    for delivery in [ReadyDelivery::Logged, ReadyDelivery::Transient, ReadyDelivery::Local] {
        app.state.deliver_ready("first/#room", vec![ReadyEntry { id: row.id, message: row.clone(), activity: ActivityLevel::Mention, origin: ReadyOrigin::Translated, delivery }]);
    }
    assert!(app.state.pending_web_events.is_empty());
    assert!(!app.state.buffers["first/#room"].messages.iter().any(|message| message.id == row.id));
    assert!(app.state.buffers[super::super::App::MENTIONS_BUFFER_ID].messages.is_empty());
    receive(&mut app, "METADATA Alice soju.im/blocked * 0");
    receive(&mut app, "METADATA Alice soju.im/muted * 1");
    app.state.deliver_ready("first/#room", vec![ReadyEntry { id: row.id, message: row, activity: ActivityLevel::Mention, origin: ReadyOrigin::Translated, delivery: ReadyDelivery::Logged }]);
    assert!(!app.state.buffers["first/#room"].messages.back().unwrap().highlight);
    assert!(app.state.buffers[super::super::App::MENTIONS_BUFFER_ID].messages.is_empty());
}

#[tokio::test]
async fn subscription_replay_reconciles_missing_targets_only_after_successful_ack() {
    let mut app = app();
    receive(&mut app, "METADATA #Room soju.im/pinned * 1");
    receive(&mut app, "METADATA Alice soju.im/blocked * 1");
    for key in Key::ALL { receive(&mut app, &format!(":bouncer 770 * {}", key.wire())); }
    command(&mut app, &["sync".into()]);
    assert!(app.state.metadata_flags("first", "Alice").blocked);
    receive(&mut app, "METADATA #ROOM soju.im/pinned * 1");
    for key in Key::ALL { receive(&mut app, &format!(":bouncer 770 * {}", key.wire())); }
    assert!(app.state.buffers["first/#room"].metadata.pinned);
    assert!(!app.state.metadata_flags("first", "Alice").blocked);
    command(&mut app, &["sync".into()]);
    receive(&mut app, "FAIL METADATA INTERNAL_ERROR * :database unavailable");
    receive(&mut app, ":bouncer 770 * soju.im/pinned");
    assert!(app.state.buffers["first/#room"].metadata.pinned);
}

#[tokio::test]
async fn scope_and_casemapping_changes_discard_incompatible_flags() {
    let mut app = app();
    receive(&mut app, "METADATA #Room soju.im/pinned * 1");
    app.state.connections.get_mut("first").unwrap().network_scope = Some("account/replaced".into());
    app.tick_bouncer_metadata();
    assert!(!app.state.buffers["first/#room"].metadata.pinned);
    receive(&mut app, "METADATA [Alice] soju.im/blocked * 1");
    assert!(app.state.metadata_flags("first", "{Alice}").blocked);
    app.bouncer_metadata.remove("first");
    app.state.connections.get_mut("first").unwrap().isupport_parsed.parse_tokens(&["CASEMAPPING=ascii"]);
    assert!(!app.state.metadata_flags("first", "{Alice}").blocked);
    app.tick_bouncer_metadata();
    receive(&mut app, "METADATA [Alice] soju.im/blocked * 1");
    assert!(app.state.metadata_flags("first", "[Alice]").blocked);
    assert!(!app.state.metadata_flags("first", "{Alice}").blocked);
}

fn finish_operation(app: &mut super::super::App) {
    let messages = app.irc_handles["first"].sender().captured();
    let irc::proto::Command::PING(nonce, _) = &messages.last().unwrap().command else { panic!("missing completion probe"); };
    receive(app, &format!(":bouncer PONG bouncer :{nonce}"));
}

#[tokio::test]
async fn failed_metadata_storage_reads_back_without_retrying_the_mutation() {
    let mut app = app();
    for key in Key::ALL { receive(&mut app, &format!(":bouncer 770 * {}", key.wire())); }
    command(&mut app, &["#Room".into(), "mute".into(), "on".into()]);
    receive(&mut app, ":bouncer 761 * #Room soju.im/muted * 1");
    receive(&mut app, "METADATA #Room soju.im/muted * 1");
    assert!(app.state.buffers["first/#room"].metadata.muted);
    receive(&mut app, "FAIL METADATA INTERNAL_ERROR #Room :failed to store");
    finish_operation(&mut app);
    let sender = app.irc_handles["first"].sender();
    assert!(sender.captured().iter().any(|message| message.to_string().starts_with("METADATA #Room GET ")));
    assert_eq!(sender.captured().iter().filter(|message| message.to_string().contains(" SET ")).count(), 1);
    for key in Key::ALL { receive(&mut app, &format!(":bouncer 761 * #Room {} * 0", key.wire())); }
    finish_operation(&mut app);
    assert!(!app.state.buffers["first/#room"].metadata.muted);
    assert!(app.bouncer_metadata["first"].operation.is_none());
    assert!(app.state.buffers.values().flat_map(|buffer| &buffer.messages).any(|row| row.text.contains("read back after failure")));
}

#[tokio::test]
async fn empty_list_completes_and_unrelated_pong_cannot_release_an_operation() {
    let mut app = app();
    receive(&mut app, "METADATA #Room soju.im/pinned * 1");
    for key in Key::ALL { receive(&mut app, &format!(":bouncer 770 * {}", key.wire())); }
    command(&mut app, &["#Room".into(), "list".into()]);
    let count = app.irc_handles["first"].sender().captured().len();
    receive(&mut app, ":bouncer PONG bouncer :unrelated");
    command(&mut app, &["#Room".into(), "block".into(), "on".into()]);
    command(&mut app, &["sync".into()]);
    assert_eq!(app.irc_handles["first"].sender().captured().len(), count);
    receive(&mut app, ":bouncer BATCH +empty metadata");
    receive(&mut app, ":bouncer BATCH -empty");
    finish_operation(&mut app);
    assert!(!app.state.buffers["first/#room"].metadata.pinned);
    assert!(app.bouncer_metadata["first"].operation.is_none());
}

#[tokio::test]
async fn timed_out_metadata_operation_waits_for_its_own_completion() {
    let mut app = app();
    for key in Key::ALL { receive(&mut app, &format!(":bouncer 770 * {}", key.wire())); }
    command(&mut app, &["#Room".into(), "list".into()]);
    app.bouncer_metadata.get_mut("first").unwrap().operation.as_mut().unwrap().started = Instant::now().checked_sub(Duration::from_secs(31)).unwrap();
    app.tick_bouncer_metadata();
    let count = app.irc_handles["first"].sender().captured().len();
    command(&mut app, &["#Room".into(), "list".into()]);
    assert_eq!(app.irc_handles["first"].sender().captured().len(), count);
    assert!(app.state.buffers.values().flat_map(|buffer| &buffer.messages).any(|row| row.text.contains("result is unknown")));
    finish_operation(&mut app);
    command(&mut app, &["#Room".into(), "list".into()]);
    assert_eq!(app.irc_handles["first"].sender().captured().len(), count + 2);
}

#[tokio::test]
async fn blocking_preserves_unrelated_unread_mentions() {
    let mut app = app();
    app.create_mentions_buffer();
    for nick in ["Alice", "Bob"] {
        receive(&mut app, &format!(":{nick}!u@h PRIVMSG #Room :me: unread mention"));
    }
    let id = super::super::App::MENTIONS_BUFFER_ID;
    assert_eq!(app.state.buffers[id].unread_count, 2);
    receive(&mut app, "METADATA Alice soju.im/blocked * 1");
    assert_eq!(app.state.buffers[id].unread_count, 1);
    assert_eq!(app.state.buffers[id].messages.len(), 1);
    receive(&mut app, "METADATA Bob soju.im/blocked * 1");
    assert_eq!(app.state.buffers[id].unread_count, 0);
    assert_eq!(app.state.buffers[id].activity, crate::state::buffer::ActivityLevel::None);
}

#[tokio::test]
async fn cap_new_negotiates_metadata_only_for_bound_bouncer_connections() {
    let mut app = app();
    receive(&mut app, ":bouncer CAP me DEL :draft/metadata-2");
    let sender = app.irc_handles["first"].sender().clone();
    receive(&mut app, ":bouncer CAP me NEW :draft/metadata-2=before-connect");
    assert!(sender.captured().last().unwrap().to_string().contains("CAP REQ draft/metadata-2"));
    receive(&mut app, ":bouncer CAP me ACK :draft/metadata-2");
    assert!(sender.captured().last().unwrap().to_string().starts_with("METADATA * SUB "));
    app.state.connections.get_mut("first").unwrap().origin_config.bouncer_control = true;
    receive(&mut app, ":bouncer CAP me DEL :draft/metadata-2");
    let count = sender.captured().len();
    receive(&mut app, ":bouncer CAP me NEW :draft/metadata-2");
    assert_eq!(sender.captured().len(), count);
}

#[tokio::test]
async fn blocked_ctcp_does_not_poison_shared_flood_control() {
    let mut app = app();
    receive(&mut app, "METADATA Alice soju.im/blocked * 1");
    let sender = app.irc_handles["first"].sender().clone();
    let before = sender.captured().len();
    for _ in 0..10 {
        receive(&mut app, ":Alice!u@h PRIVMSG me :\u{1}VERSION\u{1}");
        receive(&mut app, ":Alice!u@h NOTICE me :\u{1}VERSION response\u{1}");
    }
    assert_eq!(sender.captured().len(), before);
    assert!(!app.state.flood_state.check_ctcp_flood(Instant::now()).suppressed());
    assert!(!app.state.buffers.values().flat_map(|buffer| &buffer.messages).any(|row| row.text.contains("CTCP")));
}

#[tokio::test]
async fn later_block_removes_existing_own_and_third_party_invitations() {
    let mut app = app();
    receive(&mut app, ":Alice!u@h INVITE me #Room");
    receive(&mut app, ":Alice!u@h INVITE Someone #Room");
    receive(&mut app, ":Bob!u@h INVITE me #Room");
    assert_eq!(app.state.buffers["first/#room"].messages.iter().filter(|row| row.event_key.as_deref() == Some("invite")).count(), 3);
    receive(&mut app, "METADATA Alice soju.im/blocked * 1");
    assert_eq!(app.state.buffers["first/#room"].messages.iter().filter(|row| row.event_key.as_deref() == Some("invite")).count(), 1);
    assert!(app.state.buffers["first/#room"].messages.iter().any(|row| row.text.contains("Bob invites")));
    receive(&mut app, "METADATA #Room soju.im/blocked * 1");
    assert!(!app.state.buffers["first/#room"].messages.iter().any(|row| row.event_key.as_deref() == Some("invite")));
}

#[tokio::test]
async fn query_rename_immediately_applies_new_targets_metadata() {
    let mut app = app();
    app.state.add_buffer_with_focus(Buffer::empty("first", BufferType::Query, "Alice"), false);
    receive(&mut app, "METADATA Alice soju.im/pinned * 1");
    receive(&mut app, "METADATA Alicia soju.im/muted * 1");
    let mut web = app.web_broadcaster.subscribe();
    receive(&mut app, ":Alice!u@h NICK Alicia");
    let flags = app.state.buffers["first/alicia"].metadata;
    assert!(!flags.pinned);
    assert!(flags.muted);
    assert!(std::iter::from_fn(|| web.try_recv().ok()).any(|event| matches!(event,
        crate::web::protocol::WebEvent::BufferMetadataChanged { buffer_id, pinned: false, muted: true, .. } if buffer_id == "first/alicia")));
    receive(&mut app, ":Alicia!u@h NICK Carol");
    assert_eq!(app.state.buffers["first/carol"].metadata, crate::irc::metadata::Flags::default());
}

#[tokio::test]
async fn losing_batch_keeps_metadata_pushes_but_rejects_operations() {
    let mut app = app();
    for key in Key::ALL { receive(&mut app, &format!(":bouncer 770 * {}", key.wire())); }
    receive(&mut app, ":bouncer CAP me DEL :batch");
    assert!(app.bouncer_metadata.contains_key("first"));
    receive(&mut app, "METADATA Alice soju.im/blocked * 1");
    receive(&mut app, ":Alice!u@h PRIVMSG #Room :must remain hidden");
    assert!(!app.state.buffers["first/#room"].messages.iter().any(|row| row.nick.as_deref() == Some("Alice")));
    let count = app.irc_handles["first"].sender().captured().len();
    command(&mut app, &["#Room".into(), "pin".into(), "on".into()]);
    assert_eq!(app.irc_handles["first"].sender().captured().len(), count);
    receive(&mut app, "METADATA Alice soju.im/blocked * 0");
    receive(&mut app, ":Alice!u@h PRIVMSG #Room :visible after remote unblock");
    assert!(app.state.buffers["first/#room"].messages.iter().any(|row| row.text == "visible after remote unblock"));
}

#[tokio::test]
async fn blocked_own_echo_releases_its_translation_reservation() {
    let mut app = app();
    let id = app.state.next_message_id();
    app.state.reserve_echo_slot("first/#room", id);
    app.state.decorate_own_echo("first/#room", crate::state::AppState::own_echo_decoration("translated wire".into(), id, None, true));
    receive(&mut app, "METADATA #Room soju.im/blocked * 1");
    receive(&mut app, ":me!u@h PRIVMSG #Room :translated wire");
    assert!(!app.state.translate_queues.contains_key("first/#room"));
    assert!(app.state.take_own_echo_decoration("first/#room", "translated wire").is_none());
    assert!(!app.state.buffers["first/#room"].messages.iter().any(|row| row.text == "translated wire"));
}

#[tokio::test]
async fn blocking_inviter_recomputes_remaining_server_activity() {
    use crate::state::buffer::ActivityLevel;
    let mut app = app();
    app.state.set_active_buffer("second/#room");
    receive(&mut app, "METADATA Bob soju.im/muted * 1");
    receive(&mut app, ":Alice!u@h INVITE me #Secret");
    receive(&mut app, ":Bob!u@h INVITE me #Other");
    let id = "first/fixture";
    assert_eq!(app.state.buffers[id].activity, ActivityLevel::Mention);
    assert_eq!(app.state.buffers[id].unread_count, 2);
    let mut web = app.web_broadcaster.subscribe();
    receive(&mut app, "METADATA Alice soju.im/blocked * 1");
    assert_eq!(app.state.buffers[id].activity, ActivityLevel::Activity);
    assert_eq!(app.state.buffers[id].unread_count, 1);
    assert!(std::iter::from_fn(|| web.try_recv().ok()).any(|event| matches!(event,
        crate::web::protocol::WebEvent::ActivityChanged { buffer_id, activity, unread_count }
        if buffer_id == id && activity == ActivityLevel::Activity as u8 && unread_count == 1)));
    app.state.set_active_buffer(id);
    assert_eq!(app.state.buffers[id].unread_count, 0);
}

#[tokio::test]
async fn local_log_search_hides_blocked_rows_without_deleting_archive() {
    let mut app = app();
    let storage = crate::storage::Storage::in_memory();
    {
        let db = storage.db.lock().unwrap();
        for (nick, text) in [("Alice", "needle blocked"), ("Bob", "needle allowed")] {
            db.execute("INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text) VALUES (?1, 'account/first', '#room', 1, 'message', ?1, ?2)", rusqlite::params![nick, text]).unwrap();
        }
    }
    app.storage = Some(storage);
    receive(&mut app, "METADATA Alice soju.im/blocked * 1");
    crate::commands::handlers_admin::cmd_log(&mut app, &["search".into(), "needle".into()]);
    let texts: Vec<_> = app.state.buffers["first/#room"].messages.iter().map(|row| row.text.as_str()).collect();
    assert!(!texts.iter().any(|text| text.contains("needle blocked")));
    assert!(texts.iter().any(|text| text.contains("needle allowed")));
    assert!(texts.iter().any(|text| text.contains("1 result(s)")));
    let db = app.storage.as_ref().unwrap().db.lock().unwrap();
    assert_eq!(crate::storage::query::search_messages(&db, "needle", None, None, 20).unwrap().len(), 2);
}

#[tokio::test]
async fn local_log_search_limits_visible_matches_after_block_filter() {
    let mut app = app();
    let storage = crate::storage::Storage::in_memory();
    {
        let db = storage.db.lock().unwrap();
        for index in 0..45 {
            let nick = if index < 22 { "Bob" } else { "Alice" };
            db.execute("INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text) VALUES (?1, 'account/first', '#room', ?1, 'message', ?2, 'needle')", rusqlite::params![index, nick]).unwrap();
        }
    }
    app.storage = Some(storage);
    receive(&mut app, "METADATA Alice soju.im/blocked * 1");
    crate::commands::handlers_admin::cmd_log(&mut app, &["search".into(), "needle".into()]);
    let rows = &app.state.buffers["first/#room"].messages;
    assert_eq!(rows.iter().filter(|row| row.text.contains("<Bob>")).count(), 20);
    assert!(!rows.iter().any(|row| row.text.contains("<Alice>")));
    assert!(rows.iter().any(|row| row.text.contains("20 result(s)")));
}

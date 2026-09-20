use super::*;
use crate::irc::{IrcEvent, IrcHandle, IrcSender};

fn app() -> (super::super::App, IrcSender) {
    let mut app = crate::app::input::submit_typing_tests::test_app();
    let config = toml::from_str("label='fixture'\naddress='localhost'\nport=6667\ntls=false\nchannels=[]\nnick='me'\nbouncer_network_id='1'").unwrap();
    for id in ["first", "second"] {
        app.setup_connection(id, &config);
        let conn = app.state.connections.get_mut(id).unwrap();
        conn.status = crate::state::connection::ConnectionStatus::Connected;
        conn.enabled_caps.extend([CAP, "batch", "server-time", "message-tags", "labeled-response", "draft/message-redaction"].map(str::to_string));
        app.irc_handles.insert(id.into(), IrcHandle::new(id.into(), IrcSender::capturing(0), None, None));
        app.state.add_buffer_with_focus(Buffer::empty(id, BufferType::Channel, "#room"), false);
    }
    app.state.set_active_buffer("first/#room");
    let sender = app.irc_handles["first"].sender().clone();
    (app, sender)
}

fn receive(app: &mut super::super::App, id: &str, wire: &str) {
    app.handle_irc_event(IrcEvent::Message(id.into(), Box::new(wire.parse().unwrap())));
}

fn search(app: &mut super::super::App) {
    command(app, &["#room".into(), "--".into(), "needle".into()]);
}

#[test]
fn selectors_escape_tags_normalize_time_and_enforce_limits() {
    let args = ["#room", "-after", "2024-01-01T01:00:00+01:00", "-limit", "2", "--", "a;b\\c", "100%"].map(str::to_string);
    let result = query(&args).unwrap();
    assert_eq!(result.wire.to_string(), "SEARCH in=#room;after=2024-01-01T00:00:00.000Z;limit=2;text=a\\:b\\\\c\\s100%\r\n");
    for args in [vec!["#room", "-limit", "0"], vec!["#room", "-limit", "101"], vec!["#room", "-from"], vec!["#room", "-after", "bad"], vec!["#room", "-from", "a", "-from", "b"], vec!["#room", "--", "a\nb"]] {
        assert!(query(&args.into_iter().map(str::to_string).collect::<Vec<_>>()).is_err());
    }
    assert!(query(&["#room".into(), "--".into(), "a".repeat(512)]).is_err());
}

#[tokio::test]
async fn results_are_scoped_ephemeral_ordered_and_redactable() {
    let (mut app, sender) = app();
    let (tx, mut logs) = tokio::sync::mpsc::channel(16);
    app.state.log_tx = Some(tx);
    app.web_broadcaster = std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(128));
    let mut web = app.web_broadcaster.subscribe();
    search(&mut app);
    assert_eq!(sender.captured().len(), 1);
    app.state.set_active_buffer("second/#room");
    receive(&mut app, "second", ":server BATCH +foreign soju.im/search");
    receive(&mut app, "second", ":server BATCH -foreign");
    assert!(app.server_search.contains_key("first"));
    receive(&mut app, "first", ":server BATCH +result soju.im/search");
    for (id, text) in [("first-id", "100% result"), ("second-id", "second result")] {
        receive(&mut app, "first", &format!("@batch=result;msgid={id};time=2024-01-01T00:00:00.123Z :Alice!u@h PRIVMSG #room :{text}"));
    }
    receive(&mut app, "first", ":server BATCH -result");
    assert!(!app.server_search.contains_key("first"));
    let rows = &app.state.buffers["first/*search*"].messages;
    let results: Vec<_> = rows.iter().filter(|row| row.nick.is_some()).collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].text, "100% result");
    assert_eq!(results[1].tags.as_ref().unwrap()["msgid"], "second-id");
    assert_eq!(results[0].timestamp.timestamp_millis(), 1_704_067_200_123);
    assert!(app.state.buffers["first/#room"].messages.is_empty());
    assert_eq!(app.state.buffers["first/#room"].unread_count, 0);
    assert_eq!(app.state.active_buffer_id.as_deref(), Some("second/#room"));
    receive(&mut app, "first", ":Alice!u@h REDACT #room first-id :removed");
    assert!(!app.state.buffers["first/*search*"].messages.iter().any(|row| row.text == "100% result"));
    assert!(std::iter::from_fn(|| web.try_recv().ok()).any(|event| matches!(event, crate::web::protocol::WebEvent::RedactMessage { buffer_id, .. } if buffer_id == "first/*search*")));
    assert!(logs.try_recv().is_err());
}

#[tokio::test]
async fn timeout_cancellation_and_errors_do_not_mix_requests() {
    let (mut app, sender) = app();
    search(&mut app);
    app.server_search.get_mut("first").unwrap().started = Instant::now().checked_sub(Duration::from_secs(31)).unwrap();
    app.tick_server_search();
    search(&mut app);
    assert_eq!(sender.captured().len(), 1);
    receive(&mut app, "first", ":server BATCH +late soju.im/search");
    receive(&mut app, "first", "@batch=late;time=2024-01-01T00:00:00Z :Alice!u@h PRIVMSG #room :late result");
    receive(&mut app, "first", ":server BATCH -late");
    assert!(!app.server_search.contains_key("first"));
    assert!(!app.state.buffers["first/*search*"].messages.iter().any(|row| row.nick.is_some()));
    search(&mut app);
    command(&mut app, &["cancel".into()]);
    receive(&mut app, "first", "FAIL SEARCH INTERNAL_ERROR :Failure");
    assert!(!app.server_search.contains_key("first"));
    search(&mut app);
    receive(&mut app, "first", ":server BATCH +empty soju.im/search");
    receive(&mut app, "first", ":server BATCH -empty");
    assert!(app.state.buffers["first/*search*"].messages.back().unwrap().text.contains("0 search results"));
    search(&mut app);
    receive(&mut app, "first", "FAIL SEARCH INVALID_PARAMS in :Invalid target");
    assert!(app.state.buffers["first/*search*"].messages.back().unwrap().text.contains("Search failed"));
}

#[tokio::test]
async fn invalid_results_closed_view_and_lost_capability_are_not_replayed() {
    let (mut app, sender) = app();
    search(&mut app);
    receive(&mut app, "first", ":server BATCH +bad soju.im/search");
    receive(&mut app, "first", "@batch=bad;time=2024-01-01T00:00:00Z :Alice!u@h PRIVMSG #other :wrong target");
    receive(&mut app, "first", ":server BATCH -bad");
    assert!(app.state.buffers["first/*search*"].messages.back().unwrap().text.contains("invalid results"));
    search(&mut app);
    app.state.remove_buffer("first/*search*");
    receive(&mut app, "first", ":server BATCH +closed soju.im/search");
    receive(&mut app, "first", ":server BATCH -closed");
    assert!(!app.state.buffers.contains_key("first/*search*"));
    app.state.set_active_buffer("first/#room");
    app.state.connections.get_mut("first").unwrap().enabled_caps.remove(CAP);
    let sent = sender.captured().len();
    search(&mut app);
    assert_eq!(sender.captured().len(), sent);
}

#[tokio::test]
async fn action_context_and_close_keep_results_separate_from_the_network() {
    let (mut app, sender) = app();
    app.state.connections.get_mut("first").unwrap().enabled_caps.insert("draft/chathistory".into());
    search(&mut app);
    receive(&mut app, "first", ":server BATCH +result soju.im/search");
    receive(&mut app, "first", "@batch=result;msgid=action;time=2024-01-01T00:00:00.123Z :Alice!u@h PRIVMSG #room :\x01ACTION waves\x01");
    receive(&mut app, "first", ":server BATCH -result");
    assert_eq!(app.state.buffers["first/*search*"].messages.back().unwrap().message_type, MessageType::Action);
    assert_eq!(app.state.buffers["first/*search*"].messages.back().unwrap().text, "waves");
    command(&mut app, &["context".into(), "1".into()]);
    assert!(sender.captured().last().unwrap().to_string().contains("CHATHISTORY AROUND #room timestamp=2024-01-01T00:00:00.123Z 50"));
    let label = app.server_search["first"].label.clone().unwrap();
    receive(&mut app, "first", &format!("@label={label} :server BATCH +context chathistory #room"));
    receive(&mut app, "first", "@batch=context;msgid=surrounding;time=2024-01-01T00:00:00Z :Alice!u@h PRIVMSG #room :nearby");
    receive(&mut app, "first", ":server BATCH -context");
    assert!(!app.server_search.contains_key("first"));
    assert!(app.state.buffers["first/#room"].messages.is_empty());
    assert_eq!(app.state.buffers["first/*search*"].messages.back().unwrap().text, "nearby");
    crate::commands::handlers_ui::cmd_close(&mut app, &[]);
    assert!(!app.state.buffers.contains_key("first/*search*"));
    assert!(app.state.connections.contains_key("first"));
    assert!(app.state.buffers.contains_key("first/#room"));
}

#[tokio::test]
async fn context_preserves_msgid_and_rejects_results_from_another_scope() {
    let (mut app, sender) = app();
    let conn = app.state.connections.get_mut("first").unwrap();
    conn.enabled_caps.insert("draft/chathistory".into());
    conn.isupport_parsed.parse_tokens(&["MSGREFTYPES=msgid,timestamp"]);
    search(&mut app);
    receive(&mut app, "first", ":server BATCH +result soju.im/search");
    for id in ["first-tie", "second-tie"] {
        receive(&mut app, "first", &format!("@batch=result;msgid={id};time=2024-01-01T00:00:00Z :Alice!u@h PRIVMSG #room :same timestamp"));
    }
    receive(&mut app, "first", ":server BATCH -result");
    receive(&mut app, "first", ":Alice!u@h REDACT #room first-tie :removed");
    let scope = app.state.connections["first"].network_scope.clone();
    app.state.connections.get_mut("first").unwrap().network_scope = Some("another-account-and-network".into());
    command(&mut app, &["context".into(), "2".into()]);
    assert_eq!(sender.captured().len(), 1);
    app.state.connections.get_mut("first").unwrap().network_scope = scope;
    command(&mut app, &["context".into(), "2".into()]);
    assert!(sender.captured().last().unwrap().to_string().contains("AROUND #room msgid=second-tie 50"));
}

#[tokio::test]
async fn server_named_like_search_keeps_its_status_and_connection() {
    let (mut app, _) = app();
    app.state.add_buffer_with_focus(Buffer::empty("first", BufferType::Server, VIEW), false);
    app.add_event_to_buffer("first/*search*", "server status".into());
    search(&mut app);
    let view = app.search_view("first").unwrap();
    assert_ne!(view, "first/*search*");
    assert_eq!(app.state.buffers["first/*search*"].messages.back().unwrap().text, "server status");
    receive(&mut app, "first", ":server BATCH +empty soju.im/search");
    receive(&mut app, "first", ":server BATCH -empty");
    assert!(app.state.buffers[&view].messages.back().unwrap().text.contains("0 search results"));
    crate::commands::handlers_ui::cmd_close(&mut app, &[]);
    assert!(!app.state.buffers.contains_key(&view));
    assert!(app.state.buffers.contains_key("first/*search*"));
    assert!(app.state.connections.contains_key("first"));
}

#[tokio::test]
async fn encrypted_unreadable_rows_are_skipped_and_foreign_errors_remain_visible() {
    let (mut app, _) = app();
    assert!(!app.handle_server_search("first", &"FAIL SEARCH INTERNAL_ERROR :raw command failure".parse().unwrap()));
    assert!(!app.handle_server_search("first", &":server 421 me SEARCH :Unknown command".parse().unwrap()));
    search(&mut app);
    receive(&mut app, "first", ":server BATCH +result soju.im/search");
    for (nick, text) in [("Alice", "+RPE2E01undecryptable"), ("me", "+RPE2E01own-echo"), ("Alice", "readable")] {
        receive(&mut app, "first", &format!("@batch=result;time=2024-01-01T00:00:00Z :{nick}!u@h PRIVMSG #room :{text}"));
    }
    receive(&mut app, "first", ":server BATCH -result");
    let view = &app.state.buffers["first/*search*"].messages;
    assert_eq!(view.iter().filter(|row| row.nick.is_some()).count(), 1);
    assert_eq!(view.back().unwrap().text, "readable");
    assert!(!view.iter().any(|row| row.text.contains("+RPE2E01")));
}

#[tokio::test]
async fn direct_context_dispatch_applies_playback_redactions_before_display() {
    let (mut app, _) = app();
    app.state.connections.get_mut("first").unwrap().enabled_caps.insert("draft/chathistory".into());
    search(&mut app);
    receive(&mut app, "first", ":server BATCH +result soju.im/search");
    receive(&mut app, "first", "@batch=result;msgid=old;time=2024-01-01T00:00:00Z :Alice!u@h PRIVMSG #room :old text");
    receive(&mut app, "first", ":server BATCH -result");
    command(&mut app, &["context".into(), "1".into()]);
    let mut tracker = crate::irc::batch::BatchTracker::default();
    let label = app.server_search["first"].label.clone().unwrap();
    tracker.start_batch("context", "chathistory", vec!["#room".into()], Some(vec![irc::proto::message::Tag("label".into(), Some(label))]));
    tracker.add_message("@batch=context;msgid=old;time=2024-01-01T00:00:00Z :Alice!u@h PRIVMSG #room :deleted private content".parse().unwrap());
    tracker.add_message("@batch=context :Alice!u@h REDACT #room old :removed".parse().unwrap());
    app.dispatch_completed_batch("first", &tracker.end_batch("context").unwrap(), true);
    assert!(!app.state.buffers["first/*search*"].messages.iter().any(|row| row.text.contains("deleted private content")));
    assert!(app.state.buffers["first/*search*"].messages.iter().any(|row| row.text.contains("Message deleted")));
}

#[tokio::test]
async fn private_search_decrypts_using_recipient_handle_after_nick_change() {
    use crate::e2e::keyring::{ChannelConfig, ChannelMode, Keyring};
    use crate::e2e::manager::E2eManager;
    use std::sync::{Arc, Mutex};
    let (mut app, _) = app();
    let own = Arc::new(E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(crate::storage::db::open_database(false).unwrap())))).unwrap());
    let peer = E2eManager::load_or_init(Keyring::new(Arc::new(Mutex::new(crate::storage::db::open_database(false).unwrap())))).unwrap();
    app.state.e2e_manager = Some(Arc::clone(&own));
    app.state.connections.get_mut("first").unwrap().own_handle = Some("~me@host".into());
    let network = app.state.connections["first"].network_key().to_string();
    let context = "@~me@host";
    let request = own.build_keyreq(&crate::e2e::scoped_context(&network, context)).unwrap();
    peer.keyring().set_channel_config(&ChannelConfig { channel: context.into(), enabled: true, mode: ChannelMode::AutoAccept }).unwrap();
    let mut response = peer.handle_keyreq_with_nick("~me@host", Some("me"), &request).unwrap().unwrap();
    response.channel = crate::e2e::scoped_context(&network, &response.channel);
    own.handle_keyrsp("~alice@a.host", &response).unwrap();
    let wire = peer.encrypt_outgoing(context, "\x01ACTION waves privately\x01").unwrap().remove(0);
    command(&mut app, &["Alice".into()]);
    receive(&mut app, "first", ":server BATCH +result soju.im/search");
    receive(&mut app, "first", &format!("@batch=result;time=2024-01-01T00:00:00Z :Alice!~alice@a.host PRIVMSG EarlierNick :{wire}"));
    receive(&mut app, "first", ":server BATCH -result");
    let row = app.state.buffers["first/*search*"].messages.back().unwrap();
    assert_eq!(row.message_type, MessageType::Action);
    assert_eq!(row.text, "waves privately");
}

#[tokio::test]
async fn context_label_keeps_delayed_history_and_errors_out_of_search() {
    let (mut app, sender) = app();
    app.state.connections.get_mut("first").unwrap().enabled_caps.insert("draft/chathistory".into());
    search(&mut app);
    receive(&mut app, "first", ":server BATCH +result soju.im/search");
    receive(&mut app, "first", "@batch=result;msgid=anchor;time=2024-01-01T00:00:00Z :Alice!u@h PRIVMSG #room :anchor");
    receive(&mut app, "first", ":server BATCH -result");
    assert!(app.request_chathistory_with_limit("first", "#room", crate::irc::chathistory::Direction::Latest, None, 50));
    app.state.connections.get_mut("first").unwrap().chathistory.clear_stale(Duration::ZERO);
    command(&mut app, &["context".into(), "1".into()]);
    let label = crate::irc::labels::message_label(sender.captured().last().unwrap()).unwrap().to_string();
    receive(&mut app, "first", "FAIL CHATHISTORY MESSAGE_ERROR #room :older request failed");
    assert!(app.server_search.contains_key("first"));
    receive(&mut app, "first", ":server BATCH +old chathistory #room");
    receive(&mut app, "first", "@batch=old;msgid=old;time=2024-01-01T00:00:01Z :Alice!u@h PRIVMSG #room :earlier history");
    receive(&mut app, "first", ":server BATCH -old");
    assert!(app.server_search.contains_key("first"));
    assert!(!app.state.buffers["first/*search*"].messages.iter().any(|row| row.text == "earlier history"));
    let live_count = app.state.buffers["first/#room"].messages.len();
    receive(&mut app, "first", &format!("@label={label} :server BATCH +wrapper labeled-response"));
    receive(&mut app, "first", "@batch=wrapper :server BATCH +context chathistory #room");
    receive(&mut app, "first", "@batch=context;msgid=context;time=2024-01-01T00:00:02Z :Alice!u@h PRIVMSG #room :actual context");
    receive(&mut app, "first", ":server BATCH -context");
    receive(&mut app, "first", ":server BATCH -wrapper");
    assert!(!app.server_search.contains_key("first"));
    assert_eq!(app.state.buffers["first/*search*"].messages.back().unwrap().text, "actual context");
    assert_eq!(app.state.buffers["first/#room"].messages.len(), live_count);
    receive(&mut app, "first", &format!("@label={label} :server BATCH +duplicate chathistory #room"));
    receive(&mut app, "first", "@batch=duplicate;msgid=duplicate;time=2024-01-01T00:00:03Z :Alice!u@h PRIVMSG #room :late duplicate context");
    receive(&mut app, "first", ":server BATCH -duplicate");
    assert_eq!(app.state.buffers["first/#room"].messages.len(), live_count);
}

#[tokio::test]
async fn labeled_context_failure_without_target_releases_the_request() {
    let (mut app, _) = app();
    app.state.connections.get_mut("first").unwrap().enabled_caps.insert("draft/chathistory".into());
    search(&mut app);
    receive(&mut app, "first", ":server BATCH +result soju.im/search");
    receive(&mut app, "first", "@batch=result;msgid=anchor;time=2024-01-01T00:00:00Z :Alice!u@h PRIVMSG #room :anchor");
    receive(&mut app, "first", ":server BATCH -result");
    command(&mut app, &["context".into(), "1".into()]);
    let label = app.server_search["first"].label.clone().unwrap();
    receive(&mut app, "first", &format!("@label={label} FAIL CHATHISTORY TEMPORARILY_UNAVAILABLE :Try later"));
    assert!(!app.server_search.contains_key("first"));
    assert!(app.state.buffers["first/*search*"].messages.back().unwrap().text.contains("Search failed: Try later"));
}

#[tokio::test]
async fn unlabelled_context_requires_unambiguous_history_since_connect() {
    let (mut app, sender) = app();
    let conn = app.state.connections.get_mut("first").unwrap();
    conn.enabled_caps.insert("draft/chathistory".into());
    conn.enabled_caps.remove("labeled-response");
    search(&mut app);
    receive(&mut app, "first", ":server BATCH +result soju.im/search");
    receive(&mut app, "first", "@batch=result;msgid=anchor;time=2024-01-01T00:00:00Z :Alice!u@h PRIVMSG #room :anchor");
    receive(&mut app, "first", ":server BATCH -result");
    command(&mut app, &["context".into(), "1".into()]);
    assert!(app.server_search.contains_key("first"));
    assert!(crate::irc::labels::message_label(sender.captured().last().unwrap()).is_none());
    receive(&mut app, "first", ":server BATCH +context chathistory #room");
    receive(&mut app, "first", "@batch=context;msgid=nearby;time=2024-01-01T00:00:01Z :Alice!u@h PRIVMSG #room :nearby");
    receive(&mut app, "first", ":server BATCH -context");
    assert!(!app.server_search.contains_key("first"));
    assert!(app.state.buffers["first/#room"].messages.is_empty());
    assert!(app.request_chathistory_with_limit("first", "#room", crate::irc::chathistory::Direction::Latest, None, 50));
    app.state.connections.get_mut("first").unwrap().chathistory.clear_stale(Duration::ZERO);
    let count = sender.captured().len();
    command(&mut app, &["context".into(), "1".into()]);
    assert_eq!(sender.captured().len(), count);
    assert!(!app.server_search.contains_key("first"));
    app.state.connections.get_mut("first").unwrap().chathistory.set_gapfill_cutoff(1);
    command(&mut app, &["context".into(), "1".into()]);
    assert!(app.server_search.contains_key("first"));
    receive(&mut app, "first", ":server 421 me CHATHISTORY :History disabled");
    assert!(!app.server_search.contains_key("first"));
    assert!(app.state.buffers["first/*search*"].messages.back().unwrap().text.contains("Search failed: History disabled"));
}

#[tokio::test]
async fn context_rechecks_batch_time_and_tag_capabilities_after_results() {
    for cap in ["draft/chathistory", "batch", "server-time", "message-tags"] {
        let (mut app, sender) = app();
        app.state.connections.get_mut("first").unwrap().enabled_caps.insert("draft/chathistory".into());
        search(&mut app);
        receive(&mut app, "first", ":server BATCH +result soju.im/search");
        receive(&mut app, "first", "@batch=result;msgid=anchor;time=2024-01-01T00:00:00Z :Alice!u@h PRIVMSG #room :anchor");
        receive(&mut app, "first", ":server BATCH -result");
        receive(&mut app, "first", &format!(":server CAP me DEL :{cap}"));
        let sent = sender.captured().len();
        command(&mut app, &["context".into(), "1".into()]);
        assert_eq!(sender.captured().len(), sent, "missing {cap}");
        assert!(!app.server_search.contains_key("first"));
        app.state.connections.get_mut("first").unwrap().enabled_caps.insert(cap.into());
        command(&mut app, &["context".into(), "1".into()]);
        assert!(app.server_search.contains_key("first"));
        receive(&mut app, "first", &format!(":server CAP me DEL :{cap}"));
        app.tick_server_search();
        assert!(app.server_search["first"].discard);
    }
}

#[tokio::test]
async fn nested_labeled_context_errors_finish_without_live_chat_noise() {
    for error in ["FAIL CHATHISTORY TEMPORARILY_UNAVAILABLE :Try later", ":server 421 me CHATHISTORY :History disabled"] {
        let (mut app, _) = app();
        app.state.connections.get_mut("first").unwrap().enabled_caps.insert("draft/chathistory".into());
        search(&mut app);
        receive(&mut app, "first", ":server BATCH +result soju.im/search");
        receive(&mut app, "first", "@batch=result;msgid=anchor;time=2024-01-01T00:00:00Z :Alice!u@h PRIVMSG #room :anchor");
        receive(&mut app, "first", ":server BATCH -result");
        command(&mut app, &["context".into(), "1".into()]);
        let label = app.server_search["first"].label.clone().unwrap();
        receive(&mut app, "first", &format!("@label={label} :server BATCH +outer labeled-response"));
        receive(&mut app, "first", "@batch=outer :server BATCH +inner vendor/wrapper");
        receive(&mut app, "first", &format!("@batch=inner {error}"));
        receive(&mut app, "first", "@batch=outer :server BATCH -inner");
        receive(&mut app, "first", ":server BATCH -outer");
        assert!(!app.server_search.contains_key("first"));
        assert!(app.state.buffers["first/*search*"].messages.back().unwrap().text.contains("Search failed:"));
        assert!(app.state.buffers["first/#room"].messages.is_empty());
        assert!(!app.state.connections["first"].chathistory.any_in_flight("#room"));
    }
}

#[tokio::test]
async fn capability_withdrawal_discards_completed_results_before_the_next_tick() {
    for context in [false, true] {
        let (mut app, _) = app();
        app.state.connections.get_mut("first").unwrap().enabled_caps.insert("draft/chathistory".into());
        search(&mut app);
        if context {
            receive(&mut app, "first", ":server BATCH +result soju.im/search");
            receive(&mut app, "first", "@batch=result;msgid=anchor;time=2024-01-01T00:00:00Z :Alice!u@h PRIVMSG #room :anchor");
            receive(&mut app, "first", ":server BATCH -result");
            command(&mut app, &["context".into(), "1".into()]);
            let label = app.server_search["first"].label.clone().unwrap();
            receive(&mut app, "first", &format!("@label={label} :server BATCH +late chathistory #room"));
        } else {
            receive(&mut app, "first", ":server BATCH +late soju.im/search");
        }
        receive(&mut app, "first", "@batch=late;msgid=late;time=2024-01-01T00:00:01Z :Alice!u@h PRIVMSG #room :withdrawn result");
        let cap = if context { "draft/chathistory" } else { CAP };
        receive(&mut app, "first", &format!(":server CAP me DEL :{cap}"));
        receive(&mut app, "first", ":server BATCH -late");
        assert!(!app.server_search.contains_key("first"));
        assert!(!app.state.buffers["first/*search*"].messages.iter().any(|row| row.text == "withdrawn result"));
        assert!(app.state.buffers["first/#room"].messages.is_empty());
    }
}

#[tokio::test]
async fn metadata_filtering_counts_only_visible_search_results() {
    let (mut app, _) = app();
    app.state.set_metadata("first", "Alice", crate::irc::metadata::Key::Blocked, true);
    search(&mut app);
    receive(&mut app, "first", ":server BATCH +result soju.im/search");
    for nick in ["Alice", "Bob"] {
        receive(&mut app, "first", &format!("@batch=result;msgid={nick};time=2024-01-01T00:00:00Z :{nick}!u@h PRIVMSG #room :needle"));
    }
    receive(&mut app, "first", ":server BATCH -result");
    let rows = &app.state.buffers["first/*search*"].messages;
    assert_eq!(rows.iter().filter(|row| row.nick.is_some()).count(), 1);
    assert!(rows.iter().any(|row| row.text.contains("1 search results")));
    assert!(!rows.iter().any(|row| row.text.contains("2 search results")));
    app.state.set_metadata("first", "Bob", crate::irc::metadata::Key::Blocked, true);
    search(&mut app);
    receive(&mut app, "first", ":server BATCH +empty soju.im/search");
    receive(&mut app, "first", "@batch=empty;time=2024-01-01T00:00:00Z :Bob!u@h PRIVMSG #room :needle");
    receive(&mut app, "first", ":server BATCH -empty");
    assert!(app.state.buffers["first/*search*"].messages.iter().any(|row| row.text.contains("0 search results")));
}

#[tokio::test]
async fn private_channel_context_search_results_match_only_the_requested_channel() {
    for context in ["#room", "#other"] {
        let (mut app, _) = app();
        search(&mut app);
        receive(&mut app, "first", ":server BATCH +result soju.im/search");
        receive(&mut app, "first", &format!("@batch=result;+draft/channel-context={context};time=2024-01-01T00:00:00Z :Alice!u@h NOTICE me :context result"));
        receive(&mut app, "first", ":server BATCH -result");
        let rows = &app.state.buffers["first/*search*"].messages;
        assert_eq!(rows.iter().any(|row| row.text == "context result"), context == "#room");
        assert!(app.state.buffers["first/#room"].messages.is_empty());
    }
}

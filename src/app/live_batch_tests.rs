use super::App;
use crate::irc::{IrcEvent, IrcHandle, IrcSender};
use irc::proto::Command;

fn app() -> App {
    let mut app = super::input::submit_typing_tests::test_app();
    let config = toml::from_str(
        "label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nnick='me'",
    )
    .unwrap();
    app.setup_connection("fixture", &config);
    let conn = app.state.connections.get_mut("fixture").unwrap();
    conn.nick = "me".into();
    conn.status = crate::state::connection::ConnectionStatus::Connected;
    conn.enabled_caps
        .extend(["batch".into(), "no-implicit-names".into()]);
    app.irc_handles.insert(
        "fixture".into(),
        IrcHandle::new("fixture".into(), IrcSender::capturing(0), None, None),
    );
    app
}

fn receive(app: &mut App, wire: &str) {
    app.handle_irc_event(IrcEvent::Message(
        "fixture".into(),
        Box::new(wire.parse().unwrap()),
    ));
}

#[tokio::test]
async fn live_batch_join_and_names_run_app_hooks() {
    let mut app = app();
    receive(&mut app, ":server BATCH +live labeled-response");
    receive(&mut app, "@batch=live :me!u@h JOIN #test");
    assert!(!app.state.buffers.contains_key("fixture/#test"));
    receive(&mut app, ":server BATCH -live");
    assert!(
        app.irc_handles["fixture"]
            .sender()
            .captured()
            .iter()
            .any(|m| matches!(&m.command, Command::NAMES(Some(channel), _) if channel == "#test"))
    );
    receive(&mut app, ":server BATCH +names labeled-response");
    receive(
        &mut app,
        "@batch=names :server 353 me = #test :me @Alice Bob",
    );
    receive(&mut app, "@batch=names :server 366 me #test :End");
    receive(&mut app, ":server BATCH -names");
    assert!(app.channel_query_in_flight.contains_key("fixture"));
    let crate::web::protocol::WebEvent::NickList { nicks, .. } =
        crate::web::snapshot::build_nick_list(&app.state, "fixture/#test").unwrap()
    else {
        panic!("expected nicklist")
    };
    assert!(
        nicks
            .iter()
            .any(|nick| nick.nick == "Alice" && nick.prefix == "@")
    );
}

#[tokio::test]
async fn history_batch_does_not_run_live_join_hooks() {
    let mut app = app();
    receive(&mut app, ":server BATCH +history chathistory #old");
    receive(&mut app, "@batch=history :me!u@h JOIN #old");
    receive(&mut app, ":server BATCH -history");
    assert!(
        !app.irc_handles["fixture"]
            .sender()
            .captured()
            .iter()
            .any(|m| matches!(&m.command, Command::NAMES(_, _)))
    );
    assert!(app.channel_query_in_flight.is_empty());
}

#[tokio::test]
async fn live_generic_and_multiline_messages_broadcast_once_without_bouncer_logs() {
    for batch_type in ["vendor/live", "draft/multiline"] {
        let mut app = app();
        app.state
            .connections
            .get_mut("fixture")
            .unwrap()
            .origin_config
            .bouncer_network_id = Some("1".into());
        receive(&mut app, ":me!u@h JOIN #test");
        let (tx, mut log_rx) = tokio::sync::mpsc::channel(16);
        app.state.log_tx = Some(tx);
        app.web_broadcaster = std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(128));
        let mut web_rx = app.web_broadcaster.subscribe();
        receive(&mut app, &format!(":server BATCH +live {batch_type} #test"));
        receive(
            &mut app,
            "@batch=live;msgid=batch-message :Alice!a@h PRIVMSG #test :hello from batch",
        );
        assert!(web_rx.try_recv().is_err());
        receive(&mut app, ":server BATCH -live");
        let events: Vec<_> = std::iter::from_fn(|| web_rx.try_recv().ok()).collect();
        assert_eq!(events.iter().filter(|event| matches!(event, crate::web::protocol::WebEvent::NewMessage { buffer_id, message } if buffer_id == "fixture/#test" && message.text.contains("hello from batch"))).count(), 1);
        assert_eq!(
            app.state.buffers["fixture/#test"]
                .messages
                .iter()
                .filter(|message| message.text.contains("hello from batch"))
                .count(),
            1
        );
        assert!(log_rx.try_recv().is_err());
    }
}

#[tokio::test]
async fn expired_live_batch_uses_the_same_names_dispatch() {
    let mut app = app();
    receive(&mut app, ":server BATCH +live vendor/live");
    receive(&mut app, "@batch=live :me!u@h JOIN #test");
    let batch = app
        .batch_trackers
        .get_mut("fixture")
        .unwrap()
        .end_batch("live")
        .unwrap();
    app.dispatch_completed_batch("fixture", &batch, false);
    assert!(
        app.irc_handles["fixture"]
            .sender()
            .captured()
            .iter()
            .any(|m| matches!(&m.command, Command::NAMES(Some(channel), _) if channel == "#test"))
    );
}

#[tokio::test]
async fn nested_multiline_history_never_broadcasts_a_live_message() {
    let mut app = app();
    app.state
        .connections
        .get_mut("fixture")
        .unwrap()
        .origin_config
        .bouncer_network_id = Some("1".into());
    receive(&mut app, ":me!u@h JOIN #test");
    app.web_broadcaster = std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(128));
    let mut web_rx = app.web_broadcaster.subscribe();
    receive(&mut app, ":server BATCH +history chathistory #test");
    receive(
        &mut app,
        "@batch=history :server BATCH +multi draft/multiline #test",
    );
    receive(
        &mut app,
        "@batch=multi;msgid=historical :Alice!a@h PRIVMSG #test :old message",
    );
    receive(&mut app, ":server BATCH -multi");
    assert!(
        !app.state.buffers["fixture/#test"]
            .messages
            .iter()
            .any(|message| message.text.contains("old message"))
    );
    receive(&mut app, ":server BATCH -history");
    let events: Vec<_> = std::iter::from_fn(|| web_rx.try_recv().ok()).collect();
    assert!(!events.iter().any(|event| matches!(event, crate::web::protocol::WebEvent::NewMessage { message, .. } if message.text.contains("old message"))));
    assert!(
        app.state.buffers["fixture/#test"]
            .messages
            .iter()
            .any(|message| message.text.contains("old message"))
    );
}

#[tokio::test]
async fn nested_generic_history_never_runs_live_join_hooks() {
    let mut app = app();
    receive(&mut app, ":server BATCH +history chathistory #old");
    receive(
        &mut app,
        "@batch=history :server BATCH +wrapper vendor/wrapper",
    );
    receive(&mut app, "@batch=wrapper :me!u@h JOIN #old");
    receive(&mut app, ":server BATCH -wrapper");
    receive(&mut app, ":server BATCH -history");
    assert!(
        !app.irc_handles["fixture"]
            .sender()
            .captured()
            .iter()
            .any(|m| matches!(&m.command, Command::NAMES(_, _)))
    );
}

#[tokio::test]
async fn expired_nested_history_folds_children_before_parents_in_any_order() {
    for order in [
        ["history", "outer", "inner"],
        ["inner", "history", "outer"],
        ["outer", "inner", "history"],
    ] {
        let mut app = app();
        receive(&mut app, ":server BATCH +history chathistory #old");
        receive(&mut app, "@batch=history :server BATCH +outer vendor/outer");
        receive(&mut app, "@batch=outer :server BATCH +inner vendor/inner");
        receive(&mut app, "@batch=inner :me!u@h JOIN #old");
        let batches = order
            .iter()
            .map(|reference| {
                (
                    "fixture".into(),
                    (*reference).into(),
                    app.batch_trackers
                        .get_mut("fixture")
                        .unwrap()
                        .end_batch(reference)
                        .unwrap(),
                )
            })
            .collect();
        app.dispatch_expired_batch_set(batches);
        assert!(
            !app.irc_handles["fixture"]
                .sender()
                .captured()
                .iter()
                .any(|m| matches!(&m.command, Command::NAMES(_, _)))
        );
    }
}

#[tokio::test]
async fn live_nested_batch_waits_for_parent_but_orphan_is_not_replayed() {
    let mut app = app();
    receive(&mut app, ":server BATCH +parent vendor/parent");
    receive(&mut app, "@batch=parent :server BATCH +child vendor/child");
    receive(&mut app, "@batch=child :me!u@h JOIN #test");
    receive(&mut app, ":server BATCH -child");
    assert!(!app.state.buffers.contains_key("fixture/#test"));
    receive(&mut app, ":server BATCH -parent");
    assert!(app.state.buffers.contains_key("fixture/#test"));
    receive(&mut app, "@batch=gone :server BATCH +orphan vendor/child");
    receive(&mut app, "@batch=orphan :me!u@h JOIN #orphan");
    receive(&mut app, ":server BATCH -orphan");
    assert!(!app.state.buffers.contains_key("fixture/#orphan"));
}

#[tokio::test]
async fn nested_fold_keeps_the_message_limit_and_dropped_count() {
    let mut app = app();
    receive(&mut app, ":server BATCH +parent chathistory #old");
    let tracker = app.batch_trackers.get_mut("fixture").unwrap();
    assert!(tracker.fold_messages(
        "parent",
        vec![(0, ":me!u@h JOIN #old".parse().unwrap()); 5000],
        3
    ));
    let batch = tracker.end_batch("parent").unwrap();
    assert_eq!(batch.messages.len(), 4096);
    assert_eq!(batch.dropped_messages, 907);
}

#[tokio::test]
async fn nested_payloads_keep_wire_order_on_close_and_expiration() {
    for expired in [false, true] {
        let mut app = app();
        receive(&mut app, ":me!u@h JOIN #test");
        receive(&mut app, ":server BATCH +parent vendor/parent");
        receive(&mut app, "@batch=parent :Alice!u@h JOIN #test");
        receive(&mut app, "@batch=parent :server BATCH +child vendor/child");
        receive(&mut app, "@batch=child :Alice!u@h NICK Bob");
        receive(&mut app, "@batch=parent :Bob!u@h PART #test :gone");
        if expired {
            let batches = ["parent", "child"]
                .iter()
                .map(|reference| {
                    (
                        "fixture".into(),
                        (*reference).into(),
                        app.batch_trackers
                            .get_mut("fixture")
                            .unwrap()
                            .end_batch(reference)
                            .unwrap(),
                    )
                })
                .collect();
            app.dispatch_expired_batch_set(batches);
        } else {
            receive(&mut app, "@batch=parent :server BATCH -child");
            receive(&mut app, ":server BATCH -parent");
        }
        assert!(
            !app.state.buffers["fixture/#test"]
                .users
                .values()
                .any(|user| user.nick == "Alice" || user.nick == "Bob")
        );
    }
}

#[tokio::test]
async fn search_batches_never_enter_live_history_or_activity() {
    for bound in [false, true] {
        for expired in [false, true] {
            for nested in [false, true] {
                let mut app = app();
                if bound {
                    app.state.connections.get_mut("fixture").unwrap().origin_config.bouncer_network_id = Some("1".into());
                }
                receive(&mut app, ":me!u@h JOIN #test");
                app.state.set_active_buffer("fixture/fixture");
                let before = app.state.buffers["fixture/#test"].messages.len();
                let activity = app.state.buffers["fixture/#test"].activity;
                let (tx, mut log_rx) = tokio::sync::mpsc::channel(16);
                app.state.log_tx = Some(tx);
                app.web_broadcaster = std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(128));
                let mut web_rx = app.web_broadcaster.subscribe();
                if nested {
                    receive(&mut app, ":server BATCH +outer labeled-response");
                    receive(&mut app, "@batch=outer :server BATCH +search soju.im/search");
                } else {
                    receive(&mut app, ":server BATCH +search soju.im/search");
                }
                receive(&mut app, "@batch=search :server BATCH +inner vendor/wrapper");
                receive(&mut app, "@batch=inner;msgid=search-only;time=2024-01-01T00:00:00.000Z :Alice!u@h PRIVMSG #test :me search result");
                receive(&mut app, "@batch=search :me!u@h JOIN #unexpected");
                if expired {
                    let tracker = app.batch_trackers.get_mut("fixture").unwrap();
                    let mut batches = vec![("fixture".into(), "search".into(), tracker.end_batch("search").unwrap()),
                        ("fixture".into(), "inner".into(), tracker.end_batch("inner").unwrap())];
                    if nested { batches.push(("fixture".into(), "outer".into(), tracker.end_batch("outer").unwrap())); }
                    app.dispatch_expired_batch_set(batches);
                } else {
                    receive(&mut app, ":server BATCH -inner");
                    receive(&mut app, ":server BATCH -search");
                    if nested { receive(&mut app, ":server BATCH -outer"); }
                }
                receive(&mut app, "@batch=search;msgid=late-search;time=2024-01-01T00:00:01.000Z :Alice!u@h PRIVMSG #test :me late search result");
                assert_eq!(app.state.buffers["fixture/#test"].messages.len(), before, "bound={bound} expired={expired} nested={nested}");
                assert_eq!(app.state.buffers["fixture/#test"].activity, activity);
                assert!(!app.state.buffers.contains_key("fixture/#unexpected"));
                assert!(log_rx.try_recv().is_err());
                assert!(!std::iter::from_fn(|| web_rx.try_recv().ok()).any(|event| matches!(event, crate::web::protocol::WebEvent::NewMessage { message, .. } if message.text.contains("search result"))));
                receive(&mut app, ":Alice!u@h PRIVMSG #test :ordinary live message");
                assert_eq!(app.state.buffers["fixture/#test"].messages.len(), before + 1);
                assert_eq!(log_rx.try_recv().is_ok(), !bound);
            }
        }
    }
}

use std::path::Path;
use std::time::Duration;

use super::App;
use crate::state::buffer::{Buffer, BufferType};
use crate::web::protocol::{WebCommand, WebEvent};

async fn until(app: &mut App, stage: &str, predicate: impl Fn(&App) -> bool + Send + Sync) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            while let Ok(event) = app.irc_rx.try_recv() {
                app.handle_irc_event(event);
            }
            app.tick_history_discovery();
            app.flush_server_history_pages();
            if predicate(app) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("history storage fixture timed out: {stage}"));
}

fn prepare(path: &Path, seed: bool) -> App {
    let mut app = super::input::submit_typing_tests::test_app();
    let storage = crate::storage::Storage::fixture_at(path);
    app.state.log_tx = Some(storage.log_tx.clone());
    app.storage = Some(storage);
    app.config.general.flood_protection = false;
    app.config.display.backlog_lines = 200;
    app.state.scrollback_limit = 1000;
    app.web_broadcaster = std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(2048));
    let mut config: crate::config::ServerConfig =
        toml::from_str("label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]")
            .unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT")
        .unwrap()
        .parse()
        .unwrap();
    let user = std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap();
    let pass_mode = std::env::var("REPARTEE_BOUNCER_TEST_PASS");
    let legacy = std::env::var("REPARTEE_BOUNCER_TEST_LEGACY").as_deref() == Ok("1");
    if legacy {
        let network = if std::env::var("REPARTEE_BOUNCER_TEST_PROVIDER").as_deref() == Ok("soju") {
            "fixture".to_string()
        } else {
            std::env::var("REPARTEE_BOUNCER_TEST_NETID").unwrap()
        };
        config.username = Some(format!("{user}/{network}"));
        config.password = Some("fixture-password".into());
        config.channels = vec!["#must-not-autojoin".into()];
    } else if pass_mode.as_deref() == Ok("combined") {
        config.password = Some(format!("{user}:fixture-password"));
        config.username = Some("ignored".into());
    } else if pass_mode.as_deref() == Ok("user") {
        config.username = Some(user);
        config.password = Some("fixture-password".into());
    } else {
        config.sasl_user = Some(user);
        config.sasl_pass = Some("fixture-password".into());
    }
    if !legacy {
        config.bouncer_network_id = Some(std::env::var("REPARTEE_BOUNCER_TEST_NETID").unwrap());
    }
    app.setup_connection("fixture", &config);
    if seed {
        let scope = app.state.connections["fixture"].network_key();
        app.storage.as_ref().unwrap().db.lock().unwrap().execute(
            "INSERT INTO messages (network, buffer, timestamp, type, text) VALUES (?1, 'history-peer', 1, 'message', 'preserved legacy row')",
            [scope],
        ).unwrap();
    }
    app.start_connection_attempt("fixture", config);
    app
}

async fn verify_pages(app: &mut App) {
    let buffer_id = "fixture/history-peer";
    until(app, "initial hydration", |app| {
        app.state
            .buffers
            .get(buffer_id)
            .is_some_and(|buffer| buffer.messages.len() == 200)
            && !app.state.connections["fixture"]
                .chathistory
                .any_in_flight("history-peer")
    })
    .await;
    assert!(app.state.connections["fixture"].server_owns_history());
    assert!(
        app.state.buffers[buffer_id]
            .messages
            .iter()
            .all(|row| row.text.starts_with("fixture-history-"))
    );
    app.state.remove_buffer(buffer_id);
    app.state.set_active_buffer("fixture/fixture");
    app.handle_submit("/query history-peer");
    assert_eq!(app.state.active_buffer_id.as_deref(), Some(buffer_id));
    assert!(app.state.buffers[buffer_id].messages.is_empty());
    until(app, "native query history", |app| {
        app.state.buffers[buffer_id].messages.len() == 200
            && !app.state.connections["fixture"]
                .chathistory
                .any_in_flight("history-peer")
    })
    .await;
    let first = app.state.buffers[buffer_id].messages.front().unwrap();
    let (before, before_message_id) = (first.timestamp.timestamp_millis(), first.id);
    let mut web = app.web_broadcaster.subscribe();
    app.handle_web_command(
        WebCommand::FetchMessages {
            buffer_id: buffer_id.into(),
            limit: 200,
            before: Some(before),
            before_id: None,
            before_message_id: Some(before_message_id),
        },
        "storage-browser",
    );
    until(app, "browser older page", |app| {
        app.pending_history_pages.is_empty() && app.state.buffers[buffer_id].messages.len() == 300
    })
    .await;
    let pages: Vec<_> = std::iter::from_fn(|| web.try_recv().ok())
        .filter_map(|event| {
            if let WebEvent::Messages {
                messages,
                session_id,
                ..
            } = event
                && session_id.as_deref() == Some("storage-browser")
            {
                Some(messages)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].len(), 100);
    for (index, message) in pages[0].iter().enumerate() {
        assert_eq!(message.text, format!("fixture-history-{index}"));
    }
    assert!(
        app.state.buffers[buffer_id]
            .messages
            .iter()
            .all(|row| row.text.starts_with("fixture-history-"))
    );
}

fn direct_irc_control(app: &mut App) {
    let config: crate::config::ServerConfig =
        toml::from_str("label='direct'\naddress='fixture.invalid'\nport=1\ntls=false\nchannels=[]")
            .unwrap();
    app.setup_connection("direct", &config);
    app.state.add_buffer_with_focus(
        Buffer::empty("direct", BufferType::Channel, "#control"),
        false,
    );
    app.handle_irc_event(crate::irc::IrcEvent::Message(
        "direct".into(),
        Box::new(
            ":peer!user@fixture PRIVMSG #control :direct persistence control"
                .parse()
                .unwrap(),
        ),
    ));
    assert!(
        app.state.buffers["direct/#control"]
            .messages
            .iter()
            .any(|row| row.text == "direct persistence control")
    );
}

fn live_bouncer_messages(app: &mut App) {
    let target = "fixture/#storage-live";
    app.state.add_buffer_with_focus(
        Buffer::empty("fixture", BufferType::Channel, "#storage-live"),
        false,
    );
    app.handle_irc_event(crate::irc::IrcEvent::Message(
        "fixture".into(),
        Box::new(
            ":peer!user@fixture PRIVMSG #storage-live :volatile incoming"
                .parse()
                .unwrap(),
        ),
    ));
    let had_echo = app
        .state
        .connections
        .get_mut("fixture")
        .unwrap()
        .enabled_caps
        .remove("echo-message");
    app.handle_web_command(
        WebCommand::SendMessage {
            buffer_id: target.into(),
            text: "volatile local send".into(),
        },
        "storage-browser",
    );
    if had_echo {
        app.state
            .connections
            .get_mut("fixture")
            .unwrap()
            .enabled_caps
            .insert("echo-message".into());
    }
    let nick = app.state.connections["fixture"].nick.clone();
    app.handle_irc_event(crate::irc::IrcEvent::Message(
        "fixture".into(),
        Box::new(
            format!(":{nick}!user@fixture PRIVMSG #storage-live :volatile own echo")
                .parse()
                .unwrap(),
        ),
    ));
    for text in [
        "volatile incoming",
        "volatile local send",
        "volatile own echo",
    ] {
        assert!(
            app.state.buffers[target]
                .messages
                .iter()
                .any(|row| row.text == text),
            "missing live row: {text}"
        );
    }
}

async fn close(mut app: App) {
    app.cancel_connection_attempt("fixture");
    app.irc_handles.clear();
    for (_, task) in app.forwarder_handles.drain() {
        task.abort();
    }
    app.state.log_tx = None;
    app.storage.take().unwrap().shutdown().await;
}

fn verify_disk(path: &Path) {
    let database = crate::storage::db::open_readonly_at(path.to_str().unwrap()).unwrap();
    let mut statement = database
        .prepare("SELECT text FROM messages ORDER BY id")
        .unwrap();
    let mut rows: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    if std::env::var("REPARTEE_BOUNCER_TEST_LEGACY").as_deref() == Ok("1") {
        rows.retain(|text| text != "Connecting to fixture...");
    }
    assert_eq!(
        rows,
        [
            "preserved legacy row",
            "Connecting to direct...",
            "direct persistence control"
        ]
    );
}

#[tokio::test]
#[ignore = "requires a disposable pinned bouncer fixture"]
async fn pinned_bouncer_persistent_history() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("messages.db");
    let mut first = prepare(&path, true);
    verify_pages(&mut first).await;
    live_bouncer_messages(&mut first);
    direct_irc_control(&mut first);
    Box::pin(close(first)).await;
    verify_disk(&path);
    let mut reopened = prepare(&path, false);
    verify_pages(&mut reopened).await;
    Box::pin(close(reopened)).await;
    verify_disk(&path);
}

#[tokio::test]
#[ignore = "requires a disposable pinned bouncer fixture"]
async fn pinned_bouncer_bounded_history() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("messages.db");
    let mut app = prepare(&path, true);
    until(&mut app, "connected history", |app| {
        app.state.connections["fixture"].status == crate::state::connection::ConnectionStatus::Connected
            && app.state.buffers.contains_key("fixture/history-peer")
            && !app.state.connections["fixture"].chathistory.any_in_flight("history-peer")
    }).await;
    for (web, first, last, expected) in [
        (false, 10, 20, vec![11, 12, 13]),
        (true, 20, 10, vec![17, 18, 19]),
        (true, 10, 10, vec![]),
    ] {
        let command = format!("/bsearch between history-peer 2024-01-01T00:00:{first:02}Z 2024-01-01T00:00:{last:02}Z 3");
        app.state.set_active_buffer("fixture/history-peer");
        if web {
            app.handle_web_command(WebCommand::RunCommand { buffer_id: "fixture/history-peer".into(), text: command }, "range-browser");
        } else {
            app.handle_submit(&command);
        }
        assert!(app.server_search.contains_key("fixture"));
        until(&mut app, "range complete", |app| !app.server_search.contains_key("fixture")).await;
        let rows: Vec<_> = app.state.buffers["fixture/*search*"].messages.iter()
            .filter(|row| row.nick.is_some()).map(|row| row.text.clone()).collect();
        assert_eq!(rows, expected.into_iter().map(|index| format!("fixture-history-{index}")).collect::<Vec<_>>());
    }
    super::filehost_browser_fixture::run(&mut app, "scripts/fixtures/history-range-browser.cjs").await;
    direct_irc_control(&mut app);
    Box::pin(close(app)).await;
    let database = crate::storage::db::open_readonly_at(path.to_str().unwrap()).unwrap();
    let mut statement = database.prepare("SELECT buffer, text FROM messages ORDER BY id").unwrap();
    let rows: Vec<(String, String)> = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap().map(Result::unwrap).collect();
    let rows: Vec<_> = rows.into_iter().filter(|(buffer, text)| !(matches!(buffer.as_str(), "fixture" | "*search*") && matches!(text.as_str(),
        "%Z56b6c2You are now marked as away%N" | "%Z56b6c2You are no longer marked as away%N")))
        .map(|(_, text)| text).collect();
    assert_eq!(rows, ["preserved legacy row", "Connecting to direct...", "direct persistence control"]);
}

#[tokio::test]
#[ignore = "requires a disposable pinned bouncer and partial-batch proxy"]
async fn pinned_bouncer_partial_history() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("messages.db");
    let control = std::env::var("REPARTEE_BOUNCER_FAULT_CONTROL").unwrap();
    let http = reqwest::Client::new();
    let mut app = prepare(&path, true);
    until(&mut app, "settled history", |app| {
        app.state.connections["fixture"].status == crate::state::connection::ConnectionStatus::Connected
            && app.state.buffers.contains_key("fixture/history-peer")
            && app.state.connections["fixture"].chathistory.pending_count() == 0
    }).await;
    let baseline: Vec<_> = app.state.buffers["fixture/history-peer"].messages.iter().map(|row| row.id).collect();
    let armed: serde_json::Value = http.post(format!("{control}/arm")).send().await.unwrap().json().await.unwrap();
    assert_eq!(armed["armed"], true);
    let command = "/bsearch between history-peer 2024-01-01T00:00:10Z 2024-01-01T00:00:20Z 3";
    app.state.set_active_buffer("fixture/history-peer");
    app.handle_submit(command);
    assert!(app.server_search.contains_key("fixture"));
    let mut partial_seen = false;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            while let Ok(event) = app.irc_rx.try_recv() {
                let mut inner = &event;
                while let crate::irc::IrcEvent::Attempt(_, _, nested) = inner { inner = nested; }
                let partial = if let crate::irc::IrcEvent::Message(_, message) = inner
                    && matches!(&message.command, irc::proto::Command::PRIVMSG(_, text) if text == "fixture-history-11") {
                    crate::irc::batch::BatchTracker::get_batch_tag_owned(message)
                } else { None };
                app.handle_irc_event(event);
                if let Some(tag) = partial {
                    assert!(app.batch_trackers["fixture"].is_open(&tag));
                    assert!(app.server_search.contains_key("fixture"));
                    assert!(!app.state.buffers["fixture/*search*"].messages.iter().any(|row| row.nick.is_some()));
                    partial_seen = true;
                }
            }
            if app.state.connections["fixture"].status != crate::state::connection::ConnectionStatus::Connected
                && !app.server_search.contains_key("fixture") { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("partial history connection did not terminate");
    assert!(partial_seen);
    assert!(!app.batch_trackers.contains_key("fixture"));
    let status: serde_json::Value = http.post(format!("{control}/status")).send().await.unwrap().json().await.unwrap();
    assert_eq!(status["requested"], true);
    assert_eq!(status["batch_opened"], true);
    assert_eq!(status["forwarded_rows"], 1);
    assert_eq!(status["faulted"], true);
    assert!(!app.state.buffers["fixture/*search*"].messages.iter().any(|row| row.nick.is_some()));
    assert_eq!(app.state.buffers["fixture/history-peer"].messages.iter().map(|row| row.id).collect::<Vec<_>>(), baseline);
    assert!(http.post(format!("{control}/resume")).send().await.unwrap().status().is_success());
    let config = app.state.connections["fixture"].origin_config.clone();
    app.start_connection_attempt("fixture", config);
    until(&mut app, "reconnected history", |app| {
        app.state.connections["fixture"].status == crate::state::connection::ConnectionStatus::Connected
            && app.state.connections["fixture"].chathistory.pending_count() == 0
    }).await;
    app.state.set_active_buffer("fixture/history-peer");
    app.handle_submit(command);
    assert!(app.server_search.contains_key("fixture"));
    until(&mut app, "retried range complete", |app| !app.server_search.contains_key("fixture")).await;
    let rows: Vec<_> = app.state.buffers["fixture/*search*"].messages.iter().filter(|row| row.nick.is_some()).map(|row| row.text.as_str()).collect();
    assert_eq!(rows, ["fixture-history-11", "fixture-history-12", "fixture-history-13"]);
    direct_irc_control(&mut app);
    Box::pin(close(app)).await;
    let database = crate::storage::db::open_readonly_at(path.to_str().unwrap()).unwrap();
    let count = |pattern: &str| database.query_row("SELECT COUNT(*) FROM messages WHERE text LIKE ?1", [pattern], |row| row.get::<_, i64>(0)).unwrap();
    assert_eq!(count("fixture-history-%"), 0);
    assert_eq!(count("preserved legacy row"), 1);
    assert_eq!(count("direct persistence control"), 1);
}

async fn observe_partial_row(app: &mut App) -> String {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            while let Ok(event) = app.irc_rx.try_recv() {
                let mut inner = &event;
                while let crate::irc::IrcEvent::Attempt(_, _, nested) = inner { inner = nested; }
                let partial = if let crate::irc::IrcEvent::Message(_, message) = inner
                    && matches!(&message.command, irc::proto::Command::PRIVMSG(_, text) if text == "fixture-history-11") {
                    crate::irc::batch::BatchTracker::get_batch_tag_owned(message)
                } else { None };
                app.handle_irc_event(event);
                if let Some(tag) = partial {
                    assert!(app.batch_trackers["fixture"].is_open(&tag));
                    return tag;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("partial history row was not received")
}

#[tokio::test]
#[ignore = "requires a disposable pinned bouncer and stalled-batch proxy"]
async fn pinned_bouncer_stalled_history() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("messages.db");
    let control = std::env::var("REPARTEE_BOUNCER_FAULT_CONTROL").unwrap();
    let mode = std::env::var("REPARTEE_BOUNCER_STALL_MODE").unwrap();
    assert!(matches!(mode.as_str(), "timeout" | "cancel" | "batch-expiry" | "request-expiry"));
    let expired = mode.ends_with("expiry");
    let http = reqwest::Client::new();
    let mut app = prepare(&path, true);
    until(&mut app, "settled history", |app| {
        app.state.connections["fixture"].status == crate::state::connection::ConnectionStatus::Connected
            && app.state.buffers.contains_key("fixture/history-peer")
            && app.state.connections["fixture"].chathistory.pending_count() == 0
    }).await;
    let baseline: Vec<_> = app.state.buffers["fixture/history-peer"].messages.iter().map(|row| row.id).collect();
    let armed: serde_json::Value = http.post(format!("{control}/arm")).send().await.unwrap().json().await.unwrap();
    assert_eq!(armed["armed"], true);
    let command = "/bsearch between history-peer 2024-01-01T00:00:10Z 2024-01-01T00:00:20Z 3";
    app.state.set_active_buffer("fixture/history-peer");
    app.handle_submit(command);
    let observed_at = std::time::Instant::now();
    let partial_tag = observe_partial_row(&mut app).await;
    assert!(app.server_search.contains_key("fixture"));
    assert!(!app.state.buffers["fixture/*search*"].messages.iter().any(|row| row.nick.is_some()));
    if mode == "cancel" {
        app.handle_submit("/bsearch cancel");
    } else {
        tokio::time::timeout(Duration::from_secs(110), async {
            loop {
                while let Ok(event) = app.irc_rx.try_recv() { app.handle_irc_event(event); }
                app.purge_expired_batches();
                app.purge_stale_chathistory_requests();
                app.tick_history_discovery();
                app.tick_server_search();
                let batch_expired = app.batch_trackers.get("fixture").is_none_or(|tracker| !tracker.is_open(&partial_tag));
                let ready = match mode.as_str() {
                    "batch-expiry" => batch_expired,
                    "request-expiry" => batch_expired && app.state.connections["fixture"].chathistory.pending_count() == 0,
                    _ => app.state.buffers["fixture/*search*"].messages.iter().any(|row| row.text.contains("Search timed out")),
                };
                if ready { break; }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }).await.expect("real search deadline did not expire");
    }
    if mode == "request-expiry" {
        assert!(observed_at.elapsed() >= Duration::from_secs(89));
        assert!(app.state.connections["fixture"].chathistory.has_ambiguous_reply());
    } else if mode == "batch-expiry" {
        assert!(observed_at.elapsed() >= Duration::from_secs(59));
        assert!(app.state.connections["fixture"].chathistory.pending_count() > 0);
    }
    assert_eq!(app.state.connections["fixture"].status, crate::state::connection::ConnectionStatus::Connected);
    app.handle_submit(command);
    assert!(app.state.buffers["fixture/*search*"].messages.back().unwrap().text.contains("A search is unresolved"));
    let status: serde_json::Value = http.post(format!("{control}/status")).send().await.unwrap().json().await.unwrap();
    assert_eq!(status["forwarded_rows"], 1);
    assert_eq!(status["faulted"], true);
    assert_eq!(status["released"], false);
    let released: serde_json::Value = http.post(format!("{control}/release")).send().await.unwrap().json().await.unwrap();
    assert!(released["released_bytes"].as_u64().unwrap() > 0);
    if expired {
        consume_history_barrier(&mut app).await;
        assert!(app.server_search.contains_key("fixture"));
        app.handle_submit(command);
        assert!(app.state.buffers["fixture/*search*"].messages.back().unwrap().text.contains("A search is unresolved"));
    } else {
        until(&mut app, "late batch discarded", |app| !app.server_search.contains_key("fixture")).await;
    }
    assert!(!app.state.buffers["fixture/*search*"].messages.iter().any(|row| row.nick.is_some()));
    assert_eq!(app.state.buffers["fixture/history-peer"].messages.iter().map(|row| row.id).collect::<Vec<_>>(), baseline);
    if expired {
        app.handle_submit("/disconnect");
        until(&mut app, "disconnect quarantined range", |app| {
            app.state.connections["fixture"].status == crate::state::connection::ConnectionStatus::Disconnected
                && !app.irc_handles.contains_key("fixture")
        }).await;
        let config = app.state.connections["fixture"].origin_config.clone();
        app.start_connection_attempt("fixture", config);
        until(&mut app, "reconnect after expired range", |app| {
            app.state.connections["fixture"].status == crate::state::connection::ConnectionStatus::Connected
                && app.state.connections["fixture"].chathistory.pending_count() == 0
        }).await;
    }
    app.handle_submit(command);
    assert!(app.server_search.contains_key("fixture"));
    until(&mut app, "new range complete", |app| !app.server_search.contains_key("fixture")).await;
    let rows: Vec<_> = app.state.buffers["fixture/*search*"].messages.iter().filter(|row| row.nick.is_some()).map(|row| row.text.as_str()).collect();
    assert_eq!(rows, ["fixture-history-11", "fixture-history-12", "fixture-history-13"]);
    direct_irc_control(&mut app);
    Box::pin(close(app)).await;
    let database = crate::storage::db::open_readonly_at(path.to_str().unwrap()).unwrap();
    let count = |pattern: &str| database.query_row("SELECT COUNT(*) FROM messages WHERE text LIKE ?1", [pattern], |row| row.get::<_, i64>(0)).unwrap();
    assert_eq!(count("fixture-history-%"), 0);
    assert_eq!(count("preserved legacy row"), 1);
    assert_eq!(count("direct persistence control"), 1);
}

async fn consume_history_barrier(app: &mut App) {
    let barrier = "partial-history-drained";
    app.irc_handles["fixture"].sender().send(irc::proto::Command::PING(barrier.into(), None)).unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            while let Ok(event) = app.irc_rx.try_recv() {
                let mut inner = &event;
                while let crate::irc::IrcEvent::Attempt(_, _, nested) = inner { inner = nested; }
                let reached = matches!(inner, crate::irc::IrcEvent::Message(_, message)
                    if matches!(&message.command, irc::proto::Command::PONG(first, second)
                        if first == barrier || second.as_deref() == Some(barrier)));
                app.handle_irc_event(event);
                if reached { return; }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("history stream barrier did not arrive");
}

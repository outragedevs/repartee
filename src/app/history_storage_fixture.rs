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
    config.sasl_user = Some(std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap());
    config.sasl_pass = Some("fixture-password".into());
    config.bouncer_network_id = Some(std::env::var("REPARTEE_BOUNCER_TEST_NETID").unwrap());
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
    let rows: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
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

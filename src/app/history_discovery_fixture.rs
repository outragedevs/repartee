use super::App;
use crate::state::buffer::BufferType;
use crate::web::protocol::{WebCommand, WebEvent};

async fn until(app: &mut App, stage: &str, predicate: impl Fn(&App) -> bool + Send + Sync) {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            while let Ok(event) = app.irc_rx.try_recv() {
                app.handle_irc_event(event);
            }
            app.tick_history_discovery();
            app.flush_server_history_pages();
            if predicate(app) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("target tie fixture timed out: {stage}"));
}

#[tokio::test]
#[ignore = "requires a disposable pinned bouncer fixture with 1001 tied targets"]
async fn pinned_bouncer_discovery_limit() {
    let mut app = super::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    app.config.display.backlog_lines = 0;
    app.web_broadcaster = std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(4096));
    let mut web = app.web_broadcaster.subscribe();
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
    app.start_connection_attempt("fixture", config);
    until(&mut app, "discovery completion", |app| {
        app.history_discovery
            .get("fixture")
            .is_some_and(|discovery| discovery.finished)
    })
    .await;
    assert!(app.history_discovery["fixture"].incomplete);
    assert_eq!(
        app.state
            .buffers
            .values()
            .filter(|buffer| buffer.buffer_type == BufferType::Query)
            .count(),
        1000
    );
    let warning = "Some conversations may be missing";
    assert_eq!(
        app.state.buffers["fixture/fixture"]
            .messages
            .iter()
            .filter(|message| message.text.contains(warning))
            .count(),
        1
    );
    let warnings = std::iter::from_fn(|| web.try_recv().ok()).filter(|event| {
        matches!(event, WebEvent::NewMessage { buffer_id, message } if buffer_id == "fixture/fixture" && message.text.contains(warning))
    }).count();
    assert_eq!(warnings, 1);
    let missing = (0..1001)
        .map(|index| format!("peer-{index:04}"))
        .find(|target| !app.state.buffers.contains_key(&format!("fixture/{target}")))
        .unwrap();
    app.handle_web_command(
        WebCommand::RunCommand {
            buffer_id: "fixture/fixture".into(),
            text: format!("/query {missing}"),
        },
        "tie-browser",
    );
    let buffer_id = format!("fixture/{missing}");
    assert!(app.state.buffers.contains_key(&buffer_id));
    app.handle_web_command(
        WebCommand::FetchMessages {
            buffer_id: buffer_id.clone(),
            limit: 100,
            before: None,
            before_id: None,
            before_message_id: None,
        },
        "tie-browser",
    );
    until(&mut app, "explicit missing conversation retrieval", |app| {
        app.pending_history_pages.is_empty() && !app.state.buffers[&buffer_id].messages.is_empty()
    })
    .await;
    let page = std::iter::from_fn(|| web.try_recv().ok())
        .find_map(|event| {
            if let WebEvent::Messages {
                messages,
                session_id,
                ..
            } = event
                && session_id.as_deref() == Some("tie-browser")
            {
                Some(messages)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(page.len(), 1);
    assert!(matches!(page[0].text.as_str(), "tie" | "fixture-history-0"));
    app.cancel_connection_attempt("fixture");
    app.irc_handles.clear();
}

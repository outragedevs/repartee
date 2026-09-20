use super::App;

async fn until(app: &mut App, stage: &str, predicate: impl Fn(&App) -> bool + Send + Sync) {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            while let Ok(event) = app.irc_rx.try_recv() {
                app.handle_irc_event(event);
            }
            if predicate(app) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.unwrap_or_else(|_| panic!("redaction fixture timed out: {stage}"));
}

fn send(app: &App, wire: &str) {
    app.irc_handles["fixture"].sender().send(wire.parse::<irc::proto::Message>().unwrap()).unwrap();
}

#[tokio::test]
#[ignore = "requires a disposable pinned bouncer fixture"]
async fn pinned_bouncer_redaction() {
    let mut app = super::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    let mut config: crate::config::ServerConfig = toml::from_str("label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nbouncer_network_id='1'").unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT").unwrap().parse().unwrap();
    config.sasl_user = Some(std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap());
    config.sasl_pass = Some("fixture-password".into());
    app.setup_connection("fixture", &config);
    app.start_connection_attempt("fixture", config);
    until(&mut app, "connected", |app| app.state.connections["fixture"].status == crate::state::connection::ConnectionStatus::Connected).await;
    let soju = std::env::var("REPARTEE_PRESENCE_PROVIDER").unwrap() == "soju";
    if !soju {
        assert!(!app.state.connections["fixture"].enabled_caps.contains("draft/message-redaction"));
        app.state.set_active_buffer("fixture/fixture");
        app.execute_command(&crate::commands::parser::parse_command("/redact #redaction absent").unwrap());
        assert!(app.state.buffers["fixture/fixture"].messages.iter().any(|message| message.text.contains("redaction")));
        app.cancel_connection_attempt("fixture");
        app.irc_handles.remove("fixture");
        return;
    }
    until(&mut app, "capability ACK", |app| app.state.connections["fixture"].enabled_caps.contains("draft/message-redaction")).await;
    send(&app, "JOIN #redaction");
    until(&mut app, "joined", |app| app.state.buffers.contains_key("fixture/#redaction")).await;
    let (tx, mut logs) = tokio::sync::mpsc::channel(64);
    app.state.log_tx = Some(tx);
    app.web_broadcaster = std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(128));
    let mut web = app.web_broadcaster.subscribe();
    send(&app, "PRIVMSG FixtureControl :redaction-message");
    until(&mut app, "message", |app| app.state.buffers["fixture/#redaction"].messages.iter().any(|message| message.text == "fixture secret body")).await;
    let row = app.state.buffers["fixture/#redaction"].messages.iter().find(|message| message.text == "fixture secret body").unwrap();
    let id = row.tags.as_ref().unwrap()["msgid"].clone();
    app.state.set_active_buffer("fixture/#redaction");
    app.execute_command(&crate::commands::parser::parse_command(&format!("/redact #redaction {id} denied")).unwrap());
    until(&mut app, "server rejection", |app| {
        app.state.buffers.values().any(|buffer| buffer.messages.iter()
            .any(|message| message.text.contains("fixture denied deletion")))
    }).await;
    assert!(app.state.buffers["fixture/#redaction"].messages.iter().any(|message| message.text == "fixture secret body"));
    app.execute_command(&crate::commands::parser::parse_command(&format!("/redact #redaction {id} fixture removal")).unwrap());
    until(&mut app, "deleted", |app| app.state.buffers["fixture/#redaction"].messages.iter().any(|message| message.text.contains("Message deleted by") && message.text.contains("fixture removal"))).await;
    assert!(!app.state.buffers["fixture/#redaction"].messages.iter().any(|message| message.text == "fixture secret body"));
    let events: Vec<_> = std::iter::from_fn(|| web.try_recv().ok()).collect();
    assert!(events.iter().any(|event| matches!(event, crate::web::protocol::WebEvent::RedactMessage { buffer_id, msgid, .. } if buffer_id == "fixture/#redaction" && msgid == &id)));
    if let Ok(path) = std::env::var("REPARTEE_REDACTION_WEB_EVENTS") {
        std::fs::write(path, serde_json::to_vec(&events).unwrap()).unwrap();
    }
    assert!(logs.try_recv().is_err());
    app.state.buffers.get_mut("fixture/#redaction").unwrap().messages.clear();
    app.state.redaction_registry = crate::state::redaction_registry::Registry::new(4096);
    send(&app, "CHATHISTORY LATEST #redaction * 50");
    until(&mut app, "server history deletion", |app| {
        app.state.buffers["fixture/#redaction"].messages.iter()
            .any(|message| message.text.contains("Message deleted by") && message.text.contains("fixture removal"))
    }).await;
    assert!(!app.state.buffers["fixture/#redaction"].messages.iter().any(|message| message.text == "fixture secret body"));
    assert!(logs.try_recv().is_err());
    let events = std::fs::read_to_string(std::env::var("REPARTEE_PRESENCE_EVENTS").unwrap()).unwrap();
    assert!(events.lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok()).any(|event| event["redact"][0] == "#redaction" && event["redact"][1] == id));
    app.cancel_connection_attempt("fixture");
    app.irc_handles.remove("fixture");
}

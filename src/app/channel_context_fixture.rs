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
    })
    .await
    .unwrap_or_else(|_| panic!("channel context fixture timed out: {stage}"));
}

fn send(app: &App, wire: &str) {
    app.irc_handles["fixture"]
        .sender()
        .send(wire.parse::<irc::proto::Message>().unwrap())
        .unwrap();
}

#[tokio::test]
#[ignore = "requires a disposable pinned bouncer fixture"]
async fn pinned_bouncer_channel_context() {
    let mut app = super::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    let mut config: crate::config::ServerConfig = toml::from_str("label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nbouncer_network_id='1'").unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT")
        .unwrap()
        .parse()
        .unwrap();
    config.sasl_user = Some(std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap());
    config.sasl_pass = Some("fixture-password".into());
    app.setup_connection("fixture", &config);
    app.start_connection_attempt("fixture", config);
    until(&mut app, "connected", |app| {
        app.state.connections["fixture"].status
            == crate::state::connection::ConnectionStatus::Connected
    })
    .await;
    until(&mut app, "capability ACK", |app| {
        app.state.connections["fixture"]
            .enabled_caps
            .contains("message-tags")
    })
    .await;
    send(&app, "JOIN #context");
    until(&mut app, "joined", |app| {
        app.state
            .buffers
            .get("fixture/#context")
            .is_some_and(|buffer| buffer.users.values().any(|user| user.nick == "Alice"))
    })
    .await;
    let (tx, mut logs) = tokio::sync::mpsc::channel(64);
    app.state.log_tx = Some(tx);
    app.web_broadcaster = std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(128));
    let mut web = app.web_broadcaster.subscribe();
    send(&app, "PRIVMSG FixtureControl :channel-context");
    until(&mut app, "live context notice", |app| {
        app.state.buffers["fixture/#context"]
            .messages
            .iter()
            .any(|message| message.text == "context live notice")
    })
    .await;
    assert!(!app.state.buffers.contains_key("fixture/alice"));
    let events: Vec<_> = std::iter::from_fn(|| web.try_recv().ok()).collect();
    assert!(events.iter().any(|event| matches!(event, crate::web::protocol::WebEvent::NewMessage { buffer_id, .. } if buffer_id == "fixture/#context")));
    assert!(logs.try_recv().is_err());
    app.state
        .buffers
        .get_mut("fixture/#context")
        .unwrap()
        .messages
        .clear();
    send(&app, "CHATHISTORY LATEST #context * 50");
    until(&mut app, "history context notice", |app| {
        app.state.buffers["fixture/#context"]
            .messages
            .iter()
            .any(|message| message.text == "context live notice")
    })
    .await;
    assert!(!app.state.buffers.contains_key("fixture/alice"));
    assert!(logs.try_recv().is_err());
    if std::env::var("REPARTEE_PRESENCE_PROVIDER").unwrap() == "soju" {
        app.state.set_active_buffer("fixture/#context");
        app.handle_submit("/bsearch #context -- context");
        until(&mut app, "search context notice", |app| {
            app.state
                .buffers
                .get("fixture/*search*")
                .is_some_and(|buffer| {
                    buffer
                        .messages
                        .iter()
                        .any(|message| message.text == "context live notice")
                })
        })
        .await;
        assert!(logs.try_recv().is_err());
    }
    app.cancel_connection_attempt("fixture");
    app.irc_handles.remove("fixture");
}

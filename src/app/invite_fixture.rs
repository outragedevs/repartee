use super::App;

async fn until(app: &mut App, predicate: impl Fn(&App) -> bool + Send + Sync) {
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
    .expect("Invitation fixture did not reach expected state");
}

#[tokio::test]
#[ignore = "requires a disposable pinned bouncer fixture"]
async fn pinned_bouncer_invites() {
    let mut app = crate::app::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    let mut config: crate::config::ServerConfig = toml::from_str(
        "label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nbouncer_network_id='1'",
    ).unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT")
        .unwrap()
        .parse()
        .unwrap();
    config.sasl_user = Some(std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap());
    config.sasl_pass = Some("fixture-password".into());
    app.setup_connection("fixture", &config);
    app.start_connection_attempt("fixture", config);
    until(&mut app, |app| {
        app.state.connections["fixture"].status
            == crate::state::connection::ConnectionStatus::Connected
            && app.state.connections["fixture"]
                .enabled_caps
                .contains("invite-notify")
    })
    .await;
    app.irc_handles["fixture"]
        .sender()
        .send(irc::proto::Command::JOIN("#fixture".into(), None, None))
        .unwrap();
    until(&mut app, |app| {
        app.state.buffers.contains_key("fixture/#fixture")
    })
    .await;
    let other =
        toml::from_str("label='Other'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]")
            .unwrap();
    app.setup_connection("other", &other);
    app.state.set_active_buffer("other/other");
    let (tx, mut rx) = tokio::sync::mpsc::channel(128);
    app.state.log_tx = Some(tx);
    app.state.pending_web_events.clear();
    app.web_broadcaster = std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(128));
    let mut web_rx = app.web_broadcaster.subscribe();
    app.irc_handles["fixture"]
        .sender()
        .send(irc::proto::Command::PRIVMSG(
            "FixtureControl".into(),
            "own-invite".into(),
        ))
        .unwrap();
    until(&mut app, |app| {
        app.state.buffers["fixture/fixture"]
            .messages
            .iter()
            .any(|message| message.text.contains("invites you"))
    })
    .await;
    app.irc_handles["fixture"]
        .sender()
        .send(irc::proto::Command::PRIVMSG(
            "FixtureControl".into(),
            "peer-invite".into(),
        ))
        .unwrap();
    until(&mut app, |app| {
        app.state.buffers["fixture/#fixture"]
            .messages
            .iter()
            .any(|message| message.text.contains("invited Other"))
    })
    .await;
    assert!(
        !app.state.buffers["other/other"]
            .messages
            .iter()
            .any(|message| message.text.contains("Inviter"))
    );
    let events: Vec<_> = std::iter::from_fn(|| web_rx.try_recv().ok()).collect();
    assert!(events.iter().any(|event| matches!(event,
        crate::web::protocol::WebEvent::MentionAlert { buffer_id, .. } if buffer_id == "fixture/fixture"
    )));
    assert!(events.iter().any(|event| matches!(event,
        crate::web::protocol::WebEvent::NewMessage { buffer_id, message } if buffer_id == "fixture/#fixture" && message.text.contains("invited Other") && !message.highlight
    )));
    if let Some(path) = std::env::var_os("REPARTEE_INVITE_WEB_EVENTS") {
        std::fs::write(path, serde_json::to_vec(&events).unwrap()).unwrap();
    }
    while let Ok(row) = rx.try_recv() {
        assert!(!row.text.contains("invites you") && !row.text.contains("invited Other"));
    }
    app.cancel_connection_attempt("fixture");
    app.irc_handles.remove("fixture");
}

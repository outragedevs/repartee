use crate::app::App;

async fn until(app: &mut App, predicate: impl Fn(&App) -> bool + Send + Sync) {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            while let Ok(event) = app.irc_rx.try_recv() { app.handle_irc_event(event); }
            if predicate(app) { return; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.expect("upstream SASL fixture timed out");
}

fn stored_credentials() -> (Option<String>, Option<String>, Option<String>) {
    let db = rusqlite::Connection::open_with_flags(std::env::var("REPARTEE_SOJU_TEST_DB").unwrap(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    db.query_row("SELECT sasl_mechanism, sasl_plain_username, sasl_plain_password FROM Network WHERE id = 1", [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap()
}

async fn restart_upstream() {
    for enabled in ["false", "true"] {
        let output = tokio::time::timeout(std::time::Duration::from_secs(10),
            tokio::process::Command::new(std::env::var("REPARTEE_SOJU_TEST_CLI").unwrap())
                .arg("-config").arg(std::env::var("REPARTEE_SOJU_TEST_CONFIG").unwrap())
                .args(["user", "run", "fixture", "network", "update", "fixture", "-enabled", enabled])
                .kill_on_drop(true).output()).await.unwrap().unwrap();
        assert!(output.status.success(), "fixture network restart failed");
    }
}

#[tokio::test]
#[ignore = "requires pinned Soju and a disposable SASL upstream"]
async fn pinned_bouncer_upstream_auth() {
    let mut app = crate::app::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    let mut config: crate::config::ServerConfig = toml::from_str(
        "label='fixture'\naddress='127.0.0.1'\nport=1\ntls=true\nchannels=[]\nbouncer_network_id='1'\nsasl_mechanism='OAUTHBEARER'"
    ).unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT").unwrap().parse().unwrap();
    config.sasl_user = Some("fixture".into());
    config.sasl_pass = Some("fixture-token".into());
    let lurker = std::env::var("REPARTEE_PRESENCE_PROVIDER").unwrap() == "lurker";
    if lurker {
        config.tls = false;
        config.sasl_mechanism = Some("PLAIN".into());
        config.sasl_user = Some(std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap());
        config.sasl_pass = Some("fixture-password".into());
    }
    app.setup_connection("fixture", &config);
    app.start_connection_attempt("fixture", config);
    until(&mut app, |app| app.state.connections["fixture"].status == crate::state::connection::ConnectionStatus::Connected
        && (lurker || app.upstream_auth.get("fixture").is_some_and(|session| session.mechanisms.contains("PLAIN")))).await;
    app.state.set_active_buffer("fixture/fixture");
    if lurker {
        assert!(app.upstream_auth.get("fixture").is_none_or(|session| !session.mechanisms.contains("PLAIN")));
        app.handle_submit("/auth login -allow-insecure-upstream irc\u{ad}-account UPSTREAM_PASSWORD");
        assert!(app.upstream_auth.get("fixture").is_none_or(|session| session.pending.is_none()));
        app.irc_handles["fixture"].sender().send(irc::proto::Command::AUTHENTICATE("PLAIN".into())).unwrap();
        until(&mut app, |app| app.state.buffers.values().any(|buffer| buffer.messages.iter().any(|message| message.text.contains("Already authenticated")))).await;
        app.cancel_connection_attempt("fixture");
        app.irc_handles.remove("fixture");
        return;
    }
    app.handle_submit("/auth login -allow-insecure-upstream irc\u{ad}-account UPSTREAM_PASSWORD");
    assert!(app.upstream_auth["fixture"].pending.is_some(), "command did not start: {:?}", app.state.active_buffer().and_then(|buffer| buffer.messages.back()).map(|message| &message.text));
    until(&mut app, |app| app.upstream_auth["fixture"].pending.is_none()).await;
    assert_eq!(stored_credentials(), (Some("PLAIN".into()), Some("irc-account".into()), Some("disposable password".into())));
    assert_eq!(app.state.connections["fixture"].origin_config.sasl_pass.as_deref(), Some("fixture-token"));
    restart_upstream().await;
    until(&mut app, |app| {
        let events = std::fs::read_to_string(std::env::var("REPARTEE_PRESENCE_EVENTS").unwrap()).unwrap();
        let reauthenticated = events.lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .any(|row| row["connection"].as_u64().is_some_and(|id| id > 1) && row["upstream_auth"] == true);
        reauthenticated && app.upstream_auth.get("fixture").is_some_and(|session| session.mechanisms.contains("PLAIN"))
    }).await;
    app.handle_web_command(crate::web::protocol::WebCommand::SendMessage { buffer_id: "fixture/fixture".into(), text: "/auth login -allow-insecure-upstream irc-account WRONG_PASSWORD".into() }, "fixture-browser");
    assert!(app.upstream_auth["fixture"].pending.is_some());
    until(&mut app, |app| app.upstream_auth["fixture"].pending.is_none()).await;
    assert_eq!(stored_credentials().2.as_deref(), Some("disposable password"));
    app.handle_web_command(crate::web::protocol::WebCommand::SendMessage { buffer_id: "fixture/fixture".into(), text: "/auth clear -YES".into() }, "fixture-browser");
    assert!(app.upstream_auth["fixture"].pending.is_some());
    until(&mut app, |app| app.upstream_auth["fixture"].pending.is_none()).await;
    let credentials = stored_credentials();
    assert!(credentials.0.as_deref().is_none_or(str::is_empty));
    assert!(credentials.1.as_deref().is_none_or(str::is_empty));
    assert!(credentials.2.as_deref().is_none_or(str::is_empty));
    for buffer in app.state.buffers.values() {
        for message in &buffer.messages {
            assert!(!message.text.contains("disposable password"));
            assert!(!message.text.contains("invalid password"));
            assert!(!message.text.contains("fixture-token"));
        }
    }
    app.cancel_connection_attempt("fixture");
    app.irc_handles.remove("fixture");
}

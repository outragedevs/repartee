use crate::app::App;

async fn until(app: &mut App, predicate: impl Fn(&App) -> bool + Send + Sync) {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            while let Ok(event) = app.irc_rx.try_recv() { app.handle_irc_event(event); }
            if predicate(app) { return; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.expect("account registration fixture timed out");
}

fn saved_account() -> (String, String) {
    let db = rusqlite::Connection::open_with_flags(std::env::var("REPARTEE_SOJU_TEST_DB").unwrap(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    db.query_row("SELECT sasl_plain_username, sasl_plain_password FROM Network WHERE id = 1", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap()
}

async fn web_command(app: &mut App, text: &str) {
    app.handle_web_command(crate::web::protocol::WebCommand::SendMessage { buffer_id: "fixture/fixture".into(), text: text.into() }, "fixture-browser");
    assert!(app.has_account_registration_pending("fixture"), "account operation did not start");
    until(app, |app| !app.has_account_registration_pending("fixture")).await;
}

#[tokio::test]
#[ignore = "requires pinned bouncer and disposable account-registration upstream"]
async fn pinned_bouncer_account_registration() {
    let mut app = crate::app::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    let lurker = std::env::var("REPARTEE_PRESENCE_PROVIDER").unwrap() == "lurker";
    let mut config: crate::config::ServerConfig = toml::from_str("label='fixture'\naddress='127.0.0.1'\nport=1\ntls=true\nchannels=[]\nbouncer_network_id='1'\nsasl_mechanism='OAUTHBEARER'").unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT").unwrap().parse().unwrap();
    config.sasl_user = Some(std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap());
    config.sasl_pass = Some(if lurker { "fixture-password" } else { "fixture-token" }.into());
    if lurker { config.tls = false; config.sasl_mechanism = Some("PLAIN".into()); }
    app.setup_connection("fixture", &config);
    app.start_connection_attempt("fixture", config);
    until(&mut app, |app| app.state.connections["fixture"].status == crate::state::connection::ConnectionStatus::Connected
        && (lurker || app.state.connections["fixture"].enabled_caps.contains(super::CAP))).await;
    app.state.set_active_buffer("fixture/fixture");
    if lurker {
        assert!(!app.state.connections["fixture"].enabled_caps.contains(super::CAP));
        app.handle_submit("/account register -allow-insecure-upstream new-account user@example.org ACCOUNT_PASSWORD");
        assert!(!app.has_account_registration_pending("fixture"));
        app.cancel_connection_attempt("fixture");
        app.irc_handles.remove("fixture");
        return;
    }
    app.handle_submit("/account register -allow-insecure-upstream new-account user@example.org ACCOUNT_PASSWORD");
    assert!(app.has_account_registration_pending("fixture"));
    until(&mut app, |app| !app.has_account_registration_pending("fixture")).await;
    assert!(app.state.buffers["fixture/fixture"].messages.back().unwrap().text.contains("requires verification"));
    assert_eq!(saved_account(), ("new-account".into(), "registration-password".into()));
    web_command(&mut app, "/account verify -allow-insecure-upstream new-account WRONG_CODE").await;
    assert!(app.state.buffers["fixture/fixture"].messages.back().unwrap().text.contains("failed"));
    assert_eq!(saved_account().0, "new-account");
    web_command(&mut app, "/account verify -allow-insecure-upstream new-account VERIFY_CODE").await;
    assert!(app.state.buffers["fixture/fixture"].messages.back().unwrap().text.contains("succeeded"));
    for enabled in ["false", "true"] {
        let output = tokio::time::timeout(std::time::Duration::from_secs(10), tokio::process::Command::new(std::env::var("REPARTEE_SOJU_TEST_CLI").unwrap())
            .arg("-config").arg(std::env::var("REPARTEE_SOJU_TEST_CONFIG").unwrap())
            .args(["user", "run", "fixture", "network", "update", "fixture", "-enabled", enabled]).kill_on_drop(true).output()).await.unwrap().unwrap();
        assert!(output.status.success());
    }
    until(&mut app, |app| {
        let events = std::fs::read_to_string(std::env::var("REPARTEE_PRESENCE_EVENTS").unwrap()).unwrap();
        events.lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .any(|row| row["connection"].as_u64().is_some_and(|id| id > 1) && row["upstream_auth"] == true)
            && app.state.connections["fixture"].enabled_caps.contains(super::CAP)
    }).await;
    web_command(&mut app, "/account register -allow-insecure-upstream new-account user@example.org ACCOUNT_PASSWORD").await;
    assert!(app.state.buffers["fixture/fixture"].messages.back().unwrap().text.contains("failed"));
    web_command(&mut app, "/account register -allow-insecure-upstream instant-account user@example.org ACCOUNT_PASSWORD").await;
    assert!(app.state.buffers["fixture/fixture"].messages.back().unwrap().text.contains("succeeded"));
    assert_eq!(saved_account().0, "instant-account");
    for buffer in app.state.buffers.values() {
        for message in &buffer.messages {
            for secret in ["registration-password", "fixture-code", "wrong-code", "fixture-token"] {
                assert!(!message.text.contains(secret));
            }
        }
    }
    app.cancel_connection_attempt("fixture");
    app.irc_handles.remove("fixture");
}

use super::App;

async fn until(app: &mut App, stage: &str, predicate: impl Fn(&App) -> bool + Send + Sync) {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
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
    .unwrap_or_else(|_| panic!("service fixture timed out: {stage}"));
}

fn received(app: &App, text: &str) -> bool {
    app.state
        .buffers
        .get("fixture/bouncerserv")
        .is_some_and(|buffer| {
            buffer
                .messages
                .iter()
                .any(|row| row.nick.as_deref() == Some("BouncerServ") && row.text.contains(text))
        })
}

async fn command(app: &mut App, text: &str, reply: &str) {
    app.handle_submit(&format!("/msg BouncerServ {text}"));
    until(app, reply, |app| received(app, reply)).await;
    assert_eq!(
        app.state.buffers["fixture/bouncerserv"]
            .messages
            .iter()
            .filter(|row| row.text == text && row.nick.as_deref() != Some("BouncerServ"))
            .count(),
        1,
        "outgoing service command was duplicated or rewritten"
    );
}

async fn connection(control: bool) -> App {
    let mut app = super::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    app.web_broadcaster = std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(512));
    let mut config: crate::config::ServerConfig =
        toml::from_str("label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]")
            .unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT")
        .unwrap()
        .parse()
        .unwrap();
    config.sasl_user = Some(std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap());
    config.sasl_pass = Some("fixture-password".into());
    config.bouncer_control = control;
    if !control {
        config.bouncer_network_id = Some(std::env::var("REPARTEE_BOUNCER_TEST_NETID").unwrap());
    }
    app.setup_connection("fixture", &config);
    app.start_connection_attempt("fixture", config);
    until(&mut app, "registration", |app| {
        app.state.connections["fixture"].status
            == crate::state::connection::ConnectionStatus::Connected
    })
    .await;
    app.state.set_active_buffer("fixture/fixture");
    app
}

async fn verify_soju(app: &mut App, control: bool) {
    command(app, r"network create -name 'Native 100%; test' -addr irc+insecure://127.0.0.1:1 -enabled false -realname 'A\\B; 100%'", "created network \"Native 100%; test\"").await;
    let db = rusqlite::Connection::open_with_flags(
        std::env::var("REPARTEE_SOJU_TEST_DB").unwrap(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let realname: String = db
        .query_row(
            "SELECT realname FROM Network WHERE name = ?1",
            ["Native 100%; test"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(realname, r"A\B; 100%");
    command(
        app,
        "network status",
        "Native 100%; test (irc+insecure://127.0.0.1:1) [disabled]",
    )
    .await;
    until(app, "network context", |app| {
        received(app, "fixture (irc+insecure://127.0.0.1:")
    })
    .await;
    let status = app.state.buffers["fixture/bouncerserv"]
        .messages
        .iter()
        .find(|row| {
            row.nick.as_deref() == Some("BouncerServ")
                && row.text.starts_with("fixture (irc+insecure://127.0.0.1:")
        })
        .unwrap();
    assert_eq!(status.text.contains(", current]"), !control);
    command(
        app,
        "nonexistent-fixture-command",
        r#"error: command "nonexistent-fixture-command" not found"#,
    )
    .await;
    command(
        app,
        "network delete 'unterminated",
        "unterminated quoted string",
    )
    .await;
    command(
        app,
        "network delete 'Native 100%; test'",
        "deleted network \"Native 100%; test\"",
    )
    .await;
    let remaining: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM Network WHERE name = ?1",
            ["Native 100%; test"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0);
}

async fn verify_service_history(app: &mut App) {
    until(app, "prior service history", |app| {
        !app.state.connections["fixture"]
            .chathistory
            .any_in_flight("BouncerServ")
    })
    .await;
    let before: Vec<_> = app.state.buffers["fixture/bouncerserv"]
        .messages
        .iter()
        .map(|row| (row.id, row.text.clone()))
        .collect();
    assert!(app.request_chathistory_with_limit(
        "fixture",
        "BouncerServ",
        crate::irc::chathistory::Direction::Latest,
        None,
        100
    ));
    assert!(
        app.state.connections["fixture"]
            .chathistory
            .any_in_flight("BouncerServ")
    );
    until(app, "empty service history", |app| {
        !app.state.connections["fixture"]
            .chathistory
            .any_in_flight("BouncerServ")
    })
    .await;
    assert_eq!(
        app.state.connections["fixture"]
            .chathistory
            .last_request_succeeded("BouncerServ"),
        Some(true)
    );
    let after: Vec<_> = app.state.buffers["fixture/bouncerserv"]
        .messages
        .iter()
        .map(|row| (row.id, row.text.clone()))
        .collect();
    assert_eq!(
        before, after,
        "empty provider history changed the live service conversation"
    );
}

#[tokio::test]
#[ignore = "requires a disposable pinned bouncer fixture and Playwright WebKit"]
async fn pinned_bouncer_service() {
    let soju = std::env::var("REPARTEE_PRESENCE_PROVIDER").unwrap() == "soju";
    for control in [true, false] {
        let mut app = connection(control).await;
        let (log_tx, mut log_rx) = tokio::sync::mpsc::channel(128);
        app.state.log_tx = Some(log_tx);
        if soju {
            verify_soju(&mut app, control).await;
            verify_service_history(&mut app).await;
        } else if control {
            app.handle_submit("/msg BouncerServ control '100%; test'");
            until(&mut app, "control rejection", |app| {
                app.state.buffers.values().any(|buffer| {
                    buffer.messages.iter().any(|row| {
                        row.text.contains(
                            "Cannot interact with channels and users on the bouncer connection",
                        )
                    })
                })
            })
            .await;
        } else {
            command(
                &mut app,
                r"native '100%; test' A\B",
                r"upstream received: native '100%; test' A\B",
            )
            .await;
        }
        super::filehost_browser_fixture::run(
            &mut app,
            if soju {
                "scripts/fixtures/service-soju-browser.cjs"
            } else if control {
                "scripts/fixtures/service-control-browser.cjs"
            } else {
                "scripts/fixtures/service-forward-browser.cjs"
            },
        )
        .await;
        while let Ok(row) = log_rx.try_recv() {
            assert!(
                !row.buffer.eq_ignore_ascii_case("BouncerServ")
                    && !row.text.contains("100%")
                    && !row.text.contains("BouncerServ"),
                "service conversation reached local log queue"
            );
        }
        app.cancel_connection_attempt("fixture");
        app.irc_handles.remove("fixture");
    }
}

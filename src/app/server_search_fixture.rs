use crate::app::App;

async fn until(app: &mut App, predicate: impl Fn(&App) -> bool + Send + Sync) {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            while let Ok(event) = app.irc_rx.try_recv() { app.handle_irc_event(event); }
            if predicate(app) { return; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.expect("server search fixture timed out");
}

fn send(app: &App, wire: &str) {
    app.irc_handles["fixture"].sender().send(wire.parse::<irc::proto::Message>().unwrap()).unwrap();
}

async fn search(app: &mut App, command: &str) {
    app.handle_web_command(crate::web::protocol::WebCommand::SendMessage { buffer_id: "fixture/#search".into(), text: command.into() }, "fixture-browser");
    assert!(app.server_search.contains_key("fixture"));
    until(app, |app| !app.server_search.contains_key("fixture")).await;
}

#[tokio::test]
#[ignore = "requires pinned bouncer with disposable searchable history"]
async fn pinned_bouncer_server_search() {
    let mut app = crate::app::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    let mut config: crate::config::ServerConfig = toml::from_str("label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nbouncer_network_id='1'").unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT").unwrap().parse().unwrap();
    config.sasl_user = Some(std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap());
    config.sasl_pass = Some("fixture-password".into());
    app.setup_connection("fixture", &config);
    app.start_connection_attempt("fixture", config);
    until(&mut app, |app| app.state.connections["fixture"].status == crate::state::connection::ConnectionStatus::Connected).await;
    if std::env::var("REPARTEE_PRESENCE_PROVIDER").unwrap() == "lurker" {
        assert!(!app.state.connections["fixture"].enabled_caps.contains(super::CAP));
        app.state.set_active_buffer("fixture/fixture");
        app.handle_submit("/bsearch #search -- needle");
        assert!(!app.server_search.contains_key("fixture"));
        app.cancel_connection_attempt("fixture");
        app.irc_handles.remove("fixture");
        return;
    }
    assert!(app.state.connections["fixture"].enabled_caps.contains(super::CAP));
    assert_eq!(app.state.connections["fixture"].enabled_caps.contains("labeled-response"),
        std::env::var("REPARTEE_SEARCH_LABELS").as_deref() == Ok("1"));
    send(&app, "JOIN #search");
    until(&mut app, |app| app.state.buffers.contains_key("fixture/#search")).await;
    send(&app, "PRIVMSG FixtureControl :search-messages");
    until(&mut app, |app| app.state.buffers["fixture/#search"].messages.iter().any(|row| row.text == "needle second")).await;
    let before = app.state.buffers["fixture/#search"].messages.len();
    let (tx, mut logs) = tokio::sync::mpsc::channel(64);
    app.state.log_tx = Some(tx);
    app.state.set_active_buffer("fixture/#search");
    app.handle_submit("/bsearch #search -limit 2 -- needle");
    assert!(app.server_search.contains_key("fixture"));
    until(&mut app, |app| !app.server_search.contains_key("fixture")).await;
    let results: Vec<_> = app.state.buffers["fixture/*search*"].messages.iter().filter(|row| row.nick.is_some()).collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].text, "needle first 100%");
    assert_eq!(results[1].text, "needle second");
    assert!(results.iter().all(|row| row.tags.as_ref().unwrap().contains_key("msgid")));
    search(&mut app, "/bsearch context 1").await;
    assert!(app.state.buffers["fixture/*search*"].messages.iter().any(|row| row.text == "needle second"));
    search(&mut app, "/bsearch #search -from Alice -- needle").await;
    let results: Vec<_> = app.state.buffers["fixture/*search*"].messages.iter().filter(|row| row.nick.is_some()).collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].nick.as_deref(), Some("Alice"));
    search(&mut app, "/bsearch #search -- nonexistent-token").await;
    assert!(app.state.buffers["fixture/*search*"].messages.back().unwrap().text.contains("0 search results"));
    assert_eq!(app.state.buffers["fixture/#search"].messages.len(), before);
    send(&app, "PRIVMSG FixtureControl :search-private");
    until(&mut app, |app| app.state.connections["fixture"].nick == "renamed").await;
    search(&mut app, "/bsearch Alice -- private").await;
    let private: Vec<_> = app.state.buffers["fixture/*search*"].messages.iter().filter(|row| row.nick.is_some()).collect();
    assert_eq!(private.len(), 2, "both directions survive a nickname change");
    assert!(private.iter().any(|row| row.text == "private needle incoming"));
    assert!(private.iter().any(|row| row.text == "private needle outgoing"));
    assert!(logs.try_recv().is_err());
    if let Ok(script) = std::env::var("REPARTEE_SEARCH_BROWSER_SCRIPT") {
        app.state.remove_buffer("fixture/*search*");
        app.state.set_active_buffer("fixture/#search");
        app.web_broadcaster = std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(128));
        super::super::filehost_browser_fixture::run(&mut app, &script).await;
        assert!(logs.try_recv().is_err());
    }
    app.cancel_connection_attempt("fixture");
    app.irc_handles.remove("fixture");
}

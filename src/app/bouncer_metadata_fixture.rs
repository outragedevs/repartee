use crate::app::App;
use crate::irc::metadata::{CAP, Key};

async fn until(app: &mut App, predicate: impl Fn(&App) -> bool + Send + Sync) {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            while let Ok(event) = app.irc_rx.try_recv() { app.handle_irc_event(event); }
            app.tick_bouncer_metadata();
            if predicate(app) { return; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.expect("metadata fixture timed out");
}

fn send(app: &App, wire: &str) {
    app.irc_handles["fixture"].sender().send(wire.parse::<irc::proto::Message>().unwrap()).unwrap();
}

async fn connect() -> App {
    let mut app = crate::app::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    let mut config: crate::config::ServerConfig = toml::from_str("label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nbouncer_network_id='1'").unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT").unwrap().parse().unwrap();
    config.sasl_user = Some(std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap());
    config.sasl_pass = Some("fixture-password".into());
    app.setup_connection("fixture", &config);
    app.start_connection_attempt("fixture", config);
    until(&mut app, |app| app.state.connections["fixture"].status == crate::state::connection::ConnectionStatus::Connected).await;
    app
}

async fn subscribe(app: &mut App) {
    assert!(app.state.connections["fixture"].enabled_caps.contains(CAP));
    until(app, |app| app.bouncer_metadata.get("fixture").is_some_and(|session| session.acknowledged.len() == Key::ALL.len())).await;
}

async fn command(app: &mut App, text: &str) {
    app.state.set_active_buffer("fixture/#search");
    app.handle_submit(text);
    assert!(app.bouncer_metadata["fixture"].operation.is_some());
    until(app, |app| app.bouncer_metadata["fixture"].operation.is_none()).await;
}

#[tokio::test]
#[ignore = "requires pinned disposable bouncer metadata fixture"]
async fn pinned_bouncer_metadata() {
    let mut app = connect().await;
    if std::env::var("REPARTEE_PRESENCE_PROVIDER").unwrap() == "lurker" {
        assert!(!app.state.connections["fixture"].enabled_caps.contains(CAP));
        app.state.set_active_buffer("fixture/fixture");
        app.handle_submit("/bmeta #search");
        assert!(!app.bouncer_metadata.contains_key("fixture"));
        app.cancel_connection_attempt("fixture");
        app.irc_handles.remove("fixture");
        return;
    }
    subscribe(&mut app).await;
    send(&app, "JOIN #search");
    until(&mut app, |app| app.state.buffers.contains_key("fixture/#search")).await;
    send(&app, "PRIVMSG FixtureControl :search-messages");
    until(&mut app, |app| app.state.buffers["fixture/#search"].messages.iter().any(|row| row.text == "needle second")).await;
    let (tx, mut logs) = tokio::sync::mpsc::channel(64);
    app.state.log_tx = Some(tx);
    command(&mut app, "/bmeta #search pin on").await;
    assert!(app.state.buffers["fixture/#search"].metadata.pinned);
    command(&mut app, "/bmeta #search mute on").await;
    assert!(app.state.buffers["fixture/#search"].metadata.muted);
    command(&mut app, "/bmeta #search").await;
    let mut second = connect().await;
    subscribe(&mut second).await;
    assert!(second.state.metadata_flags("fixture", "#search").pinned);
    assert!(second.state.metadata_flags("fixture", "#search").muted);
    second.irc_handles["fixture"].sender().send(crate::irc::metadata::request(crate::irc::metadata::Request::Set("#search", Key::Pinned, Some(false))).unwrap()).unwrap();
    until(&mut app, |app| !app.state.buffers["fixture/#search"].metadata.pinned).await;
    assert!(app.state.buffers["fixture/#search"].metadata.muted);
    command(&mut app, "/bmeta #search clear").await;
    assert_eq!(app.state.buffers["fixture/#search"].metadata, crate::irc::metadata::Flags::default());
    command(&mut app, "/bmeta #missing list").await;
    command(&mut app, "/bmeta #missing pin on").await;
    assert!(app.state.metadata_flags("fixture", "#missing").pinned);
    command(&mut app, "/bmeta #missing clear").await;
    if let Ok(script) = std::env::var("REPARTEE_METADATA_BROWSER_SCRIPT") {
        app.state.set_active_buffer("fixture/#search");
        app.web_broadcaster = std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(128));
        super::super::filehost_browser_fixture::run(&mut app, &script).await;
    }
    assert!(logs.try_recv().is_err());
    for app in [&mut app, &mut second] {
        app.cancel_connection_attempt("fixture");
        app.irc_handles.remove("fixture");
    }
}

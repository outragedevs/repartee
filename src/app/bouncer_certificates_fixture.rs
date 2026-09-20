use super::*;
use crate::app::App;

async fn until(app: &mut App, predicate: impl Fn(&App) -> bool + Send + Sync) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            while let Ok(event) = app.irc_rx.try_recv() { app.handle_irc_event(event); }
            app.tick_bouncer_certificates();
            if predicate(app) { return; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("certificate fixture timed out");
}

async fn connect(config: &crate::config::ServerConfig) -> App {
    let mut app = crate::app::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    app.setup_connection("fixture", config);
    app.start_connection_attempt("fixture", config.clone());
    until(&mut app, |app| app.state.connections["fixture"].status == ConnectionStatus::Connected).await;
    app.state.set_active_buffer("fixture/fixture");
    app
}

fn disconnect(app: &mut App) {
    app.cancel_connection_attempt("fixture");
    app.irc_handles.remove("fixture");
}

async fn request(app: &mut App, text: &str) {
    app.state.buffers.get_mut("fixture/fixture").unwrap().messages.clear();
    app.handle_submit(text);
    assert!(app.bouncer_certificates.contains_key("fixture"));
    until(app, |app| !app.bouncer_certificates.contains_key("fixture")).await;
}

fn contains(app: &App, text: &str) -> bool {
    app.state.buffers["fixture/fixture"].messages.iter().any(|row| row.text.contains(text))
}

#[tokio::test]
#[ignore = "requires pinned disposable certificate-auth bouncer fixture"]
async fn pinned_bouncer_certificates() {
    let soju = std::env::var("REPARTEE_PRESENCE_PROVIDER").unwrap() == "soju";
    let mut config: crate::config::ServerConfig = toml::from_str("label='fixture'\naddress='127.0.0.1'\nport=1\ntls=true\nchannels=[]\nbouncer_control=true\nsasl_mechanism='PLAIN'").unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT").unwrap().parse().unwrap();
    config.tls = soju;
    config.sasl_user = Some(std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap());
    config.sasl_pass = Some("fixture-password".into());
    if soju { config.client_cert_path = Some(std::env::var("REPARTEE_CLIENT_CERT_TEST_PEM").unwrap()); }
    let mut app = connect(&config).await;
    assert!(app.irc_handles["fixture"].sasl_authenticated);
    if !soju {
        assert!(!app.state.connections["fixture"].enabled_caps.contains(CAP));
        app.handle_submit("/bcert list");
        assert!(!app.bouncer_certificates.contains_key("fixture"));
        disconnect(&mut app);
        return;
    }
    assert!(app.certificates_available("fixture"));
    request(&mut app, "/bcert list").await;
    assert!(contains(&app, "Pinned certificates: 0"));
    request(&mut app, "/bcert create desktop 100% ; fixture").await;
    assert!(contains(&app, "Current TLS client certificate pinned"));
    request(&mut app, "/bcert list").await;
    assert!(contains(&app, "Pinned certificates: 1"));
    assert!(contains(&app, "name=desktop 100%% ; fixture"));
    let hash = app.state.buffers["fixture/fixture"].messages.iter()
        .flat_map(|row| row.text.split_whitespace()).find(|part| fingerprint(part)).unwrap().to_string();
    if let Ok(script) = std::env::var("REPARTEE_CERTIFICATES_BROWSER_SCRIPT") {
        app.web_broadcaster = std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(128));
        super::super::filehost_browser_fixture::run(&mut app, &script).await;
    }
    disconnect(&mut app);

    let mut external = config.clone();
    external.bouncer_control = false;
    external.bouncer_network_id = Some("1".into());
    external.sasl_mechanism = Some("EXTERNAL".into());
    external.sasl_pass = None;
    let mut app = connect(&external).await;
    assert!(app.irc_handles["fixture"].sasl_authenticated);
    assert_eq!(app.state.connections["fixture"].isupport_parsed.get("BOUNCER_NETID"), Some("1"));
    request(&mut app, &format!("/bcert delete {hash}")).await;
    assert!(contains(&app, "Certificate removed"));
    disconnect(&mut app);
    let rejected = tokio::time::timeout(Duration::from_secs(20),
        crate::irc::connect_server("rejected", &external, &app.config.general)).await.unwrap();
    assert!(rejected.is_err(), "deleted certificate still authenticates");

    let mut app = connect(&config).await;
    request(&mut app, "/bcert create current").await;
    assert!(contains(&app, "Current TLS client certificate pinned"));
    request(&mut app, "/bcert delete").await;
    assert!(contains(&app, "Certificate removed"));
    request(&mut app, "/bcert list").await;
    assert!(contains(&app, "Pinned certificates: 0"));
    disconnect(&mut app);
    config.client_cert_path = None;
    let mut app = connect(&config).await;
    request(&mut app, "/bcert create missing").await;
    assert!(contains(&app, "NOCERT"));
    assert!(!contains(&app, "Current TLS client certificate pinned"));
    disconnect(&mut app);
}

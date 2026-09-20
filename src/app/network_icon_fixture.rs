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
    .unwrap_or_else(|_| panic!("network icon fixture timed out: {stage}"));
}

#[tokio::test]
#[ignore = "requires a disposable pinned bouncer fixture"]
async fn pinned_bouncer_network_icon() {
    let mut app = super::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    app.state.web_icon_extractor = Some(std::sync::Arc::new(
        crate::web::preview::WebPreviewExtractor::new(vec![1; 32], 3, 10),
    ));
    app.web_broadcaster = std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(512));
    let mut web = app.web_broadcaster.subscribe();
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
    until(&mut app, "network icon", |app| {
        app.state.connections["fixture"]
            .isupport_parsed
            .network_icon(128)
            .is_some()
    })
    .await;
    assert_eq!(
        app.state.connections["fixture"]
            .isupport_parsed
            .network_icon(128)
            .as_deref(),
        Some("https://example.org/icon/128.png")
    );
    let icon_url = crate::web::snapshot::network_icon_url(&app.state, "fixture").unwrap();
    assert!(icon_url.starts_with("/api/network-icon?h="));
    assert!(std::iter::from_fn(|| web.try_recv().ok()).any(|event| matches!(event, crate::web::protocol::WebEvent::NetworkIcon { conn_id, icon_url: Some(url) } if conn_id == "fixture" && url == icon_url)));
    app.state.set_active_buffer("fixture/fixture");
    app.handle_submit("/server icon fixture");
    assert!(
        app.state.buffers["fixture/fixture"]
            .messages
            .back()
            .unwrap()
            .text
            .contains("https://example.org/icon/128.png")
    );
    app.cancel_connection_attempt("fixture");
    app.irc_handles.remove("fixture");
}

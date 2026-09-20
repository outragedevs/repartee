use crate::state::connection::ConnectionStatus;

#[tokio::test]
#[ignore = "requires a disposable pinned bouncer TLS fixture"]
async fn pinned_bouncer_tls_validation() {
    let case = std::env::var("REPARTEE_BOUNCER_TLS_CASE").unwrap();
    assert!(matches!(
        case.as_str(),
        "valid" | "untrusted" | "wrong-host"
    ));
    let valid = case == "valid";
    let mut app = super::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    app.web_broadcaster = std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(512));
    let mut web = app.web_broadcaster.subscribe();
    let mut config: crate::config::ServerConfig = toml::from_str(
        "label='fixture'\naddress='127.0.0.1'\nport=1\ntls=true\ntls_verify=true\nchannels=[]\nauto_reconnect=false\nsasl_mechanism='PLAIN'",
    ).unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT")
        .unwrap()
        .parse()
        .unwrap();
    let network = std::env::var("REPARTEE_BOUNCER_TEST_NETID").unwrap();
    config.bouncer_network_id = Some(network.clone());
    config.sasl_user = Some(std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap());
    config.sasl_pass = Some("fixture-password".into());
    app.setup_connection("fixture", &config);
    app.start_connection_attempt("fixture", config);
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            while let Ok(event) = app.irc_rx.try_recv() {
                app.handle_irc_event(event);
                if !valid {
                    assert!(!app.irc_handles.contains_key("fixture"));
                    assert_ne!(
                        app.state.connections["fixture"].status,
                        ConnectionStatus::Connected
                    );
                }
            }
            let connection = &app.state.connections["fixture"];
            if valid
                && connection.status == ConnectionStatus::Connected
                && connection.isupport_parsed.get("BOUNCER_NETID") == Some(network.as_str())
            {
                assert!(app.irc_handles["fixture"].sasl_authenticated);
                return;
            }
            if !valid && let Some(error) = &connection.error {
                let expected = if case == "untrusted" {
                    "UnknownIssuer"
                } else {
                    "certificate not valid for name"
                };
                assert!(error.contains(expected), "expected {expected}, got {error}");
                assert!(!error.contains("fixture-password"));
                assert!(
                    app.state.buffers["fixture/fixture"]
                        .messages
                        .iter()
                        .any(|message| message.text.contains(expected))
                );
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("TLS fixture did not reach its expected terminal state");
    app.drain_pending_web_events();
    let events: Vec<_> = std::iter::from_fn(|| web.try_recv().ok()).collect();
    if !valid {
        let expected = if case == "untrusted" {
            "UnknownIssuer"
        } else {
            "certificate not valid for name"
        };
        assert!(events.iter().any(|event| matches!(event, crate::web::protocol::WebEvent::NewMessage { message, .. } if message.text.contains(expected))));
    }
    let connected_events = events.into_iter().filter(|event| {
        matches!(event, crate::web::protocol::WebEvent::ConnectionStatus { conn_id, connected: true, .. } if conn_id == "fixture")
    }).count();
    assert_eq!(connected_events > 0, valid);
    app.cancel_connection_attempt("fixture");
    app.irc_handles.remove("fixture");
}

#[tokio::test]
#[ignore = "requires a disposable pinned Soju OAuth fixture"]
async fn pinned_bouncer_oauthbearer() {
    use crate::state::connection::ConnectionStatus;
    let mut app = super::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    let mut config: crate::config::ServerConfig = toml::from_str(
        "label='fixture'\naddress='127.0.0.1'\nport=1\ntls=true\nchannels=[]\nbouncer_network_id='1'\nsasl_mechanism='OAUTHBEARER'"
    ).unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT").unwrap().parse().unwrap();
    config.sasl_user = Some("fixture".into());
    config.sasl_pass = Some("fixture-token".into());
    for _ in 0..2 {
        app.setup_connection("fixture", &config);
        app.start_connection_attempt("fixture", config.clone());
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                while let Ok(event) = app.irc_rx.try_recv() { app.handle_irc_event(event); }
                let connection = &app.state.connections["fixture"];
                if connection.status == ConnectionStatus::Connected {
                    assert_eq!(connection.isupport_parsed.get("BOUNCER_NETID"), Some("1"));
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }).await.expect("OAuth network binding timed out");
        app.cancel_connection_attempt("fixture");
        app.irc_handles.remove("fixture");
    }
    for (user, token) in [("fixture", "invalid-fixture-token"), ("another-user", "fixture-token")] {
        config.sasl_user = Some(user.into());
        config.sasl_pass = Some(token.into());
        let result = tokio::time::timeout(std::time::Duration::from_secs(20),
            crate::irc::connect_server("rejected", &config, &app.config.general)).await.unwrap();
        let error = match result {
            Ok(_) => panic!("invalid OAuth credentials accepted"),
            Err(error) => error.to_string(),
        };
        assert!(!error.contains(token));
    }
}

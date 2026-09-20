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
    .expect("Bouncer NAMES did not populate the channel");
}

fn populated(app: &App) -> bool {
    ["#one", "#two"].iter().all(|channel| {
        app.state
            .buffers
            .get(&format!("fixture/{channel}"))
            .is_some_and(|buffer| {
                buffer
                    .users
                    .values()
                    .any(|nick| nick.nick == "Alice" && nick.prefix == "@")
                    && buffer.users.values().any(|nick| nick.nick == "Bob")
            })
    })
}

#[tokio::test]
#[ignore = "requires a disposable pinned bouncer fixture"]
async fn pinned_bouncer_names() {
    let mut app = crate::app::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    let mut config: crate::config::ServerConfig = toml::from_str("label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nbouncer_network_id='1'").unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT")
        .unwrap()
        .parse()
        .unwrap();
    config.sasl_user = Some(std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap());
    config.sasl_pass = Some("fixture-password".into());
    app.setup_connection("fixture", &config);
    app.state.set_active_buffer("fixture/fixture");
    app.start_connection_attempt("fixture", config.clone());
    until(&mut app, |app| {
        app.state.connections["fixture"].status
            == crate::state::connection::ConnectionStatus::Connected
    })
    .await;
    let soju = std::env::var("REPARTEE_PRESENCE_PROVIDER").unwrap() == "soju";
    for cap in crate::irc::names::CAPABILITIES {
        assert_eq!(
            app.state.connections["fixture"].enabled_caps.contains(*cap),
            soju
        );
    }
    app.irc_handles["fixture"]
        .sender()
        .send(irc::proto::Command::JOIN("#one,#two".into(), None, None))
        .unwrap();
    until(&mut app, populated).await;
    app.irc_handles["fixture"]
        .sender()
        .send_quit("fixture reconnect")
        .unwrap();
    until(&mut app, |app| {
        app.state.connections["fixture"].status
            == crate::state::connection::ConnectionStatus::Disconnected
    })
    .await;
    assert!(!populated(&app));
    app.start_connection_attempt("fixture", config);
    until(&mut app, populated).await;
    for channel in ["#one", "#two"] {
        let crate::web::protocol::WebEvent::NickList { nicks, .. } =
            crate::web::snapshot::build_nick_list(&app.state, &format!("fixture/{channel}"))
                .unwrap()
        else {
            panic!("expected nicklist");
        };
        assert!(
            nicks
                .iter()
                .any(|nick| nick.nick == "Alice" && nick.prefix == "@")
        );
        assert!(nicks.iter().any(|nick| nick.nick == "Bob"));
    }
    app.cancel_connection_attempt("fixture");
    app.irc_handles.remove("fixture");
}

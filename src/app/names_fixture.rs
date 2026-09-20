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
    .unwrap_or_else(|_| {
        panic!(
            "Bouncer NAMES stage {stage} timed out; capabilities: {:?}",
            app.state.connections["fixture"].enabled_caps
        )
    });
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

async fn labeled_names(app: &mut App) {
    until(app, "labeled-response ACK", |app| {
        app.state.connections["fixture"]
            .enabled_caps
            .contains("labeled-response")
    })
    .await;
    app.state
        .buffers
        .get_mut("fixture/#one")
        .unwrap()
        .users
        .clear();
    app.channel_query_in_flight.remove("fixture");
    app.channel_query_queues.remove("fixture");
    let request: irc::proto::Message = "@label=fixture-names NAMES #one".parse().unwrap();
    app.irc_handles["fixture"].sender().send(request).unwrap();
    let mut saw_labeled_batch = false;
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            while let Ok(event) = app.irc_rx.try_recv() {
                let mut payload = &event;
                while let crate::irc::IrcEvent::Attempt(_, _, inner) = payload {
                    payload = inner;
                }
                if let crate::irc::IrcEvent::Message(_, message) = payload {
                    saw_labeled_batch |= matches!(&message.command, irc::proto::Command::BATCH(_, Some(kind), _) if kind.to_str().eq_ignore_ascii_case("labeled-response"))
                        && message.tags.as_ref().is_some_and(|tags| tags.iter().any(|tag| tag.0 == "label" && tag.1.as_deref() == Some("fixture-names")));
                }
                app.handle_irc_event(event);
            }
            if saw_labeled_batch && populated(app) && app.channel_query_in_flight.contains_key("fixture") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.expect("labeled NAMES did not run the App completion hooks");
}

async fn labeled_whois(app: &mut App) {
    let other =
        toml::from_str("label='other'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]")
            .unwrap();
    app.setup_connection("other", &other);
    app.state.set_active_buffer("fixture/#one");
    let (tx, mut log_rx) = tokio::sync::mpsc::channel(64);
    app.state.log_tx = Some(tx);
    app.web_broadcaster = std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(128));
    let mut web_rx = app.web_broadcaster.subscribe();
    app.execute_command(&crate::commands::parser::parse_command("/whois Alice").unwrap());
    assert_eq!(app.labeled_requests["fixture"].pending_count(), 1);
    app.state.set_active_buffer("other/other");
    until(app, "labeled WHOIS origin", |app| {
        app.state.buffers["fixture/#one"]
            .messages
            .iter()
            .any(|message| message.text.contains("fixture-labeled-whois"))
    })
    .await;
    assert_eq!(app.state.active_buffer_id.as_deref(), Some("other/other"));
    assert_eq!(app.labeled_requests["fixture"].pending_count(), 0);
    assert!(
        !app.state.buffers["other/other"]
            .messages
            .iter()
            .any(|message| message.text.contains("fixture-labeled-whois"))
    );
    let events: Vec<_> = std::iter::from_fn(|| web_rx.try_recv().ok()).collect();
    assert!(events.iter().any(|event| matches!(event, crate::web::protocol::WebEvent::NewMessage { buffer_id, message } if buffer_id == "fixture/#one" && message.text.contains("fixture-labeled-whois"))));
    assert!(log_rx.try_recv().is_err());
    app.state.log_tx = None;
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
    until(&mut app, "transport transition", |app| {
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
    until(&mut app, "channel population", populated).await;
    if soju {
        labeled_names(&mut app).await;
        labeled_whois(&mut app).await;
    }
    app.irc_handles["fixture"]
        .sender()
        .send_quit("fixture reconnect")
        .unwrap();
    until(&mut app, "transport transition", |app| {
        app.state.connections["fixture"].status
            == crate::state::connection::ConnectionStatus::Disconnected
    })
    .await;
    assert!(!populated(&app));
    app.start_connection_attempt("fixture", config);
    until(&mut app, "channel population", populated).await;
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

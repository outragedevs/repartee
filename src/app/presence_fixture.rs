use std::path::Path;
use std::time::{Duration, Instant};

use crossterm::event::Event;

use super::App;
use crate::web::protocol::WebCommand;

fn client() -> App {
    let mut app = crate::app::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    let mut config: crate::config::ServerConfig = toml::from_str(
        "label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nbouncer_network_id='1'",
    ).unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT")
        .unwrap()
        .parse()
        .unwrap();
    config.nick = Some("fixture".into());
    config.sasl_user = Some(std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap());
    config.sasl_pass = Some("fixture-password".into());
    app.setup_connection("fixture", &config);
    app.state.set_active_buffer("fixture/fixture");
    app.start_connection_attempt("fixture", config);
    app
}

fn pump(apps: &mut [&mut App]) {
    for app in apps {
        while let Ok(event) = app.irc_rx.try_recv() {
            app.handle_irc_event(event);
        }
        app.tick_bouncer_presence();
    }
}

fn events(path: &Path) -> Vec<Option<String>> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .map(|row| row["away"].as_str().map(str::to_string))
        })
        .collect()
}

async fn settle(apps: &mut [&mut App], duration: Duration) {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        pump(apps);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn connected(apps: &mut [&mut App]) {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        pump(apps);
        if apps.iter().all(|app| {
            app.state.connections["fixture"].status
                == crate::state::connection::ConnectionStatus::Connected
        }) {
            for app in apps.iter() {
                assert!(
                    app.state.connections["fixture"]
                        .enabled_caps
                        .contains("draft/pre-away")
                );
            }
            return;
        }
        assert!(
            Instant::now() < until,
            "App did not connect to the real bouncer"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn expect(apps: &mut [&mut App], path: &Path, expected: Option<&str>) {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        pump(apps);
        let rows = events(path);
        if rows.last().is_some_and(|away| away.as_deref() == expected) {
            return;
        }
        assert!(
            Instant::now() < until,
            "Expected upstream away {expected:?}, got {rows:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
#[ignore = "requires disposable pinned bouncers and an IRC upstream"]
async fn pinned_bouncer_presence() {
    let path = std::path::PathBuf::from(std::env::var("REPARTEE_PRESENCE_EVENTS").unwrap());
    let lurker = std::env::var("REPARTEE_PRESENCE_PROVIDER").unwrap() == "lurker";
    let auto = std::env::var("REPARTEE_PRESENCE_AUTO_AWAY").unwrap() == "true";
    let mut first = client();
    let mut second = client();
    connected(&mut [&mut first, &mut second]).await;
    settle(&mut [&mut first, &mut second], Duration::from_millis(1400)).await;
    let initial = events(&path);
    if auto {
        assert!(
            initial.last().unwrap().is_some(),
            "background clients prevented auto-away: {initial:?}"
        );
        assert!(
            initial.iter().skip(1).all(Option::is_some),
            "background registration became present: {initial:?}"
        );
    } else {
        assert_eq!(initial, [None]);
    }
    first.terminal =
        Some(crate::ui::setup_socket_terminal(Box::new(std::io::sink()), 120, 40).unwrap());
    first.handle_event(Event::FocusGained);
    expect(&mut [&mut first, &mut second], &path, None).await;
    second.handle_web_command(
        WebCommand::WebConnect {
            initial_buffer_id: None,
        },
        "browser",
    );
    second.handle_web_command(WebCommand::Presence { present: true }, "browser");
    settle(&mut [&mut first, &mut second], Duration::from_millis(100)).await;
    let present_start = events(&path).len();
    first.handle_event(Event::FocusLost);
    settle(&mut [&mut first, &mut second], Duration::from_millis(1400)).await;
    assert!(events(&path)[present_start..].iter().all(Option::is_none));
    second.handle_web_command(WebCommand::Presence { present: false }, "browser");
    settle(&mut [&mut first, &mut second], Duration::from_millis(1400)).await;
    if auto {
        assert!(events(&path).last().unwrap().is_some());
    } else {
        assert_eq!(events(&path), [None]);
    }

    first.execute_command(
        &crate::commands::parser::parse_command("/away manual-presence-fixture").unwrap(),
    );
    let manual = if !auto {
        None
    } else if lurker {
        Some("manual-presence-fixture")
    } else {
        Some("Auto away")
    };
    expect(&mut [&mut first, &mut second], &path, manual).await;
    first.handle_event(Event::FocusGained);
    settle(&mut [&mut first, &mut second], Duration::from_millis(1400)).await;
    assert_eq!(events(&path).last().unwrap().as_deref(), manual);
    let config = first.state.connections["fixture"].origin_config.clone();
    first.handle_irc_event(crate::irc::IrcEvent::Disconnected("fixture".into(), None));
    first.start_connection_attempt("fixture", config);
    connected(&mut [&mut first, &mut second]).await;
    settle(&mut [&mut first, &mut second], Duration::from_millis(1400)).await;
    assert_eq!(events(&path).last().unwrap().as_deref(), manual);
    first.execute_command(&crate::commands::parser::parse_command("/away").unwrap());
    expect(&mut [&mut first, &mut second], &path, None).await;
    first.terminal = None;
    first.handle_event(Event::FocusLost);
    settle(&mut [&mut first, &mut second], Duration::from_millis(1400)).await;
    if auto {
        assert!(events(&path).last().unwrap().is_some());
    } else {
        assert_eq!(events(&path), [None]);
    }
    for app in [&mut first, &mut second] {
        app.handle_irc_event(crate::irc::IrcEvent::Disconnected("fixture".into(), None));
    }
}

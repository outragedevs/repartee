use irc::client::{Client, Sender, data::Config};
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};

struct Wire {
    sender: Sender,
    lines: Lines<BufReader<TcpStream>>,
    task: JoinHandle<irc::error::Result<()>>,
}

impl Drop for Wire {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Wire {
    async fn connect(threshold: u32) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = Client::from_config(Config {
            nickname: Some("test".into()),
            server: Some("127.0.0.1".into()),
            port: Some(listener.local_addr().unwrap().port()),
            use_tls: Some(false),
            flood_penalty_threshold: Some(threshold),
            ..Config::default()
        })
        .await
        .unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        Self {
            sender: client.sender(),
            lines: BufReader::new(socket).lines(),
            task: tokio::spawn(client.outgoing().unwrap()),
        }
    }

    fn send(&self, text: &str) {
        self.sender.send_privmsg("#test", text).unwrap();
    }

    async fn expect(&mut self, text: &str) {
        let line = timeout(Duration::from_secs(1), self.lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let message: irc::proto::Message = line.parse().unwrap();
        assert_eq!(
            message.command,
            irc::proto::Command::PRIVMSG("#test".into(), text.into())
        );
    }

    async fn expect_delayed(&mut self) {
        assert!(
            timeout(Duration::from_millis(120), self.lines.next_line())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn flood_bypass_wakes_delayed_writer_and_restores_configured_limit() {
    let mut wire = Wire::connect(1_000).await;
    wire.send("delayed");
    wire.expect_delayed().await;
    let clone = wire.sender.clone();
    clone.set_flood_protection_enabled(false);
    wire.expect("delayed").await;
    for i in 0..12 {
        wire.send(&format!("burst {i}"));
    }
    for i in 0..12 {
        wire.expect(&format!("burst {i}")).await;
    }
    clone.set_flood_protection_enabled(true);
    wire.send("limited again");
    wire.expect_delayed().await;
    wire.sender.set_flood_protection_enabled(false);
    wire.expect("limited again").await;
}

#[tokio::test]
async fn flood_bypass_is_connection_local_and_preserves_disabled_config() {
    let mut limited = Wire::connect(1_000).await;
    let mut disabled = Wire::connect(0).await;
    disabled.sender.set_flood_protection_enabled(false);
    disabled.sender.set_flood_protection_enabled(true);
    limited.send("still limited");
    for i in 0..12 {
        disabled.send(&format!("unlimited {i}"));
    }
    for i in 0..12 {
        disabled.expect(&format!("unlimited {i}")).await;
    }
    limited.expect_delayed().await;
    limited.sender.set_flood_protection_enabled(false);
    limited.expect("still limited").await;
}

#[tokio::test]
async fn latest_flood_state_wins_before_the_writer_runs() {
    let mut wire = Wire::connect(1_000).await;
    wire.send("initial delay");
    wire.expect_delayed().await;
    wire.sender.set_flood_protection_enabled(false);
    wire.send("queued while disabled");
    wire.sender.set_flood_protection_enabled(true);
    wire.expect_delayed().await;
    wire.sender.set_flood_protection_enabled(false);
    wire.expect("initial delay").await;
    wire.expect("queued while disabled").await;
}

#[tokio::test]
async fn saferate_updates_real_transport_only_after_valid_isupport() {
    let mut wire = Wire::connect(1_000).await;
    let sender = crate::irc::IrcSender::new(wire.sender.clone(), 1_000);
    let mut app = crate::app::input::submit_typing_tests::test_app();
    let config = toml::from_str(
        "label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nnick='me'",
    )
    .unwrap();
    app.setup_connection("fixture", &config);
    app.irc_handles.insert(
        "fixture".into(),
        crate::irc::IrcHandle::new("fixture".into(), sender.clone(), None, None),
    );
    let receive = |app: &mut crate::app::App, text: &str| {
        app.handle_irc_event(crate::irc::IrcEvent::Message(
            "fixture".into(),
            Box::new(text.parse().unwrap()),
        ));
    };
    sender.send_privmsg("#test", "waiting for batch").unwrap();
    wire.expect_delayed().await;
    receive(&mut app, ":s BATCH +safe draft/isupport");
    receive(
        &mut app,
        "@batch=safe :s 005 me soju.im/SAFERATE :supported tokens",
    );
    wire.expect_delayed().await;
    assert!(!sender.has_typing_headroom_at(std::time::Instant::now()));
    receive(&mut app, ":s BATCH -safe");
    assert!(sender.has_typing_headroom_at(std::time::Instant::now()));
    wire.expect("waiting for batch").await;
    receive(&mut app, ":s 005 me -soju.im/SAFERATE :supported tokens");
    assert!(!sender.has_typing_headroom_at(std::time::Instant::now()));
    sender
        .send_privmsg("#test", "limited after removal")
        .unwrap();
    wire.expect_delayed().await;
    for invalid in ["soju.im/SAFERATE=", "soju.im/SAFERATE=yes"] {
        receive(&mut app, &format!(":s 005 me {invalid} :supported tokens"));
        wire.expect_delayed().await;
    }
    receive(&mut app, ":s 005 me soju.im/SAFERATE :supported tokens");
    wire.expect("limited after removal").await;
}

#[test]
fn saferate_requires_a_bare_token_and_preserves_other_isupport_keys() {
    let mut support = crate::irc::isupport::Isupport::new();
    for (token, enabled) in [
        ("soju.im/SAFERATE", true),
        ("soju.im/SAFERATE=", false),
        ("soju.im/SAFERATE", true),
        ("soju.im/SAFERATE=yes", false),
        ("soju.im/SAFERATE", true),
        ("-soju.im/SAFERATE", false),
    ] {
        support.parse_tokens(&["NETWORK=fixture", token]);
        assert_eq!(support.has_saferate(), enabled);
        assert_eq!(support.network(), Some("fixture"));
    }
}

#[tokio::test]
#[ignore = "requires scripts/test_bouncer_binding.py with a pinned provider"]
async fn pinned_bouncer_saferate() {
    let mut app = crate::app::input::submit_typing_tests::test_app();
    let mut config: crate::config::ServerConfig = toml::from_str(
        "label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nnick='me'",
    )
    .unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT")
        .unwrap()
        .parse()
        .unwrap();
    config.bouncer_network_id = Some(std::env::var("REPARTEE_BOUNCER_TEST_NETID").unwrap());
    config.sasl_user = Some(std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap());
    config.sasl_pass = Some("fixture-password".into());
    let soju = std::env::var("REPARTEE_BOUNCER_TEST_PROVIDER").unwrap() == "soju";
    app.setup_connection("fixture", &config);
    let general = crate::config::GeneralConfig {
        flood_protection: true,
        ..Default::default()
    };
    let (handle, mut events) = crate::irc::connect_server("fixture", &config, &general)
        .await
        .unwrap();
    let sender = handle.sender().clone();
    app.irc_handles.insert("fixture".into(), handle);
    timeout(Duration::from_secs(15), async {
        while let Some(event) = events.recv().await {
            let complete = matches!(&event, crate::irc::IrcEvent::Message(_, message) if matches!(&message.command, irc::proto::Command::Response(irc::proto::Response::RPL_ENDOFMOTD | irc::proto::Response::ERR_NOMOTD, _)));
            app.handle_irc_event(event);
            if complete { return; }
        }
        panic!("provider disconnected before registration completed");
    }).await.unwrap();
    assert_eq!(
        app.state.connections["fixture"]
            .isupport_parsed
            .has_saferate(),
        soju
    );
    for i in 0..6 {
        sender
            .send(irc::proto::Command::PING(
                format!("saferate-fixture-{i}"),
                None,
            ))
            .unwrap();
    }
    let started = tokio::time::Instant::now();
    let mut responses = std::collections::HashSet::new();
    timeout(Duration::from_secs(25), async {
        while let Some(event) = events.recv().await {
            if let crate::irc::IrcEvent::Message(_, message) = &event
                && let irc::proto::Command::PONG(first, second) = &message.command
            {
                for value in [Some(first.as_str()), second.as_deref()]
                    .into_iter()
                    .flatten()
                {
                    if value.starts_with("saferate-fixture-") {
                        responses.insert(value.to_owned());
                    }
                }
            }
            app.handle_irc_event(event);
            if responses.len() == 6 {
                return;
            }
        }
        panic!("provider disconnected before returning every PONG");
    })
    .await
    .unwrap();
    if soju {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "SAFERATE burst was throttled: {:?}",
            started.elapsed()
        );
    } else {
        assert!(
            started.elapsed() >= Duration::from_secs(2),
            "provider without SAFERATE bypassed configured throttling"
        );
    }
}

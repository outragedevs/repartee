use std::time::Duration;

use base64::Engine as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::TcpListener;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

use super::normalize_network_id;
use crate::config::{GeneralConfig, ServerConfig};
use crate::irc::{IrcEvent, connect_server};

async fn next_command(
    lines: &mut Lines<BufReader<OwnedReadHalf>>,
    write: &mut OwnedWriteHalf,
) -> Option<String> {
    while let Some(line) = lines.next_line().await.unwrap() {
        if let Some(token) = line.strip_prefix("PING ") {
            write
                .write_all(format!("PONG {token}\r\n").as_bytes())
                .await
                .unwrap();
        } else {
            return Some(line);
        }
    }
    None
}

#[derive(Clone, Copy, Debug)]
enum Reply {
    Success,
    ControlSuccess,
    ControlUnexpectedBound,
    NoCapability,
    Nak,
    AuthenticationFailure,
    BindingFailure,
    WrongNetwork,
    MissingNetwork,
    Disconnect,
    CancelNegotiation,
    CancelConfirmation,
}

#[expect(
    clippy::too_many_lines,
    reason = "scripted TCP registration with success and failure replies"
)]
async fn registration(reply: Reply, bound: bool) {
    let control = matches!(reply, Reply::ControlSuccess | Reply::ControlUnexpectedBound);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (read, mut write) = socket.into_split();
        let mut lines = BufReader::new(read).lines();
        assert_eq!(
            next_command(&mut lines, &mut write).await.unwrap(),
            "CAP LS 302"
        );
        assert!(
            lines
                .next_line()
                .await
                .unwrap()
                .unwrap()
                .starts_with("NICK ")
        );
        assert!(
            lines
                .next_line()
                .await
                .unwrap()
                .unwrap()
                .starts_with("USER ")
        );
        if matches!(reply, Reply::CancelNegotiation) {
            cancel_tx.send(()).unwrap();
            assert!(next_command(&mut lines, &mut write).await.is_none());
            return;
        }
        let advertised = if matches!(reply, Reply::NoCapability) {
            "sasl=PLAIN"
        } else {
            "sasl=PLAIN batch soju.im/bouncer-networks soju.im/bouncer-networks-notify draft/pre-away"
        };
        write
            .write_all(format!(":fixture CAP * LS :{advertised}\r\n").as_bytes())
            .await
            .unwrap();
        if matches!(reply, Reply::NoCapability) {
            assert!(next_command(&mut lines, &mut write).await.is_none());
            return;
        }
        let request = next_command(&mut lines, &mut write).await.unwrap();
        assert!(request.starts_with("CAP REQ "));
        let requested = request
            .trim_start_matches("CAP REQ ")
            .trim_start_matches(':');
        assert_eq!(requested.contains(super::NETWORKS_CAP), bound || control);
        assert_eq!(requested.contains(super::NETWORKS_NOTIFY_CAP), control);
        assert_eq!(requested.contains("draft/pre-away"), bound && !control);
        let ack = if matches!(reply, Reply::Nak) {
            "NAK"
        } else {
            "ACK"
        };
        write
            .write_all(format!(":fixture CAP * {ack} :{requested}\r\n").as_bytes())
            .await
            .unwrap();
        if matches!(reply, Reply::Nak) {
            assert!(next_command(&mut lines, &mut write).await.is_none());
            return;
        }
        assert_eq!(
            next_command(&mut lines, &mut write).await.unwrap(),
            "AUTHENTICATE PLAIN"
        );
        write.write_all(b"AUTHENTICATE +\r\n").await.unwrap();
        let payload = next_command(&mut lines, &mut write).await.unwrap();
        let encoded = payload.strip_prefix("AUTHENTICATE ").unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap(),
            b"fixture\0fixture\0fixture-password"
        );
        if matches!(reply, Reply::AuthenticationFailure) {
            write
                .write_all(b":fixture 904 tester :Authentication failed\r\n")
                .await
                .unwrap();
            while let Some(line) = next_command(&mut lines, &mut write).await {
                assert_eq!(line, "AUTHENTICATE *");
            }
            return;
        }
        write
            .write_all(b":fixture 903 tester :Authentication successful\r\n")
            .await
            .unwrap();
        if bound {
            assert_eq!(
                next_command(&mut lines, &mut write).await.unwrap(),
                "BOUNCER BIND 42"
            );
        }
        if bound && !control {
            let away: irc::proto::Message = next_command(&mut lines, &mut write).await.unwrap().parse().unwrap();
            assert_eq!(away.command, irc::proto::Command::AWAY(Some("*".into())));
        }
        assert_eq!(
            next_command(&mut lines, &mut write).await.unwrap(),
            "CAP END"
        );
        if matches!(reply, Reply::CancelConfirmation) {
            cancel_tx.send(()).unwrap();
            assert!(next_command(&mut lines, &mut write).await.is_none());
            return;
        }
        if matches!(reply, Reply::Disconnect) {
            return;
        }
        let burst = match reply {
            Reply::BindingFailure => {
                ":fixture FAIL BOUNCER INVALID_NETID BIND 42 :Unknown network\r\n"
            }
            Reply::WrongNetwork => {
                ":fixture 001 tester :Welcome\r\n:fixture 005 tester BOUNCER_NETID=43 :supported\r\n:fixture 422 tester :No MOTD\r\n"
            }
            Reply::MissingNetwork | Reply::ControlSuccess => {
                ":fixture 001 tester :Welcome\r\n:fixture 005 tester NETWORK=fixture :supported\r\n:fixture 422 tester :No MOTD\r\n"
            }
            _ => {
                ":fixture 001 tester :Welcome\r\n:fixture 005 tester NETWORK=fixture BOUNCER_NETID=42 :supported\r\n:fixture 422 tester :No MOTD\r\n"
            }
        };
        write.write_all(burst.as_bytes()).await.unwrap();
        if matches!(reply, Reply::Success | Reply::ControlSuccess) {
            if bound || control {
                assert!(
                    tokio::time::timeout(
                        Duration::from_millis(100),
                        next_command(&mut lines, &mut write)
                    )
                    .await
                    .is_err()
                );
            } else {
                assert_eq!(
                    next_command(&mut lines, &mut write).await.unwrap(),
                    "JOIN #configured"
                );
            }
        } else {
            assert!(next_command(&mut lines, &mut write).await.is_none());
        }
    });
    let mut server: ServerConfig = toml::from_str("label = 'fixture'\naddress = '127.0.0.1'\nport = 6667\ntls = false\nchannels = ['#configured']\n").unwrap();
    server.port = port;
    server.nick = Some("tester".into());
    server.sasl_user = Some("fixture".into());
    server.sasl_pass = Some("fixture-password".into());
    server.bouncer_control = control;
    server.bouncer_network_id = bound.then(|| "00042".into());
    let general = GeneralConfig {
        flood_protection: false,
        ..GeneralConfig::default()
    };
    let mut attempt = Box::pin(connect_server("fixture", &server, &general));
    if matches!(reply, Reply::CancelNegotiation | Reply::CancelConfirmation) {
        tokio::select! {
            _ = &mut attempt => panic!("registration completed before cancellation"),
            ready = cancel_rx => ready.unwrap(),
        }
        drop(attempt);
        peer.await.unwrap();
        return;
    }
    let result = attempt.await;
    if matches!(reply, Reply::Success | Reply::ControlSuccess) {
        let (handle, mut events) = result.unwrap();
        let mut connected = false;
        let mut confirmed = false;
        while let Some(event) = events.recv().await {
            match event {
                IrcEvent::Connected(_, caps, _) => {
                    assert!(!connected);
                    assert_eq!(caps.contains(super::NETWORKS_CAP), bound || control);
                    connected = true;
                }
                IrcEvent::Message(_, message) => {
                    confirmed |= message.to_string().contains("BOUNCER_NETID=42");
                }
                IrcEvent::Disconnected(..) => break,
                _ => {}
            }
        }
        assert!(connected);
        assert_eq!(confirmed, !control);
        drop(handle);
    } else {
        assert!(result.is_err(), "{reply:?} unexpectedly connected");
    }
    peer.await.unwrap();
}

#[tokio::test]
async fn bind_follows_sasl_and_is_confirmed_before_connecting() {
    for _ in 0..2 {
        tokio::time::timeout(Duration::from_secs(5), registration(Reply::Success, true))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn direct_irc_keeps_autojoin_and_does_not_request_network_binding() {
    tokio::time::timeout(Duration::from_secs(5), registration(Reply::Success, false))
        .await
        .unwrap();
}

#[tokio::test]
async fn binding_failures_close_the_socket_without_connecting() {
    for reply in [
        Reply::NoCapability,
        Reply::Nak,
        Reply::AuthenticationFailure,
        Reply::BindingFailure,
        Reply::WrongNetwork,
        Reply::MissingNetwork,
        Reply::Disconnect,
    ] {
        tokio::time::timeout(Duration::from_secs(5), registration(reply, true))
            .await
            .expect("registration stalled");
    }
}

#[test]
fn network_ids_are_validated_as_single_positive_protocol_parameters() {
    assert_eq!(normalize_network_id("00042").unwrap(), "42");
    for value in [
        "",
        "0",
        "-1",
        "+1",
        "42 43",
        "42\r\nQUIT",
        "abc",
        "9223372036854775808",
    ] {
        assert!(normalize_network_id(value).is_err(), "{value:?}");
    }
}

#[tokio::test]
#[ignore = "requires a disposable local bouncer fixture; see docs/validation/bouncer-binding.md"]
async fn pinned_bouncer_registration() {
    let port: u16 = std::env::var("REPARTEE_BOUNCER_TEST_PORT")
        .unwrap()
        .parse()
        .unwrap();
    let network_id = std::env::var("REPARTEE_BOUNCER_TEST_NETID").unwrap();
    let user = std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap();
    let mut config: ServerConfig = toml::from_str(
        "label = 'fixture'\naddress = '127.0.0.1'\nport = 6667\ntls = false\nchannels = []\n",
    )
    .unwrap();
    config.port = port;
    config.bouncer_network_id = Some(network_id.clone());
    config.sasl_user = Some(user);
    config.sasl_pass = Some("fixture-password".into());
    let general = GeneralConfig {
        flood_protection: false,
        ..GeneralConfig::default()
    };
    for _ in 0..2 {
        let (handle, mut events) = tokio::time::timeout(
            Duration::from_secs(5),
            connect_server("fixture", &config, &general),
        )
        .await
        .unwrap()
        .unwrap();
        let mut connected = false;
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(event) = events.recv().await {
                match event {
                    IrcEvent::Connected(_, caps, _) => {
                        assert!(caps.contains(super::NETWORKS_CAP));
                        connected = true;
                    }
                    IrcEvent::Message(_, message)
                        if message
                            .to_string()
                            .contains(&format!("BOUNCER_NETID={network_id}")) =>
                    {
                        assert!(connected);
                        return;
                    }
                    IrcEvent::Disconnected(_, reason) => panic!("bouncer disconnected: {reason:?}"),
                    _ => {}
                }
            }
            panic!("network confirmation was not replayed");
        })
        .await
        .unwrap();
        drop(handle);
        drop(events);
    }
}

#[tokio::test]
async fn control_registration_has_no_binding_or_autojoin() {
    for reply in [Reply::ControlSuccess, Reply::ControlUnexpectedBound] {
        tokio::time::timeout(Duration::from_secs(5), registration(reply, false))
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires a disposable local bouncer fixture; see docs/validation/bouncer-binding.md"]
async fn pinned_bouncer_discovery() {
    let mut config: ServerConfig = toml::from_str(
        "label = 'fixture'\naddress = '127.0.0.1'\nport = 6667\ntls = false\nchannels = []\n",
    )
    .unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT")
        .unwrap()
        .parse()
        .unwrap();
    config.bouncer_control = true;
    config.sasl_user = Some(std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap());
    config.sasl_pass = Some("fixture-password".into());
    let expected = std::env::var("REPARTEE_BOUNCER_TEST_NETID").unwrap();
    let general = GeneralConfig {
        flood_protection: false,
        ..GeneralConfig::default()
    };
    let (handle, mut events) = tokio::time::timeout(
        Duration::from_secs(5),
        connect_server("fixture", &config, &general),
    )
    .await
    .unwrap()
    .unwrap();
    let mut registry = super::NetworkRegistry::default();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = events.recv().await {
            match event {
                IrcEvent::Connected(_, caps, _) => {
                    assert!(caps.contains(super::NETWORKS_NOTIFY_CAP));
                }
                IrcEvent::Message(_, message) => {
                    registry.handle(&message);
                    if registry.complete {
                        assert!(registry.networks.contains_key(&expected));
                        assert!(registry.networks[&expected].attributes.contains_key("name"));
                        return;
                    }
                }
                IrcEvent::Disconnected(_, reason) => panic!("bouncer disconnected: {reason:?}"),
                _ => {}
            }
        }
        panic!("bouncer did not provide a complete network list");
    })
    .await
    .unwrap();
    drop(handle);
}

#[tokio::test]
async fn cancelled_registration_closes_the_socket_at_both_await_points() {
    for reply in [Reply::CancelNegotiation, Reply::CancelConfirmation] {
        tokio::time::timeout(Duration::from_secs(5), registration(reply, true))
            .await
            .expect("cancelled registration retained an open socket");
    }
}

#[tokio::test]
async fn cancellation_discards_a_blocked_registration_write() {
    use tokio::io::AsyncReadExt;

    const PAYLOAD_SIZE: usize = 16 * 1024 * 1024;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (drain_tx, drain_rx) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(socket);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert_eq!(line, "CAP LS 302\r\n");
        ready_tx.send(()).unwrap();
        drain_rx.await.unwrap();
        let mut received = Vec::new();
        reader.read_to_end(&mut received).await.unwrap();
        received.len()
    });
    let mut server: ServerConfig = toml::from_str(
        "label = 'fixture'\naddress = '127.0.0.1'\nport = 6667\ntls = false\nchannels = []\n",
    )
    .unwrap();
    server.port = port;
    server.password = Some("x".repeat(PAYLOAD_SIZE));
    server.bouncer_control = true;
    let general = GeneralConfig {
        flood_protection: false,
        ..GeneralConfig::default()
    };
    let mut attempt = Box::pin(connect_server("fixture", &server, &general));
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            _ = &mut attempt => panic!("registration completed before cancellation"),
            ready = ready_rx => ready.unwrap(),
        }
    })
    .await
    .unwrap();
    drop(attempt);
    drain_tx.send(()).unwrap();
    let received = tokio::time::timeout(Duration::from_secs(5), peer)
        .await
        .unwrap()
        .unwrap();
    assert!(
        received < PAYLOAD_SIZE,
        "cancelled registration completed its blocked write"
    );
}

#[tokio::test]
async fn ambiguous_bouncer_authentication_is_rejected_before_connecting() {
    let mut config: ServerConfig = toml::from_str(
        "label = 'fixture'\naddress = '127.0.0.1'\nport = 1\ntls = false\nchannels = []\nbouncer_control = true",
    ).unwrap();
    config.sasl_user = Some("account".into());
    config.sasl_pass = Some("fixture-password".into());
    for certificate in [true, false] {
        config.client_cert_path = certificate.then(|| "fixture-cert.pem".into());
        config.sasl_key_path = (!certificate).then(|| "fixture-key.pem".into());
        let result = connect_server("fixture", &config, &GeneralConfig::default()).await;
        assert!(result.err().unwrap().to_string().contains("explicit SASL mechanism"));
    }
}

#[tokio::test]
async fn dropping_registered_connection_closes_an_idle_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let peer = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (read, mut write) = socket.into_split();
        let mut lines = BufReader::new(read).lines();
        assert_eq!(next_command(&mut lines, &mut write).await.as_deref(), Some("CAP LS 302"));
        assert!(next_command(&mut lines, &mut write).await.unwrap().starts_with("NICK "));
        assert!(next_command(&mut lines, &mut write).await.unwrap().starts_with("USER "));
        write.write_all(b":fixture CAP * LS :\r\n").await.unwrap();
        assert_eq!(next_command(&mut lines, &mut write).await.as_deref(), Some("CAP END"));
        write.write_all(b":fixture 001 tester :Welcome\r\n").await.unwrap();
        assert!(next_command(&mut lines, &mut write).await.is_none());
    });
    let mut config: ServerConfig = toml::from_str(
        "label = 'fixture'\naddress = '127.0.0.1'\nport = 6667\ntls = false\nchannels = []",
    ).unwrap();
    config.port = port;
    config.nick = Some("tester".into());
    let general = GeneralConfig { flood_protection: false, ..GeneralConfig::default() };
    tokio::time::timeout(Duration::from_secs(5), async {
        let (handle, mut events) = connect_server("fixture", &config, &general).await.unwrap();
        while let Some(event) = events.recv().await {
            if matches!(event, IrcEvent::Connected(..)) { break; }
        }
        let reader = handle.reader_handle.as_ref().unwrap().abort_handle();
        let writer = handle.outgoing_handle.as_ref().unwrap().abort_handle();
        drop(events);
        drop(handle);
        tokio::task::yield_now().await;
        assert!(reader.is_finished());
        assert!(writer.is_finished());
        peer.await.unwrap();
    }).await.expect("cancelled idle reader or writer retained its socket");
}

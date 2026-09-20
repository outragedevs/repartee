use super::{CAP, Required, is_failure};
use crate::irc::{IrcEvent, connect_server};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Failure { None, Ls, Ack, Challenge, Outcome, After, Binding, AfterWelcome }

#[derive(Clone, Copy, Debug)]
#[expect(clippy::struct_excessive_bools, reason = "independent registration fixture options")]
struct Case { required: bool, sasl: bool, pass: bool, reject_sasl: bool, bound: bool, failure: Failure }

impl Case {
    fn denied(self) -> bool {
        !matches!(self.failure, Failure::None | Failure::AfterWelcome) || (self.required && (!self.sasl || self.reject_sasl) && !self.pass)
    }
}

#[test]
fn informational_cap_is_never_requested_and_failure_is_scoped() {
    let caps = crate::irc::cap::ServerCaps::parse("soju.im/account-required sasl batch");
    assert!(!caps.negotiate(crate::irc::cap::DESIRED_CAPS).iter().any(|cap| cap == CAP));
    assert!(!crate::irc::cap::bouncer_network_caps(&caps).iter().any(|cap| cap == CAP));
    assert!(is_failure(&":s FAIL * ACCOUNT_REQUIRED :log in".parse().unwrap()));
    assert!(is_failure(&":s FAIL * account_required :log in".parse().unwrap()));
    assert!(!is_failure(&":s FAIL JOIN ACCOUNT_REQUIRED #room :log in".parse().unwrap()));
    assert!(!is_failure(&":s FAIL * OTHER :log in".parse().unwrap()));
}

async fn registration(case: Case) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let peer = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (read, mut write) = socket.into_split();
        let mut lines = BufReader::new(read).lines();
        assert_eq!(lines.next_line().await.unwrap().unwrap(), "CAP LS 302");
        if case.pass { assert!(lines.next_line().await.unwrap().unwrap().starts_with("PASS ")); }
        assert!(lines.next_line().await.unwrap().unwrap().starts_with("NICK "));
        assert!(lines.next_line().await.unwrap().unwrap().starts_with("USER "));
        let refusal: &[u8] = if case.failure == Failure::Binding {
            b":fixture FAIL BOUNCER ACCOUNT_REQUIRED BIND :untrusted binding details\r\n"
        } else { b":fixture FAIL * ACCOUNT_REQUIRED :untrusted server details must not be displayed\r\n" };
        if case.failure == Failure::Ls {
            write.write_all(refusal).await.unwrap();
            assert!(lines.next_line().await.unwrap().is_none());
            return;
        }
        let required = if case.required { " soju.im/account-required" } else { "" };
        write.write_all(format!(":fixture CAP * LS :batch sasl=PLAIN soju.im/bouncer-networks{required}\r\n").as_bytes()).await.unwrap();
        let request = lines.next_line().await.unwrap().unwrap();
        let requested = request.strip_prefix("CAP REQ ").unwrap().trim_start_matches(':');
        assert!(!requested.contains(CAP));
        if case.failure == Failure::Ack {
            write.write_all(refusal).await.unwrap();
            assert!(lines.next_line().await.unwrap().is_none());
            return;
        }
        write.write_all(format!(":fixture CAP * ACK :{requested}\r\n").as_bytes()).await.unwrap();
        if case.sasl {
            assert_eq!(lines.next_line().await.unwrap().unwrap(), "AUTHENTICATE PLAIN");
            if case.failure == Failure::Challenge {
                write.write_all(refusal).await.unwrap();
                assert!(lines.next_line().await.unwrap().is_none());
                return;
            }
            write.write_all(b"AUTHENTICATE +\r\n").await.unwrap();
            assert!(lines.next_line().await.unwrap().unwrap().starts_with("AUTHENTICATE "));
            if case.failure == Failure::Outcome {
                write.write_all(refusal).await.unwrap();
                assert!(lines.next_line().await.unwrap().is_none());
                return;
            }
            write.write_all(if case.reject_sasl { b":fixture 904 me :Authentication failed\r\n" } else { b":fixture 903 me :Authentication successful\r\n" }).await.unwrap();
            if case.reject_sasl {
                let abort = lines.next_line().await.unwrap();
                if abort.is_none() { assert!(case.denied()); return; }
                assert_eq!(abort.as_deref(), Some("AUTHENTICATE *"));
            }
        }
        if case.required && (!case.sasl || case.reject_sasl) && !case.pass {
            assert!(lines.next_line().await.unwrap().is_none());
            return;
        }
        if case.bound { assert_eq!(lines.next_line().await.unwrap().unwrap(), "BOUNCER BIND 42"); }
        assert_eq!(lines.next_line().await.unwrap().unwrap(), "CAP END");
        if matches!(case.failure, Failure::After | Failure::Binding) { write.write_all(refusal).await.unwrap(); }
        else {
            write.write_all(b":fixture 001 me :Welcome\r\n:fixture 005 me BOUNCER_NETID=42 :supported tokens\r\n").await.unwrap();
            if case.failure == Failure::AfterWelcome { write.write_all(refusal).await.unwrap(); }
            write.write_all(b":fixture 422 me :No MOTD\r\n").await.unwrap();
        }
        let closed = lines.next_line().await;
        assert!(matches!(closed, Ok(None)) || matches!(closed, Err(ref error) if error.kind() == std::io::ErrorKind::ConnectionReset), "unexpected frame at fixture teardown: {closed:?}");
    });
    let mut config: crate::config::ServerConfig = toml::from_str("label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nnick='me'").unwrap();
    config.port = port;
    if case.pass { config.password = Some("fixture-password".into()); }
    if case.sasl { config.sasl_user = Some("fixture".into()); config.sasl_pass = Some("fixture-password".into()); }
    if case.bound { config.bouncer_network_id = Some("42".into()); }
    let general = crate::config::GeneralConfig { flood_protection: false, ..Default::default() };
    match connect_server("fixture", &config, &general).await {
        Ok((handle, mut events)) => {
            let mut welcomed = false;
            let event = loop {
                let event = events.recv().await.expect("registration outcome");
                if matches!(event, IrcEvent::Connected(..)) {
                    welcomed = true;
                    if case.failure == Failure::AfterWelcome { continue; }
                }
                if case.failure == Failure::AfterWelcome && matches!(&event, IrcEvent::Message(_, message) if is_failure(message)) {
                    assert!(welcomed);
                    break event;
                }
                if matches!(event, IrcEvent::Connected(..) | IrcEvent::Disconnected(..)) { break event; }
            };
            if case.denied() {
                let IrcEvent::Disconnected(_, Some(reason)) = event else { panic!("unexpected success: {case:?}"); };
                assert_eq!(reason, Required.to_string());
            } else if case.failure == Failure::AfterWelcome {
                assert!(matches!(event, IrcEvent::Message(_, message) if is_failure(&message)));
            } else { assert!(matches!(event, IrcEvent::Connected(..))); }
            drop(handle);
            drop(events);
        }
        Err(error) => { assert!(case.denied(), "unexpected failure: {case:?}"); assert!(error.downcast_ref::<Required>().is_some()); }
    }
    peer.await.unwrap();
}

#[tokio::test]
async fn account_required_registration_paths() {
    let base = Case { required: true, sasl: false, pass: false, reject_sasl: false, bound: false, failure: Failure::None };
    let cases = [base,
        Case { pass: true, ..base }, Case { sasl: true, ..base },
        Case { sasl: true, reject_sasl: true, ..base },
        Case { sasl: true, reject_sasl: true, pass: true, ..base },
        Case { required: false, ..base },
        Case { required: false, failure: Failure::Ls, ..base },
        Case { required: false, failure: Failure::Ack, ..base },
        Case { required: false, sasl: true, failure: Failure::Challenge, ..base },
        Case { required: false, sasl: true, failure: Failure::Outcome, ..base },
        Case { required: false, failure: Failure::After, ..base },
        Case { required: false, sasl: true, bound: true, failure: Failure::After, ..base },
        Case { required: false, sasl: true, bound: true, failure: Failure::Binding, ..base },
        Case { required: false, sasl: true, bound: true, failure: Failure::AfterWelcome, ..base },
        Case { required: false, failure: Failure::AfterWelcome, ..base },
    ];
    for case in cases {
        tokio::time::timeout(std::time::Duration::from_secs(5), registration(case)).await.expect("registration must not hang");
    }
}

#[tokio::test]
async fn post_registration_requirement_is_visible_in_native_and_web_server_buffer() {
    let mut app = crate::app::input::submit_typing_tests::test_app();
    let config = toml::from_str("label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]").unwrap();
    app.setup_connection("fixture", &config);
    app.setup_connection("other", &config);
    app.state.set_active_buffer("other/fixture");
    app.state.connections.get_mut("fixture").unwrap().status = crate::state::connection::ConnectionStatus::Connected;
    let mut events = app.web_broadcaster.subscribe();
    app.handle_irc_event(IrcEvent::Message("fixture".into(), Box::new(":s FAIL * ACCOUNT_REQUIRED :untrusted private details".parse().unwrap())));
    assert_eq!(app.state.connections["fixture"].status, crate::state::connection::ConnectionStatus::Connected);
    assert_eq!(app.state.buffers["fixture/fixture"].messages.back().unwrap().text, Required.to_string());
    let mut visible = false;
    while let Ok(event) = events.try_recv() {
        if let crate::web::protocol::WebEvent::NewMessage { buffer_id, message } = event {
            visible |= buffer_id == "fixture/fixture" && message.text == Required.to_string();
        }
    }
    assert!(visible);
}

#[tokio::test]
#[ignore = "requires disposable pinned bouncer; scripts/test_bouncer_binding.py"]
async fn pinned_bouncer_account_required() {
    let mut config: crate::config::ServerConfig = toml::from_str("label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nbouncer_control=true").unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT").unwrap().parse().unwrap();
    let user = std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap();
    config.username = Some(user.clone());
    let general = crate::config::GeneralConfig { flood_protection: false, ..Default::default() };
    if std::env::var("REPARTEE_BOUNCER_TEST_PROVIDER").unwrap() == "soju" {
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), connect_server("fixture", &config, &general)).await.unwrap();
        assert!(result.err().unwrap().downcast_ref::<Required>().is_some());
    }
    for sasl in [false, true] {
        config.password = (!sasl).then(|| "fixture-password".into());
        config.sasl_user = sasl.then(|| user.clone());
        config.sasl_pass = sasl.then(|| "fixture-password".into());
        let (handle, mut events) = tokio::time::timeout(std::time::Duration::from_secs(5), connect_server("fixture", &config, &general)).await.unwrap().unwrap();
        assert_eq!(handle.sasl_authenticated, sasl);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(event) = events.recv().await {
                match event {
                    IrcEvent::Connected(_, caps, _) => { assert!(!caps.contains(CAP)); return; }
                    IrcEvent::Disconnected(..) => panic!("authentication did not complete"),
                    _ => {}
                }
            }
            panic!("missing registration result");
        }).await.unwrap();
        drop(handle);
        drop(events);
    }
}

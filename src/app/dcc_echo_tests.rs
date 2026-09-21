use super::App;
use crate::irc::IrcEvent;

#[tokio::test]
async fn dcc_own_echo_is_ignored_but_peer_offers_remain_connection_scoped() {
    let mut app = crate::app::input::submit_typing_tests::test_app();
    let config = toml::from_str(
        "label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nnick='kofany['",
    )
    .unwrap();
    app.setup_connection("fixture", &config);
    app.setup_connection("other", &config);
    app.state.connections.get_mut("other").unwrap().nick = "amiantos".into();

    for offer in [
        "DCC CHAT chat 167772180 39807",
        "DCC CHAT chat 167772180 0 42",
        "DCC CHAT chat 167772180 39807 42",
    ] {
        receive(
            &mut app,
            "fixture",
            &format!(":KOFANY{{!user@host PRIVMSG amiantos :\x01{offer}\x01"),
        );
        assert!(app.dcc.records.is_empty());
        assert!(app.dcc.chat_senders.is_empty());
    }

    receive(
        &mut app,
        "fixture",
        ":amiantos!user@host PRIVMSG kofany[ :\x01DCC CHAT chat 167772180 39807\x01",
    );
    receive(
        &mut app,
        "other",
        ":kofany[!user@host PRIVMSG amiantos :\x01DCC CHAT chat 167772180 39808\x01",
    );
    assert_eq!(app.dcc.records.len(), 2);
    for (connection, nick, port) in [("fixture", "amiantos", 39807), ("other", "kofany[", 39808)] {
        assert!(app.dcc.records.values().any(|record| {
            record.conn_id == connection
                && record.nick == nick
                && record.port == port
                && record.state == crate::dcc::types::DccState::WaitingUser
        }));
    }
}

fn receive(app: &mut App, connection: &str, wire: &str) {
    app.handle_irc_event(IrcEvent::Message(
        connection.into(),
        Box::new(wire.parse().unwrap()),
    ));
}

#[tokio::test]
async fn passive_reply_requires_the_outgoing_peer_network_and_pending_state() {
    use crate::dcc::types::DccState;
    use crate::irc::{IrcHandle, IrcSender};

    let mut app = crate::app::input::submit_typing_tests::test_app();
    let config = toml::from_str(
        "label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nnick='me'",
    )
    .unwrap();
    for id in ["fixture", "other"] {
        app.setup_connection(id, &config);
        app.irc_handles.insert(
            id.into(),
            IrcHandle::new(id.into(), IrcSender::capturing(0), None, None),
        );
    }
    app.state.set_active_buffer("fixture/fixture");
    app.execute_command(
        &crate::commands::parser::parse_command("/dcc chat -passive peer[").unwrap(),
    );
    let original = app.dcc.records.values().next().unwrap().clone();
    let token = original.passive_token.unwrap();
    assert!(original.outgoing);
    assert!((1..=63).contains(&token));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let reply = |nick: &str, token: u32, port: u16| {
        format!(":{nick}!user@host PRIVMSG me :\x01DCC CHAT chat 2130706433 {port} {token}\x01")
    };
    for (network, nick, reply_token, outgoing, state) in [
        ("other", "peer[", token, true, DccState::WaitingUser),
        ("fixture", "stranger", token, true, DccState::WaitingUser),
        ("fixture", "peer[", token.wrapping_add(1), true, DccState::WaitingUser),
        ("fixture", "peer[", token, false, DccState::WaitingUser),
        ("fixture", "peer[", token, true, DccState::Connected),
        ("fixture", "peer[", token, true, DccState::Listening),
    ] {
        let mut record = original.clone();
        record.outgoing = outgoing;
        record.state = state;
        app.dcc.records.insert(original.id.clone(), record);
        receive(&mut app, network, &reply(nick, reply_token, port));
        assert_eq!(app.dcc.records.len(), 1);
        assert_eq!(app.dcc.records[&original.id].state, state);
        assert_eq!(app.dcc.records[&original.id].port, 0);
        assert!(app.dcc.chat_senders.is_empty());
    }

    app.dcc.records.insert(original.id.clone(), original.clone());
    receive(&mut app, "other", ":PEER{!user@host NICK :unrelated");
    assert_eq!(app.dcc.records[&original.id].nick, "peer[");
    receive(&mut app, "fixture", ":PEER{!user@host NICK :renamed[");
    let renamed_id = app.dcc.records.values().next().unwrap().id.clone();
    assert_eq!(app.dcc.records[&renamed_id].nick, "renamed[");
    receive(&mut app, "fixture", &reply("RENAMED{", token, port));
    assert_eq!(app.dcc.records[&renamed_id].state, DccState::Connecting);
    assert_eq!(app.dcc.records[&renamed_id].port, port);
    receive(&mut app, "fixture", &reply("RENAMED{", token, port.wrapping_add(1)));
    assert_eq!(app.dcc.records[&renamed_id].port, port);
    assert_eq!(app.dcc.chat_senders.len(), 1);
    let (stream, _) = tokio::time::timeout(std::time::Duration::from_secs(2), listener.accept())
        .await.unwrap().unwrap();
    drop(stream);
}

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


#[tokio::test]
async fn dcc_commands_route_identical_peers_by_network() {
    use crate::dcc::DccEvent;
    use crate::irc::{IrcHandle, IrcSender};
    let mut app = crate::app::input::submit_typing_tests::test_app();
    let config = toml::from_str(
        "label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nnick='me'",
    ).unwrap();
    let mut sessions = Vec::new();
    for network in ["fixture", "other"] {
        app.setup_connection(network, &config);
        let sender = IrcSender::capturing(0);
        app.irc_handles.insert(network.into(), IrcHandle::new(network.into(), sender.clone(), None, None));
        receive(&mut app, network, ":peer[!user@host PRIVMSG me :\x01DCC CHAT chat 2130706433 39807\x01");
        let id = app.dcc.records.values().find(|r| r.conn_id == network).unwrap().id.clone();
        app.handle_dcc_event(DccEvent::ChatConnected { id: id.clone() });
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        app.dcc.chat_senders.insert(id.clone(), tx);
        sessions.push((id, sender, rx));
    }
    for (index, network) in ["fixture", "other"].into_iter().enumerate() {
        app.state.set_active_buffer(&format!("{network}/=peer["));
        for (input, expected) in [("/msg =PEER{ hello", "hello"), ("/me waves", "\x01ACTION waves\x01"), ("plain", "plain")] {
            app.handle_submit(input);
            assert_eq!(sessions[index].2.try_recv().unwrap(), expected);
            assert!(sessions[1 - index].2.try_recv().is_err());
        }
    }
    app.state.set_active_buffer("other/=peer[");
    app.handle_submit("/dcc close chat PEER{");
    assert!(!app.dcc.records.contains_key(&sessions[1].0));
    assert!(app.dcc.records.contains_key(&sessions[0].0));
    app.state.set_active_buffer("fixture/=peer[");
    app.handle_submit("/dcc reject chat PEER{");
    assert!(app.dcc.records.is_empty());
    assert!(sessions[0].1.captured().iter().any(|m| m.to_string().contains("DCC REJECT")));
    assert!(sessions[1].1.captured().is_empty());
}

#[tokio::test]
async fn dcc_acceptance_and_missing_nick_stay_on_the_offer_network() {
    use crate::dcc::types::DccState;
    use crate::irc::{IrcHandle, IrcSender};
    let mut app = crate::app::input::submit_typing_tests::test_app();
    let config = toml::from_str(
        "label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nnick='me'",
    ).unwrap();
    app.dcc.own_ip = Some(std::net::Ipv4Addr::LOCALHOST.into());
    for network in ["fixture", "other"] {
        app.setup_connection(network, &config);
        app.irc_handles.insert(network.into(), IrcHandle::new(network.into(), IrcSender::capturing(0), None, None));
        receive(&mut app, network, ":peer[!user@host PRIVMSG me :\x01DCC CHAT chat 2130706433 0 42\x01");
    }
    app.state.set_active_buffer("fixture/fixture");
    app.handle_submit("/dcc chat");
    assert_eq!(app.dcc.records.values().find(|r| r.conn_id == "fixture").unwrap().state, DccState::Listening);
    assert_eq!(app.dcc.records.values().find(|r| r.conn_id == "other").unwrap().state, DccState::WaitingUser);
    assert_eq!(app.irc_handles["fixture"].sender().captured().len(), 1);
    assert!(app.irc_handles["other"].sender().captured().is_empty());
    app.dcc.autochat_masks = vec!["*!*@*".into()];
    receive(&mut app, "other", ":auto!user@host PRIVMSG me :\x01DCC CHAT chat 2130706433 0 43\x01");
    assert_eq!(app.dcc.records.values().find(|r| r.nick == "auto").unwrap().state, DccState::Listening);
    assert_eq!(app.irc_handles["other"].sender().captured().len(), 1);
    receive(&mut app, "other", ":server 401 me PEER{ :No such nick");
    assert!(app.dcc.records.values().any(|r| r.conn_id == "fixture" && r.nick == "peer["));
    assert!(!app.dcc.records.values().any(|r| r.conn_id == "other" && r.nick == "peer["));
}


#[tokio::test]
async fn repeated_dcc_chat_never_accepts_our_own_offer() {
    use crate::dcc::types::DccState;
    use crate::irc::{IrcHandle, IrcSender};
    for passive in [false, true] {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let config = toml::from_str("label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nnick='me'").unwrap();
        app.setup_connection("fixture", &config);
        app.state.set_active_buffer("fixture/fixture");
        app.dcc.own_ip = Some(std::net::Ipv4Addr::LOCALHOST.into());
        let sender = IrcSender::capturing(0);
        app.irc_handles.insert("fixture".into(), IrcHandle::new("fixture".into(), sender.clone(), None, None));
        app.handle_submit(if passive { "/dcc chat -passive peer[" } else { "/dcc chat peer[" });
        let original = app.dcc.records.values().next().unwrap().clone();
        for command in ["/dcc chat", "/dcc chat PEER{", "/dcc chat -passive PEER{"] {
            app.handle_submit(command);
            assert_eq!(app.dcc.records.len(), 1);
            assert_eq!(app.dcc.records[&original.id].state, original.state);
            assert_eq!(sender.captured().len(), 1);
        }
        let mut incoming = original.clone();
        incoming.id = "incoming".into();
        incoming.nick = "incoming".into();
        incoming.outgoing = false;
        incoming.state = DccState::WaitingUser;
        incoming.port = 0;
        incoming.passive_token = Some(62);
        incoming.created = original.created.checked_sub(std::time::Duration::from_secs(1)).unwrap();
        app.dcc.records.insert(incoming.id.clone(), incoming);
        app.handle_submit("/dcc chat");
        assert_eq!(app.dcc.records["incoming"].state, DccState::Listening);
        assert_eq!(app.dcc.records[&original.id].state, original.state);
        assert_eq!(sender.captured().len(), 2);
    }
}

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

use std::collections::HashMap;

use irc::proto::{Command, Prefix};

use crate::state::AppState;
use crate::state::buffer::{BufferType, Message, MessageType, make_buffer_id};

pub fn command(app: &mut crate::app::App, args: &[String]) {
    use crate::commands::helpers::add_local_event;
    let Some(id) = app.active_conn_id().map(str::to_string) else {
        add_local_event(app, "Not connected");
        return;
    };
    if !app
        .state
        .connections
        .get(&id)
        .is_some_and(|conn| conn.enabled_caps.contains("setname"))
    {
        add_local_event(app, "This connection has not negotiated SETNAME support");
        return;
    }
    let realname = args.join(" ");
    if realname.is_empty() || realname.chars().any(char::is_control) {
        add_local_event(
            app,
            "Usage: /setname <real name without control characters>",
        );
        return;
    }
    let command = Command::Raw("SETNAME".into(), vec![realname]);
    if irc::proto::Message::from(command.clone()).to_string().len() > 512 {
        add_local_event(app, "Real name exceeds the IRC line limit");
        return;
    }
    if app
        .irc_handles
        .get(&id)
        .is_none_or(|handle| handle.sender().send(command).is_err())
    {
        add_local_event(app, "Could not send SETNAME");
    }
}

pub fn handle(
    state: &mut AppState,
    conn_id: &str,
    prefix: Option<&Prefix>,
    args: &[String],
    tags: Option<&HashMap<String, String>>,
) {
    let Some(Prefix::Nickname(nick, _, _)) = prefix else {
        return;
    };
    let [realname] = args else {
        return;
    };
    if nick.is_empty() || realname.chars().any(char::is_control) {
        return;
    }
    let Some(conn) = state.connections.get_mut(conn_id) else {
        return;
    };
    let own = conn.nick.eq_ignore_ascii_case(nick);
    let server_buffer = make_buffer_id(conn_id, &conn.label);
    if own {
        conn.own_realname = Some(realname.clone());
    }
    let key = nick.to_lowercase();
    let mut affected = Vec::new();
    for buffer in state
        .buffers
        .values_mut()
        .filter(|buffer| buffer.connection_id == conn_id)
    {
        let entry = buffer.users.get_mut(&key);
        let shared = entry.is_some();
        if let Some(entry) = entry {
            entry.realname = Some(realname.clone());
        }
        if shared
            || (buffer.buffer_type == BufferType::Query && buffer.name.eq_ignore_ascii_case(nick))
        {
            affected.push(buffer.id.clone());
        }
    }
    for id in &affected {
        if let Some(event) = crate::web::snapshot::build_nick_list(state, id) {
            state.pending_web_events.push(event);
        }
    }
    if own && !affected.contains(&server_buffer) {
        affected.push(server_buffer);
    }
    let text = format!("{nick} changed real name to {realname}").replace('%', "%%");
    for id in affected {
        emit(state, &id, text.clone(), tags.cloned());
    }
}

pub fn failure(state: &mut AppState, conn_id: &str, args: &[String]) {
    if args.len() < 3 {
        return;
    }
    let Some(conn) = state.connections.get(conn_id) else {
        return;
    };
    let id = make_buffer_id(conn_id, &conn.label);
    let text =
        format!("SETNAME rejected: {} — {}", args[1], args.last().unwrap()).replace('%', "%%");
    emit(state, &id, text, None);
}

fn emit(
    state: &mut AppState,
    buffer_id: &str,
    text: String,
    tags: Option<HashMap<String, String>>,
) {
    let id = state.next_message_id();
    state.add_message(
        buffer_id,
        Message {
            id,
            timestamp: super::events::message_timestamp(tags.as_ref()),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text,
            highlight: false,
            event_key: None,
            event_params: None,
            redaction_ref: None,
            redaction_msgid: None,
            log_key: None,
            log_msg_id: None,
            log_ref_id: None,
            tags,
            wire_origin: None,
            translation_suffix_at: None,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> crate::app::App {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        for id in ["one", "two"] {
            let config = toml::from_str(
                "label='Server'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nnick='me'",
            )
            .unwrap();
            app.setup_connection(id, &config);
            let conn = app.state.connections.get_mut(id).unwrap();
            conn.status = crate::state::connection::ConnectionStatus::Connected;
            conn.enabled_caps
                .extend(["setname".into(), "extended-join".into()]);
            app.irc_handles.insert(
                id.into(),
                crate::irc::IrcHandle::new(
                    id.into(),
                    crate::irc::IrcSender::capturing(0),
                    None,
                    None,
                ),
            );
            receive(&mut app, id, ":me!u@h JOIN #test * :Original Self");
            receive(&mut app, id, ":peer!u@h JOIN #test * :Original Peer");
            receive(&mut app, id, ":server 353 me = #test :me peer");
        }
        app.state.set_active_buffer("one/#test");
        app
    }

    fn receive(app: &mut crate::app::App, id: &str, wire: &str) {
        app.handle_irc_event(crate::irc::IrcEvent::Message(
            id.into(),
            Box::new(wire.parse().unwrap()),
        ));
    }

    #[tokio::test]
    async fn live_changes_update_shared_nicks_and_web_without_crossing_connections() {
        let mut app = app();
        receive(&mut app, "one", ":peer!u@h SETNAME :New %N Name");
        assert_eq!(
            app.state.buffers["one/#test"].users["peer"]
                .realname
                .as_deref(),
            Some("New %N Name")
        );
        assert_eq!(
            app.state.buffers["two/#test"].users["peer"]
                .realname
                .as_deref(),
            Some("Original Peer")
        );
        let crate::web::protocol::WebEvent::NickList { nicks, .. } =
            crate::web::snapshot::build_nick_list(&app.state, "one/#test").unwrap()
        else {
            panic!("expected nick list");
        };
        assert_eq!(
            nicks
                .iter()
                .find(|nick| nick.nick == "peer")
                .unwrap()
                .realname
                .as_deref(),
            Some("New %N Name")
        );
        receive(&mut app, "one", ":server 353 me = #test :me peer");
        assert_eq!(
            app.state.buffers["one/#test"].users["peer"]
                .realname
                .as_deref(),
            Some("New %N Name")
        );
        receive(&mut app, "one", ":peer!u@h NICK :renamed");
        assert_eq!(
            app.state.buffers["one/#test"].users["renamed"]
                .realname
                .as_deref(),
            Some("New %N Name")
        );
    }

    #[tokio::test]
    async fn own_state_changes_only_on_confirmation_and_failures_are_visible() {
        let mut app = app();
        let parsed = crate::commands::parser::parse_command("/setname New   Self").unwrap();
        command(&mut app, &parsed.args);
        assert!(app.state.connections["one"].own_realname.is_none());
        let frames = app.irc_handles["one"].sender().captured();
        assert!(
            frames
                .iter()
                .any(|frame| frame.to_string().contains("SETNAME :New   Self"))
        );
        receive(
            &mut app,
            "one",
            ":server FAIL SETNAME CANNOT_CHANGE_REALNAME :Not permitted",
        );
        assert!(app.state.connections["one"].own_realname.is_none());
        assert!(
            app.state.buffers["one/server"]
                .messages
                .iter()
                .any(|message| message.text.contains("SETNAME rejected"))
        );
        receive(&mut app, "one", ":me!u@h SETNAME :Confirmed Self");
        assert_eq!(
            app.state.connections["one"].own_realname.as_deref(),
            Some("Confirmed Self")
        );
        assert_eq!(
            app.state.buffers["one/#test"].users["me"]
                .realname
                .as_deref(),
            Some("Confirmed Self")
        );
        super::super::events::handle_connected(&mut app.state, "one");
        assert!(app.state.connections["one"].own_realname.is_none());
    }

    #[tokio::test]
    async fn unsupported_invalid_and_malformed_commands_do_not_change_state() {
        let mut app = app();
        let before = app.irc_handles["one"].sender().captured().len();
        for args in [vec![], vec!["bad\r\nQUIT".into()], vec!["x".repeat(512)]] {
            command(&mut app, &args);
        }
        app.state
            .connections
            .get_mut("one")
            .unwrap()
            .enabled_caps
            .remove("setname");
        command(&mut app, &["Valid Name".into()]);
        assert_eq!(app.irc_handles["one"].sender().captured().len(), before);
        for wire in [
            "SETNAME :No prefix",
            ":server SETNAME :Server prefix",
            ":peer!u@h SETNAME one two",
        ] {
            receive(&mut app, "one", wire);
        }
        assert_eq!(
            app.state.buffers["one/#test"].users["peer"]
                .realname
                .as_deref(),
            Some("Original Peer")
        );
    }

    #[tokio::test]
    async fn initial_who_and_incremental_join_transport_real_names() {
        let mut app = app();
        app.state.pending_web_events.clear();
        super::super::events::handle_irc_message(
            &mut app.state,
            "one",
            &":late!u@h JOIN #test * :Late Join Name".parse().unwrap(),
        );
        assert!(
            app.state
                .pending_web_events
                .iter()
                .any(|event| matches!(event,
            crate::web::protocol::WebEvent::NickEvent { nick, realname: Some(name), .. }
            if nick == "late" && name == "Late Join Name"))
        );
        app.state
            .connections
            .get_mut("one")
            .unwrap()
            .silent_who_channels
            .insert("#test".into());
        for (wire, expected) in [
            (
                ":server 354 me 1 #test user 127.0.0.1 host peer H 0 :Initial WHOX Name",
                "Initial WHOX Name",
            ),
            (
                ":server 352 me #test user host server peer H :0 Legacy WHO Name",
                "Legacy WHO Name",
            ),
        ] {
            app.state.pending_web_events.clear();
            super::super::events::handle_irc_message(&mut app.state, "one", &wire.parse().unwrap());
            assert_eq!(
                app.state.buffers["one/#test"].users["peer"]
                    .realname
                    .as_deref(),
                Some(expected)
            );
            assert!(
                app.state
                    .pending_web_events
                    .iter()
                    .any(|event| matches!(event,
                crate::web::protocol::WebEvent::NickEvent { nick, realname: Some(name), .. }
                if nick == "peer" && name == expected))
            );
        }
    }

    #[test]
    fn older_web_nick_payload_remains_compatible() {
        let nick: crate::web::protocol::WireNick =
            serde_json::from_str(r#"{"nick":"peer","prefix":"","modes":"","away":false}"#).unwrap();
        assert!(nick.realname.is_none());
    }
}

#[cfg(test)]
#[tokio::test]
#[ignore = "requires a disposable pinned bouncer fixture"]
#[expect(clippy::too_many_lines)]
async fn pinned_bouncer_setname() {
    fn client() -> crate::app::App {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        app.config.general.flood_protection = false;
        let mut config: crate::config::ServerConfig = toml::from_str("label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nbouncer_control=true").unwrap();
        if std::env::var_os("REPARTEE_SETNAME_BOUND").is_some() {
            config.bouncer_control = false;
            config.bouncer_network_id = Some("1".into());
        }
        config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT")
            .unwrap()
            .parse()
            .unwrap();
        config.sasl_user = Some(std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap());
        config.sasl_pass = Some("fixture-password".into());
        app.setup_connection("fixture", &config);
        app.state.set_active_buffer("fixture/fixture");
        app.start_connection_attempt("fixture", config);
        app
    }
    async fn until(apps: &mut [&mut crate::app::App], realname: Option<&str>) {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                for app in apps.iter_mut() {
                    while let Ok(event) = app.irc_rx.try_recv() {
                        app.handle_irc_event(event);
                    }
                }
                if apps.iter().all(|app| {
                    app.state.connections["fixture"].status
                        == crate::state::connection::ConnectionStatus::Connected
                        && (!app.state.connections["fixture"]
                            .origin_config
                            .bouncer_control
                            || app
                                .bouncer_networks
                                .get("fixture")
                                .is_some_and(|registry| registry.complete))
                        && realname.is_none_or(|name| {
                            app.state.connections["fixture"].own_realname.as_deref() == Some(name)
                        })
                }) {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("SETNAME bouncer response did not arrive");
    }
    fn disconnect(app: &mut crate::app::App) {
        app.suspend_bouncer_children("fixture");
        app.cancel_connection_attempt("fixture");
        app.irc_handles.remove("fixture");
    }
    let mut first = client();
    let mut second = client();
    until(&mut [&mut first, &mut second], None).await;
    let soju = std::env::var("REPARTEE_BOUNCER_TEST_PROVIDER").unwrap() == "soju";
    assert_eq!(
        first.state.connections["fixture"]
            .enabled_caps
            .contains("setname"),
        soju
    );
    first.handle_web_command(
        crate::web::protocol::WebCommand::RunCommand {
            buffer_id: "fixture/fixture".into(),
            text: "/setname Fixture changed name %".into(),
        },
        "browser",
    );
    if soju {
        until(
            &mut [&mut first, &mut second],
            Some("Fixture changed name %"),
        )
        .await;
        disconnect(&mut first);
        first = client();
        until(
            &mut [&mut first, &mut second],
            Some("Fixture changed name %"),
        )
        .await;
        assert!(
            second.state.buffers["fixture/fixture"]
                .messages
                .iter()
                .any(|message| message.text.contains("changed real name"))
        );
    } else {
        assert!(first.state.connections["fixture"].own_realname.is_none());
        assert!(
            first.state.buffers["fixture/fixture"]
                .messages
                .iter()
                .any(|message| message.text.contains("not negotiated SETNAME"))
        );
    }
    disconnect(&mut first);
    disconnect(&mut second);
}

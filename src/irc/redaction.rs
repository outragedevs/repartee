use irc::proto::{Command, Message};

pub fn is_redaction(message: &Message) -> bool {
    matches!(&message.command, Command::Raw(command, _) if command.eq_ignore_ascii_case("REDACT"))
}

pub fn command(app: &mut crate::app::App, args: &[String]) {
    use crate::commands::helpers::{add_local_event, escape_format};
    let Some(conn_id) = app.active_conn_id().map(str::to_string) else {
        add_local_event(app, "Not connected");
        return;
    };
    let supported = app.state.connections.get(&conn_id).is_some_and(|conn| {
        conn.status == crate::state::connection::ConnectionStatus::Connected
            && conn.server_owns_history()
            && !conn.bouncer_control()
            && conn.enabled_caps.contains("draft/message-redaction")
            && conn.enabled_caps.contains("message-tags")
    });
    if !supported {
        add_local_event(app, "Message redaction is unavailable on this connection");
        return;
    }
    let Some((target, rest)) = args.split_first() else {
        add_local_event(app, "Usage: /redact <target> <msgid> [reason]");
        return;
    };
    let Some((id, reason)) = rest.split_first() else {
        add_local_event(app, "Usage: /redact <target> <msgid> [reason]");
        return;
    };
    if !valid_parameter(target)
        || !valid_parameter(id)
        || target.contains(',')
        || target.starts_with('$')
        || reason.iter().any(|part| part.chars().any(char::is_control))
    {
        add_local_event(app, "Invalid redaction target, message ID or reason");
        return;
    }
    let mut params = vec![target.clone(), id.clone()];
    if !reason.is_empty() {
        params.push(reason.join(" "));
    }
    let command = Command::Raw("REDACT".into(), params);
    if Message::from(command.clone()).to_string().len() > 512 {
        add_local_event(app, "Redaction request exceeds the IRC line limit");
        return;
    }
    if let Err(error) = app.send_active_labeled_request(command) {
        add_local_event(
            app,
            &escape_format(&format!("Could not send redaction: {error}")),
        );
    }
}

fn valid_parameter(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with(':')
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> crate::app::App {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let config = toml::from_str("label='one'\naddress='localhost'\nport=1\ntls=false\nchannels=[]\nnick='me'\nbouncer_network_id='1'").unwrap();
        app.setup_connection("one", &config);
        let conn = app.state.connections.get_mut("one").unwrap();
        conn.status = crate::state::connection::ConnectionStatus::Connected;
        conn.enabled_caps
            .extend(["draft/message-redaction".into(), "message-tags".into()]);
        app.irc_handles.insert(
            "one".into(),
            crate::irc::IrcHandle::new(
                "one".into(),
                crate::irc::IrcSender::capturing(0),
                None,
                None,
            ),
        );
        app
    }

    #[test]
    fn requests_preserve_opaque_ids_and_omit_unspecified_reason() {
        let mut app = setup();
        for input in [
            "/redact #a Opaque-ID",
            "/redact #a Opaque-ID a reason with %N",
        ] {
            let parsed = crate::commands::parser::parse_command(input).unwrap();
            app.execute_command(&parsed);
        }
        let frames = app.irc_handles["one"].sender().captured();
        assert_eq!(frames.len(), 2);
        assert_eq!(
            frames[0].command,
            Command::Raw("REDACT".into(), vec!["#a".into(), "Opaque-ID".into()])
        );
        assert_eq!(
            frames[1].command,
            Command::Raw(
                "REDACT".into(),
                vec!["#a".into(), "Opaque-ID".into(), "a reason with %N".into()]
            )
        );
        assert_eq!(app.state.redaction_registry.deletion_count(), 0);
    }

    #[test]
    fn unsupported_connections_and_invalid_parameters_send_nothing() {
        for mode in [
            "direct",
            "control",
            "missing-cap",
            "missing-tags",
            "disconnected",
        ] {
            let mut app = setup();
            let conn = app.state.connections.get_mut("one").unwrap();
            match mode {
                "direct" => conn.origin_config.bouncer_network_id = None,
                "control" => conn.origin_config.bouncer_control = true,
                "missing-cap" => {
                    conn.enabled_caps.remove("draft/message-redaction");
                }
                "missing-tags" => {
                    conn.enabled_caps.remove("message-tags");
                }
                _ => conn.status = crate::state::connection::ConnectionStatus::Disconnected,
            }
            command(&mut app, &["#a".into(), "ID".into()]);
            assert!(app.irc_handles["one"].sender().captured().is_empty());
        }
        let mut app = setup();
        for args in [
            vec![],
            vec!["#a".into()],
            vec!["#a,#b".into(), "ID".into()],
            vec!["#a".into(), ":ID".into()],
            vec!["#a".into(), "ID with space".into()],
            vec!["#a".into(), "ID".into(), "bad\r\nPRIVMSG #a :inject".into()],
            vec!["#a".into(), "ID".into(), "x".repeat(600)],
        ] {
            command(&mut app, &args);
        }
        assert!(app.irc_handles["one"].sender().captured().is_empty());
    }
    #[test]
    fn labeled_request_checks_final_wire_length() {
        let mut app = setup();
        app.state.connections.get_mut("one").unwrap().enabled_caps.extend(["labeled-response".into(), "batch".into()]);
        let reason = "x".repeat(470);
        let untagged: Message = Command::Raw("REDACT".into(), vec!["#a".into(), "ID".into(), reason.clone()]).into();
        assert!(untagged.to_string().len() <= crate::irc::PROTOCOL_LINE_MAX_BYTES);
        command(&mut app, &["#a".into(), "ID".into(), reason]);
        assert!(app.irc_handles["one"].sender().captured().is_empty());
        assert_eq!(app.labeled_requests["one"].pending_count(), 0);
    }

}

use irc::proto::{Command, Message, Prefix};

pub const CAPABILITIES: &[&str] = &[
    "no-implicit-names",
    "draft/no-implicit-names",
    "soju.im/no-implicit-names",
];

pub fn request_after_join(
    state: &crate::state::AppState,
    id: &str,
    message: &Message,
) -> Option<Command> {
    let conn = state.connections.get(id)?;
    if !CAPABILITIES
        .iter()
        .any(|cap| conn.enabled_caps.contains(*cap))
    {
        return None;
    }
    let Prefix::Nickname(nick, _, _) = message.prefix.as_ref()? else {
        return None;
    };
    let mapping = conn.isupport_parsed.casemapping();
    if super::isupport::casefold(nick, mapping) != super::isupport::casefold(&conn.nick, mapping) {
        return None;
    }
    let channel = super::events::join_fields(&message.command)?.channel;
    let chantypes = conn.isupport_parsed.get("CHANTYPES").unwrap_or("#&");
    if !channel.starts_with(|c| chantypes.contains(c))
        || channel
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == ',')
    {
        return None;
    }
    let buffer_id = crate::state::buffer::make_buffer_id(id, channel);
    if state
        .buffers
        .get(&buffer_id)
        .is_some_and(|buffer| !buffer.users.is_empty())
    {
        return None;
    }
    Some(Command::NAMES(Some(channel.into()), None))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(cap: Option<&str>) -> crate::app::App {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let config = toml::from_str(
            "label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nnick='me'",
        )
        .unwrap();
        app.setup_connection("fixture", &config);
        let conn = app.state.connections.get_mut("fixture").unwrap();
        conn.status = crate::state::connection::ConnectionStatus::Connected;
        conn.nick = "me".into();
        if let Some(cap) = cap {
            conn.enabled_caps.insert(cap.into());
        }
        app.irc_handles.insert(
            "fixture".into(),
            crate::irc::IrcHandle::new(
                "fixture".into(),
                crate::irc::IrcSender::capturing(0),
                None,
                None,
            ),
        );
        app
    }

    fn receive(app: &mut crate::app::App, wire: &str) {
        app.handle_irc_event(crate::irc::IrcEvent::Message(
            "fixture".into(),
            Box::new(wire.parse().unwrap()),
        ));
    }

    #[tokio::test]
    async fn each_alias_requests_names_once_and_populates_the_web_nicklist() {
        for cap in CAPABILITIES {
            let mut app = app(Some(cap));
            receive(&mut app, ":me!u@h JOIN #test");
            receive(&mut app, ":peer!u@h JOIN #test");
            receive(&mut app, ":me!u@h JOIN #test");
            let frames = app.irc_handles["fixture"].sender().captured();
            assert_eq!(
                frames
                    .iter()
                    .filter(|frame| matches!(frame.command, Command::NAMES(_, _)))
                    .count(),
                1
            );
            receive(&mut app, ":server 353 me = #test :me @Alice Bob");
            receive(&mut app, ":server 366 me #test :End");
            let crate::web::protocol::WebEvent::NickList { nicks, .. } =
                crate::web::snapshot::build_nick_list(&app.state, "fixture/#test").unwrap()
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
    }

    #[tokio::test]
    async fn capability_changes_apply_only_to_subsequent_joins_on_the_connection() {
        let mut app = app(None);
        let config = toml::from_str(
            "label='other'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nnick='me'",
        )
        .unwrap();
        app.setup_connection("other", &config);
        app.state.connections.get_mut("other").unwrap().nick = "me".into();
        for cap in CAPABILITIES {
            receive(&mut app, &format!(":server CAP me NEW :{cap}"));
            assert!(!app.state.connections["fixture"].enabled_caps.contains(*cap));
            receive(&mut app, &format!(":server CAP me ACK :{cap}"));
            let join: Message = ":me!u@h JOIN #new account :Real Name".parse().unwrap();
            assert!(request_after_join(&app.state, "fixture", &join).is_some());
            assert!(request_after_join(&app.state, "other", &join).is_none());
            receive(&mut app, &format!(":server CAP me DEL :{cap}"));
            assert!(request_after_join(&app.state, "fixture", &join).is_none());
        }
    }

    #[tokio::test]
    async fn implicit_names_and_history_join_do_not_trigger_queries() {
        let mut app = app(None);
        receive(&mut app, ":me!u@h JOIN #normal");
        assert!(app.irc_handles["fixture"].sender().captured().is_empty());
        app.state
            .connections
            .get_mut("fixture")
            .unwrap()
            .enabled_caps
            .extend(["batch".into(), "no-implicit-names".into()]);
        receive(&mut app, ":server BATCH +history chathistory #old");
        receive(&mut app, "@batch=history :me!u@h JOIN #old");
        receive(&mut app, ":server BATCH -history");
        assert!(
            !app.irc_handles["fixture"]
                .sender()
                .captured()
                .iter()
                .any(|frame| matches!(frame.command, Command::NAMES(_, _)))
        );
    }
}

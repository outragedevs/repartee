use crate::irc::monitor::{Change, MonitorState};
use irc::proto::Message;

impl super::App {
    fn ensure_monitor(&mut self, id: &str) -> bool {
        let Some(conn) = self.state.connections.get(id) else {
            return false;
        };
        let config = &conn.origin_config;
        let scope = serde_json::to_string(&(
            conn.network_key(),
            &config.address,
            config.port,
            config.tls,
            &config.sasl_user,
            &config.sasl_mechanism,
            config
                .username
                .as_deref()
                .unwrap_or(&self.config.general.username),
            &config.client_cert_path,
            &config.sasl_key_path,
        ))
        .unwrap();
        let mapping = conn.isupport_parsed.casemapping().to_string();
        if self
            .monitors
            .get(id)
            .is_none_or(|monitor| monitor.scope != scope)
        {
            self.monitors
                .insert(id.into(), MonitorState::new(scope, mapping));
        } else if let Some(monitor) = self.monitors.get_mut(id) {
            monitor.set_mapping(&mapping);
        }
        let monitor = self.monitors.get_mut(id).unwrap();
        monitor.set_metadata_caps(&conn.enabled_caps);
        monitor.set_supported(conn.isupport_parsed.get("MONITOR").is_some());
        true
    }

    pub(crate) fn reset_monitor(&mut self, id: &str) {
        if let Some(monitor) = self.monitors.get_mut(id) {
            monitor.reset_transport();
        }
    }

    fn monitor_event(&mut self, id: &str, text: &str) {
        if let Some(conn) = self.state.connections.get(id) {
            let buffer = crate::state::buffer::make_buffer_id(id, &conn.label);
            self.add_event_to_buffer(&buffer, text.replace('%', "%%"));
        }
    }

    pub(crate) fn observe_monitor(&mut self, id: &str, message: &Message) -> bool {
        if !self.monitors.contains_key(id) || !self.ensure_monitor(id) {
            return false;
        }
        let (handled, output) = self.monitors.get_mut(id).unwrap().receive(message);
        if !handled {
            self.monitors.get_mut(id).unwrap().observe_extended(message);
        }
        for line in output {
            self.monitor_event(id, &line);
        }
        handled
    }

    pub(crate) fn tick_monitors(&mut self) {
        self.monitors
            .retain(|id, _| self.state.connections.contains_key(id));
        let ids: Vec<_> = self.monitors.keys().cloned().collect();
        for id in ids {
            if !self.ensure_monitor(&id) {
                continue;
            }
            if self.monitors.get_mut(&id).unwrap().expire() {
                self.monitor_event(&id, "MONITOR list timed out; cached data is retained. Reconnect if its end reply does not arrive.");
            }
            let conn = &self.state.connections[&id];
            if conn.status != crate::state::connection::ConnectionStatus::Connected
                || conn.bouncer_control()
                || conn.isupport_parsed.get("MONITOR").is_none()
            {
                continue;
            }
            for _ in 0..2 {
                let Some(command) = self.monitors.get_mut(&id).unwrap().next_command() else {
                    break;
                };
                let sent = self
                    .irc_handles
                    .get(&id)
                    .is_some_and(|handle| handle.sender().send(command.clone()).is_ok());
                self.monitors.get_mut(&id).unwrap().sent(&command, sent);
                if !sent {
                    break;
                }
            }
        }
    }
}

pub fn command(app: &mut super::App, args: &[String]) {
    use crate::commands::helpers::add_local_event;
    let Some(id) = app.active_conn_id().map(str::to_string) else {
        add_local_event(app, "No active connection");
        return;
    };
    if app
        .state
        .connections
        .get(&id)
        .is_some_and(crate::state::connection::Connection::bouncer_control)
    {
        add_local_event(
            app,
            "Use /monitor on a network connection, not the bouncer control connection",
        );
        return;
    }
    if !app.ensure_monitor(&id) {
        return;
    }
    let action = args
        .first()
        .map_or("list", String::as_str)
        .to_ascii_lowercase();
    let change = match action.as_str() {
        "+" | "add" | "-" | "remove" => {
            let names = match crate::irc::monitor::targets(&args[1..]) {
                Ok(names) => names,
                Err(error) => {
                    add_local_event(app, error);
                    return;
                }
            };
            Some(if matches!(action.as_str(), "+" | "add") {
                Change::Add(names)
            } else {
                Change::Remove(names)
            })
        }
        "clear" | "c" if args.len() == 1 => Some(Change::Clear),
        "list" | "l" if args.len() <= 1 => {
            app.monitors.get_mut(&id).unwrap().show_list = true;
            None
        }
        "status" | "s" if args.len() == 1 => {
            app.monitors.get_mut(&id).unwrap().request_status = true;
            None
        }
        "show" if args.len() == 1 => {
            for row in app.monitors[&id].rows() {
                app.monitor_event(&id, &row);
            }
            return;
        }
        _ => {
            add_local_event(
                app,
                "Usage: /monitor [add NICKS|remove NICKS|clear|list|status|show]",
            );
            return;
        }
    };
    if let Some(change) = change {
        if let Err(error) = app.monitors.get_mut(&id).unwrap().change(change) {
            add_local_event(app, error);
            return;
        }
        add_local_event(app, "MONITOR targets queued for synchronization");
    }
    if app.state.connections[&id]
        .isupport_parsed
        .get("MONITOR")
        .is_none()
    {
        add_local_event(
            app,
            "MONITOR is not currently advertised; queued targets will wait for support",
        );
    }
    app.tick_monitors();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receive(app: &mut crate::app::App, id: &str, wire: &str) {
        app.handle_irc_event(crate::irc::IrcEvent::Message(
            id.into(),
            Box::new(wire.parse().unwrap()),
        ));
    }

    fn app() -> crate::app::App {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        for id in ["one", "two"] {
            let config = toml::from_str(
                "label='Server'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nnick='me'",
            )
            .unwrap();
            app.setup_connection(id, &config);
            app.state.connections.get_mut(id).unwrap().status =
                crate::state::connection::ConnectionStatus::Connected;
            app.irc_handles.insert(
                id.into(),
                crate::irc::IrcHandle::new(
                    id.into(),
                    crate::irc::IrcSender::capturing(0),
                    None,
                    None,
                ),
            );
            receive(
                &mut app,
                id,
                ":server 005 me MONITOR=100 CASEMAPPING=rfc1459 :supported",
            );
        }
        app.state.set_active_buffer("one/server");
        app
    }

    #[tokio::test]
    async fn web_input_uses_its_network_and_retains_metadata_without_shared_channels() {
        let mut app = app();
        app.handle_web_command(
            crate::web::protocol::WebCommand::RunCommand {
                buffer_id: "two/server".into(),
                text: "/monitor add Alice".into(),
            },
            "browser",
        );
        assert!(app.monitors.contains_key("two"));
        assert!(!app.monitors.contains_key("one"));
        receive(&mut app, "two", ":server 733 me :End");
        app.tick_monitors();
        receive(&mut app, "two", ":server 730 me :Alice!u@h");
        receive(&mut app, "two", ":server 732 me :Alice");
        receive(&mut app, "two", ":server 733 me :End");
        receive(&mut app, "two", ":Alice!u@h ACCOUNT alice");
        receive(&mut app, "two", ":Alice!u@h SETNAME :Alice %N Name");
        assert_eq!(app.monitors["two"].peers["alice"].online, Some(true));
        assert_eq!(
            app.monitors["two"].peers["alice"].account.as_deref(),
            Some("alice")
        );
        assert_eq!(
            app.monitors["two"].peers["alice"].realname.as_deref(),
            Some("Alice %N Name")
        );
        receive(&mut app, "one", ":server 731 me :Alice");
        assert_eq!(app.monitors["two"].peers["alice"].online, Some(true));
    }

    #[tokio::test]
    async fn account_scope_change_discards_old_targets_before_any_resubscription() {
        for selector in 0..5 {
            let mut app = app();
            command(&mut app, &["add".into(), "Alice".into()]);
            receive(&mut app, "one", ":server 733 me :End");
            app.tick_monitors();
            receive(&mut app, "one", ":server 732 me :Alice");
            receive(&mut app, "one", ":server 733 me :End");
            let config = &mut app.state.connections.get_mut("one").unwrap().origin_config;
            match selector {
                0 => config.sasl_user = Some("different-account".into()),
                1 => config.username = Some("different-user".into()),
                2 => config.sasl_mechanism = Some("EXTERNAL".into()),
                3 => config.client_cert_path = Some("fixture-other.pem".into()),
                _ => config.sasl_key_path = Some("fixture-other-key.pem".into()),
            }
            app.tick_monitors();
            assert!(app.monitors["one"].peers.is_empty());
            receive(&mut app, "one", ":server 733 me :End");
            assert!(app.monitors["one"].synchronized());
            assert_eq!(app.monitors["one"].rows(), vec!["MONITOR list is empty"]);
        }
    }
}

#[cfg(test)]
#[tokio::test]
#[ignore = "requires a disposable pinned bouncer fixture"]
#[expect(clippy::too_many_lines)]
async fn pinned_bouncer_monitor() {
    async fn until(
        apps: &mut [&mut crate::app::App],
        predicate: impl Fn(&crate::app::App) -> bool + Send + Sync,
    ) {
        let result = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                for app in apps.iter_mut() {
                    while let Ok(event) = app.irc_rx.try_recv() {
                        app.handle_irc_event(event);
                    }
                    app.tick_monitors();
                }
                if apps.iter().all(|app| predicate(app)) {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(
            result.is_ok(),
            "MONITOR bouncer response did not arrive: {:?}",
            apps.iter()
                .map(|app| (
                    app.state.connections["fixture"]
                        .isupport_parsed
                        .get("MONITOR"),
                    app.state.connections["fixture"].enabled_caps.clone(),
                    app.monitors.get("fixture").map(MonitorState::rows),
                ))
                .collect::<Vec<_>>()
        );
    }
    fn web(app: &mut crate::app::App, text: &str) {
        app.handle_web_command(
            crate::web::protocol::WebCommand::RunCommand {
                buffer_id: "fixture/fixture".into(),
                text: text.into(),
            },
            "browser",
        );
    }
    fn control(app: &crate::app::App, text: &str) {
        app.irc_handles["fixture"]
            .sender()
            .send(irc::proto::Command::PRIVMSG(
                "FixtureControl".into(),
                text.into(),
            ))
            .unwrap();
    }
    let mut first = monitor_fixture_client();
    let mut second = monitor_fixture_client();
    until(&mut [&mut first, &mut second], |app| {
        app.state.connections["fixture"]
            .isupport_parsed
            .get("MONITOR")
            .is_some()
    })
    .await;
    web(&mut first, "/monitor add Alice");
    command(&mut second, &["add".into(), "Bob".into()]);
    until(&mut [&mut first, &mut second], |app| {
        app.monitors.get("fixture").is_some_and(|state| {
            state.peers.len() == 1
                && state.peers.values().all(|peer| peer.online.is_some())
                && state.synchronized()
        })
    })
    .await;
    assert_eq!(first.monitors["fixture"].peers["alice"].online, Some(true));
    assert_eq!(second.monitors["fixture"].peers["bob"].online, Some(false));
    until(&mut [&mut first], |app| {
        let peer = &app.monitors["fixture"].peers["alice"];
        peer.account.as_deref() == Some("alice-account")
            && peer.away == Some(true)
            && peer.host.as_deref() == Some("changed.example")
    })
    .await;
    assert!(
        first.state.connections["fixture"]
            .enabled_caps
            .iter()
            .any(|cap| matches!(cap.as_str(), "extended-monitor" | "draft/extended-monitor"))
    );
    if std::env::var("REPARTEE_PRESENCE_PROVIDER").unwrap() == "soju" {
        until(&mut [&mut first], |app| {
            app.monitors["fixture"].peers["alice"].realname.as_deref() == Some("Alice Fixture")
        })
        .await;
    }
    web(&mut first, "/monitor list");
    web(&mut second, "/monitor list");
    until(&mut [&mut first, &mut second], |app| {
        !app.monitors["fixture"].show_list
    })
    .await;
    assert_eq!(first.monitors["fixture"].peers.len(), 1);
    assert_eq!(second.monitors["fixture"].peers.len(), 1);
    web(&mut first, "/monitor add Rejected");
    until(&mut [&mut first], |app| {
        app.state.buffers["fixture/fixture"]
            .messages
            .iter()
            .any(|message| message.text.contains("rejected because the list is full"))
            && !app.monitors["fixture"].peers.contains_key("rejected")
            && app.monitors["fixture"].synchronized()
    })
    .await;
    web(&mut first, "/monitor list");
    until(&mut [&mut first], |app| !app.monitors["fixture"].show_list).await;
    assert_eq!(first.monitors["fixture"].peers.len(), 1);
    web(&mut first, "/disconnect");
    until(&mut [&mut first], |app| {
        app.state.connections["fixture"].status
            == crate::state::connection::ConnectionStatus::Disconnected
    })
    .await;
    assert_eq!(first.monitors["fixture"].peers["alice"].online, None);
    let config = first.state.connections["fixture"].origin_config.clone();
    first.start_connection_attempt("fixture", config);
    until(&mut [&mut first], |app| {
        app.monitors["fixture"]
            .peers
            .get("alice")
            .is_some_and(|peer| peer.online == Some(true))
            && app.monitors["fixture"].synchronized()
    })
    .await;
    assert_eq!(first.monitors["fixture"].peers.len(), 1);
    control(&first, "account-off");
    until(&mut [&mut first, &mut second], |app| {
        !app.state.connections["fixture"]
            .enabled_caps
            .contains("account-notify")
    })
    .await;
    assert!(first.monitors["fixture"].peers["alice"].account.is_none());
    control(&first, "account-on");
    until(&mut [&mut first, &mut second], |app| {
        app.state.connections["fixture"]
            .enabled_caps
            .contains("account-notify")
    })
    .await;
    control(&first, "account-change");
    until(&mut [&mut first], |app| {
        app.monitors["fixture"].peers["alice"].account.as_deref() == Some("restored-account")
    })
    .await;
    assert!(!second.monitors["fixture"].peers.contains_key("alice"));
    control(&first, "monitor-off");
    if std::env::var("REPARTEE_PRESENCE_PROVIDER").unwrap() == "soju" {
        until(&mut [&mut first, &mut second], |app| {
            app.state.connections["fixture"]
                .isupport_parsed
                .get("MONITOR")
                .is_none()
        })
        .await;
        assert_eq!(first.monitors["fixture"].peers["alice"].online, None);
    } else {
        until(&mut [&mut first, &mut second], |app| {
            app.state.buffers.values().any(|buffer| {
                buffer
                    .messages
                    .iter()
                    .any(|message| message.text.contains("control-complete monitor-off"))
            })
        })
        .await;
        assert!(
            first.state.connections["fixture"]
                .isupport_parsed
                .get("MONITOR")
                .is_some()
        );
        assert!(
            second.state.connections["fixture"]
                .isupport_parsed
                .get("MONITOR")
                .is_some()
        );
    }
    control(&first, "monitor-on");
    until(&mut [&mut first, &mut second], |app| {
        app.state.buffers.values().any(|buffer| {
            buffer
                .messages
                .iter()
                .any(|message| message.text.contains("control-complete monitor-on"))
        }) && app.monitors["fixture"].synchronized()
            && app.monitors["fixture"]
                .peers
                .values()
                .all(|peer| peer.online.is_some())
    })
    .await;
    web(&mut first, "/monitor clear");
    until(&mut [&mut first], |app| {
        app.monitors["fixture"].peers.is_empty()
    })
    .await;
    assert_eq!(second.monitors["fixture"].peers["bob"].online, Some(false));
    for app in [&mut first, &mut second] {
        app.suspend_bouncer_children("fixture");
        app.cancel_connection_attempt("fixture");
        app.irc_handles.remove("fixture");
    }
}

#[cfg(test)]
fn monitor_fixture_client() -> crate::app::App {
    let mut app = crate::app::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    let mut config: crate::config::ServerConfig = toml::from_str("label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nbouncer_network_id='1'").unwrap();
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

#[cfg(test)]
#[tokio::test]
#[ignore = "requires a disposable pinned bouncer fixture"]
async fn pinned_bouncer_no_monitor() {
    let mut app = monitor_fixture_client();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            while let Ok(event) = app.irc_rx.try_recv() {
                app.handle_irc_event(event);
            }
            if app.state.connections["fixture"].status
                == crate::state::connection::ConnectionStatus::Connected
                && app.state.connections["fixture"]
                    .isupport_parsed
                    .get("CHANTYPES")
                    .is_some()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        app.state.connections["fixture"]
            .isupport_parsed
            .get("MONITOR")
            .is_none()
    );
    command(&mut app, &["add".into(), "Alice".into()]);
    assert!(app.monitors["fixture"].peers.is_empty());
    assert!(
        app.state.buffers["fixture/fixture"]
            .messages
            .iter()
            .any(|message| message.text.contains("not currently advertised"))
    );
    app.irc_handles["fixture"]
        .sender()
        .send(irc::proto::Command::MONITOR("L".into(), None))
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            while let Ok(event) = app.irc_rx.try_recv() {
                app.handle_irc_event(event);
            }
            app.tick_monitors();
            if app.state.buffers["fixture/fixture"]
                .messages
                .iter()
                .any(|message| message.text.contains("MONITOR is currently unavailable"))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(app.monitors["fixture"].peers.is_empty());
    app.cancel_connection_attempt("fixture");
    app.irc_handles.remove("fixture");
}

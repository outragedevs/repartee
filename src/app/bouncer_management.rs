use std::time::{Duration, Instant};

use irc::proto::{Command, Message};

use super::App;

pub struct Pending {
    operation: &'static str,
    network_id: Option<String>,
    started: Instant,
    timed_out: bool,
}

impl App {
    pub(crate) fn submit_bouncer_mutation(&mut self, id: &str, action: &str, tail: &str) {
        use crate::commands::helpers::add_local_event;
        if !self.bouncer_networks.contains_key(id)
            || !self.state.connections.get(id).is_some_and(|conn| {
                conn.status == crate::state::connection::ConnectionStatus::Connected
                    && conn
                        .enabled_caps
                        .contains(crate::irc::bouncer::NETWORKS_CAP)
            })
        {
            add_local_event(
                self,
                "Use a connected bouncer control connection for network management",
            );
            return;
        }
        if self.bouncer_mutations.contains_key(id) {
            add_local_event(
                self,
                "A network operation is unresolved; wait for its reply or reconnect and inspect /bouncer list",
            );
            return;
        }
        let mutation = crate::irc::bouncer::mutations::parse(action, tail, |key| {
            crate::config::load_env(&crate::constants::env_path())
                .ok()?
                .get(key)
                .cloned()
        });
        let mutation = match mutation {
            Ok(mutation) => mutation,
            Err(error) => {
                add_local_event(self, &error);
                return;
            }
        };
        let sent = self
            .irc_handles
            .get(id)
            .is_some_and(|handle| handle.sender().send(mutation.command).is_ok());
        if !sent {
            add_local_event(self, "Could not submit the bouncer network operation");
            return;
        }
        self.bouncer_mutations.insert(
            id.to_string(),
            Pending {
                operation: mutation.operation,
                network_id: mutation.network_id,
                started: Instant::now(),
                timed_out: false,
            },
        );
        self.bouncer_mutation_event(
            id,
            "Network operation submitted; waiting for the bouncer's reply",
        );
    }

    fn bouncer_mutation_event(&mut self, id: &str, text: &str) {
        if let Some(conn) = self.state.connections.get(id) {
            let buffer = crate::state::buffer::make_buffer_id(id, &conn.label);
            self.add_event_to_buffer(&buffer, text.to_string());
        }
    }

    pub(crate) fn handle_bouncer_mutation(&mut self, id: &str, message: &Message) -> bool {
        let Command::Raw(command, args) = &message.command else {
            return false;
        };
        let Some(pending) = self.bouncer_mutations.get(id) else {
            return false;
        };
        let success = command.eq_ignore_ascii_case("BOUNCER")
            && args.len() == 2
            && args[0].eq_ignore_ascii_case(pending.operation)
            && crate::irc::bouncer::normalize_network_id(&args[1]).is_ok_and(|network_id| {
                pending
                    .network_id
                    .as_ref()
                    .is_none_or(|expected| expected == &network_id)
            });
        let failure = command.eq_ignore_ascii_case("FAIL")
            && args.len() >= 4
            && args[0].eq_ignore_ascii_case("BOUNCER")
            && args[2].eq_ignore_ascii_case(pending.operation);
        if !success && !failure {
            return false;
        }
        let operation = pending.operation;
        self.bouncer_mutations.remove(id);
        if success {
            self.bouncer_mutation_event(
                id,
                &format!("Bouncer confirmed {operation} for network {}", args[1]),
            );
            if let Some(handle) = self.irc_handles.get(id) {
                let _ = handle
                    .sender()
                    .send(Command::Raw("BOUNCER".into(), vec!["LISTNETWORKS".into()]));
            }
        } else {
            let reason = match args[1].as_str() {
                "UNKNOWN_COMMAND" => "operation unsupported; Lurker manages networks in its web UI",
                "INVALID_NETID" => "network ID not found",
                "UNKNOWN_ATTRIBUTE" | "INVALID_ATTRIBUTE" | "READ_ONLY_ATTRIBUTE" => {
                    "attribute rejected"
                }
                "NEED_ATTRIBUTE" => "required attribute missing",
                _ => "request rejected",
            };
            self.bouncer_mutation_event(id, &format!("Bouncer rejected {operation}: {reason}"));
        }
        true
    }

    pub(crate) fn disconnect_bouncer_mutation(&mut self, id: &str) {
        if self.bouncer_mutations.remove(id).is_some() {
            self.bouncer_mutation_event(id, "Connection lost with a network operation unresolved; inspect the network list after reconnect before retrying");
        }
    }

    pub(crate) fn tick_bouncer_mutations(&mut self) {
        self.bouncer_mutations
            .retain(|id, _| self.state.connections.contains_key(id));
        let mut expired = Vec::new();
        for (id, pending) in &mut self.bouncer_mutations {
            if !pending.timed_out && pending.started.elapsed() >= Duration::from_secs(30) {
                pending.timed_out = true;
                expired.push(id.clone());
            }
        }
        for id in expired {
            self.bouncer_mutation_event(&id, "No reply to the network operation; its outcome is unknown. It will not be retried automatically. Inspect /bouncer list before reconnecting to retry.");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        for id in ["one", "two"] {
            let config = toml::from_str("label='Control'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nbouncer_control=true").unwrap();
            app.setup_connection(id, &config);
            let conn = app.state.connections.get_mut(id).unwrap();
            conn.status = crate::state::connection::ConnectionStatus::Connected;
            conn.enabled_caps
                .insert(crate::irc::bouncer::NETWORKS_CAP.into());
            app.bouncer_networks
                .insert(id.into(), crate::irc::bouncer::NetworkRegistry::default());
            app.irc_handles.insert(
                id.into(),
                crate::irc::IrcHandle::new(
                    id.into(),
                    crate::irc::IrcSender::capturing(0),
                    None,
                    None,
                ),
            );
        }
        app
    }

    #[tokio::test]
    async fn replies_are_scoped_and_never_optimistically_change_registry() {
        let mut app = app();
        app.submit_bouncer_mutation("one", "change", "1 name='New Name'");
        app.submit_bouncer_mutation("two", "change", "1 name='Other Name'");
        assert!(app.bouncer_networks["one"].networks.is_empty());
        assert!(!app.handle_bouncer_mutation("one", &"BOUNCER CHANGENETWORK 2".parse().unwrap()));
        assert!(app.handle_bouncer_mutation("one", &"BOUNCER CHANGENETWORK 1".parse().unwrap()));
        assert!(!app.bouncer_mutations.contains_key("one"));
        assert!(app.bouncer_mutations.contains_key("two"));
        let frames = app.irc_handles["one"].sender().captured();
        assert_eq!(frames.len(), 2);
        assert!(frames[1].to_string().contains("LISTNETWORKS"));
        assert!(app.bouncer_networks["one"].networks.is_empty());
    }

    #[tokio::test]
    async fn timeout_blocks_ambiguous_retries_but_accepts_late_reply() {
        let mut app = app();
        app.submit_bouncer_mutation("one", "add", "host=irc.example");
        app.bouncer_mutations.get_mut("one").unwrap().started =
            Instant::now().checked_sub(Duration::from_secs(31)).unwrap();
        app.tick_bouncer_mutations();
        app.tick_bouncer_mutations();
        assert!(app.bouncer_mutations["one"].timed_out);
        app.submit_bouncer_mutation("one", "add", "host=another.example");
        assert_eq!(app.irc_handles["one"].sender().captured().len(), 1);
        assert!(app.handle_bouncer_mutation("one", &"BOUNCER ADDNETWORK 4".parse().unwrap()));
        assert!(!app.bouncer_mutations.contains_key("one"));
    }

    #[tokio::test]
    async fn actual_soju_and_lurker_failure_shapes_retire_pending() {
        let mut app = app();
        for reply in [
            "FAIL BOUNCER UNKNOWN_ATTRIBUTE CHANGENETWORK nonsense :Unknown attribute",
            "FAIL BOUNCER UNKNOWN_COMMAND CHANGENETWORK :Manage networks in the Lurker web UI",
            "FAIL BOUNCER INVALID_NETID CHANGENETWORK 1 :Invalid network ID",
        ] {
            app.submit_bouncer_mutation("one", "change", "1 name=x");
            assert!(app.handle_bouncer_mutation("one", &reply.parse().unwrap()));
            assert!(!app.bouncer_mutations.contains_key("one"));
        }
    }

    #[tokio::test]
    async fn send_failure_and_disconnect_do_not_replay_mutations() {
        let mut app = app();
        app.irc_handles.insert(
            "one".into(),
            crate::irc::IrcHandle::new(
                "one".into(),
                crate::irc::IrcSender::capturing_then_failing(0),
                None,
                None,
            ),
        );
        app.submit_bouncer_mutation("one", "delete", "1");
        assert!(!app.bouncer_mutations.contains_key("one"));
        app.submit_bouncer_mutation("two", "delete", "1");
        app.disconnect_bouncer_mutation("two");
        app.tick_bouncer_mutations();
        assert!(!app.bouncer_mutations.contains_key("two"));
        assert_eq!(app.irc_handles["two"].sender().captured().len(), 1);
    }
}

#[cfg(test)]
#[tokio::test]
#[ignore = "requires a disposable pinned bouncer fixture"]
#[expect(clippy::too_many_lines)]
async fn pinned_bouncer_network_management() {
    async fn until(app: &mut App, predicate: impl Fn(&App) -> bool + Send + Sync) {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                while let Ok(event) = app.irc_rx.try_recv() {
                    app.handle_irc_event(event);
                }
                app.tick_bouncer_mutations();
                if predicate(app) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("bouncer mutation fixture timed out");
    }
    fn web_command(app: &mut App, text: &str) {
        app.handle_web_command(
            crate::web::protocol::WebCommand::RunCommand {
                buffer_id: "fixture/fixture".into(),
                text: text.into(),
            },
            "browser",
        );
    }
    let mut app = crate::app::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    let mut config: crate::config::ServerConfig = toml::from_str(
        "label='fixture'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nbouncer_control=true",
    ).unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT")
        .unwrap()
        .parse()
        .unwrap();
    config.sasl_user = Some(std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap());
    config.sasl_pass = Some("fixture-password".into());
    app.setup_connection("fixture", &config);
    app.state.set_active_buffer("fixture/fixture");
    app.start_connection_attempt("fixture", config);
    until(&mut app, |app| {
        app.bouncer_networks
            .get("fixture")
            .is_some_and(|registry| registry.complete)
    })
    .await;
    let original = app.bouncer_networks["fixture"].networks.clone();
    if std::env::var("REPARTEE_BOUNCER_TEST_PROVIDER").unwrap() == "lurker" {
        for command in [
            "/bouncer add host=127.0.0.1 port=1 tls=0",
            "/bouncer change 1 name=x",
            "/bouncer delete 1",
        ] {
            web_command(&mut app, command);
            assert!(app.bouncer_mutations.contains_key("fixture"));
            until(&mut app, |app| {
                !app.bouncer_mutations.contains_key("fixture")
            })
            .await;
            assert_eq!(app.bouncer_networks["fixture"].networks, original);
        }
        assert!(
            app.state.buffers["fixture/fixture"]
                .messages
                .iter()
                .any(|message| message.text.contains("Lurker manages networks"))
        );
    } else {
        web_command(
            &mut app,
            r#"/bouncer add host=127.0.0.1 port=1 tls=0 name="Test #1; Network""#,
        );
        assert!(app.bouncer_mutations.contains_key("fixture"));
        until(&mut app, |app| {
            !app.bouncer_mutations.contains_key("fixture")
                && app.bouncer_networks["fixture"].networks.len() == original.len() + 1
        })
        .await;
        let id = app.bouncer_networks["fixture"]
            .networks
            .keys()
            .find(|id| !original.contains_key(*id))
            .unwrap()
            .clone();
        assert_eq!(
            app.bouncer_networks["fixture"].networks[&id].name(),
            "Test #1; Network"
        );
        web_command(
            &mut app,
            &format!(r#"/bouncer change {id} name="Renamed Network" realname='A\B; C'"#),
        );
        until(&mut app, |app| {
            !app.bouncer_mutations.contains_key("fixture")
                && app.bouncer_networks["fixture"].networks[&id].name() == "Renamed Network"
        })
        .await;
        assert_eq!(
            app.bouncer_networks["fixture"].networks[&id].attributes["realname"],
            "A\\B; C"
        );
        let snapshot = crate::web::snapshot::build_sync_init(
            &app.state,
            0,
            "%H:%M",
            false,
            false,
            &app.config.statusbar,
        );
        let crate::web::protocol::WebEvent::SyncInit { connections, .. } = snapshot else {
            panic!("expected snapshot");
        };
        assert!(
            connections
                .iter()
                .any(|connection| connection.label.contains("Renamed Network"))
        );
        web_command(&mut app, &format!("/bouncer delete {id}"));
        until(&mut app, |app| {
            !app.bouncer_mutations.contains_key("fixture")
                && !app.bouncer_networks["fixture"].networks.contains_key(&id)
        })
        .await;
        assert!(
            !app.bouncer_children
                .values()
                .any(|child| child.network.id == id)
        );
        assert_eq!(
            app.bouncer_networks["fixture"].networks.len(),
            original.len()
        );
    }
    app.suspend_bouncer_children("fixture");
    app.cancel_connection_attempt("fixture");
    app.irc_handles.remove("fixture");
}

use crate::irc::bouncer::{Network, RegistryEvent};
use crate::state::buffer::make_buffer_id;

impl super::App {
    pub(crate) fn handle_bouncer_network_message(
        &mut self,
        conn_id: &str,
        message: &::irc::proto::Message,
    ) -> bool {
        let Some(registry) = self.bouncer_networks.get_mut(conn_id) else {
            return false;
        };
        let event = registry.handle(message);
        let changed = matches!(event, RegistryEvent::Snapshot | RegistryEvent::Changed(_));
        let text = match &event {
            RegistryEvent::Unrelated => return false,
            RegistryEvent::Pending => return true,
            RegistryEvent::Snapshot => format!(
                "Bouncer network list received: {} networks. Use /bouncer list.",
                registry.networks.len()
            ),
            RegistryEvent::Changed(id) => registry
                .networks
                .get(id)
                .map_or_else(|| format!("Bouncer network {id} removed"), network_summary),
            RegistryEvent::Invalid => {
                "Invalid bouncer network update ignored; the previous list was retained".to_string()
            }
        };
        if let Some(connection) = self.state.connections.get(conn_id) {
            let buffer = make_buffer_id(conn_id, &connection.label);
            self.add_event_to_buffer(&buffer, text);
        }
        if changed {
            self.reconcile_bouncer_children(conn_id, &event);
        }
        true
    }
}

pub(super) fn network_summary(network: &Network) -> String {
    let state = network
        .attributes
        .get("state")
        .map_or("unknown", String::as_str);
    let error = network
        .attributes
        .get("error")
        .map_or(String::new(), |error| format!(" — {error}"));
    format!("{}: {} ({state}){error}", network.id, network.name())
}

pub fn command(app: &mut super::App, args: &[String]) {
    use crate::commands::helpers::add_local_event;
    let Some(id) = app.active_conn_id().map(str::to_string) else {
        add_local_event(app, "No active connection");
        return;
    };
    let id = app.bouncer_children.get(&id).map_or_else(|| id.clone(), |child| child.parent.clone());
    if args.first().is_some_and(|action| matches!(action.as_str(), "add" | "change" | "delete")) {
        app.submit_bouncer_mutation(&id, &args[0], &args[1..].join(" "));
        return;
    }
    if args.first().is_some_and(|arg| arg == "connect") && args.len() == 2 {
        if app.reconnect_bouncer_child(&id, &args[1]) {
            add_local_event(app, "Connecting to the bouncer network");
        } else {
            add_local_event(app, "Network unavailable or already connecting/connected");
        }
        return;
    }
    let Some(registry) = app.bouncer_networks.get_mut(&id) else {
        add_local_event(
            app,
            "Use /bouncer on a connected bouncer control connection",
        );
        return;
    };
    registry.expire();
    match args.first().map_or("list", String::as_str) {
        "list" if args.len() <= 1 => {
            let mut rows: Vec<String> = registry.networks.values().map(network_summary).collect();
            if rows.is_empty() {
                rows.push(
                    if registry.complete {
                        "The bouncer has no configured networks"
                    } else {
                        "Waiting for the bouncer network list"
                    }
                    .into(),
                );
            }
            for row in rows {
                add_local_event(app, &row);
            }
        }
        "refresh" if args.len() == 1 => {
            let result = app.irc_handles.get(&id).map(|handle| {
                handle.sender().send(::irc::proto::Command::Raw(
                    "BOUNCER".into(),
                    vec!["LISTNETWORKS".into()],
                ))
            });
            if !matches!(result, Some(Ok(()))) {
                add_local_event(app, "Could not request the bouncer network list");
            }
        }
        _ => add_local_event(app, "Usage: /bouncer [list|refresh|connect ID|add ATTRS|change ID ATTRS|delete ID]"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::irc::{IrcEvent, IrcHandle, IrcSender, bouncer::NETWORKS_NOTIFY_CAP};

    #[tokio::test]
    async fn control_connect_requests_only_without_notify_and_reconnect_clears_cache() {
        for notify in [false, true] {
            let mut app = crate::app::input::submit_typing_tests::test_app();
            let mut connection = crate::state::events::tests::make_test_connection();
            connection.origin_config.bouncer_control = true;
            connection.origin_config.channels = vec!["#configured".into()];
            connection.joined_channels = vec!["#stale".into()];
            app.state.add_connection(connection);
            let sender = IrcSender::capturing(0);
            app.irc_handles.insert(
                "libera".into(),
                IrcHandle::new("libera".into(), sender.clone(), None, None),
            );
            let caps = if notify {
                HashSet::from([NETWORKS_NOTIFY_CAP.into()])
            } else {
                HashSet::new()
            };
            for _ in 0..2 {
                app.handle_irc_event(IrcEvent::Connected("libera".into(), caps.clone(), None));
                assert!(app.bouncer_networks["libera"].networks.is_empty());
                app.handle_bouncer_network_message(
                    "libera",
                    &"BOUNCER NETWORK 1 name=First".parse().unwrap(),
                );
            }
            let sent = sender.captured();
            assert_eq!(
                sent.iter()
                    .filter(|message| message.to_string().contains("BOUNCER LISTNETWORKS"))
                    .count(),
                if notify { 0 } else { 2 }
            );
            assert!(
                sent.iter()
                    .all(|message| !matches!(message.command, ::irc::proto::Command::JOIN(..)))
            );
            app.handle_irc_event(IrcEvent::Disconnected("libera".into(), None));
            assert!(!app.bouncer_networks.contains_key("libera"));
        }
    }

    #[test]
    fn equal_network_ids_on_different_accounts_remain_isolated() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        for account in ["one", "two"] {
            app.bouncer_networks.insert(
                account.into(),
                crate::irc::bouncer::NetworkRegistry::default(),
            );
            let message = format!("BOUNCER NETWORK 42 name={account}")
                .parse()
                .unwrap();
            assert!(app.handle_bouncer_network_message(account, &message));
        }
        app.handle_bouncer_network_message("one", &"BOUNCER NETWORK 42 *".parse().unwrap());
        assert!(app.bouncer_networks["one"].networks.is_empty());
        assert_eq!(app.bouncer_networks["two"].networks["42"].name(), "two");
    }
    #[test]
    fn equal_network_names_keep_separate_storage_and_e2e_scopes_after_rename() {
        use crate::e2e::keyring::{ChannelConfig, ChannelMode};
        use crate::state::buffer::{Buffer, BufferType};
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let server: crate::config::ServerConfig = toml::from_str(
            "label = 'Same'\naddress = 'bnc.example.org'\nport = 6697\ntls = true\nchannels = []\nbouncer_network_id = '42'",
        ).unwrap();
        for account in ["one", "two"] {
            app.setup_connection(account, &server);
            app.state
                .add_buffer(Buffer::for_test(account, BufferType::Channel, "#secret"));
        }
        let one = app.state.connections["one"].network_key().to_string();
        let two = app.state.connections["two"].network_key().to_string();
        assert_ne!(one, two);
        app.state
            .e2e_manager
            .as_ref()
            .unwrap()
            .keyring()
            .set_channel_config(&ChannelConfig {
                channel: crate::e2e::scoped_context(&one, "#secret"),
                enabled: true,
                mode: ChannelMode::Normal,
            })
            .unwrap();
        assert!(app.state.e2e_enabled_for_target("one", "#secret"));
        assert!(!app.state.e2e_enabled_for_target("two", "#secret"));
        app.state.connections.get_mut("one").unwrap().label = "Renamed".into();
        assert!(app.state.e2e_enabled_for_target("one", "#secret"));
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        app.state.log_tx = Some(tx);
        for account in ["one", "two"] {
            let message = crate::state::events::tests::make_test_message(&mut app.state, "hello");
            app.state
                .add_message(&format!("{account}/#secret"), message);
            assert!(rx.try_recv().is_err());
        }
        let snapshot = crate::web::snapshot::build_sync_init(
            &app.state,
            0,
            "%H:%M",
            false,
            &app.config.statusbar,
        );
        let encoded = serde_json::to_string(&snapshot).unwrap();
        assert!(encoded.contains("Renamed"));
        assert!(encoded.contains("Same"));
        assert!(!encoded.contains("bouncer:v1:"));
    }
    #[test]
    fn old_bouncer_label_requires_an_explicit_e2e_decision_before_sending() {
        use crate::e2e::keyring::{ChannelConfig, ChannelMode};
        use crate::state::buffer::{Buffer, BufferType};
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let server: crate::config::ServerConfig = toml::from_str(
            "label = 'Renamed'\naddress = 'bnc.example.org'\nport = 6697\ntls = true\nchannels = []\nbouncer_network_id = '42'",
        ).unwrap();
        let mut direct = server.clone();
        direct.label = "Old".into();
        direct.bouncer_network_id = None;
        app.config.servers.insert("direct".into(), direct);
        app.setup_connection("account", &server);
        let network = app.state.connections["account"].network_key().to_string();
        let manager = app.state.e2e_manager.clone().unwrap();
        for (target, kind, wire) in [
            ("#private", BufferType::Channel, "#private"),
            ("bob", BufferType::Query, "@~bob@host"),
        ] {
            let buffer_id = format!("account/{target}");
            let mut buffer = Buffer::for_test("account", kind.clone(), target);
            buffer.peer_handle = Some("~bob@new-host".into());
            manager
                .keyring()
                .cache_dm_handle("Old", "bob", "~bob@host")
                .unwrap();
            app.state.add_buffer(buffer);
            manager
                .keyring()
                .set_channel_config(&ChannelConfig {
                    channel: crate::e2e::scoped_context("Old", wire),
                    enabled: true,
                    mode: ChannelMode::Normal,
                })
                .unwrap();
            manager
                .keyring()
                .set_channel_config(&ChannelConfig {
                    channel: crate::e2e::scoped_context("Another orphan", wire),
                    enabled: false,
                    mode: ChannelMode::Normal,
                })
                .unwrap();
            assert!(app.state.e2e_possible_for_target("account", target));
            assert!(matches!(
                app.state.e2e_encrypt_or_passthrough(
                    &buffer_id,
                    target,
                    &kind,
                    "private content",
                    None
                ),
                Err(crate::app::e2e_gate::E2eRefusal::BouncerScopeChanged)
            ));
            for text in [".private", "!private"] {
                assert!(matches!(
                    app.state
                        .e2e_encrypt_or_passthrough(&buffer_id, target, &kind, text, None),
                    Err(crate::app::e2e_gate::E2eRefusal::BouncerScopeChanged)
                ));
            }
            manager
                .keyring()
                .set_channel_config(&ChannelConfig {
                    channel: crate::e2e::scoped_context(
                        &network,
                        if kind == BufferType::Query {
                            "@~bob@new-host"
                        } else {
                            wire
                        },
                    ),
                    enabled: false,
                    mode: ChannelMode::Normal,
                })
                .unwrap();
            assert!(!app.state.e2e_possible_for_target("account", target));
            let (lines, _) = app
                .state
                .e2e_encrypt_or_passthrough(&buffer_id, target, &kind, "explicit plaintext", None)
                .unwrap_or_else(|_| panic!("explicit E2E off must allow plaintext"));
            assert_eq!(lines, vec!["explicit plaintext"]);
        }
    }
    #[test]
    fn bouncer_reconnect_preserves_effective_login_after_global_config_changes() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let server: crate::config::ServerConfig = toml::from_str(
            "label = 'Bouncer'\naddress = 'bnc.example.org'\nport = 6697\ntls = true\nchannels = []\nbouncer_network_id = '42'",
        ).unwrap();
        app.config.general.username = "first-account".into();
        app.setup_connection("account", &server);
        app.config.general.username = "second-account".into();
        let connection = &app.state.connections["account"];
        assert_eq!(connection.origin_config.username.as_deref(), Some("first-account"));
        assert_eq!(connection.network_key(), crate::config::network_scope::network_scope("account", &connection.origin_config, &app.config.general.username));
        assert_ne!(connection.network_key(), crate::config::network_scope::network_scope("account", &server, &app.config.general.username));
    }
    #[test]
    fn explicit_bouncer_dm_decision_resolves_previous_unique_handle() {
        use crate::e2e::keyring::{ChannelConfig, ChannelMode};
        use crate::state::buffer::{Buffer, BufferType};
        for decision in ["on", "off", "ambiguous"] {
            let mut app = crate::app::input::submit_typing_tests::test_app();
            let server: crate::config::ServerConfig = toml::from_str(
                "label = 'Renamed'\naddress = 'bnc.example.org'\nport = 6697\ntls = true\nchannels = []\nbouncer_network_id = '42'",
            ).unwrap();
            app.setup_connection("account", &server);
            app.state.add_buffer(Buffer::for_test("account", BufferType::Query, "bob"));
            app.state.active_buffer_id = Some("account/bob".into());
            let network = app.state.connections["account"].network_key().to_string();
            let manager = app.state.e2e_manager.clone().unwrap();
            manager.keyring().cache_dm_handle("Old", "bob", "~bob@host").unwrap();
            manager.keyring().set_channel_config(&ChannelConfig {
                channel: crate::e2e::scoped_context("Old", "@~bob@host"),
                enabled: true,
                mode: ChannelMode::Normal,
            }).unwrap();
            if decision == "ambiguous" {
                manager.keyring().cache_dm_handle("Other", "bob", "~other@host").unwrap();
            }
            app.execute_command(&crate::commands::parser::ParsedCommand { name: "e2e".into(), args: vec![if decision == "on" { "on" } else { "off" }.into()] });
            let sent = app.state.e2e_encrypt_or_passthrough("account/bob", "bob", &BufferType::Query, "private content", None);
            if decision == "ambiguous" {
                assert!(manager.keyring().cached_dm_handle("bob", &network).unwrap().is_none());
                assert!(matches!(sent, Err(crate::app::e2e_gate::E2eRefusal::BouncerScopeChanged)));
            } else {
                assert_eq!(manager.keyring().cached_dm_handle("bob", &network).unwrap().as_deref(), Some("~bob@host"));
                let (lines, _) = sent.unwrap_or_else(|_| panic!("explicit decision must resolve the send gate"));
                if decision == "on" {
                    assert!(!lines.is_empty());
                    assert!(lines.iter().all(|line| !line.contains("private content")));
                } else {
                    assert_eq!(lines, vec!["private content"]);
                }
            }
        }
    }
    #[tokio::test]
    async fn volatile_mentions_remain_with_their_original_bouncer_identity() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        app.storage = Some(crate::storage::Storage::in_memory());
        let mut server: crate::config::ServerConfig = toml::from_str(
            "label = 'Bouncer'\naddress = 'bnc.example.org'\nport = 6697\ntls = true\nchannels = []\nbouncer_network_id = '42'",
        ).unwrap();
        app.setup_connection("account", &server);
        app.state.add_buffer(crate::state::buffer::Buffer::for_test("account", crate::state::buffer::BufferType::Channel, "#room"));
        let old_scope = app.state.connections["account"].network_key().to_string();
        let message = crate::state::events::tests::make_test_message(&mut app.state, "hello");
        let wire = crate::web::snapshot::message_to_wire(&message, None);
        app.record_mention("account/#room", &wire);
        server.bouncer_network_id = Some("43".into());
        app.setup_connection("account", &server);
        let new_scope = app.state.connections["account"].network_key().to_string();
        app.record_mention("account/#room", &wire);
        let rows = {
            let db = app.storage.as_ref().unwrap().db.lock().unwrap();
            crate::storage::query::get_unread_mentions(&db).unwrap()
        };
        assert!(rows.is_empty());
        assert_eq!(app.volatile_mentions.len(), 2);
        assert_eq!(app.volatile_mentions[0].0, old_scope);
        assert_eq!(app.volatile_mentions[1].0, new_scope);
        assert!(app.mention_target(&old_scope).is_none());
        assert!(app.mention_target("account").is_none());
        assert_eq!(
            app.mention_target(&new_scope),
            Some(("account".into(), "Bouncer".into()))
        );
        app.config.servers.insert("account".into(), {
            server.bouncer_network_id = Some("42".into());
            server
        });
        assert!(app.mention_target(&old_scope).is_none());
        app.storage.take().unwrap().shutdown().await;
    }
}

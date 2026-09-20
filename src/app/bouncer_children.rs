use crate::irc::bouncer::{Network, RegistryEvent};
use crate::state::buffer::make_buffer_id;
use crate::state::connection::ConnectionStatus;

pub struct ChildNetwork {
    pub parent: String,
    pub network: Network,
    pub scope: String,
    pub suspended: bool,
    pub manually_disconnected: bool,
}

impl super::App {
    pub(crate) fn suspend_bouncer_children(&mut self, parent: &str) {
        let children: Vec<String> = self
            .bouncer_children
            .iter_mut()
            .filter_map(|(id, child)| {
                if child.parent != parent || child.suspended {
                    return None;
                }
                child.suspended = true;
                Some(id.clone())
            })
            .collect();
        for id in children {
            self.stop_bouncer_child(&id);
        }
    }

    fn stop_bouncer_child(&mut self, id: &str) {
        let was_active = self.irc_handles.contains_key(id)
            || self.forwarder_handles.contains_key(id)
            || self.state.connections.get(id).is_some_and(|connection| {
                matches!(
                    connection.status,
                    ConnectionStatus::Connected | ConnectionStatus::Connecting
                )
            });
        self.cancel_connection_attempt(id);
        if let Some(requested) = self.state.background_join_connections.get_mut(id) {
            requested.clear();
        }
        if let Some(connection) = self.state.connections.get_mut(id) {
            connection.should_reconnect = false;
            connection.next_reconnect = None;
        }
        if was_active {
            self.handle_irc_event(crate::irc::IrcEvent::Disconnected(id.to_string(), None));
        }
    }

    pub(crate) fn prepare_bouncer_close(&mut self, id: &str) {
        let is_bouncer = self.bouncer_children.contains_key(id)
            || self
                .state
                .connections
                .get(id)
                .is_some_and(|connection| connection.origin_config.bouncer_control);
        if !is_bouncer {
            return;
        }
        let children: Vec<String> = self
            .bouncer_children
            .iter()
            .filter(|(_, child)| child.parent == id)
            .map(|(id, _)| id.clone())
            .collect();
        for child in children {
            self.remove_bouncer_child(&child);
        }
        if let Some(child) = self.bouncer_children.get_mut(id) {
            child.manually_disconnected = true;
        }
        self.cancel_connection_attempt(id);
        self.irc_handles.remove(id);
        self.bouncer_networks.remove(id);
        self.state.background_join_connections.remove(id);
        self.state
            .pending_web_events
            .push(crate::web::protocol::WebEvent::ConnectionRemoved {
                conn_id: id.to_string(),
            });
    }

    fn remove_bouncer_child(&mut self, id: &str) {
        self.bouncer_children.remove(id);
        self.stop_bouncer_child(id);
        let buffers: Vec<String> = self
            .state
            .buffers
            .values()
            .filter(|buffer| buffer.connection_id == id)
            .map(|buffer| buffer.id.clone())
            .collect();
        for buffer in buffers {
            self.state.remove_buffer(&buffer);
        }
        self.state.remove_connection(id);
        self.read_markers.remove(id);
        self.state.background_join_connections.remove(id);
        self.state
            .pending_web_events
            .push(crate::web::protocol::WebEvent::ConnectionRemoved {
                conn_id: id.to_string(),
            });
        self.refresh_e2e_configured_networks();
    }

    pub(crate) fn reconcile_bouncer_children(&mut self, parent: &str, event: &RegistryEvent) {
        let Some(connection) = self.state.connections.get(parent) else {
            return;
        };
        if connection.status != ConnectionStatus::Connected
            || !connection.origin_config.bouncer_control
        {
            return;
        }
        let parent_config = connection.origin_config.clone();
        let Some(registry) = self.bouncer_networks.get(parent) else {
            return;
        };
        let networks = registry.networks.clone();
        let removed: Vec<String> = self.bouncer_children.iter().filter_map(|(id, child)| {
            let covered = matches!(event, RegistryEvent::Snapshot)
                || matches!(event, RegistryEvent::Changed(network) if network == &child.network.id);
            (child.parent == parent && covered && !networks.contains_key(&child.network.id)).then(|| id.clone())
        }).collect();
        for id in removed {
            self.remove_bouncer_child(&id);
        }
        for network in networks.into_values() {
            self.reconcile_bouncer_child(parent, &parent_config, network);
        }
    }

    fn reconcile_bouncer_child(
        &mut self,
        parent: &str,
        parent_config: &crate::config::ServerConfig,
        network: Network,
    ) {
        let mut config = parent_config.clone();
        config.bouncer_control = false;
        config.bouncer_network_id = Some(network.id.clone());
        config.label = format!("{} [{parent}:{}]", network.name(), network.id);
        config.channels.clear();
        let scope = crate::config::network_scope::network_scope(
            parent,
            &config,
            &self.config.general.username,
        );
        let existing = self
            .bouncer_children
            .iter()
            .find(|(_, child)| child.parent == parent && child.network.id == network.id)
            .map(|(id, child)| (id.clone(), child.scope == scope));
        if let Some((id, _)) = &existing
            && !self.state.connections.contains_key(id)
            && self.bouncer_children[id].manually_disconnected
        {
            self.bouncer_children.get_mut(id).unwrap().network = network;
            return;
        }
        let existing = existing.map(|(id, same_scope)| {
            let live_state = self.state.connections.contains_key(&id);
            (id, same_scope && live_state)
        });
        let existing = match existing {
            Some((id, false)) => {
                self.remove_bouncer_child(&id);
                None
            }
            Some((id, true)) => Some(id),
            None => None,
        };
        if let Some(id) = existing {
            self.rename_bouncer_child(&id, &config.label);
            let child = self.bouncer_children.get_mut(&id).unwrap();
            let restart = child.suspended && !child.manually_disconnected;
            let changed = child.network != network;
            child.network = network.clone();
            child.suspended = false;
            if changed {
                self.add_event_to_buffer(
                    &make_buffer_id(&id, &config.label),
                    super::bouncer::network_summary(&network),
                );
            }
            if restart {
                if let Some(connection) = self.state.connections.get_mut(&id) {
                    connection.origin_config = config.clone();
                    connection.should_reconnect = config.auto_reconnect.unwrap_or(true);
                    connection.status = ConnectionStatus::Connecting;
                    connection.next_reconnect = None;
                }
                self.start_bouncer_child(&id, config);
            }
            return;
        }
        let base = format!("_bouncer:{scope}");
        let mut id = base.clone();
        let mut suffix = 1_u64;
        while self.state.connections.contains_key(&id) || self.config.servers.contains_key(&id) {
            id = format!("{base}:{suffix}");
            suffix += 1;
        }
        self.bouncer_children.insert(
            id.clone(),
            ChildNetwork {
                parent: parent.to_string(),
                network: network.clone(),
                scope,
                suspended: false,
                manually_disconnected: false,
            },
        );
        self.state
            .background_join_connections
            .insert(id.clone(), std::collections::HashSet::new());
        let buffer = self.setup_connection_for_account(&id, &config, parent, false);
        self.add_event_to_buffer(&buffer, super::bouncer::network_summary(&network));
        self.queue_bouncer_connection_status(&id);
        self.start_bouncer_child(&id, config);
    }

    pub(crate) fn reconnect_bouncer_child(&mut self, parent: &str, network: &str) -> bool {
        if !self
            .state
            .connections
            .get(parent)
            .is_some_and(|connection| connection.status == ConnectionStatus::Connected)
        {
            return false;
        }
        let Some(id) = self
            .bouncer_children
            .iter()
            .find(|(_, child)| child.parent == parent && child.network.id == network)
            .map(|(id, _)| id.clone())
        else {
            return false;
        };
        if self.irc_handles.contains_key(&id) || self.forwarder_handles.contains_key(&id) {
            return false;
        }
        let child = self.bouncer_children.get_mut(&id).unwrap();
        child.manually_disconnected = false;
        child.suspended = true;
        self.reconcile_bouncer_children(parent, &RegistryEvent::Changed(network.to_string()));
        true
    }

    pub(crate) fn request_bouncer_join_focus(&mut self, channel: &str) {
        let Some(id) = self.active_conn_id().map(str::to_string) else {
            return;
        };
        if let Some(requested) = self.state.background_join_connections.get_mut(&id) {
            requested.extend(channel.split(',').map(str::to_ascii_lowercase));
        }
    }

    fn start_bouncer_child(&mut self, id: &str, mut config: crate::config::ServerConfig) {
        config.bind_ip = crate::irc::resolve_bind_ip(
            &config,
            self.cli_bind_override.as_deref(),
            &self.config.general,
        );
        self.start_connection_attempt(id, config);
    }

    fn queue_bouncer_connection_status(&mut self, id: &str) {
        if let Some(connection) = self.state.connections.get(id) {
            self.state
                .pending_web_events
                .push(crate::web::protocol::WebEvent::ConnectionStatus {
                    conn_id: id.to_string(),
                    label: connection.label.clone(),
                    nick: connection.nick.clone(),
                    connected: connection.status == ConnectionStatus::Connected,
                });
        }
    }

    fn rename_bouncer_child(&mut self, id: &str, name: &str) {
        let Some(connection) = self.state.connections.get_mut(id) else {
            return;
        };
        if connection.label == name {
            return;
        }
        let old = make_buffer_id(id, &connection.label);
        let new = make_buffer_id(id, name);
        connection.label = name.to_string();
        connection.origin_config.label = name.to_string();
        if let Some(mut buffer) = self.state.buffers.shift_remove(&old) {
            buffer.id.clone_from(&new);
            buffer.name = name.to_string();
            self.state.buffers.insert(new.clone(), buffer);
            self.state.rekey_buffer_state(&old, &new);
            self.drain_pending_buffer_rekeys();
            for selected in self.web_active_buffers.values_mut() {
                if selected == &old {
                    selected.clone_from(&new);
                }
            }
            if self.state.active_buffer_id.as_deref() == Some(&old) {
                self.state.active_buffer_id = Some(new.clone());
            }
            if self.state.previous_buffer_id.as_deref() == Some(&old) {
                self.state.previous_buffer_id = Some(new.clone());
            }
            self.state
                .pending_web_events
                .push(crate::web::protocol::WebEvent::BufferRenamed {
                    old_id: old,
                    new_id: new,
                    name: name.to_string(),
                });
        }
        self.queue_bouncer_connection_status(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(app: &mut crate::app::App, id: &str) {
        let config: crate::config::ServerConfig = toml::from_str(
            "label = 'Bouncer'\naddress = '127.0.0.1'\nport = 1\ntls = false\nchannels = ['#never-autojoin']\nbouncer_control = true",
        ).unwrap();
        app.setup_connection(id, &config);
        app.state.connections.get_mut(id).unwrap().status = ConnectionStatus::Connected;
        app.bouncer_networks
            .insert(id.into(), crate::irc::bouncer::NetworkRegistry::default());
    }

    fn receive(app: &mut crate::app::App, id: &str, message: &str) {
        assert!(app.handle_bouncer_network_message(id, &message.parse().unwrap()));
    }

    #[tokio::test]
    async fn snapshots_create_isolated_children_and_rename_without_reconnecting() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        account(&mut app, "composing");
        let compose = make_buffer_id("composing", "Bouncer");
        app.input.value = "unfinished draft".into();
        for parent in ["first", "second"] {
            account(&mut app, parent);
            receive(&mut app, parent, "BATCH +list soju.im/bouncer-networks");
            receive(
                &mut app,
                parent,
                "@batch=list BOUNCER NETWORK 42 name=Same;state=disconnected",
            );
            assert!(
                !app.bouncer_children
                    .values()
                    .any(|child| child.parent == parent)
            );
            app.state.active_buffer_id = Some(compose.clone());
            receive(&mut app, parent, "BATCH -list");
            assert_eq!(
                app.state.active_buffer_id.as_deref(),
                Some(compose.as_str())
            );
            assert_eq!(app.input.value, "unfinished draft");
        }
        assert!(
            app.state
                .pending_web_events
                .iter()
                .filter_map(|event| match event {
                    crate::web::protocol::WebEvent::BufferCreated { buffer, activate }
                        if buffer.connection_id.starts_with("_bouncer:") =>
                        Some(activate),
                    _ => None,
                })
                .all(|activate| !activate)
        );
        assert_eq!(app.bouncer_children.len(), 2);
        let first = app
            .bouncer_children
            .iter()
            .find(|(_, child)| child.parent == "first")
            .unwrap()
            .0
            .clone();
        let second = app
            .bouncer_children
            .iter()
            .find(|(_, child)| child.parent == "second")
            .unwrap()
            .0
            .clone();
        assert_ne!(first, second);
        assert!(!first.contains('/'));
        assert_ne!(
            app.state.connections[&first].network_key(),
            app.state.connections[&second].network_key()
        );
        let scope = app.state.connections[&first].network_key().to_string();
        let generation = app.connection_attempts[&first];
        let old = make_buffer_id(&first, "Same [first:42]");
        let count = app.state.buffers[&old].messages.len();
        app.state.active_buffer_id = Some(old.clone());
        receive(
            &mut app,
            "first",
            "BOUNCER NETWORK 42 name=Renamed;state=connected",
        );
        let new = make_buffer_id(&first, "Renamed [first:42]");
        assert!(!app.state.buffers.contains_key(&old));
        assert!(app.state.buffers[&new].messages.len() >= count);
        assert_eq!(app.state.active_buffer_id.as_deref(), Some(new.as_str()));
        assert_eq!(app.connection_attempts[&first], generation);
        assert_eq!(app.state.connections[&first].network_key(), scope);
        assert!(
            app.state.connections[&first]
                .origin_config
                .channels
                .is_empty()
        );
        assert!(!app.state.connections[&first].origin_config.bouncer_control);
        assert!(app.state.pending_web_events.iter().any(|event| matches!(event, crate::web::protocol::WebEvent::BufferRenamed { old_id, new_id, .. } if old_id == &old && new_id == &new)));
        receive(&mut app, "first", "BOUNCER NETWORK 42 *");
        assert!(!app.state.connections.contains_key(&first));
        assert!(!app.forwarder_handles.contains_key(&first));
        app.handle_irc_event(crate::irc::IrcEvent::Attempt(
            first.clone(),
            generation,
            Box::new(crate::irc::IrcEvent::Connected(
                first.clone(),
                std::collections::HashSet::new(),
                None,
            )),
        ));
        assert!(!app.state.connections.contains_key(&first));
        assert!(app.state.connections.contains_key(&second));
        app.suspend_bouncer_children("second");
    }

    #[tokio::test]
    async fn replayed_joins_and_dms_preserve_focus_but_requested_joins_activate() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        account(&mut app, "account");
        receive(&mut app, "account", "BOUNCER NETWORK 1 name=Network");
        let id = app.bouncer_children.keys().next().unwrap().clone();
        let nick = app.state.connections[&id].nick.clone();
        let original = app.state.active_buffer_id.clone();
        app.state.pending_web_events.clear();
        let mut web = app.web_broadcaster.subscribe();
        app.handle_irc_event(crate::irc::IrcEvent::Message(
            id.clone(),
            Box::new(format!(":{nick}!user@host JOIN #restored").parse().unwrap()),
        ));
        app.handle_irc_event(crate::irc::IrcEvent::Message(
            id.clone(),
            Box::new(
                format!(":peer!user@host PRIVMSG {nick} :hello")
                    .parse()
                    .unwrap(),
            ),
        ));
        assert_eq!(app.state.active_buffer_id, original);
        app.drain_pending_web_events();
        let mut created = 0;
        while let Ok(event) = web.try_recv() {
            if let crate::web::protocol::WebEvent::BufferCreated { activate, .. } = event {
                assert!(!activate);
                created += 1;
            }
        }
        assert_eq!(created, 2);
        app.irc_handles.insert(
            id.clone(),
            crate::irc::IrcHandle::new(id.clone(), crate::irc::IrcSender::capturing(0), None, None),
        );
        app.state.active_buffer_id = Some(make_buffer_id(&id, "Network [account:1]"));
        app.execute_command(&crate::commands::parser::parse_command("/join #requested").unwrap());
        app.handle_irc_event(crate::irc::IrcEvent::Message(
            id.clone(),
            Box::new(
                format!(":{nick}!user@host JOIN #requested")
                    .parse()
                    .unwrap(),
            ),
        ));
        assert_eq!(
            app.state.active_buffer_id,
            Some(make_buffer_id(&id, "#requested"))
        );
        app.suspend_bouncer_children("account");
    }

    #[tokio::test]
    async fn rejected_join_requests_do_not_activate_later_replayed_joins() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        account(&mut app, "account");
        receive(&mut app, "account", "BOUNCER NETWORK 1 name=Network");
        let id = app.bouncer_children.keys().next().unwrap().clone();
        let nick = app.state.connections[&id].nick.clone();
        app.irc_handles.insert(
            id.clone(),
            crate::irc::IrcHandle::new(id.clone(), crate::irc::IrcSender::capturing(0), None, None),
        );
        let server = make_buffer_id(&id, "Network [account:1]");
        for code in [403, 405, 437, 471, 473, 474, 475, 476, 477, 479, 489, 520] {
            let channel = format!("#rejected{code}");
            app.state.active_buffer_id = Some(server.clone());
            app.execute_command(
                &crate::commands::parser::parse_command(&format!("/join {channel}")).unwrap(),
            );
            assert!(app.state.background_join_connections[&id].contains(&channel));
            app.handle_irc_event(crate::irc::IrcEvent::Message(
                id.clone(),
                Box::new(
                    format!(":server {code} {nick} {channel} :Cannot join")
                        .parse()
                        .unwrap(),
                ),
            ));
            assert!(app.state.background_join_connections[&id].is_empty());
            app.handle_irc_event(crate::irc::IrcEvent::Message(
                id.clone(),
                Box::new(format!(":{nick}!user@host JOIN {channel}").parse().unwrap()),
            ));
            assert_eq!(app.state.active_buffer_id.as_ref(), Some(&server));
        }
        for failure in [
            "FAIL JOIN NEED_REGISTRATION #one :Cannot join",
            "FAIL JOIN UNKNOWN_ERROR :Cannot join",
            "461 tester JOIN :Not enough parameters",
            "263 tester JOIN :Try again",
        ] {
            app.request_bouncer_join_focus("#one");
            app.handle_irc_event(crate::irc::IrcEvent::Message(
                id.clone(),
                Box::new(format!(":server {failure}").parse().unwrap()),
            ));
            assert!(app.state.background_join_connections[&id].is_empty());
        }
        app.suspend_bouncer_children("account");
    }

    #[tokio::test]
    async fn closed_children_can_be_reopened_without_overwriting_query_buffers() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        account(&mut app, "account");
        receive(&mut app, "account", "BOUNCER NETWORK 1 name=One");
        let id = app.bouncer_children.keys().next().unwrap().clone();
        app.state.add_buffer(crate::state::buffer::Buffer::for_test(
            &id,
            crate::state::buffer::BufferType::Query,
            "bob",
        ));
        receive(&mut app, "account", "BOUNCER NETWORK 1 name=bob");
        assert_eq!(
            app.state.buffers[&make_buffer_id(&id, "bob")].buffer_type,
            crate::state::buffer::BufferType::Query
        );
        let server = make_buffer_id(&id, "bob [account:1]");
        assert_eq!(
            app.state.buffers[&server].buffer_type,
            crate::state::buffer::BufferType::Server
        );
        app.state.active_buffer_id = Some(server);
        app.execute_command(&crate::commands::parser::parse_command("/disconnect").unwrap());
        app.execute_command(&crate::commands::parser::parse_command("/close").unwrap());
        assert!(!app.state.connections.contains_key(&id));
        receive(&mut app, "account", "BOUNCER NETWORK 1 state=connected");
        assert!(!app.state.connections.contains_key(&id));
        assert!(app.reconnect_bouncer_child("account", "1"));
        assert!(app.state.connections.contains_key(&id));
        assert!(
            app.state
                .buffers
                .contains_key(&make_buffer_id(&id, "bob [account:1]"))
        );
        assert!(app.forwarder_handles.contains_key(&id));
        app.suspend_bouncer_children("account");
    }

    #[tokio::test]
    async fn parent_disconnect_is_idempotent_and_close_removes_its_children() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        account(&mut app, "account");
        receive(&mut app, "account", "BOUNCER NETWORK 1 name=One");
        receive(&mut app, "account", "BOUNCER NETWORK 2 name=Two");
        let children: Vec<String> = app.bouncer_children.keys().cloned().collect();
        app.irc_handles.insert(
            "account".into(),
            crate::irc::IrcHandle::new(
                "account".into(),
                crate::irc::IrcSender::capturing(0),
                None,
                None,
            ),
        );
        app.drain_pending_web_events();
        let mut web = app.web_broadcaster.subscribe();
        app.execute_command(&crate::commands::parser::parse_command("/disconnect").unwrap());
        let sizes: Vec<usize> = children
            .iter()
            .map(|id| {
                app.state
                    .buffers
                    .values()
                    .filter(|buffer| &buffer.connection_id == id)
                    .map(|buffer| buffer.messages.len())
                    .sum()
            })
            .collect();
        app.handle_irc_event(crate::irc::IrcEvent::Disconnected("account".into(), None));
        for (id, size) in children.iter().zip(sizes) {
            assert_eq!(
                app.state
                    .buffers
                    .values()
                    .filter(|buffer| &buffer.connection_id == id)
                    .map(|buffer| buffer.messages.len())
                    .sum::<usize>(),
                size
            );
        }
        let mut disconnected = std::collections::HashMap::<String, usize>::new();
        while let Ok(event) = web.try_recv() {
            if let crate::web::protocol::WebEvent::ConnectionStatus {
                conn_id,
                connected: false,
                ..
            } = event
            {
                *disconnected.entry(conn_id).or_default() += 1;
            }
        }
        for id in &children {
            assert_eq!(disconnected[id], 1);
        }
        app.execute_command(&crate::commands::parser::parse_command("/close").unwrap());
        assert!(!app.state.connections.contains_key("account"));
        assert!(app.bouncer_children.is_empty());
        assert!(
            children
                .iter()
                .all(|id| !app.state.connections.contains_key(id)
                    && !app.forwarder_handles.contains_key(id))
        );
        assert!(
            app.state
                .buffers
                .values()
                .all(|buffer| !children.contains(&buffer.connection_id))
        );
    }

    #[tokio::test]
    async fn closing_a_failed_child_keeps_it_closed_until_explicit_reconnect() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        account(&mut app, "account");
        receive(&mut app, "account", "BOUNCER NETWORK 1 name=One");
        let id = app.bouncer_children.keys().next().unwrap().clone();
        app.handle_irc_event(crate::irc::IrcEvent::Disconnected(
            id.clone(),
            Some("failed".into()),
        ));
        app.state.active_buffer_id = Some(make_buffer_id(&id, "One [account:1]"));
        app.execute_command(&crate::commands::parser::parse_command("/close").unwrap());
        receive(&mut app, "account", "BOUNCER NETWORK 1 state=connected");
        assert!(!app.state.connections.contains_key(&id));
        assert!(app.bouncer_children[&id].manually_disconnected);
        assert!(app.reconnect_bouncer_child("account", "1"));
        assert!(app.state.connections.contains_key(&id));
        app.suspend_bouncer_children("account");
    }

    #[tokio::test]
    async fn parent_reconnect_waits_for_a_valid_list_and_preserves_manual_disconnect() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        account(&mut app, "account");
        receive(&mut app, "account", "BOUNCER NETWORK 1 name=One");
        receive(&mut app, "account", "BOUNCER NETWORK 2 name=Two");
        let one = app
            .bouncer_children
            .iter()
            .find(|(_, child)| child.network.id == "1")
            .unwrap()
            .0
            .clone();
        let two = app
            .bouncer_children
            .iter()
            .find(|(_, child)| child.network.id == "2")
            .unwrap()
            .0
            .clone();
        app.state.active_buffer_id = Some(make_buffer_id(&one, "One [account:1]"));
        app.execute_command(&crate::commands::parser::parse_command("/disconnect").unwrap());
        assert!(app.bouncer_children[&one].manually_disconnected);
        app.handle_irc_event(crate::irc::IrcEvent::Disconnected("account".into(), None));
        assert!(!app.forwarder_handles.contains_key(&two));
        assert!(!app.state.connections[&two].should_reconnect);
        app.state.connections.get_mut("account").unwrap().status = ConnectionStatus::Connected;
        app.bouncer_networks.insert(
            "account".into(),
            crate::irc::bouncer::NetworkRegistry::default(),
        );
        receive(&mut app, "account", "BATCH +list soju.im/bouncer-networks");
        receive(
            &mut app,
            "account",
            "@batch=list BOUNCER NETWORK 1 name=One",
        );
        receive(
            &mut app,
            "account",
            "@batch=list BOUNCER NETWORK 2 name=Two",
        );
        assert!(!app.forwarder_handles.contains_key(&two));
        receive(&mut app, "account", "BATCH -list");
        assert!(!app.forwarder_handles.contains_key(&one));
        assert!(app.forwarder_handles.contains_key(&two));
        assert!(app.reconnect_bouncer_child("account", "1"));
        assert!(app.forwarder_handles.contains_key(&one));
        receive(&mut app, "account", "BATCH +empty soju.im/bouncer-networks");
        receive(&mut app, "account", "BATCH -empty");
        assert!(app.bouncer_children.is_empty());
        assert!(!app.state.connections.contains_key(&one));
        assert!(!app.state.connections.contains_key(&two));
    }
}

#[cfg(test)]
#[tokio::test]
#[ignore = "requires a disposable pinned bouncer fixture"]
async fn pinned_bouncer_generated_children() {
    let mut app = crate::app::input::submit_typing_tests::test_app();
    app.config.general.flood_protection = false;
    let mut config: crate::config::ServerConfig = toml::from_str(
        "label = 'fixture'\naddress = '127.0.0.1'\nport = 6667\ntls = false\nchannels = []\nbouncer_control = true",
    ).unwrap();
    config.port = std::env::var("REPARTEE_BOUNCER_TEST_PORT")
        .unwrap()
        .parse()
        .unwrap();
    config.sasl_user = Some(std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap());
    config.sasl_pass = Some("fixture-password".into());
    let network = std::env::var("REPARTEE_BOUNCER_TEST_NETID").unwrap();
    app.setup_connection("fixture", &config);
    app.start_connection_attempt("fixture", config);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(event) = app.irc_rx.recv().await {
            app.handle_irc_event(event);
            if app.bouncer_children.iter().any(|(id, child)| {
                child.network.id == network
                    && app.state.connections[id].status == ConnectionStatus::Connected
            }) {
                return;
            }
        }
        panic!("control connection ended before the child connected");
    })
    .await
    .expect("discovered network never connected");
    assert_eq!(app.bouncer_children.len(), 1);
    app.state.scrollback_limit = 1000;
    app.config.display.backlog_lines = 200;
    let id = app.bouncer_children.keys().next().unwrap().clone();
    let buffer_id = make_buffer_id(&id, "history-peer");
    let channel_id = make_buffer_id(&id, "#history-channel");
    let (log_tx, mut log_rx) = tokio::sync::mpsc::channel(512);
    app.state.log_tx = Some(log_tx);
    for expected in [200, 300] {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some(event) = app.irc_rx.recv().await {
                app.handle_irc_event(event);
                if app.state.buffers.get(&buffer_id).is_some_and(|buffer| buffer.messages.len() >= expected)
                    && app.state.buffers.get(&channel_id).is_some_and(|buffer| buffer.messages.len() == 200)
                    && !app.state.connections[&id].chathistory.any_in_flight("history-peer")
                    && !app.state.connections[&id].chathistory.any_in_flight("#history-channel")
                {
                    return;
                }
            }
            panic!("connection closed while fetching history");
        }).await.expect("server history page did not arrive");
        if expected == 200 {
            assert!(app.fetch_older_via_chathistory(&buffer_id));
        }
    }
    let messages = &app.state.buffers[&buffer_id].messages;
    assert_eq!(messages.len(), 300);
    for (index, message) in messages.iter().enumerate() {
        assert_eq!(message.text, format!("fixture-history-{index}"));
    }
    assert!(app.state.connections[&id].chathistory.is_before_exhausted("history-peer"));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !app.history_discovery[&id].finished {
            let event = app.irc_rx.recv().await.expect("connection closed during TARGETS pagination");
            app.handle_irc_event(event);
        }
    }).await.expect("TARGETS discovery did not finish");
    verify_pinned_read_markers(&mut app, &id, &buffer_id).await;
    while let Ok(row) = log_rx.try_recv() {
        assert_ne!(row.buffer, "history-peer");
        assert_ne!(row.buffer, "#history-channel");
    }
    app.suspend_bouncer_children("fixture");
    assert!(
        app.bouncer_children
            .keys()
            .all(|id| !app.irc_handles.contains_key(id) && !app.forwarder_handles.contains_key(id))
    );
    app.cancel_connection_attempt("fixture");
    app.irc_handles.remove("fixture");
}

#[cfg(test)]
async fn verify_pinned_read_markers(app: &mut crate::app::App, id: &str, buffer_id: &str) {
    assert!(app.state.connections[id].enabled_caps.contains("draft/read-marker"));
    assert_eq!(app.state.buffers[buffer_id].unread_count, 300);
    let mut observer_config = app.state.connections[id].origin_config.clone();
    observer_config.sasl_user = Some(std::env::var("REPARTEE_BOUNCER_TEST_USER").unwrap());
    observer_config.sasl_pass = Some("fixture-password".into());
    let (observer, mut observer_events) = crate::irc::connect_server("marker-observer", &observer_config, &app.config.general).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(event) = observer_events.recv().await {
            if let crate::irc::IrcEvent::Connected(_, caps, _) = event {
                assert!(caps.contains("draft/read-marker"));
                return;
            }
        }
        panic!("observer disconnected during registration");
    }).await.unwrap();
    let seen = &app.state.buffers[buffer_id].messages[149];
    let (seen_id, seen_time) = (seen.id, seen.timestamp.timestamp_millis());
    app.mark_visible_message_read(buffer_id, seen_id);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while app.confirmed_read_marker(id, "history-peer") != Some(seen_time) {
            let event = app.irc_rx.recv().await.expect("connection closed during MARKREAD");
            app.handle_irc_event(event);
        }
    }).await.expect("server did not acknowledge the read marker");
    assert_eq!(app.state.buffers[buffer_id].unread_count, 150);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let expected = crate::irc::chathistory::rfc3339_millis(seen_time);
        while let Some(event) = observer_events.recv().await {
            if let crate::irc::IrcEvent::Message(_, message) = event
                && let irc::proto::Command::Raw(command, params) = &message.command
                && command == "MARKREAD" && params == &["history-peer".to_string(), format!("timestamp={expected}")] {
                return;
            }
        }
        panic!("observer disconnected before receiving remote read marker");
    }).await.expect("read marker was not synchronized to another client");
    drop(observer);
    drop(observer_events);
    app.reconnect_read_markers(id);
    app.tick_read_markers();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while app.confirmed_read_marker(id, "history-peer") != Some(seen_time) {
            let event = app.irc_rx.recv().await.expect("connection closed during read-marker query");
            app.handle_irc_event(event);
        }
    }).await.expect("server did not return the stored read marker");
}

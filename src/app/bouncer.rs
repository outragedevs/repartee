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
        let text = match event {
            RegistryEvent::Unrelated => return false,
            RegistryEvent::Pending => return true,
            RegistryEvent::Snapshot => format!(
                "Bouncer network list received: {} networks. Use /bouncer list.",
                registry.networks.len()
            ),
            RegistryEvent::Changed(id) => registry
                .networks
                .get(&id)
                .map_or_else(|| format!("Bouncer network {id} removed"), network_summary),
            RegistryEvent::Invalid => {
                "Invalid bouncer network update ignored; the previous list was retained".to_string()
            }
        };
        if let Some(connection) = self.state.connections.get(conn_id) {
            let buffer = make_buffer_id(conn_id, &connection.label);
            self.add_event_to_buffer(&buffer, text);
        }
        true
    }
}

fn network_summary(network: &Network) -> String {
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
        _ => add_local_event(app, "Usage: /bouncer [list|refresh]"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::irc::{IrcEvent, IrcHandle, IrcSender, bouncer::NETWORKS_NOTIFY_CAP};

    #[test]
    fn control_connect_requests_only_without_notify_and_reconnect_clears_cache() {
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
}

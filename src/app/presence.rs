use std::collections::HashMap;
use std::time::{Duration, Instant};

use irc::proto::{Command, Message, Prefix};

use super::App;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Provider {
    Soju,
    Lurker,
}

#[derive(Clone, PartialEq, Eq)]
enum Mode {
    Present,
    Absent,
    Manual(String),
}

impl Mode {
    fn command(&self) -> Command {
        Command::AWAY(match self {
            Self::Present => None,
            Self::Absent => Some("*".into()),
            Self::Manual(reason) => Some(reason.clone()),
        })
    }
}

struct ConnectionPresence {
    scope: String,
    provider: Option<Provider>,
    sent: Option<Mode>,
    attempted: Option<Instant>,
}

struct ManualIntent {
    reason: Option<String>,
    revision: u64,
    provider: Option<Provider>,
}

#[derive(Default)]
pub struct BouncerPresence {
    connections: HashMap<String, ConnectionPresence>,
    browsers: HashMap<String, Option<(bool, Instant)>>,
    manual: HashMap<String, ManualIntent>,
    revision: u64,
}

fn account_scope(scope: &str) -> &str {
    scope.rsplit_once(':').map_or(scope, |(account, _)| account)
}

impl App {
    pub(crate) fn reconnect_bouncer_presence(&mut self, conn_id: &str) {
        let Some(conn) = self
            .state
            .connections
            .get(conn_id)
            .filter(|conn| conn.server_owns_history())
        else {
            return;
        };
        self.bouncer_presence.connections.insert(
            conn_id.to_string(),
            ConnectionPresence {
                scope: conn.network_key().to_string(),
                provider: None,
                sent: conn
                    .enabled_caps
                    .contains("draft/pre-away")
                    .then_some(Mode::Absent),
                attempted: None,
            },
        );
    }

    pub(crate) fn observe_bouncer_presence(&mut self, conn_id: &str, message: &Message) {
        let Some(connection) = self.bouncer_presence.connections.get_mut(conn_id) else {
            return;
        };
        let (number, args) = match &message.command {
            Command::Response(response, args) => (*response as u16, args),
            Command::Raw(command, args) => (command.parse::<u16>().unwrap_or(0), args),
            _ => return,
        };
        if number == 5
            && matches!(message.prefix.as_ref(), Some(Prefix::ServerName(name)) if name == "lurker.bouncer")
            && args.iter().any(|arg| arg.starts_with("BOUNCER_NETID="))
        {
            connection.provider = Some(Provider::Lurker);
        } else if number == 4 && args.get(2).is_some_and(|version| version == "soju") {
            connection.provider = Some(Provider::Soju);
        }
        if let Some(intent) = self.bouncer_presence.manual.get_mut(&connection.scope) {
            intent.provider = connection.provider.or(intent.provider);
        }
    }

    pub(crate) fn register_presence_browser(&mut self, session_id: &str) {
        self.bouncer_presence
            .browsers
            .insert(session_id.to_string(), None);
    }

    pub(crate) fn update_presence_browser(&mut self, session_id: &str, present: bool) {
        if let Some(browser) = self.bouncer_presence.browsers.get_mut(session_id) {
            *browser = Some((present, Instant::now()));
            self.tick_bouncer_presence();
        }
    }

    pub(crate) fn remove_presence_browser(&mut self, session_id: &str) {
        self.bouncer_presence.browsers.remove(session_id);
        self.tick_bouncer_presence();
    }

    pub(crate) fn set_bouncer_away(&mut self, conn_id: &str, reason: Option<&str>) -> bool {
        let Some(conn) = self
            .state
            .connections
            .get(conn_id)
            .filter(|conn| conn.server_owns_history())
        else {
            return false;
        };
        let scope = conn.network_key().to_string();
        let mode = reason.map_or(Mode::Present, |reason| Mode::Manual(reason.to_string()));
        self.bouncer_presence.revision = self.bouncer_presence.revision.saturating_add(1);
        self.bouncer_presence.manual.insert(
            scope,
            ManualIntent {
                reason: reason.map(str::to_string),
                revision: self.bouncer_presence.revision,
                provider: self.bouncer_presence.connections.get(conn_id).and_then(|entry| entry.provider),
            },
        );
        let result = self
            .irc_handles
            .get(conn_id)
            .ok_or_else(|| "Not connected".to_string())
            .and_then(|handle| {
                handle
                    .sender()
                    .send(mode.command())
                    .map_err(|error| error.to_string())
            });
        match result {
            Ok(()) => {
                if let Some(connection) = self.bouncer_presence.connections.get_mut(conn_id) {
                    connection.sent = Some(mode);
                    connection.attempted = None;
                }
            }
            Err(error) => crate::commands::helpers::add_local_event(
                self,
                &format!("Failed to send AWAY: {error}"),
            ),
        }
        self.tick_bouncer_presence();
        true
    }

    pub(crate) fn tick_bouncer_presence(&mut self) {
        let now = Instant::now();
        let present = (self.terminal.is_some() && self.terminal_focused)
            || self.bouncer_presence.browsers.values().any(|browser| {
                browser.is_some_and(|(present, updated)| {
                    present && now.duration_since(updated) < Duration::from_secs(45)
                })
            });
        self.bouncer_presence.connections.retain(|id, entry| {
            self.state
                .connections
                .get(id)
                .is_some_and(|conn| conn.network_key() == entry.scope)
        });
        let scopes: Vec<_> = self
            .state
            .connections
            .values()
            .filter(|conn| conn.server_owns_history())
            .map(crate::state::connection::Connection::network_key)
            .collect();
        self.bouncer_presence.manual.retain(|scope, intent| {
            scopes
                .iter()
                .any(|current| *current == scope || (intent.provider == Some(Provider::Lurker)
                    && account_scope(current) == account_scope(scope)))
        });
        for (id, presence) in &mut self.bouncer_presence.connections {
            let Some(conn) = self.state.connections.get(id) else {
                continue;
            };
            if conn.bouncer_control()
                || conn.status != crate::state::connection::ConnectionStatus::Connected
            {
                continue;
            }
            let Some(provider) = presence.provider else {
                continue;
            };
            let manual = self
                .bouncer_presence
                .manual
                .iter()
                .filter(|(scope, _)| {
                    if provider == Provider::Lurker {
                        account_scope(scope) == account_scope(&presence.scope)
                    } else {
                        *scope == &presence.scope
                    }
                })
                .max_by_key(|(_, intent)| intent.revision)
                .and_then(|(_, intent)| intent.reason.as_ref());
            let desired = manual.map_or_else(
                || if present { Mode::Present } else { Mode::Absent },
                |reason| Mode::Manual(reason.clone()),
            );
            if presence.sent.as_ref() == Some(&desired)
                || presence
                    .attempted
                    .is_some_and(|attempt| now.duration_since(attempt) < Duration::from_secs(5))
            {
                continue;
            }
            let Some(handle) = self.irc_handles.get(id) else {
                continue;
            };
            presence.attempted = Some(now);
            if handle.sender().send(desired.command()).is_ok() {
                presence.sent = Some(desired);
                presence.attempted = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        for (id, scope) in [
            ("first", "account-a:1"),
            ("second", "account-a:2"),
            ("other", "account-b:1"),
        ] {
            let config = toml::from_str("label='Bouncer'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nbouncer_network_id='1'").unwrap();
            app.setup_connection(id, &config);
            let conn = app.state.connections.get_mut(id).unwrap();
            conn.status = crate::state::connection::ConnectionStatus::Connected;
            conn.network_scope = Some(scope.into());
            app.irc_handles.insert(
                id.into(),
                crate::irc::IrcHandle::new(
                    id.into(),
                    crate::irc::IrcSender::capturing(0),
                    None,
                    None,
                ),
            );
            app.reconnect_bouncer_presence(id);
        }
        app
    }

    fn identify(app: &mut App, id: &str, provider: Provider) {
        let wire = if provider == Provider::Lurker {
            ":lurker.bouncer 005 me BOUNCER_NETID=1 :supported"
        } else {
            ":bnc 004 me bnc soju o o"
        };
        app.observe_bouncer_presence(id, &wire.parse().unwrap());
    }

    fn commands(app: &App, id: &str) -> Vec<Command> {
        app.irc_handles[id]
            .sender()
            .captured()
            .iter()
            .map(|message| message.command.clone())
            .collect()
    }

    #[tokio::test]
    async fn failed_presence_write_retries_without_recording_success() {
        let mut app = app();
        identify(&mut app, "first", Provider::Soju);
        app.irc_handles.insert(
            "first".into(),
            crate::irc::IrcHandle::new(
                "first".into(),
                crate::irc::IrcSender::capturing_then_failing(0),
                None,
                None,
            ),
        );
        app.tick_bouncer_presence();
        assert!(app.bouncer_presence.connections["first"].sent.is_none());
        assert!(app.bouncer_presence.connections["first"].attempted.is_some());
        app.irc_handles.insert(
            "first".into(),
            crate::irc::IrcHandle::new(
                "first".into(),
                crate::irc::IrcSender::capturing(0),
                None,
                None,
            ),
        );
        app.tick_bouncer_presence();
        assert!(commands(&app, "first").is_empty());
        app.bouncer_presence.connections.get_mut("first").unwrap().attempted =
            Instant::now().checked_sub(Duration::from_secs(6));
        app.tick_bouncer_presence();
        app.tick_bouncer_presence();
        assert_eq!(commands(&app, "first"), [Mode::Absent.command()]);
        assert!(app.bouncer_presence.connections["first"].sent == Some(Mode::Absent));
        assert!(app.bouncer_presence.connections["first"].attempted.is_none());
    }

    #[tokio::test]
    async fn terminal_focus_changes_send_only_presence_transitions() {
        let mut app = app();
        identify(&mut app, "first", Provider::Soju);
        app.tick_bouncer_presence();
        app.tick_bouncer_presence();
        assert_eq!(commands(&app, "first"), [Mode::Absent.command()]);
        app.terminal =
            Some(crate::ui::setup_socket_terminal(Box::new(std::io::sink()), 120, 40).unwrap());
        app.terminal_focused = true;
        app.tick_bouncer_presence();
        app.terminal_focused = false;
        app.tick_bouncer_presence();
        assert_eq!(
            commands(&app, "first"),
            [
                Mode::Absent.command(),
                Mode::Present.command(),
                Mode::Absent.command()
            ]
        );
        assert!(commands(&app, "other").is_empty());
    }

    #[tokio::test]
    async fn browser_presence_requires_registration_and_expires_per_source() {
        let mut app = app();
        identify(&mut app, "first", Provider::Soju);
        app.update_presence_browser("unknown", true);
        assert!(app.bouncer_presence.browsers.is_empty());
        app.register_presence_browser("one");
        app.register_presence_browser("two");
        app.tick_bouncer_presence();
        app.update_presence_browser("one", true);
        app.update_presence_browser("two", true);
        app.update_presence_browser("one", false);
        assert_eq!(
            commands(&app, "first"),
            [Mode::Absent.command(), Mode::Present.command()]
        );
        app.bouncer_presence.browsers.insert(
            "two".into(),
            Some((
                true,
                Instant::now().checked_sub(Duration::from_secs(46)).unwrap(),
            )),
        );
        app.tick_bouncer_presence();
        assert_eq!(
            commands(&app, "first").last(),
            Some(&Mode::Absent.command())
        );
        app.update_presence_browser("one", true);
        app.remove_presence_browser("one");
        assert_eq!(
            commands(&app, "first").last(),
            Some(&Mode::Absent.command())
        );
    }

    #[tokio::test]
    async fn lurker_manual_away_is_shared_with_sibling_networks_only() {
        let mut app = app();
        for id in ["first", "second", "other"] {
            identify(&mut app, id, Provider::Lurker);
        }
        assert!(app.set_bouncer_away("first", Some("at lunch")));
        assert_eq!(
            commands(&app, "first"),
            [Mode::Manual("at lunch".into()).command()]
        );
        assert_eq!(
            commands(&app, "second"),
            [Mode::Manual("at lunch".into()).command()]
        );
        assert_eq!(commands(&app, "other"), [Mode::Absent.command()]);
        app.terminal =
            Some(crate::ui::setup_socket_terminal(Box::new(std::io::sink()), 120, 40).unwrap());
        app.terminal_focused = true;
        app.tick_bouncer_presence();
        assert_eq!(commands(&app, "first").len(), 1);
        assert_eq!(commands(&app, "second").len(), 1);
        app.reconnect_bouncer_presence("second");
        identify(&mut app, "second", Provider::Lurker);
        app.tick_bouncer_presence();
        assert_eq!(
            commands(&app, "second").last(),
            Some(&Mode::Manual("at lunch".into()).command())
        );
        assert!(app.set_bouncer_away("second", None));
        assert_eq!(
            commands(&app, "first").last(),
            Some(&Mode::Present.command())
        );
    }

    #[tokio::test]
    async fn soju_manual_away_is_network_scoped_and_scope_changes_discard_it() {
        let mut app = app();
        identify(&mut app, "first", Provider::Soju);
        identify(&mut app, "second", Provider::Soju);
        assert!(app.set_bouncer_away("first", Some("away")));
        assert_eq!(commands(&app, "second"), [Mode::Absent.command()]);
        app.state
            .connections
            .get_mut("first")
            .unwrap()
            .network_scope = Some("account-c:1".into());
        app.reconnect_bouncer_presence("first");
        identify(&mut app, "first", Provider::Soju);
        app.tick_bouncer_presence();
        assert_eq!(
            commands(&app, "first").last(),
            Some(&Mode::Absent.command())
        );
    }

    #[tokio::test]
    async fn removed_network_intent_survives_only_for_lurker_siblings() {
        for provider in [Provider::Soju, Provider::Lurker] {
            let mut app = app();
            identify(&mut app, "first", provider);
            identify(&mut app, "second", provider);
            app.set_bouncer_away("first", Some("old network away"));
            app.state.connections.get_mut("first").unwrap().network_scope =
                Some("account-c:1".into());
            app.tick_bouncer_presence();
            assert_eq!(
                app.bouncer_presence.manual.contains_key("account-a:1"),
                provider == Provider::Lurker,
            );
            app.state.connections.get_mut("first").unwrap().network_scope =
                Some("account-a:1".into());
            app.reconnect_bouncer_presence("first");
            identify(&mut app, "first", provider);
            app.tick_bouncer_presence();
            let expected = if provider == Provider::Lurker {
                Mode::Manual("old network away".into())
            } else {
                Mode::Absent
            };
            assert_eq!(commands(&app, "first").last(), Some(&expected.command()));
        }
    }

    #[tokio::test]
    async fn direct_irc_and_control_connections_do_not_emit_automatic_away() {
        let mut app = app();
        app.state
            .connections
            .get_mut("first")
            .unwrap()
            .origin_config
            .bouncer_control = true;
        identify(&mut app, "first", Provider::Lurker);
        app.tick_bouncer_presence();
        assert!(commands(&app, "first").is_empty());
        let config =
            toml::from_str("label='Direct'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]")
                .unwrap();
        app.setup_connection("direct", &config);
        app.reconnect_bouncer_presence("direct");
        assert!(!app.bouncer_presence.connections.contains_key("direct"));
        assert!(!app.set_bouncer_away("direct", Some("away")));
    }
}

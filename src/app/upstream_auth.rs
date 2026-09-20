use std::collections::HashSet;
use std::time::{Duration, Instant};

use base64::Engine as _;
use irc::proto::{Command, Message, command::CapSubCommand};
use zeroize::Zeroizing;

#[derive(Default)]
pub struct Session {
    mechanisms: HashSet<String>,
    pending: Option<Pending>,
}

enum Operation { Login, Clear }

struct Pending {
    payload: Option<Zeroizing<String>>,
    started: Instant,
    expired: bool,
    operation: Operation,
    payload_sent: bool,
    allow_insecure_upstream: bool,
}

pub fn command(app: &mut super::App, args: &[String]) {
    command_with_secret(app, args, |key| {
        let path = crate::constants::env_path();
        #[cfg(test)]
        let path = std::env::var_os("REPARTEE_UPSTREAM_AUTH_ENV").map_or(path, std::path::PathBuf::from);
        crate::config::load_env(&path).ok()?.remove(key)
    });
}

fn command_with_secret(app: &mut super::App, args: &[String], secret: impl FnOnce(&str) -> Option<String>) {
    use crate::commands::helpers::add_local_event;
    let Some(id) = app.active_conn_id().map(str::to_string) else {
        add_local_event(app, "No active connection");
        return;
    };
    let clear = args == ["clear", "-YES"];
    let allow_insecure = args.get(1).is_some_and(|arg| arg == "-allow-insecure-upstream");
    let login_args = args.get(if allow_insecure { 2.. } else { 1.. }).unwrap_or_default();
    let payload = if clear {
        Some(Zeroizing::new("+".into()))
    } else if args.first().is_some_and(|arg| arg == "login") && login_args.len() == 2 {
        if login_args[0].is_empty() || login_args[0].chars().any(char::is_control)
            || login_args[1].is_empty() || !login_args[1].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            add_local_event(app, "Invalid account or secret reference");
            return;
        }
        let upstream_tls = app.bouncer_children.get(&id).is_some_and(|child| {
            !child.suspended && child.network.attributes.get("tls").is_some_and(|tls| tls == "1")
        });
        if !upstream_tls && !allow_insecure {
            add_local_event(app, "Upstream TLS is disabled or unknown. SASL PLAIN exposes the IRC password on an unencrypted upstream hop. Use /auth login -allow-insecure-upstream <account> <password-env-name> only to explicitly accept that risk");
            return;
        }
        let secret = secret(&login_args[1]).map(Zeroizing::new);
        let Some(secret) = secret.filter(|s| !s.is_empty() && !s.contains('\0')) else {
            add_local_event(app, "Password secret is missing or invalid in .env");
            return;
        };
        let user = crate::irc::sasl_scram::saslprep(&login_args[0]);
        let secret = Zeroizing::new(crate::irc::sasl_scram::saslprep(&secret));
        if user.is_empty() || secret.is_empty() {
            add_local_event(app, "Prepared SASL account or password is empty");
            return;
        }
        let raw = Zeroizing::new(format!("\0{user}\0{}", secret.as_str()));
        Some(Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(raw.as_bytes())))
    } else {
        add_local_event(app, "Usage: /auth login [-allow-insecure-upstream] <IRC-account> <password-env-name> | /auth clear -YES (remove saved upstream SASL credentials)");
        return;
    };
    let supported = app.state.connections.get(&id).is_some_and(|conn| {
        conn.status == crate::state::connection::ConnectionStatus::Connected
            && conn.origin_config.bouncer_network_id.is_some() && !conn.origin_config.bouncer_control
            && conn.origin_config.tls && conn.origin_config.tls_verify
    });
    let mechanism = if clear { "ANONYMOUS" } else { "PLAIN" };
    let Some(session) = app.upstream_auth.get_mut(&id).filter(|session| supported && session.mechanisms.contains(mechanism)) else {
        add_local_event(app, "Upstream authentication is unavailable; use a bound bouncer network with verified TLS and advertised upstream SASL");
        return;
    };
    if session.pending.is_some() {
        add_local_event(app, "An upstream authentication operation is unresolved; wait for its reply or reconnect");
        return;
    }
    if app.irc_handles.get(&id).is_none_or(|handle| handle.sender().send(Command::AUTHENTICATE(mechanism.into())).is_err()) {
        add_local_event(app, "Could not start upstream authentication");
        return;
    }
    session.pending = Some(Pending { payload, started: Instant::now(), expired: false, operation: if clear { Operation::Clear } else { Operation::Login }, payload_sent: false, allow_insecure_upstream: allow_insecure });
    app.upstream_auth_event(&id, "Upstream authentication submitted; waiting for the bouncer");
}

impl super::App {
    fn upstream_auth_event(&mut self, id: &str, text: &str) {
        if let Some(conn) = self.state.connections.get(id) {
            let buffer = crate::state::buffer::make_buffer_id(id, &conn.label);
            self.add_event_to_buffer(&buffer, text.into());
        }
    }

    pub(crate) fn handle_upstream_auth(&mut self, id: &str, message: &Message) -> bool {
        if let Command::CAP(_, subcommand, field3, field4) = &message.command {
            if matches!(subcommand, CapSubCommand::NEW | CapSubCommand::DEL) {
                for token in field4.as_deref().or(field3.as_deref()).unwrap_or("").split_whitespace() {
                    let (name, value) = token.split_once('=').unwrap_or((token, ""));
                    if name.eq_ignore_ascii_case("sasl") {
                        let session = self.upstream_auth.entry(id.into()).or_default();
                        session.mechanisms = if *subcommand == CapSubCommand::DEL { HashSet::new() }
                            else { value.split(',').map(str::to_ascii_uppercase).collect() };
                        let invalidated = session.pending.as_mut().is_some_and(|pending| {
                            let mechanism = if matches!(pending.operation, Operation::Clear) { "ANONYMOUS" } else { "PLAIN" };
                            if pending.expired || session.mechanisms.contains(mechanism) { return false; }
                            pending.payload = None;
                            pending.expired = true;
                            true
                        });
                        if invalidated {
                            if let Some(handle) = self.irc_handles.get(id) { let _ = handle.sender().send(Command::AUTHENTICATE("*".into())); }
                            self.upstream_auth_event(id, "Upstream SASL availability changed; operation aborted, await its reply before retrying");
                        }
                    }
                }
            }
            return false;
        }
        let upstream_tls = self.bouncer_children.get(id).is_some_and(|child| {
            !child.suspended && child.network.attributes.get("tls").is_some_and(|tls| tls == "1")
        });
        let Some(pending) = self.upstream_auth.get_mut(id).and_then(|session| session.pending.as_mut()) else { return false; };
        if let Command::AUTHENTICATE(challenge) = &message.command {
            if pending.expired { return true; }
            if !matches!(pending.operation, Operation::Clear) && !pending.allow_insecure_upstream && !upstream_tls {
                pending.payload = None;
                pending.expired = true;
                if let Some(handle) = self.irc_handles.get(id) { let _ = handle.sender().send(Command::AUTHENTICATE("*".into())); }
                self.upstream_auth_event(id, "Upstream TLS changed before credential delivery; authentication aborted");
                return true;
            }
            if challenge == "+" && let Some(payload) = pending.payload.take() {
                let sent = self.irc_handles.get(id).is_some_and(|handle| {
                    crate::irc::sasl_scram::chunk_authenticate(&payload).into_iter()
                        .all(|chunk| handle.sender().send(Command::AUTHENTICATE(chunk)).is_ok())
                });
                pending.payload_sent = sent;
                if !sent {
                    pending.expired = true;
                    self.upstream_auth_event(id, "Upstream authentication transport failed; reconnect before retrying");
                }
            } else {
                pending.payload = None;
                pending.expired = true;
                if let Some(handle) = self.irc_handles.get(id) { let _ = handle.sender().send(Command::AUTHENTICATE("*".into())); }
                self.upstream_auth_event(id, "Invalid upstream SASL challenge; operation aborted");
            }
            return true;
        }
        let numeric = match &message.command {
            Command::Response(response, _) => *response as u16,
            Command::Raw(name, _) => name.parse().unwrap_or(0),
            _ => 0,
        };
        if matches!(numeric, 900..=908) {
            if matches!(numeric, 900 | 901 | 908) { return false; }
            if pending.expired && numeric != 903 {
                self.upstream_auth_event(id, "Aborted upstream authentication remains unresolved; reconnect before retrying");
                return true;
            }
            let sent_payload = pending.payload_sent;
            let clear = matches!(pending.operation, Operation::Clear);
            self.upstream_auth.get_mut(id).unwrap().pending = None;
            let text = if numeric == 903 && sent_payload {
                if clear { "Saved upstream SASL credentials removed by the bouncer" }
                else { "Upstream authentication succeeded; the bouncer saved the IRC credentials" }
            } else if numeric == 907 { "The upstream connection is already authenticated" }
            else { "Upstream authentication failed" };
            self.upstream_auth_event(id, text);
            return true;
        }
        false
    }

    pub(crate) fn tick_upstream_auth(&mut self) {
        let mut expired = Vec::new();
        for (id, session) in &mut self.upstream_auth {
            if let Some(pending) = &mut session.pending
                && !pending.expired && pending.started.elapsed() >= Duration::from_secs(30) {
                pending.payload = None;
                pending.expired = true;
                expired.push(id.clone());
            }
        }
        for id in expired {
            if let Some(handle) = self.irc_handles.get(&id) { let _ = handle.sender().send(Command::AUTHENTICATE("*".into())); }
            self.upstream_auth_event(&id, "Upstream authentication timed out; outcome unknown, wait for a reply or reconnect before retrying");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::irc::{IrcHandle, IrcSender};

    fn app() -> (crate::app::App, IrcSender) {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let mut connection = crate::state::events::tests::make_test_connection();
        connection.origin_config.bouncer_network_id = Some("1".into());
        connection.origin_config.tls = true;
        connection.origin_config.tls_verify = true;
        connection.status = crate::state::connection::ConnectionStatus::Connected;
        app.state.add_connection(connection);
        app.bouncer_children.insert("libera".into(), crate::app::bouncer_children::ChildNetwork {
            parent: "control".into(), scope: "fixture-scope".into(), suspended: false, manually_disconnected: false,
            network: crate::irc::bouncer::Network { id: "1".into(), attributes: std::collections::BTreeMap::from([("tls".into(), "1".into())]) },
        });
        app.state.add_buffer(crate::state::events::tests::make_test_buffer("libera", crate::state::buffer::BufferType::Server, "libera"));
        let sender = IrcSender::capturing(0);
        app.irc_handles.insert("libera".into(), IrcHandle::new("libera".into(), sender.clone(), None, None));
        app.state.set_active_buffer("libera/libera");
        app.handle_upstream_auth("libera", &":bouncer CAP * NEW :sasl=PLAIN,ANONYMOUS".parse().unwrap());
        (app, sender)
    }

    fn login(app: &mut crate::app::App) {
        command_with_secret(app, &["login".into(), "irc-account".into(), "IRC_SECRET".into()], |key| {
            assert_eq!(key, "IRC_SECRET");
            Some("disposable password".into())
        });
    }

    #[tokio::test]
    async fn login_uses_separate_credentials_and_waits_for_challenge() {
        let (mut app, sender) = app();
        let old = app.state.connections["libera"].origin_config.sasl_pass.clone();
        login(&mut app);
        assert_eq!(sender.captured().len(), 1);
        assert!(app.upstream_auth["libera"].pending.is_some());
        app.handle_upstream_auth("libera", &"AUTHENTICATE +".parse().unwrap());
        let commands = sender.captured();
        let Command::AUTHENTICATE(encoded) = &commands[1].command else { panic!("expected credentials"); };
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(encoded).unwrap(), b"\0irc-account\0disposable password");
        app.handle_upstream_auth("libera", &":bouncer 903 me :Success".parse().unwrap());
        assert!(app.upstream_auth["libera"].pending.is_none());
        assert_eq!(app.state.connections["libera"].origin_config.sasl_pass, old);
    }

    #[tokio::test]
    async fn unavailable_or_unverified_connections_never_send_secrets() {
        let (mut app, sender) = app();
        app.handle_upstream_auth("libera", &":bouncer CAP * DEL :sasl".parse().unwrap());
        login(&mut app);
        assert!(sender.captured().is_empty());
        app.handle_upstream_auth("libera", &":bouncer CAP * NEW :sasl=PLAIN,ANONYMOUS".parse().unwrap());
        app.state.connections.get_mut("libera").unwrap().origin_config.tls_verify = false;
        login(&mut app);
        assert!(sender.captured().is_empty());
    }

    #[tokio::test]
    async fn expired_exchange_blocks_retry_and_drops_unsent_secret() {
        let (mut app, sender) = app();
        login(&mut app);
        app.upstream_auth.get_mut("libera").unwrap().pending.as_mut().unwrap().started = Instant::now().checked_sub(Duration::from_secs(31)).unwrap();
        app.tick_upstream_auth();
        assert!(app.upstream_auth["libera"].pending.as_ref().unwrap().payload.is_none());
        let count = sender.captured().len();
        app.handle_upstream_auth("libera", &"AUTHENTICATE +".parse().unwrap());
        login(&mut app);
        assert_eq!(sender.captured().len(), count);
        app.cancel_connection_attempt("libera");
        assert!(!app.upstream_auth.contains_key("libera"));
    }

    #[tokio::test]
    async fn clear_requires_explicit_confirmation_and_sends_empty_trace() {
        let (mut app, sender) = app();
        command_with_secret(&mut app, &["clear".into()], |_| panic!("must not load secret"));
        assert!(sender.captured().is_empty());
        command_with_secret(&mut app, &["clear".into(), "-YES".into()], |_| panic!("must not load secret"));
        assert!(matches!(&sender.captured()[0].command, Command::AUTHENTICATE(mechanism) if mechanism == "ANONYMOUS"));
        app.handle_upstream_auth("libera", &"AUTHENTICATE +".parse().unwrap());
        assert!(matches!(&sender.captured()[1].command, Command::AUTHENTICATE(value) if value == "+"));
    }
    #[tokio::test]
    async fn withdrawn_capability_drops_a_pending_secret() {
        let (mut app, sender) = app();
        login(&mut app);
        app.handle_upstream_auth("libera", &":bouncer CAP * DEL :sasl".parse().unwrap());
        let count = sender.captured().len();
        assert!(app.upstream_auth["libera"].pending.as_ref().unwrap().payload.is_none());
        app.handle_upstream_auth("libera", &"AUTHENTICATE +".parse().unwrap());
        assert_eq!(sender.captured().len(), count);
    }

    #[tokio::test]
    async fn prewelcome_upstream_capability_survives_connected_event() {
        let (mut app, _) = app();
        app.handle_irc_event(crate::irc::IrcEvent::Connected("libera".into(), HashSet::new(), None));
        assert!(app.upstream_auth["libera"].mechanisms.contains("PLAIN"));
        login(&mut app);
        assert!(app.upstream_auth["libera"].pending.is_some());
    }

    #[tokio::test]
    async fn replies_stay_on_origin_and_premature_success_is_rejected() {
        let (mut app, sender) = app();
        app.state.add_buffer(crate::state::events::tests::make_test_buffer("other", crate::state::buffer::BufferType::Server, "other"));
        app.state.set_active_buffer("libera/libera");
        login(&mut app);
        login(&mut app);
        assert_eq!(sender.captured().len(), 1);
        app.state.set_active_buffer("other/other");
        assert!(!app.handle_upstream_auth("other", &":server 903 me :Success".parse().unwrap()));
        assert!(app.upstream_auth["libera"].pending.is_some());
        app.handle_upstream_auth("libera", &":server 903 me :Unverified success".parse().unwrap());
        assert!(app.state.buffers["libera/libera"].messages.back().unwrap().text.contains("failed"));
        assert!(app.state.buffers["other/other"].messages.is_empty());
        assert_eq!(sender.captured().len(), 1);
    }

    #[tokio::test]
    async fn late_abort_cannot_unlock_an_ambiguous_exchange() {
        let (mut app, sender) = app();
        login(&mut app);
        app.handle_upstream_auth("libera", &"AUTHENTICATE +".parse().unwrap());
        app.upstream_auth.get_mut("libera").unwrap().pending.as_mut().unwrap().started = Instant::now().checked_sub(Duration::from_secs(31)).unwrap();
        app.tick_upstream_auth();
        app.handle_upstream_auth("libera", &":server 906 me :Aborted".parse().unwrap());
        let count = sender.captured().len();
        login(&mut app);
        assert_eq!(sender.captured().len(), count);
        assert!(app.upstream_auth["libera"].pending.is_some());
    }

    #[tokio::test]
    async fn upstream_logged_in_updates_history_ownership_during_exchange() {
        let (mut app, _) = app();
        login(&mut app);
        let nick = app.state.connections["libera"].nick.clone();
        app.handle_irc_event(crate::irc::IrcEvent::Message("libera".into(), Box::new(
            format!(":bouncer 900 {nick} {nick}!user@host irc-account :Logged in").parse().unwrap()
        )));
        assert!(app.upstream_auth["libera"].pending.is_some());
        app.state.add_buffer(crate::state::events::tests::make_test_buffer("libera", crate::state::buffer::BufferType::Query, "peer"));
        app.state.set_active_buffer("libera/libera");
        let mut history = app.state.buffers["libera/libera"].messages.front().unwrap().clone();
        history.id = app.state.next_message_id();
        history.message_type = crate::state::buffer::MessageType::Message;
        history.nick = Some("EarlierNick".into());
        history.text = "own historical message".into();
        history.event_key = None;
        history.event_params = None;
        history.tags = Some(std::collections::HashMap::from([("account".into(), "irc-account".into()), ("msgid".into(), "own-history".into())]));
        app.state.surface_history_page("libera/peer", vec![history], false);
        assert_eq!(app.state.buffers["libera/peer"].unread_count, 0);
    }

    #[tokio::test]
    async fn upstream_plain_prepares_unicode_credentials() {
        let (mut app, sender) = app();
        command_with_secret(&mut app, &["login".into(), "irc\u{ad}-account".into(), "IRC_SECRET".into()], |_| Some("disposable\u{a0}password".into()));
        app.handle_upstream_auth("libera", &"AUTHENTICATE +".parse().unwrap());
        let commands = sender.captured();
        let Command::AUTHENTICATE(encoded) = &commands[1].command else { panic!("expected credentials"); };
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(encoded).unwrap(), b"\0irc-account\0disposable password");
    }

    #[tokio::test]
    async fn clear_or_unknown_upstream_requires_explicit_override_before_secret_load() {
        for tls in [Some("0"), None] {
            let (mut app, sender) = app();
            let attributes = &mut app.bouncer_children.get_mut("libera").unwrap().network.attributes;
            attributes.remove("tls");
            if let Some(tls) = tls { attributes.insert("tls".into(), tls.into()); }
            command_with_secret(&mut app, &["login".into(), "account".into(), "SECRET".into()], |_| panic!("must not load a secret"));
            assert!(sender.captured().is_empty());
            command_with_secret(&mut app, &["login".into(), "-allow-insecure-upstream".into(), "account".into(), "SECRET".into()], |_| Some("password".into()));
            assert_eq!(sender.captured().len(), 1);
        }
    }

    #[tokio::test]
    async fn upstream_downgrade_before_challenge_discards_secret() {
        let (mut app, sender) = app();
        login(&mut app);
        app.bouncer_children.get_mut("libera").unwrap().network.attributes.insert("tls".into(), "0".into());
        app.handle_upstream_auth("libera", &"AUTHENTICATE +".parse().unwrap());
        assert!(matches!(&sender.captured()[1].command, Command::AUTHENTICATE(value) if value == "*"));
        assert!(app.upstream_auth["libera"].pending.as_ref().unwrap().payload.is_none());
    }

}

#[cfg(test)]
#[path = "upstream_auth_fixture.rs"]
mod fixture;

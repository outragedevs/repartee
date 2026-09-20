use std::collections::HashMap;
use std::time::{Duration, Instant};

use irc::proto::{Command, Message, command::CapSubCommand};
use zeroize::Zeroizing;

const CAP: &str = "draft/account-registration";

#[derive(Default)]
pub struct Session {
    rules: Option<HashMap<String, Option<String>>>,
    pending: Option<Pending>,
}

impl Session {
    pub(crate) fn from_rules(value: &str) -> Self {
        Self { rules: Some(value.split(',').filter(|part| !part.is_empty()).map(|part| {
            let (key, value) = part.split_once('=').map_or((part, None), |(key, value)| (key, Some(value.to_string())));
            (key.to_string(), value)
        }).collect()), pending: None }
    }
}

struct Pending {
    command: &'static str,
    secret: Zeroizing<String>,
    started: Instant,
    expired: bool,
}

fn token(value: &str) -> bool {
    !value.is_empty() && !value.starts_with(':') && !value.chars().any(|c| c.is_control() || c.is_whitespace())
}

pub fn command(app: &mut super::App, args: &[String]) {
    command_with_secret(app, args, |key| {
        let path = crate::constants::env_path();
        #[cfg(test)]
        let path = std::env::var_os("REPARTEE_UPSTREAM_AUTH_ENV").map_or(path, std::path::PathBuf::from);
        crate::config::load_env(&path).ok()?.remove(key)
    });
}

fn command_with_secret(app: &mut super::App, args: &[String], load: impl FnOnce(&str) -> Option<String>) {
    use crate::commands::helpers::add_local_event;
    let register = args.first().is_some_and(|arg| arg == "register");
    let verify = args.first().is_some_and(|arg| arg == "verify");
    let insecure = args.get(1).is_some_and(|arg| arg == "-allow-insecure-upstream");
    let values = args.get(if insecure { 2.. } else { 1.. }).unwrap_or_default();
    if (!register && !verify) || values.len() != if register { 3 } else { 2 } {
        add_local_event(app, "Usage: /account register [-allow-insecure-upstream] <account|*> <email|*> <password-env> | /account verify [-allow-insecure-upstream] <account|*> <code-env>");
        return;
    }
    let Some(id) = app.active_conn_id().map(str::to_string) else { add_local_event(app, "No active connection"); return; };
    let Some(conn) = app.state.connections.get(&id).filter(|conn| {
        conn.status == crate::state::connection::ConnectionStatus::Connected
            && conn.bouncer_network_id().is_some() && !conn.bouncer_control()
            && conn.origin_config.tls && conn.origin_config.tls_verify && conn.enabled_caps.contains(CAP)
    }) else { add_local_event(app, "Account registration requires a bound bouncer network, verified TLS and acknowledged account-registration support"); return; };
    let Some(session) = app.account_registration.get(&id).filter(|session| session.rules.is_some()) else {
        add_local_event(app, "The upstream account registration rules are unavailable"); return;
    };
    if session.pending.is_some() || app.has_upstream_auth_pending(&id) {
        add_local_event(app, "An account operation is unresolved; wait for its reply or reconnect"); return;
    }
    if !insecure && !app.bouncer_children.get(&id).is_some_and(|child| !child.suspended && child.network.attributes.get("tls").is_some_and(|tls| tls == "1")) {
        add_local_event(app, "Upstream TLS is disabled or unknown. Passwords and verification codes would be exposed on an unencrypted upstream hop; use -allow-insecure-upstream only to explicitly accept that risk"); return;
    }
    let rules = session.rules.as_ref().unwrap();
    let key = values.last().unwrap();
    if !token(&values[0]) || (register && !token(&values[1])) || key.is_empty()
        || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        add_local_event(app, "Invalid account, email or secret reference"); return;
    }
    if register && !rules.contains_key("custom-account-name") && values[0] != "*" && values[0] != conn.nick {
        add_local_event(app, "This server requires the current nickname as the account name; use *"); return;
    }
    if register && rules.contains_key("email-required") && values[1] == "*" {
        add_local_event(app, "This server requires an email address"); return;
    }
    let Some(secret) = load(key).map(Zeroizing::new).filter(|secret| !secret.is_empty() && !secret.chars().any(char::is_control)) else {
        add_local_event(app, "Account secret is missing or invalid in .env"); return;
    };
    if register {
        for (rule, minimum) in [("min-password-length", true), ("max-password-length", false)] {
            if let Some(value) = rules.get(rule) {
                let Some(limit) = value.as_deref().and_then(|value| value.parse::<usize>().ok()).filter(|limit| *limit > 0) else {
                    add_local_event(app, "Server advertised an invalid password length constraint"); return;
                };
                if (minimum && secret.len() < limit) || (!minimum && secret.len() > limit) {
                    add_local_event(app, "Password does not meet the server's advertised byte-length constraint"); return;
                }
            }
        }
    }
    let verb = if register { "REGISTER" } else { "VERIFY" };
    let mut params = values[..values.len() - 1].to_vec();
    params.push(secret.to_string());
    let message: Message = Command::Raw(verb.into(), params).into();
    if message.to_string().len() > 512 {
        add_local_event(app, "Account request exceeds the IRC line limit"); return;
    }
    if app.irc_handles.get(&id).is_none_or(|handle| handle.sender().send(message).is_err()) {
        add_local_event(app, "Could not send the account request"); return;
    }
    app.account_registration.get_mut(&id).unwrap().pending = Some(Pending { command: verb, secret, started: Instant::now(), expired: false });
    app.account_registration_event(&id, "Account request submitted; waiting for the upstream server");
}

impl super::App {
    pub(crate) fn has_account_registration_pending(&self, id: &str) -> bool {
        self.account_registration.get(id).is_some_and(|session| session.pending.is_some())
    }

    fn account_registration_event(&mut self, id: &str, text: &str) {
        if let Some(conn) = self.state.connections.get(id) {
            let buffer = crate::state::buffer::make_buffer_id(id, &conn.label);
            self.add_event_to_buffer(&buffer, crate::commands::helpers::escape_format(text));
        }
    }

    pub(crate) fn handle_account_registration(&mut self, id: &str, message: &Message) -> bool {
        if let Command::CAP(_, subcommand, field3, field4) = &message.command {
            if matches!(subcommand, CapSubCommand::NEW | CapSubCommand::DEL) {
                for token in field4.as_deref().or(field3.as_deref()).unwrap_or("").split_whitespace() {
                    let (name, value) = token.split_once('=').unwrap_or((token, ""));
                    if name.eq_ignore_ascii_case(CAP) {
                        let session = self.account_registration.entry(id.into()).or_default();
                        session.rules = if *subcommand == CapSubCommand::DEL { None } else {
                            Session::from_rules(value).rules
                        };
                    }
                }
            }
            return false;
        }
        let Command::Raw(command, args) = &message.command else { return false; };
        let Some(pending) = self.account_registration.get(id).and_then(|session| session.pending.as_ref()) else { return false; };
        let failure = command.eq_ignore_ascii_case("FAIL") && args.first().is_some_and(|arg| arg.eq_ignore_ascii_case(pending.command));
        let reply = command.eq_ignore_ascii_case(pending.command);
        if !failure && !reply { return false; }
        let valid = if failure { args.len() >= 3 } else {
            args.len() >= 3 && (args[0].eq_ignore_ascii_case("SUCCESS") || (pending.command == "REGISTER" && args[0].eq_ignore_ascii_case("VERIFICATION_REQUIRED")))
        };
        if !valid { return true; }
        let status = if failure { "failed" } else if args[0].eq_ignore_ascii_case("VERIFICATION_REQUIRED") { "requires verification" } else { "succeeded" };
        let detail = args.last().unwrap().replace(pending.secret.as_str(), "[redacted]");
        let text = format!("{} {status}: {detail}", pending.command);
        if !failure && args[0].eq_ignore_ascii_case("SUCCESS") && let Some(conn) = self.state.connections.get(id) {
            let nick = conn.nick.clone();
            self.state.record_read_account(id, &nick, Some(&args[1]));
        }
        self.account_registration.get_mut(id).unwrap().pending = None;
        self.account_registration_event(id, &text);
        true
    }

    pub(crate) fn tick_account_registration(&mut self) {
        let expired: Vec<String> = self.account_registration.iter_mut().filter_map(|(id, session)| {
            let pending = session.pending.as_mut()?;
            if pending.expired || pending.started.elapsed() < Duration::from_secs(30) { return None; }
            pending.expired = true;
            Some(id.clone())
        }).collect();
        for id in expired {
            self.account_registration_event(&id, "Account request timed out; outcome unknown. Wait for the server's reply or reconnect before retrying");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::irc::{IrcHandle, IrcSender};

    fn app(rules: &str) -> (crate::app::App, IrcSender) {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let config = toml::from_str("label='fixture'\naddress='localhost'\nport=6697\ntls=true\nchannels=[]\nbouncer_network_id='1'").unwrap();
        app.setup_connection("fixture", &config);
        let conn = app.state.connections.get_mut("fixture").unwrap();
        conn.status = crate::state::connection::ConnectionStatus::Connected;
        conn.enabled_caps.insert(CAP.into());
        let sender = IrcSender::capturing(0);
        app.irc_handles.insert("fixture".into(), IrcHandle::new("fixture".into(), sender.clone(), None, None));
        app.handle_account_registration("fixture", &format!(":server CAP * NEW :{CAP}={rules}").parse().unwrap());
        (app, sender)
    }

    fn register(app: &mut crate::app::App, secret: &str) {
        command_with_secret(app, &["register".into(), "-allow-insecure-upstream".into(), "*".into(), "user@example.org".into(), "SECRET".into()], |key| {
            assert_eq!(key, "SECRET");
            Some(secret.into())
        });
    }

    #[tokio::test]
    async fn initial_rules_survive_handle_ready_and_withdrawal_blocks_new_requests() {
        let (mut app, sender) = app("");
        let mut handle = IrcHandle::new("fixture".into(), sender.clone(), None, None);
        handle.account_registration_rules = Some("email-required,min-password-length=8".into());
        app.handle_irc_event(crate::irc::IrcEvent::HandleReady(Box::new(handle)));
        register(&mut app, "short");
        assert!(sender.captured().is_empty());
        register(&mut app, "long-password");
        assert_eq!(sender.captured().len(), 1);
        app.handle_account_registration("fixture", &"CAP * DEL :draft/account-registration".parse().unwrap());
        app.handle_account_registration("fixture", &"register success account :Created".parse().unwrap());
        register(&mut app, "long-password");
        assert_eq!(sender.captured().len(), 1);
    }

    #[tokio::test]
    async fn success_without_numeric_login_records_history_ownership() {
        use crate::state::buffer::{Buffer, BufferType};
        for (verb, status, expected) in [("REGISTER", "SUCCESS", 0), ("VERIFY", "SUCCESS", 0), ("REGISTER", "VERIFICATION_REQUIRED", 1)] {
            let (mut app, _) = app("");
            app.state.connections.get_mut("fixture").unwrap().enabled_caps.insert("draft/read-marker".into());
            app.state.add_buffer_with_focus(Buffer::empty("fixture", BufferType::Query, "Peer"), false);
            app.state.apply_server_read_marker("fixture/peer", 1000);
            app.account_registration.get_mut("fixture").unwrap().pending = Some(Pending {
                command: verb, secret: Zeroizing::new("private-secret".into()), started: Instant::now(), expired: false,
            });
            app.handle_irc_event(crate::irc::IrcEvent::Message("fixture".into(), Box::new(format!("{verb} {status} new-account :Result").parse().unwrap())));
            let mut row = crate::state::events::tests::make_test_message(&mut app.state, "Earlier own message");
            row.nick = Some("EarlierNick".into());
            row.timestamp = chrono::DateTime::from_timestamp_millis(2000).unwrap();
            row.tags = Some(HashMap::from([("account".into(), "new-account".into()), ("time".into(), row.timestamp.to_rfc3339())]));
            app.state.surface_history_page("fixture/peer", vec![row], false);
            assert_eq!(app.state.buffers["fixture/peer"].unread_count, expected, "{verb} {status}");
        }
    }

    #[tokio::test]
    async fn registration_preserves_opaque_password_and_redacts_server_echo() {
        let (mut app, sender) = app("email-required,min-password-length=2,max-password-length=100");
        register(&mut app, "pass\u{a0}word");
        assert_eq!(sender.captured().len(), 1);
        let Command::Raw(command, args) = &sender.captured()[0].command else { panic!("expected REGISTER"); };
        assert_eq!(command, "REGISTER");
        assert_eq!(args[2], "pass\u{a0}word");
        app.handle_account_registration("fixture", &"REGISTER VERIFICATION_REQUIRED account :Verify pass\u{a0}word at https://example.org/check".parse().unwrap());
        assert!(!app.has_account_registration_pending("fixture"));
        let text = &app.state.buffers["fixture/fixture"].messages.back().unwrap().text;
        assert!(text.contains("requires verification"));
        assert!(text.contains("https://example.org/check"));
        assert!(!text.contains("pass\u{a0}word"));
    }

    #[tokio::test]
    async fn constraints_use_bytes_and_refuse_invalid_metadata() {
        for rules in ["max-password-length=3", "min-password-length=5", "max-password-length=0", "min-password-length=invalid", "max-password-length"] {
            let (mut app, sender) = app(rules);
            register(&mut app, "😀");
            assert!(sender.captured().is_empty(), "{rules}");
        }
        let (mut app, sender) = app("min-password-length=4,max-password-length=4");
        register(&mut app, "😀");
        assert_eq!(sender.captured().len(), 1);
    }

    #[tokio::test]
    async fn timeout_blocks_both_account_and_sasl_retries_until_terminal_reply() {
        let (mut app, sender) = app("");
        register(&mut app, "password");
        app.account_registration.get_mut("fixture").unwrap().pending.as_mut().unwrap().started = Instant::now().checked_sub(Duration::from_secs(31)).unwrap();
        app.tick_account_registration();
        register(&mut app, "another");
        crate::app::upstream_auth::command(&mut app, &["clear".into(), "-YES".into()]);
        assert_eq!(sender.captured().len(), 1);
        app.handle_account_registration("fixture", &"fail register ACCOUNT_EXISTS * :Account already exists".parse().unwrap());
        assert!(!app.has_account_registration_pending("fixture"));
        assert!(app.state.buffers["fixture/fixture"].messages.back().unwrap().text.contains("failed"));
    }

    #[tokio::test]
    async fn verify_requires_acknowledged_capability_and_handles_success() {
        let (mut app, sender) = app("");
        let args = ["verify".into(), "-allow-insecure-upstream".into(), "*".into(), "CODE".into()];
        app.state.connections.get_mut("fixture").unwrap().enabled_caps.remove(CAP);
        command_with_secret(&mut app, &args, |_| panic!("unacknowledged capability must not load secret"));
        assert!(sender.captured().is_empty());
        app.state.connections.get_mut("fixture").unwrap().enabled_caps.insert(CAP.into());
        command_with_secret(&mut app, &args, |_| Some("verification-code".into()));
        assert!(app.has_account_registration_pending("fixture"));
        assert!(!app.handle_account_registration("other", &"verify success account :Verified".parse().unwrap()));
        app.handle_account_registration("fixture", &"verify success account :Verified".parse().unwrap());
        assert!(!app.has_account_registration_pending("fixture"));
    }
}

#[cfg(test)]
#[path = "account_registration_fixture.rs"]
mod fixture;

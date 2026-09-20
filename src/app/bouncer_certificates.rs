use std::time::{Duration, Instant};

use irc::proto::{Command, Message, Prefix};

use crate::commands::helpers::{add_local_event, escape_format};
use crate::state::connection::ConnectionStatus;

const CAP: &str = "soju.im/client-cert";
const MAX_CERTIFICATES: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
enum Request {
    List,
    Create,
    Delete(Option<String>),
}

pub struct Operation {
    request: Request,
    nonce: String,
    started: Instant,
    timed_out: bool,
    failed: bool,
    acknowledged: bool,
    batch: Option<String>,
    rows: Vec<String>,
}

fn fingerprint(value: &str) -> bool {
    value.len() == 128 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn attribute(value: &str) -> String {
    let mut chars = value.chars();
    let mut output = String::new();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some(':') => output.push(';'),
                Some('s' | 'r' | 'n') => output.push(' '),
                Some(ch) => output.push(ch),
                None => {}
            }
        } else if !ch.is_control() {
            output.push(ch);
        }
    }
    output
}

fn certificate_row(args: &[String]) -> Option<String> {
    let hash = args.first().filter(|value| fingerprint(value))?;
    let mut fields = Vec::new();
    for entry in args.iter().skip(1).flat_map(|arg| arg.split(';')) {
        let Some((key, value)) = entry.split_once('=') else { continue; };
        if matches!(key, "name" | "time" | "ip") {
            fields.push(format!("{key}={}", attribute(value)));
        }
    }
    Some(format!("{} {}", hash.to_ascii_uppercase(), fields.join(" ")).trim_end().to_string())
}

pub fn command(app: &mut super::App, args: &[String]) {
    let Some(id) = app.active_conn_id().map(str::to_string) else { add_local_event(app, "No active connection"); return; };
    if !app.certificates_available(&id) {
        add_local_event(app, "Certificates require verified TLS, successful bouncer SASL authentication, and client-cert and batch support");
        return;
    }
    if app.bouncer_certificates.contains_key(&id) {
        add_local_event(app, "A certificate request is pending; wait for its reply or reconnect before retrying");
        return;
    }
    let (request, parameters) = match args {
        [] => (Request::List, vec!["LIST".into()]),
        [action] if action.eq_ignore_ascii_case("list") => (Request::List, vec!["LIST".into()]),
        [action, name @ ..] if action.eq_ignore_ascii_case("create") && !name.is_empty() => {
            let name = name.join(" ");
            if name.is_empty() || name.len() > 64 || name.chars().any(char::is_control) {
                add_local_event(app, "Certificate name must contain 1–64 UTF-8 bytes and no control characters"); return;
            }
            (Request::Create, vec!["CREATE".into(), name])
        }
        [action] if action.eq_ignore_ascii_case("delete") => (Request::Delete(None), vec!["DELETE".into()]),
        [action, hash] if action.eq_ignore_ascii_case("delete") && fingerprint(hash) =>
            (Request::Delete(Some(hash.to_ascii_uppercase())), vec!["DELETE".into(), hash.clone()]),
        _ => { add_local_event(app, "Usage: /bcert list | create <name> | delete [SHA-512 fingerprint]"); return; }
    };
    let sender = app.irc_handles[&id].sender();
    if sender.send(Command::Raw("CLIENTCERT".into(), parameters)).is_err() {
        add_local_event(app, "Could not send certificate request"); return;
    }
    let nonce = format!("{}-cert-{}", crate::constants::APP_NAME, uuid::Uuid::new_v4().simple());
    app.bouncer_certificates.insert(id.clone(), Operation {
        request, nonce: nonce.clone(), started: Instant::now(), timed_out: false,
        failed: false, acknowledged: false, batch: None, rows: Vec::new(),
    });
    if sender.send(Command::PING(nonce, None)).is_err() {
        add_local_event(app, "Certificate request sent, but completion check failed; result unknown, reconnect before retrying");
    } else {
        add_local_event(app, "Certificate request sent; waiting for the bouncer reply");
    }
}

impl super::App {
    fn certificates_available(&self, id: &str) -> bool {
        self.state.connections.get(id).is_some_and(|conn| conn.status == ConnectionStatus::Connected
            && conn.origin_config.tls && conn.origin_config.tls_verify
            && (conn.bouncer_control() || conn.bouncer_network_id().is_some())
            && conn.enabled_caps.contains(CAP) && conn.enabled_caps.contains("batch"))
            && self.irc_handles.get(id).is_some_and(|handle| handle.sasl_authenticated)
    }

    pub(crate) fn tick_bouncer_certificates(&mut self) {
        let ids: Vec<_> = self.bouncer_certificates.keys().cloned().collect();
        for id in ids {
            let available = self.certificates_available(&id);
            let operation = self.bouncer_certificates.get_mut(&id).unwrap();
            operation.failed |= !available;
            if !operation.timed_out && operation.started.elapsed() > Duration::from_secs(30) {
                operation.timed_out = true;
                self.certificate_notice(&id, "Certificate request timed out; result unknown. Wait for its reply or reconnect before retrying");
            }
        }
    }

    fn certificate_notice(&mut self, id: &str, text: &str) {
        if let Some(conn) = self.state.connections.get(id) {
            let buffer = crate::state::buffer::make_buffer_id(id, &conn.label);
            self.add_event_to_buffer(&buffer, escape_format(text));
            self.drain_pending_web_events();
        }
    }

    pub(crate) fn handle_bouncer_certificates(&mut self, id: &str, message: &Message) -> bool {
        if matches!(&message.prefix, Some(Prefix::Nickname(_, user, host)) if !user.is_empty() || !host.is_empty()) { return false; }
        if let Command::PONG(first, second) = &message.command {
            if !self.bouncer_certificates.get(id).is_some_and(|operation| first == &operation.nonce || second.as_ref() == Some(&operation.nonce)) { return false; }
            let operation = self.bouncer_certificates.remove(id).unwrap();
            if operation.failed || !self.certificates_available(id) || !operation.acknowledged {
                self.certificate_notice(id, "Certificate request did not complete successfully; no change is assumed");
            } else {
                match operation.request {
                    Request::List => {
                        self.certificate_notice(id, &format!("Pinned certificates: {}", operation.rows.len()));
                        for row in operation.rows { self.certificate_notice(id, &row); }
                    }
                    Request::Create => self.certificate_notice(id, "Current TLS client certificate pinned. SASL EXTERNAL can be selected for future connections; authentication settings were not changed"),
                    Request::Delete(_) => self.certificate_notice(id, "Certificate removed from the bouncer account"),
                }
            }
            return true;
        }
        if let Command::BATCH(reference, kind, _) = &message.command {
            if reference.starts_with('+') && kind.as_ref().is_some_and(|kind| kind.to_str().eq_ignore_ascii_case(CAP)) {
                if let Some(operation) = self.bouncer_certificates.get_mut(id) {
                    if operation.request != Request::List || operation.batch.is_some() { operation.failed = true; }
                    else { operation.batch = Some(reference[1..].to_string()); }
                }
                return true;
            }
            if let Some(operation) = self.bouncer_certificates.get_mut(id)
                && reference.strip_prefix('-') == operation.batch.as_deref() && operation.batch.is_some() {
                operation.acknowledged = true;
                return true;
            }
        }
        let Command::Raw(command, args) = &message.command else { return false; };
        if command.eq_ignore_ascii_case("FAIL") && args.first().is_some_and(|arg| arg.eq_ignore_ascii_case("CLIENTCERT")) {
            if let Some(operation) = self.bouncer_certificates.get_mut(id) { operation.failed = true; }
            self.certificate_notice(id, &format!("Certificate failure: {}", args[1..].join(" ")));
            return true;
        }
        if !command.eq_ignore_ascii_case("CLIENTCERT") { return false; }
        if args.len() == 1 && args[0].eq_ignore_ascii_case("CREATE")
            && self.bouncer_certificates.get(id).is_none_or(|operation| operation.request != Request::Create) {
            if self.certificates_available(id) {
                self.certificate_notice(id, "Bouncer confirmed that the current TLS client certificate is pinned");
            }
            return true;
        }
        let Some(operation) = self.bouncer_certificates.get_mut(id) else { return true; };
        let Some(action) = args.first() else { operation.failed = true; return true; };
        match (&operation.request, action.to_ascii_uppercase().as_str()) {
            (Request::List, "LIST") => {
                let batch = message.tags.as_ref().and_then(|tags| tags.iter().find(|tag| tag.0 == "batch")).and_then(|tag| tag.1.as_deref());
                if operation.batch.is_none() || batch != operation.batch.as_deref() || operation.acknowledged || operation.rows.len() >= MAX_CERTIFICATES {
                    operation.failed = true;
                } else if let Some(row) = certificate_row(&args[1..]) { operation.rows.push(row); }
                else { operation.failed = true; }
            }
            (Request::Create, "CREATE") | (Request::Delete(None), "DELETE") if args.len() == 1 => operation.acknowledged = true,
            (Request::Delete(expected), "DELETE") if args.len() == 2 && fingerprint(&args[1])
                && expected.as_ref().is_none_or(|hash| hash.eq_ignore_ascii_case(&args[1])) => operation.acknowledged = true,
            _ => operation.failed = true,
        }
        true
    }
}

#[cfg(test)]
#[path = "bouncer_certificates_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "bouncer_certificates_fixture.rs"]
mod fixture;

use std::time::{Duration, Instant};

use irc::proto::{Command, Message, Prefix};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use crate::irc::webpush::{self, Action, Reply, Status, WebRequest};
use crate::state::connection::ConnectionStatus;
use crate::web::protocol::WebEvent;

pub struct Operation {
    request_id: String,
    session_id: String,
    scope: String,
    vapid: String,
    endpoint: Zeroizing<String>,
    success: Status,
    nonce: String,
    started: Instant,
    acknowledged: bool,
    failed: bool,
    notified: bool,
}

impl super::App {
    fn webpush_configuration(&self, id: &str) -> Option<(String, String)> {
        let conn = self.state.connections.get(id)?;
        if conn.status != ConnectionStatus::Connected || !conn.origin_config.tls || !conn.origin_config.tls_verify
            || conn.origin_config.bouncer_control || conn.origin_config.bouncer_network_id.is_none()
            || !conn.enabled_caps.contains(webpush::CAP)
            || !self.irc_handles.get(id).is_some_and(|handle| handle.sasl_authenticated) { return None; }
        let vapid = conn.isupport_parsed.get("VAPID")?;
        if !webpush::valid_public_key(vapid) { return None; }
        let scope = format!("{:x}", Sha256::digest(conn.network_key().as_bytes()));
        Some((scope, vapid.to_string()))
    }

    fn webpush_response(&self, id: &str, request_id: &str, session_id: &str, status: Status) {
        let configuration = self.webpush_configuration(id);
        let _ = self.web_broadcaster.send(WebEvent::WebPush {
            connection_id: id.into(), request_id: request_id.into(), session_id: session_id.into(), status,
            scope: configuration.as_ref().map(|(scope, _)| scope.clone()),
            vapid: configuration.map(|(_, vapid)| vapid),
        });
    }

    pub(crate) fn handle_webpush_request(&mut self, request: &WebRequest, session_id: &str) {
        let id = &request.connection_id;
        let request_id = &request.request_id;
        if uuid::Uuid::parse_str(request_id).is_err() {
            self.webpush_response(id, request_id, session_id, Status::Invalid); return;
        }
        let Some((scope, vapid)) = self.webpush_configuration(id) else {
            self.webpush_response(id, request_id, session_id, Status::Unavailable); return;
        };
        if matches!(request.action, Action::Get) {
            self.webpush_response(id, request_id, session_id, Status::Ready); return;
        }
        if self.bouncer_webpush.contains_key(id) {
            self.webpush_response(id, request_id, session_id, Status::Busy); return;
        }
        let (command, endpoint, success) = match &request.action {
            Action::Register { scope: expected_scope, vapid: expected_vapid, subscription } if expected_scope == &scope && expected_vapid == &vapid =>
                (subscription.register(), subscription.endpoint.clone(), Status::Registered),
            Action::Unregister { scope: expected_scope, endpoint } if expected_scope == &scope =>
                (webpush::unregister(endpoint), endpoint.clone(), Status::Unregistered),
            Action::Register { .. } | Action::Unregister { .. } => {
                self.webpush_response(id, request_id, session_id, Status::Unavailable); return;
            }
            Action::Get => unreachable!(),
        };
        let Ok(command) = command else { self.webpush_response(id, request_id, session_id, Status::Invalid); return; };
        let sender = self.irc_handles[id].sender();
        if sender.send(command).is_err() { self.webpush_response(id, request_id, session_id, Status::Failed); return; }
        let nonce = format!("{}-push-{}", crate::constants::APP_NAME, uuid::Uuid::new_v4().simple());
        self.bouncer_webpush.insert(id.clone(), Operation {
            request_id: request_id.clone(), session_id: session_id.into(), scope, vapid,
            endpoint: Zeroizing::new(endpoint), success, nonce: nonce.clone(), started: Instant::now(),
            acknowledged: false, failed: false, notified: false,
        });
        if sender.send(Command::PING(nonce, None)).is_err() {
            self.bouncer_webpush.get_mut(id).unwrap().notified = true;
            self.webpush_response(id, request_id, session_id, Status::Unknown);
        }
    }

    pub(crate) fn cancel_bouncer_webpush(&mut self, id: &str) {
        if let Some(operation) = self.bouncer_webpush.remove(id) {
            self.webpush_response(id, &operation.request_id, &operation.session_id, Status::Unknown);
        }
    }

    pub(crate) fn tick_bouncer_webpush(&mut self) {
        let ids: Vec<_> = self.bouncer_webpush.keys().cloned().collect();
        for id in ids {
            let configuration = self.webpush_configuration(&id);
            let operation = self.bouncer_webpush.get_mut(&id).unwrap();
            operation.failed |= configuration.as_ref().is_none_or(|(scope, vapid)| scope != &operation.scope || vapid != &operation.vapid);
            if !operation.notified && (operation.failed || operation.started.elapsed() > Duration::from_secs(30)) {
                operation.notified = true;
                let request = operation.request_id.clone();
                let session = operation.session_id.clone();
                self.webpush_response(&id, &request, &session, Status::Unknown);
            }
        }
    }

    pub(crate) fn handle_bouncer_webpush(&mut self, id: &str, message: &Message) -> bool {
        if matches!(&message.prefix, Some(Prefix::Nickname(_, user, host)) if !user.is_empty() || !host.is_empty()) { return webpush::parse(message).is_some(); }
        if let Command::PONG(first, second) = &message.command {
            if !self.bouncer_webpush.get(id).is_some_and(|operation| first == &operation.nonce || second.as_ref() == Some(&operation.nonce)) { return false; }
            let operation = self.bouncer_webpush.remove(id).unwrap();
            let same_configuration = self.webpush_configuration(id).is_some_and(|(scope, vapid)| scope == operation.scope && vapid == operation.vapid);
            let status = if operation.failed || !operation.acknowledged || !same_configuration { Status::Failed }
                else { operation.success };
            self.webpush_response(id, &operation.request_id, &operation.session_id, status);
            return true;
        }
        let Some(reply) = webpush::parse(message) else { return false; };
        if let Some(operation) = self.bouncer_webpush.get_mut(id) {
            match reply {
                Reply::Registered(endpoint) if operation.success == Status::Registered && endpoint == operation.endpoint.as_str() => operation.acknowledged = true,
                Reply::Unregistered(endpoint) if operation.success == Status::Unregistered && endpoint == operation.endpoint.as_str() => operation.acknowledged = true,
                _ => operation.failed = true,
            }
        }
        true
    }
}

#[cfg(test)]
#[path = "bouncer_webpush_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "bouncer_webpush_fixture.rs"]
mod fixture;

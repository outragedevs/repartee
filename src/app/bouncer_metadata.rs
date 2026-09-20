#[path = "bouncer_metadata_operations.rs"]
mod operations;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use crate::commands::helpers::{add_local_event, escape_format};
use crate::irc::metadata::{self, Event, Key, Request};
use crate::state::connection::ConnectionStatus;

pub struct Session {
    scope: String,
    casemapping: String,
    seen: HashMap<Key, HashSet<String>>,
    started: Instant,
    acknowledged: HashSet<Key>,
    failed: bool,
    operation: Option<operations::Operation>,
}

pub fn command(app: &mut super::App, args: &[String]) {
    let Some(id) = app.active_conn_id().map(str::to_string) else { add_local_event(app, "No active connection"); return; };
    if !app.metadata_available(&id) || !app.state.connections[&id].enabled_caps.contains("batch") { add_local_event(app, "Metadata requires a connected bouncer network with acknowledged metadata and batch support"); return; }
    app.tick_bouncer_metadata();
    if args == ["sync"] {
        if app.bouncer_metadata.get(&id).is_some_and(|session| session.operation.is_some()) {
            add_local_event(app, "A metadata operation is pending; wait for its reply or reconnect before syncing"); return;
        }
        if let Ok(message) = metadata::request(Request::Unsubscribe) { let _ = app.irc_handles[&id].sender().send(message); }
        app.bouncer_metadata.remove(&id);
        app.tick_bouncer_metadata();
        return;
    }
    if !app.bouncer_metadata.get(&id).is_some_and(|session| !session.failed && session.acknowledged.len() == Key::ALL.len()) {
        add_local_event(app, "Metadata subscription is unresolved; wait for acknowledgement or use /bmeta sync"); return;
    }
    let key = |value: &str| match value { "pin" => Some(Key::Pinned), "mute" => Some(Key::Muted), "block" => Some(Key::Blocked), _ => Key::parse(value) };
    let request = match args {
        [action] if action == "subscriptions" => Request::Subscriptions,
        [target] => Request::Get(target),
        [target, action] if action == "list" => Request::List(target),
        [target, action] if action == "clear" => Request::Clear(target),
        [target, action, value] if key(action).is_some() => {
            let value = match value.as_str() { "on" | "1" => Some(true), "off" | "0" => Some(false), "unset" => None,
                _ => { add_local_event(app, "Metadata value must be on, off or unset"); return; } };
            Request::Set(target, key(action).unwrap(), value)
        }
        _ => { add_local_event(app, "Usage: /bmeta <target> [list|clear|pin|mute|block on|off|unset] | /bmeta sync|subscriptions"); return; }
    };
    match app.start_metadata_operation(&id, request, false) {
        Ok(()) => add_local_event(app, "Metadata request sent; server replies determine the resulting state"),
        Err(error) => add_local_event(app, error),
    }
}

impl super::App {
    fn metadata_available(&self, id: &str) -> bool {
        self.state.connections.get(id).is_some_and(|conn| conn.status == ConnectionStatus::Connected
            && conn.origin_config.bouncer_network_id.is_some() && !conn.origin_config.bouncer_control
            && conn.enabled_caps.contains(metadata::CAP)) && self.irc_handles.contains_key(id)
    }

    pub(crate) fn tick_bouncer_metadata(&mut self) {
        self.tick_metadata_operations();
        let ids: Vec<_> = self.state.connections.keys().cloned().collect();
        for id in ids {
            if !self.metadata_available(&id) { self.bouncer_metadata.remove(&id); continue; }
            let scope = self.state.connections[&id].network_key().to_string();
            let casemapping = self.state.connections[&id].isupport_parsed.casemapping().to_string();
            if self.bouncer_metadata.get(&id).is_some_and(|session| session.scope != scope || session.casemapping != casemapping) {
                self.bouncer_metadata.remove(&id);
            }
            self.state.reconcile_metadata_mapping(&scope, &casemapping);
            self.state.refresh_metadata_buffers(&id);
            if let Some(session) = self.bouncer_metadata.get_mut(&id) {
                if !session.failed && session.acknowledged.len() < Key::ALL.len() && session.started.elapsed() > Duration::from_secs(30) {
                    session.failed = true;
                    let buffer = crate::state::buffer::make_buffer_id(&id, &self.state.connections[&id].label);
                    self.add_event_to_buffer(&buffer, "Metadata subscription timed out; use /bmeta sync after checking the connection".into());
                }
            } else if let Ok(message) = metadata::request(Request::Subscribe)
                && self.irc_handles[&id].sender().send(message).is_ok() {
                self.bouncer_metadata.insert(id, Session { scope, casemapping, seen: HashMap::new(), started: Instant::now(), acknowledged: HashSet::new(), failed: false, operation: None });
            }
        }
    }

    fn purge_metadata_blocked_mentions(&mut self) {
        let mut message_ids = Vec::new();
        self.volatile_mentions.retain(|(scope, mention, _)| {
            let blocked = self.state.connections.iter().find(|(_, conn)| conn.network_key() == scope)
                .is_some_and(|(id, _)| self.state.metadata_policy(id, &mention.channel, Some(&mention.nick)).blocked);
            if blocked { message_ids.push(mention.source_message_id); }
            !blocked
        });
        if !message_ids.is_empty() {
            self.state.pending_web_events.push(crate::web::protocol::WebEvent::MentionsRedacted { message_ids });
        }
    }

    pub(crate) fn handle_bouncer_metadata(&mut self, id: &str, message: &irc::proto::Message) -> bool {
        let Some(event) = metadata::parse(message) else { return false; };
        if !self.metadata_available(id) { return false; }
        if matches!(&message.prefix, Some(irc::proto::Prefix::Nickname(_, user, host)) if !user.is_empty() || !host.is_empty()) { return true; }
        if self.bouncer_metadata.get(id).is_none_or(|session| session.scope != self.state.connections[id].network_key()) { return true; }
        self.observe_metadata_operation(id, message, &event);
        let session = self.bouncer_metadata.get_mut(id).unwrap();
        match event {
            Event::Value { target, key, value } => {
                if !session.acknowledged.contains(&key) {
                    session.seen.entry(key).or_default().insert(crate::irc::isupport::casefold(&target, &session.casemapping));
                }
                self.state.set_metadata(id, &target, key, value);
                if key == Key::Blocked && value { self.purge_metadata_blocked_mentions(); self.state.purge_metadata_blocked_rows(); }
            }
            Event::Subscribed(key) => {
                if session.acknowledged.insert(key) && !session.failed {
                    let seen = session.seen.remove(&key).unwrap_or_default();
                    self.state.reconcile_metadata_key(id, key, &seen);
                }
            }
            Event::Unsubscribed(key) => { session.acknowledged.remove(&key); }
            Event::Subscription(key) => {
                let buffer = crate::state::buffer::make_buffer_id(id, &self.state.connections[id].label);
                self.add_event_to_buffer(&buffer, format!("Subscribed metadata: {}", key.wire()));
            }
            Event::Failure { code, details } => {
                if session.acknowledged.len() < Key::ALL.len() { session.failed = true; }
                let buffer = crate::state::buffer::make_buffer_id(id, &self.state.connections[id].label);
                self.add_event_to_buffer(&buffer, escape_format(&format!("Metadata failed: {code} {details}")));
            }
            Event::Invalid => {}
        }
        self.drain_pending_web_events();
        true
    }
}

#[cfg(test)]
#[path = "bouncer_metadata_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "bouncer_metadata_fixture.rs"]
mod fixture;

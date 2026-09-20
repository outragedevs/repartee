use std::collections::HashSet;
use std::time::{Duration, Instant};
use irc::proto::{Command, Message};
use crate::irc::metadata::{self, Event, Key, Request};
use crate::commands::helpers::escape_format;

pub(super) struct Operation {
    target: Option<String>,
    reconciliation: bool,
    nonce: String,
    pub(super) started: Instant,
    timed_out: bool,
    failed: bool,
    values: HashSet<Key>,
    expected: HashSet<Key>,
}

impl super::super::App {
    pub(super) fn start_metadata_operation(&mut self, id: &str, request: Request<'_>, reconciliation: bool) -> Result<(), &'static str> {
        if !self.state.connections.get(id).is_some_and(|conn| conn.enabled_caps.contains("batch")) { return Err("Metadata operations require batch support; subscription updates remain active"); }
        let session = self.bouncer_metadata.get(id).ok_or("Metadata subscription is unavailable")?;
        if session.operation.is_some() { return Err("A metadata operation is still pending; wait for its reply or reconnect"); }
        let target = match request {
            Request::Get(target) | Request::List(target) | Request::Set(target, _, _) | Request::Clear(target) => Some(target.to_string()),
            _ => None,
        };
        let message = metadata::request(request)?;
        let nonce = format!("{}-metadata-{}", crate::constants::APP_NAME, uuid::Uuid::new_v4().simple());
        let sender = self.irc_handles.get(id).ok_or("Connection is unavailable")?.sender();
        sender.send(message).map_err(|_| "Could not send metadata request")?;
        self.bouncer_metadata.get_mut(id).unwrap().operation = Some(Operation {
            target, reconciliation,
            nonce: nonce.clone(), started: Instant::now(), timed_out: false, failed: false, values: HashSet::new(),
            expected: match request { Request::Set(_, key, _) => HashSet::from([key]), Request::Clear(_) => Key::ALL.into_iter().collect(), _ => HashSet::new() },
        });
        sender.send(Command::PING(nonce, None)).map_err(|_| "Metadata request sent, but its completion check failed; reconnect before retrying")
    }

    pub(super) fn observe_metadata_operation(&mut self, id: &str, message: &Message, event: &Event) {
        let Some(operation) = self.bouncer_metadata.get_mut(id).and_then(|session| session.operation.as_mut()) else { return; };
        match event {
            Event::Failure { .. } => operation.failed = true,
            Event::Value { target, key, .. } if matches!(&message.command, Command::Response(response, _) if *response as u16 == 761)
                || matches!(&message.command, Command::Raw(command, _) if command == "761") => {
                let mapping = self.state.connections[id].isupport_parsed.casemapping();
                if operation.target.as_ref().is_some_and(|expected| crate::irc::isupport::casefold(expected, mapping) == crate::irc::isupport::casefold(target, mapping)) {
                    operation.values.insert(*key);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn finish_metadata_operation(&mut self, id: &str, message: &Message) -> bool {
        let Command::PONG(first, second) = &message.command else { return false; };
        if matches!(&message.prefix, Some(irc::proto::Prefix::Nickname(_, user, host)) if !user.is_empty() || !host.is_empty()) { return false; }
        let Some(operation) = self.bouncer_metadata.get(id).and_then(|session| session.operation.as_ref()) else { return false; };
        if first != &operation.nonce && second.as_ref() != Some(&operation.nonce) { return false; }
        let operation = self.bouncer_metadata.get_mut(id).unwrap().operation.take().unwrap();
        let buffer = crate::state::buffer::make_buffer_id(id, &self.state.connections[id].label);
        if operation.failed || !operation.expected.is_subset(&operation.values) {
            self.add_event_to_buffer(&buffer, "Metadata operation failed; no successful persistence is claimed".into());
            if !operation.expected.is_empty() && !operation.reconciliation && let Some(target) = operation.target
                && let Err(error) = self.start_metadata_operation(id, Request::Get(&target), true) {
                self.add_event_to_buffer(&buffer, error.into());
            }
        } else if let Some(target) = operation.target {
            if operation.expected.is_empty() {
                for key in Key::ALL {
                    if !operation.values.contains(&key) { self.state.set_metadata(id, &target, key, false); }
                }
            }
            let flags = self.state.metadata_flags(id, &target);
            self.add_event_to_buffer(&buffer, escape_format(&format!("Metadata {target}: pinned={}, muted={}, blocked={}{}", flags.pinned, flags.muted, flags.blocked,
                if operation.reconciliation { " (read back after failure)" } else { "" })));
        } else {
            self.add_event_to_buffer(&buffer, "Metadata subscription list complete".into());
        }
        self.drain_pending_web_events();
        true
    }

    pub(super) fn tick_metadata_operations(&mut self) {
        let mut notices = Vec::new();
        for (id, session) in &mut self.bouncer_metadata {
            if !self.state.connections.get(id).is_some_and(|conn| conn.enabled_caps.contains("batch"))
                && let Some(operation) = &mut session.operation { operation.failed = true; }
            if let Some(operation) = &mut session.operation
                && !operation.timed_out && operation.started.elapsed() > Duration::from_secs(30) {
                operation.timed_out = true;
                notices.push(id.clone());
            }
        }
        for id in notices {
            let buffer = crate::state::buffer::make_buffer_id(&id, &self.state.connections[&id].label);
            self.add_event_to_buffer(&buffer, "Metadata operation timed out; its result is unknown. Wait for the outstanding reply or reconnect before retrying".into());
        }
    }
}

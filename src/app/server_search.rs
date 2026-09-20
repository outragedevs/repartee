use std::time::{Duration, Instant};

use irc::proto::{Command, Message as WireMessage};

use crate::commands::helpers::{add_local_event, escape_format};
use crate::state::buffer::{Buffer, BufferType, Message, MessageType, make_buffer_id};

const CAP: &str = "soju.im/search";
const VIEW: &str = "*search*";
use crate::state::buffer::{SEARCH_TARGET_TAG as TARGET_TAG, SEARCH_SCOPE_TAG as SCOPE_TAG};

pub struct Pending {
    started: Instant,
    target: String,
    limit: usize,
    discard: bool,
    context: bool,
    label: Option<String>,
}

struct Query {
    target: String,
    limit: usize,
    wire: WireMessage,
}

fn query(args: &[String]) -> Result<Query, &'static str> {
    let Some(target) = args.first().filter(|value| !value.is_empty() && !value.starts_with(':') && !value.chars().any(char::is_whitespace)) else {
        return Err("Usage: /bsearch <target> [-from nick] [-after timestamp] [-before timestamp] [-limit 1..100] -- <text> | /bsearch cancel");
    };
    let mut attrs = vec![("in", target.clone())];
    let mut limit = 100;
    let mut index = 1;
    while index < args.len() {
        let key = match args[index].as_str() {
            "--" => { attrs.push(("text", args[index + 1..].join(" "))); break; }
            "-from" => "from",
            "-after" => "after",
            "-before" => "before",
            "-limit" => "limit",
            _ => return Err("Unknown search selector; use -- before the search text"),
        };
        let Some(value) = args.get(index + 1).filter(|value| !value.is_empty()) else { return Err("Missing search selector value"); };
        if attrs.iter().any(|(other, _)| *other == key) { return Err("Duplicate search selector"); }
        if key == "limit" {
            limit = value.parse::<usize>().ok().filter(|limit| (1..=100).contains(limit)).ok_or("Search limit must be between 1 and 100")?;
        }
        let value = if matches!(key, "after" | "before") {
            chrono::DateTime::parse_from_rfc3339(value).map_err(|_| "Invalid search timestamp; use RFC3339")?
                .with_timezone(&chrono::Utc).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        } else { value.clone() };
        attrs.push((key, value));
        index += 2;
    }
    if attrs.iter().any(|(_, value)| value.chars().any(char::is_control)) { return Err("Search selectors cannot contain control characters"); }
    let attributes = attrs.iter().map(|(key, value)| format!("{key}={}", value.replace('\\', "\\\\").replace(';', "\\:").replace(' ', "\\s"))).collect::<Vec<_>>().join(";");
    let wire: WireMessage = Command::Raw("SEARCH".into(), vec![attributes]).into();
    if wire.to_string().len() > 512 { return Err("Search request exceeds the IRC line limit"); }
    Ok(Query { target: target.clone(), limit, wire })
}

pub fn command(app: &mut super::App, args: &[String]) {
    let Some(id) = app.active_conn_id().map(str::to_string) else { add_local_event(app, "No active connection"); return; };
    if args.first().is_some_and(|arg| arg == "context") {
        app.open_search_context(&id, &args[1..]);
        return;
    }
    if args == ["cancel"] {
        if let Some(pending) = app.server_search.get_mut(&id) {
            pending.discard = true;
            app.search_note(&id, "Search cancelled locally; wait for its terminal reply or reconnect before another search");
        } else { add_local_event(app, "No search is pending"); }
        return;
    }
    if app.server_search.contains_key(&id) { add_local_event(app, "A search is unresolved; wait for its reply or reconnect"); return; }
    if !app.state.connections.get(&id).is_some_and(|conn| conn.origin_config.bouncer_network_id.is_some()
        && !conn.origin_config.bouncer_control && conn.status == crate::state::connection::ConnectionStatus::Connected
        && [CAP, "batch", "server-time", "message-tags"].iter().all(|cap| conn.enabled_caps.contains(*cap))) {
        add_local_event(app, "Server search requires a connected bouncer network with acknowledged search, batch, server-time and message-tags support"); return;
    }
    let query = match query(args) { Ok(query) => query, Err(error) => { add_local_event(app, error); return; } };
    if app.irc_handles.get(&id).is_none_or(|handle| handle.sender().send(query.wire).is_err()) {
        add_local_event(app, "Could not send search request"); return;
    }
    let view = if let Some(view) = app.search_view(&id) { view } else {
        let mut buffer = Buffer::empty(&id, BufferType::Special, VIEW);
        while app.state.buffers.contains_key(&buffer.id) { buffer.id = format!("{id}/search-{}", uuid::Uuid::new_v4()); }
        let view = buffer.id.clone();
        app.state.add_buffer_with_focus(buffer, true);
        app.server_search_views.insert(id.clone(), view.clone());
        view
    };
    if let Some(buffer) = app.state.buffers.get_mut(&view) {
        let message_ids = buffer.messages.drain(..).map(|message| message.id).collect();
        app.state.pending_web_events.push(crate::web::protocol::WebEvent::DeleteMessages { buffer_id: view.clone(), message_ids });
    }
    app.state.set_active_buffer(&view);
    app.server_search.insert(id.clone(), Pending { started: Instant::now(), target: query.target.clone(), limit: query.limit, discard: false, context: false, label: None });
    app.search_note(&id, &format!("Searching {} (up to {} results); /bsearch cancel to stop displaying this request", query.target, query.limit));
}

impl super::App {
    fn search_view(&self, id: &str) -> Option<String> {
        self.server_search_views.get(id).filter(|view| self.state.buffers.get(*view).is_some_and(|buffer|
            buffer.connection_id == id && buffer.buffer_type == BufferType::Special && buffer.name == VIEW)).cloned()
    }

    fn search_note(&mut self, id: &str, text: &str) {
        if let Some(view) = self.search_view(id) { self.add_event_to_buffer(&view, escape_format(text)); }
    }

    pub(crate) fn reset_server_search(&mut self, id: &str) {
        if self.server_search.remove(id).is_some() { self.search_note(id, "Search interrupted by a connection change; submit a new request after reconnect"); }
    }

    pub(crate) fn handle_server_search(&mut self, id: &str, message: &WireMessage) -> bool {
        let Some(pending) = self.server_search.get(id) else { return false; };
        let context = pending.context;
        if context && crate::irc::labels::message_label(message) != pending.label.as_deref() { return false; }
        if context && pending.label.is_none() && let Command::Raw(command, args) = &message.command
            && command.eq_ignore_ascii_case("FAIL")
            && !args.iter().any(|arg| arg.eq_ignore_ascii_case(&pending.target) || arg.eq_ignore_ascii_case("AROUND")) { return false; }
        let expected = if context { "CHATHISTORY" } else { "SEARCH" };
        let error = match &message.command {
            Command::Raw(command, args) if command.eq_ignore_ascii_case("FAIL") && args.first().is_some_and(|arg| arg.eq_ignore_ascii_case(expected)) => args.last(),
            Command::Response(response, args) if matches!(*response as u16, 421 | 461) && args.get(1).is_some_and(|arg| arg.eq_ignore_ascii_case(expected)) => args.last(),
            _ => None,
        };
        if let Some(error) = error {
            if let Some(pending) = self.server_search.remove(id) {
                if pending.context && let Some(conn) = self.state.connections.get_mut(id) { conn.chathistory.complete_target(&pending.target, 0, None, false); }
                if !pending.discard { self.search_note(id, &format!("Search failed: {error}")); }
            }
            return true;
        }
        false
    }

    pub(crate) fn receive_search_results(&mut self, id: &str, batch: &crate::irc::batch::BatchInfo, clean_end: bool) {
        self.tick_server_search();
        let Some(pending) = self.server_search.get(id) else { return; };
        if batch.started_at < pending.started || (batch.batch_type == "SOJU.IM/SEARCH" && pending.context) { return; }
        if !clean_end {
            if !pending.discard { self.search_note(id, "Search response was interrupted; results discarded. Reconnect before retrying"); }
            self.server_search.get_mut(id).unwrap().discard = true;
            return;
        }
        let pending = self.server_search.remove(id).unwrap();
        let Some(view) = self.search_view(id) else { return; };
        if pending.discard { return; }
        if batch.dropped_messages > 0 || batch.messages.len() > pending.limit {
            self.search_note(id, "Search response exceeded the requested limit; results discarded"); return;
        }
        let Some(conn) = self.state.connections.get(id) else { return; };
        let own = conn.nick.clone();
        let own_handle = conn.own_handle.clone();
        let channel_types = conn.isupport_parsed.chan_types();
        let scope = conn.network_key().to_string();
        let mapping = conn.isupport_parsed.casemapping();
        let rows = batch.messages.iter().map(|wire| {
            let (target, body, kind) = match &wire.command {
                Command::PRIVMSG(target, body) => (target, body, MessageType::Message),
                Command::NOTICE(target, body) => (target, body, MessageType::Notice),
                _ => return None,
            };
            wire.prefix.as_ref()?;
            let (nick, ident, host) = crate::irc::formatting::extract_nick_userhost(wire.prefix.as_ref());
            let target_matches = crate::irc::isupport::casefold(target, mapping) == crate::irc::isupport::casefold(&pending.target, mapping);
            let incoming_private = !target.starts_with(|c| channel_types.contains(c))
                && crate::irc::isupport::casefold(&nick, mapping) == crate::irc::isupport::casefold(&pending.target, mapping);
            if !target_matches && !incoming_private { return None; }
            let mut tags: std::collections::HashMap<_, _> = wire.tags.as_ref()?.iter().filter_map(|tag| tag.1.as_ref().map(|value| (tag.0.clone(), value.clone()))).filter(|(key, _)| key != "batch" && key != "label").collect();
            tags.insert(TARGET_TAG.clone(), pending.target.clone());
            tags.insert(SCOPE_TAG.clone(), scope.clone());
            let timestamp = chrono::DateTime::parse_from_rfc3339(tags.get("time")?).ok()?.with_timezone(&chrono::Utc);
            Some((nick, body.clone(), kind, tags, timestamp, format!("{ident}@{host}"), target.clone()))
        }).collect::<Option<Vec<_>>>();
        let Some(mut rows) = rows else { self.search_note(id, "Search response contained invalid results; results discarded"); return; };
        rows.retain(|(nick, ..)| !self.state.metadata_policy(id, &pending.target, Some(nick)).blocked);
        let raw_count = rows.len();
        let rows: Vec<_> = rows.into_iter().filter_map(|(nick, raw, kind, tags, timestamp, handle, target)| {
            let is_own = nick.eq_ignore_ascii_case(&own) || own_handle.as_ref().is_some_and(|own| own.eq_ignore_ascii_case(&handle));
            let text = crate::irc::events::decrypt_chathistory_text(&self.state, &scope, &target, &handle, own_handle.as_deref(), is_own, &raw)?;
            let (text, kind) = if let Some(inner) = text.strip_prefix('\x01').and_then(|text| text.strip_suffix('\x01')) {
                (inner.strip_prefix("ACTION ")?.to_string(), MessageType::Action)
            } else { (text, kind) };
            Some((nick, text, kind, tags, timestamp))
        }).collect();
        if rows.len() < raw_count { self.search_note(id, "Some protocol messages or encrypted rows could not be displayed; server-side text matching cannot search encrypted plaintext"); }
        self.search_note(id, &format!("{} {} in {}; /bsearch context <row> opens surrounding history (rows count from 1)", rows.len(), if pending.context { "context messages" } else { "search results" }, pending.target));
        for (nick, text, kind, tags, timestamp) in rows {
            let message_id = self.state.next_message_id();
            let mut message = Message { id: message_id, timestamp, message_type: kind, nick: Some(nick), nick_mode: None,
                text, highlight: false, event_key: None, event_params: None, log_msg_id: None, log_ref_id: None,
                tags: Some(tags), wire_origin: None, translation_suffix_at: None, redaction_ref: None, redaction_msgid: None, log_key: None };
            let source = make_buffer_id(id, &pending.target);
            self.state.attach_redaction_ref(&source, &mut message);
            self.state.apply_redaction(&source, &mut message);
            self.state.add_local_message(&view, message);
        }
    }

    pub(crate) fn search_context_pending(&self, id: &str, target: &str) -> bool {
        self.server_search.get(id).is_some_and(|pending| pending.context && self.state.connections.get(id).is_some_and(|conn| {
            let mapping = conn.isupport_parsed.casemapping();
            crate::irc::isupport::casefold(&pending.target, mapping) == crate::irc::isupport::casefold(target, mapping)
        }))
    }

    fn open_search_context(&mut self, id: &str, args: &[String]) {
        if self.server_search.contains_key(id) { add_local_event(self, "A search is unresolved; wait for its reply or reconnect"); return; }
        let row = if args.len() == 1 { args[0].parse::<usize>().ok().and_then(|row| row.checked_sub(1)) } else { None };
        let anchor = row.and_then(|index| self.state.buffers.get(&self.search_view(id)?)?.messages.iter()
            .filter(|message| message.tags.as_ref().is_some_and(|tags| tags.contains_key(TARGET_TAG.as_str()))).nth(index))
            .and_then(|message| Some((message.tags.as_ref()?.get(TARGET_TAG.as_str())?.clone(), message.timestamp.timestamp_millis(),
                message.tags.as_ref()?.get("msgid").cloned(), message.tags.as_ref()?.get(SCOPE_TAG.as_str())?.clone())));
        let Some((target, timestamp, msgid, scope)) = anchor else { add_local_event(self, "Usage: /bsearch context <result row number>"); return; };
        let label = self.state.connections.get(id).filter(|conn| conn.enabled_caps.contains("labeled-response"))
            .map(|_| format!("{}-search-{}", crate::constants::APP_NAME, uuid::Uuid::new_v4().simple()));
        if label.is_none() && self.state.connections.get(id).is_some_and(|conn| conn.chathistory.has_ambiguous_reply()) {
            add_local_event(self, "An earlier history request timed out; reconnect before requesting unlabelled context"); return;
        }
        if !self.state.connections.get(id).is_some_and(|conn| conn.status == crate::state::connection::ConnectionStatus::Connected && conn.network_key() == scope
            && ["draft/chathistory", "batch", "server-time", "message-tags"].iter().all(|cap| conn.enabled_caps.contains(*cap)))
            || !self.request_chathistory_labeled(id, &target, crate::irc::chathistory::Direction::Around, Some((msgid, timestamp)), 50, label.as_deref()) {
            add_local_event(self, "Context history is unavailable or another history request is pending"); return;
        }
        let Some(view) = self.search_view(id) else { return; };
        if let Some(buffer) = self.state.buffers.get_mut(&view) {
            let message_ids = buffer.messages.drain(..).map(|message| message.id).collect();
            self.state.pending_web_events.push(crate::web::protocol::WebEvent::DeleteMessages { buffer_id: view.clone(), message_ids });
        }
        self.server_search.insert(id.into(), Pending { started: Instant::now(), target: target.clone(), limit: 50, discard: false, context: true, label });
        self.state.set_active_buffer(&view);
        self.search_note(id, &format!("Loading context from {target} around {}", crate::irc::chathistory::rfc3339_millis(timestamp)));
    }

    pub(crate) fn receive_search_context(&mut self, id: &str, batch: &crate::irc::batch::BatchInfo, clean_end: bool) -> bool {
        let label = batch.opener_tags.as_ref().and_then(|tags| tags.iter().find(|tag| tag.0 == "label")).and_then(|tag| tag.1.as_deref());
        let ours = label.is_some_and(|label| label.starts_with(&format!("{}-search-", crate::constants::APP_NAME)));
        let Some(target) = batch.params.first() else { return ours; };
        if !self.search_context_pending(id, target) { return ours; }
        let expected = self.server_search.get(id).and_then(|pending| pending.label.as_deref());
        if label != expected { return ours; }
        if clean_end && let Some(conn) = self.state.connections.get_mut(id) { conn.chathistory.complete_target(target, 0, None, false); }
        for message in &batch.messages { self.state.receive_redaction(id, message); }
        let mut display = batch.clone();
        display.messages.retain(|message| matches!(message.command, Command::PRIVMSG(..) | Command::NOTICE(..)));
        self.receive_search_results(id, &display, clean_end);
        true
    }

    pub(crate) fn tick_server_search(&mut self) {
        self.server_search_views.retain(|_, view| self.state.buffers.contains_key(view));
        let expired = self.server_search.iter_mut().filter_map(|(id, pending)| {
            if !pending.discard && (pending.started.elapsed() > Duration::from_secs(30)
                || self.state.connections.get(id).is_none_or(|conn| ![if pending.context { "draft/chathistory" } else { CAP }, "batch", "server-time", "message-tags"].iter().all(|cap| conn.enabled_caps.contains(*cap)))) {
                pending.discard = true;
                Some(id.clone())
            } else { None }
        }).collect::<Vec<_>>();
        for id in expired { self.search_note(&id, "Search timed out; late results will be discarded. Wait for the terminal reply or reconnect"); }
    }
}

#[cfg(test)]
#[path = "server_search_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "server_search_fixture.rs"]
mod fixture;

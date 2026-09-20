use super::{Buffer, BufferType, Pending, VIEW, add_local_event};
use crate::irc::chathistory::{Direction, clamp_limit, rfc3339_millis};
use std::time::Instant;

struct Range {
    target: String,
    first: i64,
    last: i64,
    limit: usize,
}

fn parse(args: &[String]) -> Result<Range, &'static str> {
    if !(3..=4).contains(&args.len()) {
        return Err("Usage: /bsearch between <target> <first RFC3339 time> <last RFC3339 time> [1..1000]");
    }
    let target = &args[0];
    if target.is_empty() || target.starts_with(':') || target.chars().any(|c| c.is_whitespace() || c.is_control() || c == ',') {
        return Err("Range history requires one valid target");
    }
    let time = |value: &str| chrono::DateTime::parse_from_rfc3339(value)
        .map(|time| time.timestamp_millis()).map_err(|_| "Range bounds must be RFC3339 timestamps");
    let limit = args.get(3).map_or(Ok(100), |value| value.parse::<usize>())
        .ok().filter(|limit| (1..=1000).contains(limit)).ok_or("Range limit must be between 1 and 1000")?;
    Ok(Range { target: target.clone(), first: time(&args[1])?, last: time(&args[2])?, limit })
}

impl crate::app::App {
    pub(super) fn open_history_range(&mut self, id: &str, args: &[String]) {
        if self.server_search.contains_key(id) { add_local_event(self, "A search is unresolved; wait for its reply or reconnect"); return; }
        let range = match parse(args) { Ok(range) => range, Err(error) => { add_local_event(self, error); return; } };
        let Some(conn) = self.state.connections.get(id).filter(|conn|
            conn.server_owns_history() && !conn.bouncer_control()
            && conn.status == crate::state::connection::ConnectionStatus::Connected
            && ["draft/chathistory", "batch", "server-time", "message-tags"].iter().all(|cap| conn.enabled_caps.contains(*cap))) else {
            add_local_event(self, "Range history requires a connected bouncer network with history, batch, server-time and message-tags support"); return;
        };
        let references = conn.isupport_parsed.msgreftypes();
        if !references.is_empty() && !references.iter().any(|kind| kind == "timestamp") {
            add_local_event(self, "This server does not support timestamp-bounded history"); return;
        }
        if !conn.chathistory.should_request(&range.target, Direction::Between, true) {
            add_local_event(self, "Another history request is pending for this target"); return;
        }
        let label = conn.enabled_caps.contains("labeled-response")
            .then(|| format!("{}-search-{}", crate::constants::APP_NAME, uuid::Uuid::new_v4().simple()));
        if label.is_none() && conn.chathistory.has_ambiguous_reply() {
            add_local_event(self, "An earlier history request timed out; reconnect before requesting unlabelled history"); return;
        }
        let limit = clamp_limit(range.limit, conn.isupport_parsed.chathistory_max());
        let first = rfc3339_millis(range.first);
        let last = rfc3339_millis(range.last);
        let mut wire: irc::proto::Message = irc::proto::Command::Raw("CHATHISTORY".into(), vec![
            Direction::Between.subcommand().into(), range.target.clone(), format!("timestamp={first}"), format!("timestamp={last}"), limit.to_string(),
        ]).into();
        if let Some(label) = &label { wire.tags = Some(vec![irc::proto::message::Tag("label".into(), Some(label.clone()))]); }
        if wire.to_string().len() > crate::irc::PROTOCOL_LINE_MAX_BYTES { add_local_event(self, "History request exceeds the IRC line limit"); return; }
        if self.irc_handles.get(id).is_none_or(|handle| handle.sender().send(wire).is_err()) {
            add_local_event(self, "Could not send history request"); return;
        }
        self.state.connections.get_mut(id).unwrap().chathistory.mark_in_flight(&range.target, Direction::Between, limit);
        let view = if let Some(view) = self.search_view(id) { view } else {
            let mut buffer = Buffer::empty(id, BufferType::Special, VIEW);
            while self.state.buffers.contains_key(&buffer.id) { buffer.id = format!("{id}/search-{}", uuid::Uuid::new_v4()); }
            let view = buffer.id.clone();
            self.state.add_buffer_with_focus(buffer, true);
            self.server_search_views.insert(id.into(), view.clone());
            view
        };
        if let Some(buffer) = self.state.buffers.get_mut(&view) {
            let message_ids = buffer.messages.drain(..).map(|message| message.id).collect();
            self.state.pending_web_events.push(crate::web::protocol::WebEvent::DeleteMessages { buffer_id: view.clone(), message_ids });
        }
        self.server_search.insert(id.into(), Pending { started: Instant::now(), target: range.target.clone(), limit,
            discard: false, context: true, bounds: Some((range.first, range.last)), label });
        self.state.set_active_buffer(&view);
        self.search_note(id, &format!("Loading up to {limit} messages in {} between {first} and {last}; /bsearch cancel to stop displaying this request", range.target));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_bounds_keep_direction_and_validate_input() {
        let args = ["#room", "2024-01-01T01:00:20+01:00", "2024-01-01T00:00:10Z", "3"].map(str::to_string);
        let range = parse(&args).unwrap();
        assert_eq!(range.first - range.last, 10_000);
        assert_eq!(range.limit, 3);
        for target in ["", ":room", "#one,#two", "#two rooms", "#room\r\nQUIT"] {
            let mut invalid = args.clone();
            invalid[0] = target.into();
            assert!(parse(&invalid).is_err());
        }
        for value in ["*", "msgid=one", "invalid"] {
            for index in [1, 2] {
                let mut invalid = args.clone();
                invalid[index] = value.into();
                assert!(parse(&invalid).is_err());
            }
        }
        for limit in ["0", "1001", "-1", "invalid"] {
            let mut invalid = args.clone();
            invalid[3] = limit.into();
            assert!(parse(&invalid).is_err());
        }
    }
}

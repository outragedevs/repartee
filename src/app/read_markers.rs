use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use crate::state::buffer::make_buffer_id;

use super::App;

#[derive(Default)]
pub struct ReadMarkers {
    scope: String,
    confirmed: HashMap<String, Option<i64>>,
    desired: HashMap<String, i64>,
    sent: HashMap<String, (i64, Instant)>,
    queried: HashMap<String, Instant>,
    rejected: HashMap<String, i64>,
    rejected_queries: HashSet<String>,
    pending: VecDeque<PendingMarker>,
    failures: HashMap<String, u8>,
    retry_after: HashMap<String, Instant>,
}

struct PendingMarker {
    target: String,
    timestamp: Option<i64>,
    sent_at: Instant,
}

#[derive(Debug, PartialEq, Eq)]
enum Marker {
    Unknown,
    Timestamp(i64),
}

fn parse_marker(value: &str) -> Option<Marker> {
    if value == "*" {
        return Some(Marker::Unknown);
    }
    let time = value.strip_prefix("timestamp=")?;
    chrono::DateTime::parse_from_rfc3339(time)
        .ok()
        .map(|time| Marker::Timestamp(time.timestamp_millis()))
}

impl ReadMarkers {
    fn prepare_scope(&mut self, scope: &str) {
        if self.scope != scope {
            *self = Self {
                scope: scope.to_string(),
                ..Self::default()
            };
        }
    }

    fn prioritized_targets(
        &self,
        mut targets: std::collections::BTreeMap<String, String>,
    ) -> Vec<(String, String)> {
        for target in self.desired.keys() {
            targets
                .entry(target.clone())
                .or_insert_with(|| target.clone());
        }
        let mut targets: Vec<_> = targets.into_iter().collect();
        targets.sort_by_key(|(target, _)| {
            (
                !self.desired.contains_key(target),
                self.sent
                    .get(target)
                    .map(|(_, sent)| *sent)
                    .or_else(|| self.queried.get(target).copied()),
            )
        });
        targets
    }

    fn receive(&mut self, target: &str, marker: Option<i64>) -> bool {
        let target = target.to_ascii_lowercase();
        self.pending.retain(|request| {
            request.target != target
                || request
                    .timestamp
                    .is_some_and(|requested| marker.is_none_or(|time| requested > time))
        });
        self.rejected.remove(&target);
        self.rejected_queries.remove(&target);
        self.failures.remove(&target);
        self.retry_after.remove(&target);
        if let Some(previous) = self.confirmed.get(&target)
            && *previous >= marker
        {
            return false;
        }
        if marker.is_some_and(|time| {
            self.desired
                .get(&target)
                .is_some_and(|desired| *desired <= time)
        }) {
            self.desired.remove(&target);
            self.sent.remove(&target);
        }
        self.confirmed.insert(target, marker);
        true
    }
}

impl App {
    #[cfg(test)]
    pub(crate) fn confirmed_read_marker(&self, conn_id: &str, target: &str) -> Option<i64> {
        self.read_markers
            .get(conn_id)?
            .confirmed
            .get(&target.to_ascii_lowercase())
            .copied()
            .flatten()
    }

    pub(crate) fn reconnect_read_markers(&mut self, conn_id: &str) {
        let Some(conn) = self.state.connections.get(conn_id) else {
            return;
        };
        let markers = self.read_markers.entry(conn_id.to_string()).or_default();
        let changed_scope = markers.scope != conn.network_key();
        markers.prepare_scope(conn.network_key());
        if changed_scope {
            self.state.reset_connection_read_markers(conn_id);
        }
        markers.confirmed.clear();
        markers.sent.clear();
        markers.queried.clear();
        markers.rejected.clear();
        markers.rejected_queries.clear();
        markers.pending.clear();
        markers.failures.clear();
        markers.retry_after.clear();
    }

    pub(crate) fn mark_visible_message_read(&mut self, buffer_id: &str, message_id: u64) {
        if !self.state.uses_read_markers(buffer_id) {
            return;
        }
        let Some(buffer) = self.state.buffers.get(buffer_id) else {
            return;
        };
        let conn_id = buffer.connection_id.clone();
        let Some(boundaries) = self.state.visible_read_markers(buffer_id, message_id) else {
            return;
        };
        let has_boundaries = !boundaries.is_empty();
        self.state.clear_visible_read_rows(buffer_id, message_id);
        let scope = self.state.connections[&conn_id].network_key().to_string();
        for (origin, millis) in boundaries {
            let Some((_, target)) = origin.split_once('/') else {
                continue;
            };
            let target = target.to_ascii_lowercase();
            let markers = self.read_markers.entry(conn_id.clone()).or_default();
            markers.prepare_scope(&scope);
            if markers
                .rejected
                .get(&target)
                .is_some_and(|rejected| *rejected >= millis)
            {
                continue;
            }
            markers.rejected.remove(&target);
            if markers.rejected_queries.remove(&target) {
                markers.failures.remove(&target);
                markers.retry_after.remove(&target);
            }
            if !markers
                .confirmed
                .get(&target)
                .is_some_and(|confirmed| confirmed.is_some_and(|time| time >= millis))
            {
                let desired = markers.desired.entry(target).or_insert(millis);
                *desired = (*desired).max(millis);
            }
            self.state.apply_server_read_marker(&origin, millis);
        }
        if has_boundaries {
            self.tick_read_markers();
        }
        self.drain_pending_web_events();
    }

    pub(crate) fn mark_terminal_read(&mut self) {
        if !self.terminal_focused
            || self.terminal.is_none()
            || self.scroll_offset != 0
            || self.log_browser_mode
            || self.wizard.is_some()
            || self.emote_picker.is_open()
            || !matches!(
                self.image_preview,
                crate::image_preview::PreviewStatus::Hidden
            )
        {
            return;
        }
        let Some(buffer_id) = self.state.active_buffer_id.clone() else {
            return;
        };
        if self.state.buffer_uses_server_history(&buffer_id)
            && !self.state.uses_read_markers(&buffer_id)
        {
            if self.state.buffers.get(&buffer_id).is_some_and(|buffer| {
                buffer.unread_count != 0
                    || buffer.activity != crate::state::buffer::ActivityLevel::None
            }) {
                self.state.clear_activity(&buffer_id);
                self.broadcast_web(crate::web::protocol::WebEvent::ActivityChanged {
                    buffer_id,
                    activity: 0,
                    unread_count: 0,
                });
            }
            return;
        }
        let message_id = self
            .state
            .buffers
            .get(&buffer_id)
            .and_then(|buffer| buffer.messages.back().map(|message| message.id));
        if let Some(message_id) = message_id {
            self.mark_visible_message_read(&buffer_id, message_id);
        }
    }

    pub(crate) fn tick_read_markers(&mut self) {
        self.read_markers
            .retain(|id, _| self.state.connections.contains_key(id));
        let now = Instant::now();
        let mut budget = 8;
        for (conn_id, conn) in &self.state.connections {
            if !conn.server_owns_history()
                || conn.origin_config.bouncer_control
                || conn.status != crate::state::connection::ConnectionStatus::Connected
            {
                continue;
            }
            let command = if conn.enabled_caps.contains("draft/read-marker") {
                "MARKREAD"
            } else if conn.enabled_caps.contains("soju.im/read") {
                "READ"
            } else {
                continue;
            };
            let Some(handle) = self.irc_handles.get(conn_id) else {
                continue;
            };
            let markers = self.read_markers.entry(conn_id.clone()).or_default();
            markers.prepare_scope(conn.network_key());
            markers
                .pending
                .retain(|request| now.duration_since(request.sent_at) < Duration::from_secs(45));
            let targets: std::collections::BTreeMap<String, String> = self
                .state
                .buffers
                .values()
                .filter(|buffer| {
                    buffer.connection_id == *conn_id
                        && matches!(
                            buffer.buffer_type,
                            crate::state::buffer::BufferType::Channel
                                | crate::state::buffer::BufferType::Query
                        )
                })
                .map(|buffer| (buffer.name.to_ascii_lowercase(), buffer.name.clone()))
                .collect();
            let targets = markers.prioritized_targets(targets);
            for (target, name) in targets {
                if budget == 0 {
                    return;
                }
                if markers.rejected.contains_key(&target)
                    || markers
                        .retry_after
                        .get(&target)
                        .is_some_and(|until| now < *until)
                {
                    continue;
                }
                let desired = markers.desired.get(&target).copied();
                let mut params = vec![name];
                if let Some(millis) = desired {
                    if markers.sent.get(&target).is_some_and(|(previous, sent)| {
                        now.duration_since(*sent)
                            < Duration::from_secs(if *previous == millis { 5 } else { 1 })
                    }) {
                        continue;
                    }
                    params.push(format!(
                        "timestamp={}",
                        crate::irc::chathistory::rfc3339_millis(millis)
                    ));
                } else if markers.rejected_queries.contains(&target)
                    || markers.confirmed.contains_key(&target)
                    || markers
                        .queried
                        .get(&target)
                        .is_some_and(|sent| now.duration_since(*sent) < Duration::from_secs(30))
                {
                    continue;
                }
                if markers.pending.len() >= 128 {
                    continue;
                }
                if handle
                    .sender()
                    .send(irc::proto::Command::Raw(command.into(), params))
                    .is_ok()
                {
                    budget -= 1;
                    markers.pending.push_back(PendingMarker {
                        target: target.clone(),
                        timestamp: desired,
                        sent_at: now,
                    });
                    if let Some(millis) = desired {
                        markers.sent.insert(target, (millis, now));
                    } else {
                        markers.queried.insert(target, now);
                    }
                }
            }
        }
    }

    fn handle_read_marker_failure(&mut self, conn_id: &str, params: &[String]) -> bool {
        if params.len() < 3 {
            return true;
        }
        let Some(markers) = self.read_markers.get_mut(conn_id) else {
            return false;
        };
        let context = (params.len() > 3).then(|| params[2].as_str());
        let position = markers.pending.iter().position(|request| {
            context.is_none_or(|context| {
                context.eq_ignore_ascii_case(&request.target)
                    || request.timestamp.is_some_and(|millis| {
                        context
                            == format!(
                                "timestamp={}",
                                crate::irc::chathistory::rfc3339_millis(millis)
                            )
                    })
            })
        });
        let Some(request) = position.and_then(|position| markers.pending.remove(position)) else {
            return false;
        };
        let target = request.target;
        let rejected = request.timestamp;
        let failures = markers.failures.entry(target.clone()).or_default();
        *failures = failures.saturating_add(1);
        if params[1] == "INTERNAL_ERROR" && *failures < 3 {
            markers
                .retry_after
                .insert(target, Instant::now() + Duration::from_secs(5 << *failures));
        } else {
            if let Some(rejected) = rejected {
                if markers
                    .sent
                    .get(&target)
                    .is_some_and(|(sent, _)| *sent == rejected)
                {
                    markers.sent.remove(&target);
                }
                if markers
                    .desired
                    .get(&target)
                    .is_none_or(|desired| *desired <= rejected)
                {
                    markers.desired.remove(&target);
                    markers.rejected.insert(target.clone(), rejected);
                }
            } else {
                markers.rejected_queries.insert(target.clone());
                markers.queried.remove(&target);
            }
            markers.retry_after.remove(&target);
        }
        if let Some(buffer_id) = self
            .state
            .buffers
            .values()
            .find(|buffer| {
                buffer.connection_id == conn_id
                    && buffer.buffer_type == crate::state::buffer::BufferType::Server
            })
            .map(|buffer| buffer.id.clone())
        {
            let text = crate::commands::helpers::escape_format(&format!(
                "Read-marker synchronization failed: {}",
                params[1..].join(" ")
            ));
            crate::irc::events::emit(&mut self.state, &buffer_id, &text);
        }
        self.drain_pending_web_events();
        true
    }

    pub(crate) fn handle_read_marker(
        &mut self,
        conn_id: &str,
        message: &irc::proto::Message,
    ) -> bool {
        let irc::proto::Command::Raw(command, params) = &message.command else {
            return false;
        };
        if command == "FAIL"
            && params
                .first()
                .is_some_and(|name| matches!(name.as_str(), "MARKREAD" | "READ"))
        {
            return self.handle_read_marker_failure(conn_id, params);
        }
        if !matches!(command.as_str(), "MARKREAD" | "READ") {
            return false;
        }
        let Some(conn) = self.state.connections.get(conn_id) else {
            return true;
        };
        if !conn.server_owns_history()
            || conn.origin_config.bouncer_control
            || !(conn.enabled_caps.contains("draft/read-marker")
                || conn.enabled_caps.contains("soju.im/read"))
        {
            return true;
        }
        if params.len() != 2 || params[0].is_empty() {
            return true;
        }
        let Some(marker) = parse_marker(&params[1]) else {
            return true;
        };
        let marker = match marker {
            Marker::Unknown => None,
            Marker::Timestamp(millis) => Some(millis),
        };
        let markers = self.read_markers.entry(conn_id.to_string()).or_default();
        markers.prepare_scope(conn.network_key());
        if !markers.receive(&params[0], marker)
            && markers.confirmed.get(&params[0].to_ascii_lowercase()) != Some(&marker)
        {
            return true;
        }
        let Some(millis) = marker else {
            return true;
        };
        let buffer_id = make_buffer_id(conn_id, &params[0]);
        self.state.apply_server_read_marker(&buffer_id, millis);
        self.drain_pending_web_events();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sending_app() -> App {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let config = toml::from_str("label='Bouncer'\naddress='bnc.example.org'\nport=6697\ntls=true\nchannels=[]\nbouncer_network_id='42'").unwrap();
        app.setup_connection("account", &config);
        let conn = app.state.connections.get_mut("account").unwrap();
        conn.status = crate::state::connection::ConnectionStatus::Connected;
        conn.enabled_caps.insert("draft/read-marker".into());
        app.irc_handles.insert(
            "account".into(),
            crate::irc::IrcHandle::new(
                "account".into(),
                crate::irc::IrcSender::capturing(0),
                None,
                None,
            ),
        );
        app.state.add_buffer_with_focus(
            crate::state::buffer::Buffer::empty(
                "account",
                crate::state::buffer::BufferType::Query,
                "Peer",
            ),
            false,
        );
        app
    }

    fn server_message(app: &mut App, millis: i64) -> u64 {
        let mut message =
            crate::state::events::tests::make_test_message(&mut app.state, "server message");
        message.timestamp = chrono::DateTime::from_timestamp_millis(millis).unwrap();
            message.tags = Some(HashMap::from([("time".into(), message.timestamp.to_rfc3339())]));
        message.tags = Some(HashMap::from([(
            "time".into(),
            crate::irc::chathistory::rfc3339_millis(millis),
        )]));
        let id = message.id;
        app.state.add_transient_message_with_activity(
            "account/peer",
            message,
            crate::state::buffer::ActivityLevel::Activity,
        );
        id
    }

    #[tokio::test]
    async fn renamed_rows_keep_their_original_marker_target_in_both_directions() {
        let mut app = sending_app();
        let seen = server_message(&mut app, 1000);
        crate::irc::events::rename_query_buffers_for_test(
            &mut app.state,
            "account",
            "Peer",
            "Renamed",
            &["account/peer".into()],
        );
        app.handle_read_marker(
            "account",
            &":bnc MARKREAD Renamed timestamp=1970-01-01T00:00:02.000Z"
                .parse()
                .unwrap(),
        );
        assert_eq!(app.state.buffers["account/renamed"].unread_count, 1);
        app.mark_visible_message_read("account/renamed", seen);
        let captured = app.irc_handles["account"].sender().captured();
        assert!(captured.iter().any(|message| matches!(&message.command, irc::proto::Command::Raw(command, params) if command == "MARKREAD" && params == &["peer", "timestamp=1970-01-01T00:00:01.000Z"])));
        assert!(!captured.iter().any(|message| matches!(&message.command, irc::proto::Command::Raw(_, params) if params.first().is_some_and(|target| target.eq_ignore_ascii_case("renamed")) && params.len() == 2)));
        let mut late = crate::state::events::tests::make_test_message(
            &mut app.state,
            "late old-target history",
        );
        late.nick = Some("Peer".into());
        late.timestamp = chrono::DateTime::from_timestamp_millis(1500).unwrap();
            late.tags = Some(HashMap::from([("time".into(), late.timestamp.to_rfc3339())]));
        app.state.surface_history_page_from_target(
            "account/renamed",
            vec![late],
            false,
            "account/peer",
        );
        assert_eq!(app.state.buffers["account/renamed"].unread_count, 1);
        app.handle_read_marker(
            "account",
            &":bnc MARKREAD Renamed timestamp=1970-01-01T00:00:03.000Z"
                .parse()
                .unwrap(),
        );
        assert_eq!(app.state.buffers["account/renamed"].unread_count, 1);
        app.handle_read_marker(
            "account",
            &":bnc MARKREAD Peer timestamp=1970-01-01T00:00:02.000Z"
                .parse()
                .unwrap(),
        );
        assert_eq!(app.state.buffers["account/renamed"].unread_count, 0);
    }

    #[tokio::test]
    async fn unanswered_requests_expire_and_do_not_starve_new_read_positions() {
        let mut app = sending_app();
        for index in 0..140 {
            app.state.add_buffer_with_focus(
                crate::state::buffer::Buffer::empty(
                    "account",
                    crate::state::buffer::BufferType::Query,
                    &format!("target{index:03}"),
                ),
                false,
            );
        }
        for _ in 0..20 {
            app.tick_read_markers();
        }
        assert_eq!(app.read_markers["account"].pending.len(), 128);
        let seen = server_message(&mut app, 1123);
        app.mark_visible_message_read("account/peer", seen);
        for request in &mut app.read_markers.get_mut("account").unwrap().pending {
            request.sent_at = Instant::now().checked_sub(Duration::from_secs(46)).unwrap();
        }
        for _ in 0..20 {
            app.tick_read_markers();
        }
        let captured = app.irc_handles["account"].sender().captured();
        assert!(captured.iter().any(|message| matches!(&message.command, irc::proto::Command::Raw(_, params) if params == &["Peer", "timestamp=1970-01-01T00:00:01.123Z"])));
        assert!(captured.iter().any(|message| matches!(&message.command, irc::proto::Command::Raw(_, params) if params == &["target139"])));
        assert!(app.read_markers["account"].pending.len() < 128);
    }

    #[tokio::test]
    async fn historical_self_messages_use_nick_ownership_intervals() {
        let mut app = sending_app();
        let old = app.state.connections["account"].nick.clone();
        app.handle_irc_event(crate::irc::IrcEvent::Message(
            "account".into(),
            Box::new(
                format!("@time=1970-01-01T00:00:02.000Z :{old}!user@host NICK :NewSelf")
                    .parse()
                    .unwrap(),
            ),
        ));
        let rows = [
            (old.as_str(), 1000),
            ("NewSelf", 3000),
            (old.as_str(), 3000),
        ]
        .into_iter()
        .map(|(nick, timestamp)| {
            let mut message = crate::state::events::tests::make_test_message(
                &mut app.state,
                &format!("history {nick} {timestamp}"),
            );
            message.nick = Some(nick.into());
            message.timestamp = chrono::DateTime::from_timestamp_millis(timestamp).unwrap();
            message
        })
        .collect();
        app.state.surface_history_page("account/peer", rows, false);
        assert_eq!(app.state.buffers["account/peer"].unread_count, 1);
    }

    #[tokio::test]
    async fn historical_account_identity_overrides_nickname_reuse() {
        let mut app = sending_app();
        let own = app.state.connections["account"].nick.clone();
        app.state.add_nick(
            "account/peer",
            crate::state::buffer::NickEntry {
                nick: own.clone(),
                prefix: String::new(),
                modes: String::new(),
                away: false,
                account: Some("our-upstream-account".into()),
                ident: None,
                host: None,
            },
        );
        let rows = [
            ("EarlierNick", "our-upstream-account"),
            (own.as_str(), "somebody-else"),
        ]
        .into_iter()
        .map(|(nick, account)| {
            let mut message =
                crate::state::events::tests::make_test_message(&mut app.state, account);
            message.nick = Some(nick.into());
            message.tags = Some(HashMap::from([("account".into(), account.into())]));
            message
        })
        .collect();
        app.state.surface_history_page("account/peer", rows, false);
        assert_eq!(app.state.buffers["account/peer"].unread_count, 1);
    }

    #[tokio::test]
    async fn one_confirmation_retires_all_satisfied_retransmissions() {
        let mut app = sending_app();
        let seen = server_message(&mut app, 1123);
        app.mark_visible_message_read("account/peer", seen);
        app.read_markers
            .get_mut("account")
            .unwrap()
            .sent
            .get_mut("peer")
            .unwrap()
            .1 = Instant::now().checked_sub(Duration::from_secs(6)).unwrap();
        app.tick_read_markers();
        assert_eq!(app.read_markers["account"].pending.len(), 2);
        app.handle_read_marker(
            "account",
            &":bnc MARKREAD Peer timestamp=1970-01-01T00:00:01.123Z"
                .parse()
                .unwrap(),
        );
        assert!(app.read_markers["account"].pending.is_empty());
        let next = server_message(&mut app, 2456);
        app.mark_visible_message_read("account/peer", next);
        assert_eq!(app.irc_handles["account"].sender().captured().len(), 3);
        app.handle_read_marker(
            "account",
            &":bnc MARKREAD Peer timestamp=1970-01-01T00:00:01.123Z"
                .parse()
                .unwrap(),
        );
        assert_eq!(app.read_markers["account"].pending.len(), 1);
    }

    #[tokio::test]
    async fn network_scope_change_discards_previous_unread_rows() {
        let mut app = sending_app();
        server_message(&mut app, 1000);
        server_message(&mut app, 2000);
        app.handle_read_marker(
            "account",
            &":bnc MARKREAD Peer timestamp=1970-01-01T00:00:01.000Z"
                .parse()
                .unwrap(),
        );
        assert_eq!(app.state.buffers["account/peer"].unread_count, 1);
        app.state
            .connections
            .get_mut("account")
            .unwrap()
            .network_scope = Some("different-network".into());
        app.reconnect_read_markers("account");
        assert_eq!(app.state.buffers["account/peer"].unread_count, 0);
        assert_eq!(
            app.state.buffers["account/peer"].activity,
            crate::state::buffer::ActivityLevel::None
        );
        app.handle_read_marker(
            "account",
            &":bnc MARKREAD Peer timestamp=1970-01-01T00:00:00.500Z"
                .parse()
                .unwrap(),
        );
        server_message(&mut app, 750);
        assert_eq!(app.state.buffers["account/peer"].unread_count, 1);
    }

    #[tokio::test]
    async fn returning_to_a_previous_nick_restores_its_own_read_threshold() {
        let mut app = sending_app();
        app.handle_read_marker(
            "account",
            &":bnc MARKREAD Peer timestamp=1970-01-01T00:00:02.000Z"
                .parse()
                .unwrap(),
        );
        crate::irc::events::rename_query_buffers_for_test(
            &mut app.state,
            "account",
            "Peer",
            "Renamed",
            &["account/peer".into()],
        );
        app.handle_read_marker(
            "account",
            &":bnc MARKREAD Renamed timestamp=1970-01-01T00:00:00.500Z"
                .parse()
                .unwrap(),
        );
        crate::irc::events::rename_query_buffers_for_test(
            &mut app.state,
            "account",
            "Renamed",
            "Peer",
            &["account/renamed".into()],
        );
        app.drain_pending_buffer_rekeys();
        app.tick_read_markers();
        server_message(&mut app, 1500);
        assert_eq!(app.state.buffers["account/peer"].unread_count, 0);
        assert_eq!(
            app.state.buffers["account/peer"]
                .last_read
                .timestamp_millis(),
            2000
        );
    }

    #[tokio::test]
    async fn mouse_only_interaction_confirms_focus_but_hover_does_not() {
        let mut app = sending_app();
        app.terminal =
            Some(crate::ui::setup_socket_terminal(Box::new(std::io::sink()), 120, 40).unwrap());
        app.terminal_focused = false;
        app.state.set_active_buffer("account/peer");
        server_message(&mut app, 1123);
        assert!(app.render_terminal_frame());
        let mut mouse = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Moved,
            column: 60,
            row: 10,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        app.handle_event(crossterm::event::Event::Mouse(mouse));
        assert!(!app.terminal_focused);
        assert!(app.render_terminal_frame());
        assert_eq!(app.state.buffers["account/peer"].unread_count, 1);
        mouse.kind = crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left);
        app.handle_event(crossterm::event::Event::Mouse(mouse));
        assert!(app.terminal_focused);
        assert!(app.render_terminal_frame());
        assert_eq!(app.state.buffers["account/peer"].unread_count, 0);
    }

    #[tokio::test]
    async fn unknown_initial_focus_preserves_unread_until_keyboard_input() {
        let mut app = sending_app();
        app.terminal =
            Some(crate::ui::setup_socket_terminal(Box::new(std::io::sink()), 120, 40).unwrap());
        app.terminal_focused = false;
        app.state.set_active_buffer("account/peer");
        server_message(&mut app, 1123);
        assert!(app.render_terminal_frame());
        assert_eq!(app.state.buffers["account/peer"].unread_count, 1);
        app.handle_event(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new_with_kind(
                crossterm::event::KeyCode::Char('x'),
                crossterm::event::KeyModifiers::NONE,
                crossterm::event::KeyEventKind::Release,
            ),
        ));
        assert!(!app.terminal_focused);
        app.handle_event(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('x'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ));
        assert!(app.terminal_focused);
        assert!(app.render_terminal_frame());
        assert_eq!(app.state.buffers["account/peer"].unread_count, 0);
    }

    #[tokio::test]
    async fn background_reattach_waits_for_focus_confirmation() {
        let mut app = sending_app();
        app.state.set_active_buffer("account/peer");
        server_message(&mut app, 1123);
        let (mut shim, daemon) = tokio::net::UnixStream::pair().unwrap();
        crate::session::protocol::write_message(
            &mut shim,
            &crate::session::protocol::TerminalEnv {
                cols: 120,
                rows: 40,
                font_size: None,
                env_vars: HashMap::new(),
            },
        )
        .await
        .unwrap();
        app.handle_shim_connect(daemon).await.unwrap();
        assert!(!app.terminal_focused);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !app.render_terminal_frame() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(app.state.buffers["account/peer"].unread_count > 0);
        assert!(app.irc_handles["account"].sender().captured().is_empty());
        app.handle_event(crossterm::event::Event::FocusGained);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !app.render_terminal_frame() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(app.state.buffers["account/peer"].unread_count, 0);
        app.disconnect_shim();
    }

    #[tokio::test]
    async fn unscoped_marker_failure_affects_only_the_first_pending_request() {
        let mut app = sending_app();
        app.state.add_buffer_with_focus(
            crate::state::buffer::Buffer::empty(
                "account",
                crate::state::buffer::BufferType::Query,
                "Other",
            ),
            false,
        );
        app.tick_read_markers();
        assert_eq!(app.irc_handles["account"].sender().captured().len(), 2);
        assert!(
            app.handle_read_marker(
                "account",
                &":bnc FAIL MARKREAD NEED_MORE_PARAMS :Missing parameters"
                    .parse()
                    .unwrap()
            )
        );
        let markers = &app.read_markers["account"];
        assert!(markers.rejected_queries.contains("other"));
        assert!(!markers.rejected_queries.contains("peer"));
        assert_eq!(markers.pending.front().unwrap().target, "peer");
        assert_eq!(markers.pending.front().unwrap().timestamp, None);
        app.handle_read_marker("account", &":bnc MARKREAD Peer *".parse().unwrap());
        let seen = server_message(&mut app, 1123);
        app.mark_visible_message_read("account/peer", seen);
        assert_eq!(app.irc_handles["account"].sender().captured().len(), 3);
    }

    #[tokio::test]
    async fn failed_initial_query_does_not_reject_later_read_updates() {
        let mut app = sending_app();
        app.tick_read_markers();
        app.handle_read_marker(
            "account",
            &":bnc FAIL MARKREAD INVALID_TARGET Peer :Not joined"
                .parse()
                .unwrap(),
        );
        app.tick_read_markers();
        assert_eq!(app.irc_handles["account"].sender().captured().len(), 1);
        let seen = server_message(&mut app, 1123);
        app.mark_visible_message_read("account/peer", seen);
        assert_eq!(app.irc_handles["account"].sender().captured().len(), 2);
        app.handle_read_marker(
            "account",
            &":bnc MARKREAD Peer timestamp=1970-01-01T00:00:01.123Z"
                .parse()
                .unwrap(),
        );
        assert!(app.read_markers["account"].desired.is_empty());
        assert!(app.read_markers["account"].pending.is_empty());
    }

    #[tokio::test]
    async fn permanent_marker_failure_stops_the_rejected_timestamp() {
        for command in ["MARKREAD", "READ"] {
            let mut app = sending_app();
            let seen = server_message(&mut app, 1123);
            app.mark_visible_message_read("account/peer", seen);
            let failure = format!(
                ":bnc FAIL {command} INVALID_PARAMS timestamp=1970-01-01T00:00:01.123Z :Invalid timestamp"
            );
            assert!(app.handle_read_marker("account", &failure.parse().unwrap()));
            assert!(app.read_markers["account"].desired.is_empty());
            app.mark_visible_message_read("account/peer", seen);
            app.tick_read_markers();
            assert_eq!(app.irc_handles["account"].sender().captured().len(), 1);
            assert!(app.state.buffers.values().any(|buffer| {
                buffer
                    .messages
                    .iter()
                    .any(|message| message.text.contains("Read-marker synchronization failed"))
            }));
            let newer = server_message(&mut app, 2456);
            app.mark_visible_message_read("account/peer", newer);
            assert_eq!(app.irc_handles["account"].sender().captured().len(), 2);
        }
    }

    #[tokio::test]
    async fn transient_marker_failure_retries_with_bounded_backoff() {
        let mut app = sending_app();
        let seen = server_message(&mut app, 1123);
        app.mark_visible_message_read("account/peer", seen);
        let failure = ":bnc FAIL MARKREAD INTERNAL_ERROR Peer :Internal error"
            .parse()
            .unwrap();
        for attempt in 1..=3 {
            assert!(app.handle_read_marker("account", &failure));
            app.tick_read_markers();
            assert_eq!(
                app.irc_handles["account"].sender().captured().len(),
                attempt
            );
            if attempt < 3 {
                let markers = app.read_markers.get_mut("account").unwrap();
                let past = Instant::now().checked_sub(Duration::from_secs(30)).unwrap();
                *markers.retry_after.get_mut("peer").unwrap() = past;
                markers.sent.get_mut("peer").unwrap().1 = past;
                app.tick_read_markers();
            }
        }
        app.mark_visible_message_read("account/peer", seen);
        app.tick_read_markers();
        assert_eq!(app.irc_handles["account"].sender().captured().len(), 3);
        app.reconnect_read_markers("account");
        app.mark_visible_message_read("account/peer", seen);
        assert_eq!(app.irc_handles["account"].sender().captured().len(), 4);
    }

    #[tokio::test]
    async fn query_rename_does_not_transfer_the_old_targets_watermark() {
        let mut app = sending_app();
        server_message(&mut app, 2000);
        app.handle_read_marker(
            "account",
            &":bnc MARKREAD Peer timestamp=1970-01-01T00:00:02.000Z"
                .parse()
                .unwrap(),
        );
        crate::irc::events::rename_query_buffers_for_test(
            &mut app.state,
            "account",
            "Peer",
            "Renamed",
            &["account/peer".into()],
        );
        app.handle_read_marker(
            "account",
            &":bnc MARKREAD Renamed timestamp=1970-01-01T00:00:00.500Z"
                .parse()
                .unwrap(),
        );
        let mut message =
            crate::state::events::tests::make_test_message(&mut app.state, "new target unread");
        message.timestamp = chrono::DateTime::from_timestamp_millis(1000).unwrap();
        app.state.add_transient_message_with_activity(
            "account/renamed",
            message,
            crate::state::buffer::ActivityLevel::Activity,
        );
        assert_eq!(app.state.buffers["account/renamed"].unread_count, 1);
        assert_eq!(
            app.state.buffers["account/renamed"]
                .last_read
                .timestamp_millis(),
            500
        );
    }

    #[tokio::test]
    async fn repeated_visible_reads_do_not_broadcast_unchanged_activity() {
        let mut app = sending_app();
        let seen = server_message(&mut app, 1123);
        app.mark_visible_message_read("account/peer", seen);
        let mut receiver = app.web_broadcaster.subscribe();
        app.mark_visible_message_read("account/peer", seen);
        assert!(receiver.try_recv().is_err());
        let local = crate::state::events::tests::make_test_message(&mut app.state, "placeholder");
        let seen = local.id;
        app.state.add_transient_message_with_activity(
            "account/peer",
            local,
            crate::state::buffer::ActivityLevel::Activity,
        );
        app.mark_visible_message_read("account/peer", seen);
        while receiver.try_recv().is_ok() {}
        app.mark_visible_message_read("account/peer", seen);
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn capability_loss_clears_visible_terminal_activity_only_after_focus() {
        let mut app = sending_app();
        app.terminal =
            Some(crate::ui::setup_socket_terminal(Box::new(std::io::sink()), 120, 40).unwrap());
        app.state.set_active_buffer("account/peer");
        for time in 1000..1060 {
            server_message(&mut app, time);
        }
        crate::irc::events::handle_cap_del(
            &mut app.state,
            "account",
            Some("draft/read-marker"),
            None,
        );
        app.terminal_focused = false;
        assert!(app.render_terminal_frame());
        assert_eq!(app.state.buffers["account/peer"].unread_count, 60);
        app.terminal_focused = true;
        app.scroll_offset = 1;
        assert!(app.render_terminal_frame());
        assert_eq!(app.state.buffers["account/peer"].unread_count, 60);
        app.scroll_offset = 0;
        assert!(app.render_terminal_frame());
        assert_eq!(app.state.buffers["account/peer"].unread_count, 0);
        assert_eq!(
            app.state.buffers["account/peer"].activity,
            crate::state::buffer::ActivityLevel::None
        );
        assert!(app.irc_handles["account"].sender().captured().is_empty());
        let mut receiver = app.web_broadcaster.subscribe();
        assert!(app.render_terminal_frame());
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn visible_untimed_rows_clear_locally_without_a_server_marker() {
        let mut app = sending_app();
        let local =
            crate::state::events::tests::make_test_message(&mut app.state, "encrypted placeholder");
        let seen = local.id;
        app.state.add_transient_message_with_activity(
            "account/peer",
            local,
            crate::state::buffer::ActivityLevel::Activity,
        );
        server_message(&mut app, 2456);
        app.mark_visible_message_read("account/peer", seen);
        assert_eq!(app.state.buffers["account/peer"].unread_count, 1);
        assert!(app.irc_handles["account"].sender().captured().is_empty());
    }

    #[tokio::test]
    async fn terminal_read_includes_untimed_tail() {
        let mut app = sending_app();
        app.terminal =
            Some(crate::ui::setup_socket_terminal(Box::new(std::io::sink()), 120, 40).unwrap());
        app.state.set_active_buffer("account/peer");
        server_message(&mut app, 1123);
        let local =
            crate::state::events::tests::make_test_message(&mut app.state, "encrypted placeholder");
        app.state.add_transient_message_with_activity(
            "account/peer",
            local,
            crate::state::buffer::ActivityLevel::Activity,
        );
        assert_eq!(app.state.buffers["account/peer"].unread_count, 2);
        assert!(app.render_terminal_frame());
        assert_eq!(app.state.buffers["account/peer"].unread_count, 0);
        let captured = app.irc_handles["account"].sender().captured();
        assert_eq!(captured.len(), 1);
        assert_eq!(
            captured[0].command,
            irc::proto::Command::Raw(
                "MARKREAD".into(),
                vec!["Peer".into(), "timestamp=1970-01-01T00:00:01.123Z".into()]
            )
        );
    }

    #[tokio::test]
    async fn local_read_uses_exact_rendered_message_and_preserves_later_arrival() {
        let mut app = sending_app();
        let seen = server_message(&mut app, 1123);
        server_message(&mut app, 2456);
        app.mark_visible_message_read("account/peer", seen);
        assert_eq!(app.state.buffers["account/peer"].unread_count, 1);
        let sender = app.irc_handles["account"].sender();
        let captured = sender.captured();
        assert_eq!(captured.len(), 1);
        assert_eq!(
            captured[0].command,
            irc::proto::Command::Raw(
                "MARKREAD".into(),
                vec!["Peer".into(), "timestamp=1970-01-01T00:00:01.123Z".into()]
            )
        );
        app.mark_visible_message_read("account/peer", seen);
        app.tick_read_markers();
        assert_eq!(app.irc_handles["account"].sender().captured().len(), 1);
    }

    #[tokio::test]
    async fn focus_and_headless_render_do_not_mark_messages_read() {
        let mut app = sending_app();
        server_message(&mut app, 1123);
        app.state.set_active_buffer("account/peer");
        assert_eq!(app.state.buffers["account/peer"].unread_count, 1);
        server_message(&mut app, 2456);
        app.mark_terminal_read();
        assert_eq!(app.state.buffers["account/peer"].unread_count, 2);
        assert!(app.irc_handles["account"].sender().captured().is_empty());
    }

    #[tokio::test]
    async fn queries_and_updates_use_legacy_read_when_needed() {
        let mut app = sending_app();
        let conn = app.state.connections.get_mut("account").unwrap();
        conn.enabled_caps.remove("draft/read-marker");
        conn.enabled_caps.insert("soju.im/read".into());
        app.tick_read_markers();
        let captured = app.irc_handles["account"].sender().captured();
        assert_eq!(
            captured[0].command,
            irc::proto::Command::Raw("READ".into(), vec!["Peer".into()])
        );
        let seen = server_message(&mut app, 1123);
        app.mark_visible_message_read("account/peer", seen);
        let captured = app.irc_handles["account"].sender().captured();
        assert_eq!(
            captured[1].command,
            irc::proto::Command::Raw(
                "READ".into(),
                vec!["Peer".into(), "timestamp=1970-01-01T00:00:01.123Z".into()]
            )
        );
        app.handle_read_marker(
            "account",
            &":bnc READ Peer timestamp=1970-01-01T00:00:01.123Z"
                .parse()
                .unwrap(),
        );
        assert!(app.read_markers["account"].desired.is_empty());
    }

    #[tokio::test]
    async fn reconnect_retries_unconfirmed_reads_only_for_the_same_network_scope() {
        let mut app = sending_app();
        let seen = server_message(&mut app, 1123);
        app.mark_visible_message_read("account/peer", seen);
        app.reconnect_read_markers("account");
        app.tick_read_markers();
        assert_eq!(app.irc_handles["account"].sender().captured().len(), 2);
        app.state
            .connections
            .get_mut("account")
            .unwrap()
            .network_scope = Some("different-account-and-network".into());
        app.reconnect_read_markers("account");
        assert!(app.read_markers["account"].desired.is_empty());
        app.tick_read_markers();
        let captured = app.irc_handles["account"].sender().captured();
        assert_eq!(
            captured[2].command,
            irc::proto::Command::Raw("MARKREAD".into(), vec!["Peer".into()])
        );
    }

    #[tokio::test]
    async fn local_tail_resolves_preceding_irc_time_without_reading_later_arrivals() {
        let mut app = sending_app();
        server_message(&mut app, 1123);
        let local = crate::state::events::tests::make_test_message(&mut app.state, "local output");
        let local_id = local.id;
        app.state
            .buffers
            .get_mut("account/peer")
            .unwrap()
            .messages
            .push_back(local);
        server_message(&mut app, 2456);
        app.mark_visible_message_read("account/peer", local_id);
        app.mark_visible_message_read("account/peer", u64::MAX);
        let captured = app.irc_handles["account"].sender().captured();
        assert_eq!(captured.len(), 1);
        assert_eq!(
            captured[0].command,
            irc::proto::Command::Raw(
                "MARKREAD".into(),
                vec!["Peer".into(), "timestamp=1970-01-01T00:00:01.123Z".into()]
            )
        );
        assert_eq!(app.state.buffers["account/peer"].unread_count, 1);
    }

    #[tokio::test]
    async fn unfocused_terminal_preserves_unread_until_focus_returns() {
        let mut app = sending_app();
        app.terminal =
            Some(crate::ui::setup_socket_terminal(Box::new(std::io::sink()), 120, 40).unwrap());
        app.state.set_active_buffer("account/peer");
        server_message(&mut app, 1123);
        app.handle_event(crossterm::event::Event::FocusLost);
        assert!(app.render_terminal_frame());
        assert_eq!(app.state.buffers["account/peer"].unread_count, 1);
        assert!(app.irc_handles["account"].sender().captured().is_empty());
        app.handle_event(crossterm::event::Event::FocusGained);
        assert!(app.render_terminal_frame());
        assert_eq!(app.state.buffers["account/peer"].unread_count, 0);
        assert_eq!(app.irc_handles["account"].sender().captured().len(), 1);
    }

    #[tokio::test]
    async fn removing_preferred_capability_keeps_legacy_read_synchronization() {
        let mut app = sending_app();
        let caps = crate::irc::cap::ServerCaps::parse("draft/read-marker soju.im/read");
        assert_eq!(
            crate::irc::cap::bouncer_read_caps(&caps),
            vec!["draft/read-marker", "soju.im/read"]
        );
        let requested = crate::irc::events::handle_cap_new(
            &mut app.state,
            "account",
            Some("soju.im/read"),
            None,
        );
        assert_eq!(requested, vec!["soju.im/read"]);
        crate::irc::events::handle_cap_ack(&mut app.state, "account", Some("soju.im/read"), None);
        crate::irc::events::handle_cap_del(
            &mut app.state,
            "account",
            Some("draft/read-marker"),
            None,
        );
        let seen = server_message(&mut app, 1123);
        app.mark_visible_message_read("account/peer", seen);
        let captured = app.irc_handles["account"].sender().captured();
        assert_eq!(
            captured.last().unwrap().command,
            irc::proto::Command::Raw(
                "READ".into(),
                vec!["Peer".into(), "timestamp=1970-01-01T00:00:01.123Z".into()]
            )
        );
    }

    #[tokio::test]
    async fn same_scope_reconnect_preserves_read_threshold_before_server_reply() {
        for caps in [
            std::collections::HashSet::from(["draft/read-marker".into()]),
            std::collections::HashSet::new(),
        ] {
            let mut app = sending_app();
            let seen = server_message(&mut app, 1123);
            app.mark_visible_message_read("account/peer", seen);
            app.handle_read_marker(
                "account",
                &":bnc MARKREAD Peer timestamp=1970-01-01T00:00:01.123Z"
                    .parse()
                    .unwrap(),
            );
            app.handle_irc_event(crate::irc::IrcEvent::Connected(
                "account".into(),
                caps,
                None,
            ));
            app.state.pending_web_events.clear();
            let mut delayed =
                crate::state::events::tests::make_test_message(&mut app.state, "delayed mention");
            delayed.timestamp = chrono::DateTime::from_timestamp_millis(1000).unwrap();
            delayed.tags = Some(HashMap::from([("time".into(), delayed.timestamp.to_rfc3339())]));
            delayed.highlight = true;
            app.state.add_transient_message_with_activity(
                "account/peer",
                delayed,
                crate::state::buffer::ActivityLevel::Mention,
            );
            assert_eq!(app.state.buffers["account/peer"].unread_count, 0);
            assert!(
                !app.state.pending_web_events.iter().any(|event| matches!(
                    event,
                    crate::web::protocol::WebEvent::MentionAlert { .. }
                ))
            );
        }
    }

    #[tokio::test]
    async fn query_rename_keeps_retrying_the_original_server_target() {
        let mut app = sending_app();
        let seen = server_message(&mut app, 1123);
        app.mark_visible_message_read("account/peer", seen);
        crate::irc::events::rename_query_buffers_for_test(
            &mut app.state,
            "account",
            "Peer",
            "Renamed",
            &["account/peer".into()],
        );
        app.drain_pending_buffer_rekeys();
        app.read_markers
            .get_mut("account")
            .unwrap()
            .sent
            .get_mut("peer")
            .unwrap()
            .1 = Instant::now().checked_sub(Duration::from_secs(6)).unwrap();
        app.tick_read_markers();
        let captured = app.irc_handles["account"].sender().captured();
        let writes: Vec<_> = captured
            .iter()
            .filter_map(|message| {
                if let irc::proto::Command::Raw(command, params) = &message.command
                    && command == "MARKREAD"
                    && params.len() == 2
                {
                    Some(params)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(writes.len(), 2);
        assert!(
            writes
                .iter()
                .all(|params| params[0].eq_ignore_ascii_case("peer"))
        );
        assert!(app.state.buffers.contains_key("account/renamed"));
        app.handle_read_marker(
            "account",
            &":bnc MARKREAD peer timestamp=1970-01-01T00:00:01.123Z"
                .parse()
                .unwrap(),
        );
        assert!(!app.read_markers["account"].desired.contains_key("peer"));
    }

    #[test]
    fn markers_accept_unknown_and_millisecond_timestamps() {
        assert_eq!(parse_marker("*"), Some(Marker::Unknown));
        assert_eq!(
            parse_marker("timestamp=2024-01-01T00:00:00.123Z"),
            Some(Marker::Timestamp(1_704_067_200_123))
        );
        assert!(parse_marker("msgid=123").is_none());
        assert!(parse_marker("timestamp=broken").is_none());
    }

    #[tokio::test]
    async fn remote_marker_keeps_newer_messages_unread() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let config = toml::from_str("label='Bouncer'\naddress='bnc.example.org'\nport=6697\ntls=true\nchannels=[]\nbouncer_network_id='42'").unwrap();
        app.setup_connection("account", &config);
        app.state
            .connections
            .get_mut("account")
            .unwrap()
            .enabled_caps
            .insert("draft/read-marker".into());
        let buffer = crate::state::buffer::Buffer::empty(
            "account",
            crate::state::buffer::BufferType::Query,
            "Peer",
        );
        app.state.add_buffer_with_focus(buffer, false);
        for millis in [1000, 2000, 3000] {
            let mut message =
                crate::state::events::tests::make_test_message(&mut app.state, "unread");
            message.timestamp = chrono::DateTime::from_timestamp_millis(millis).unwrap();
            message.tags = Some(HashMap::from([("time".into(), message.timestamp.to_rfc3339())]));
            app.state.add_transient_message_with_activity(
                "account/peer",
                message,
                crate::state::buffer::ActivityLevel::Activity,
            );
        }
        assert_eq!(app.state.buffers["account/peer"].unread_count, 3);
        assert!(
            app.handle_read_marker(
                "account",
                &":bnc MARKREAD Peer timestamp=1970-01-01T00:00:02.000Z"
                    .parse()
                    .unwrap()
            )
        );
        assert_eq!(app.state.buffers["account/peer"].unread_count, 1);
        app.handle_read_marker(
            "account",
            &":bnc MARKREAD Peer timestamp=1970-01-01T00:00:01.000Z"
                .parse()
                .unwrap(),
        );
        assert_eq!(app.state.buffers["account/peer"].unread_count, 1);
        app.handle_read_marker(
            "account",
            &":bnc MARKREAD Peer timestamp=1970-01-01T00:00:03.000Z"
                .parse()
                .unwrap(),
        );
        assert_eq!(app.state.buffers["account/peer"].unread_count, 0);
    }

    #[tokio::test]
    async fn playback_marker_does_not_advance_live_read_state() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let config = toml::from_str("label='Bouncer'\naddress='bnc.example.org'\nport=6697\ntls=true\nchannels=[]\nbouncer_network_id='42'").unwrap();
        app.setup_connection("account", &config);
        app.state
            .connections
            .get_mut("account")
            .unwrap()
            .enabled_caps
            .insert("draft/read-marker".into());
        for line in [
            ":bnc BATCH +history chathistory Peer",
            "@batch=history :bnc MARKREAD Peer timestamp=2024-01-01T00:00:00.123Z",
            ":bnc BATCH -history",
        ] {
            app.handle_irc_event(crate::irc::IrcEvent::Message(
                "account".into(),
                Box::new(line.parse().unwrap()),
            ));
        }
        assert!(!app.read_markers.contains_key("account"));
    }

    #[tokio::test]
    async fn markers_require_a_bound_bouncer_and_negotiated_capability() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let config = toml::from_str("label='Bouncer'\naddress='bnc.example.org'\nport=6697\ntls=true\nchannels=[]\nbouncer_network_id='42'").unwrap();
        app.setup_connection("account", &config);
        let marker = ":bnc READ Peer timestamp=2024-01-01T00:00:00.123Z"
            .parse()
            .unwrap();
        assert!(app.handle_read_marker("account", &marker));
        assert!(app.read_markers.is_empty());
        app.state
            .connections
            .get_mut("account")
            .unwrap()
            .enabled_caps
            .insert("soju.im/read".into());
        assert!(app.handle_read_marker("account", &marker));
        assert_eq!(
            app.read_markers["account"].confirmed["peer"],
            Some(1_704_067_200_123)
        );
        assert!(app.handle_read_marker("other-account", &marker));
        assert!(!app.read_markers.contains_key("other-account"));
        app.read_markers.clear();
        app.state
            .connections
            .get_mut("account")
            .unwrap()
            .origin_config
            .bouncer_network_id = None;
        assert!(app.handle_read_marker("account", &marker));
        assert!(app.read_markers.is_empty());
    }

    #[test]
    fn read_markers_never_move_backwards_or_reset_to_unknown() {
        let mut state = ReadMarkers::default();
        assert!(state.receive("Peer", None));
        assert!(state.receive("Peer", Some(1000)));
        assert!(!state.receive("peer", None));
        assert!(!state.receive("PEER", Some(999)));
        assert!(!state.receive("Peer", Some(1000)));
        assert!(state.receive("Peer", Some(1001)));
        assert_eq!(state.confirmed["peer"], Some(1001));
    }
}

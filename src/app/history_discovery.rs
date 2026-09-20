use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use crate::irc::batch::BatchInfo;
use crate::state::buffer::{Buffer, BufferType, make_buffer_id};

use super::App;

pub struct HistoryDiscovery {
    pending: Option<Instant>,
    discovery_started: bool,
    upper_ms: i64,
    limit: usize,
    queue: VecDeque<String>,
    active: HashSet<String>,
    hydration_attempts: HashMap<String, usize>,
    seen: HashSet<String>,
    pub(super) finished: bool,
    pub(super) incomplete: bool,
    attempts: usize,
    retry_at: Instant,
}

fn decode_targets(batch: &BatchInfo, upper_ms: i64) -> Option<Vec<(String, i64)>> {
    if batch.dropped_messages != 0 {
        return None;
    }
    let mut targets = Vec::new();
    for message in &batch.messages {
        let irc::proto::Command::Raw(command, params) = &message.command else {
            return None;
        };
        if !command.eq_ignore_ascii_case("CHATHISTORY")
            || params.len() != 3
            || !params[0].eq_ignore_ascii_case("TARGETS")
            || params[1].is_empty()
            || params[1].starts_with(':')
            || params[1]
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return None;
        }
        let time = chrono::DateTime::parse_from_rfc3339(&params[2])
            .ok()?
            .timestamp_millis();
        if time <= 0 || time >= upper_ms {
            return None;
        }
        targets.push((params[1].clone(), time));
    }
    Some(targets)
}

impl App {
    fn ensure_history_discovery(&mut self, conn_id: &str) -> bool {
        let Some(conn) = self.state.connections.get(conn_id) else {
            return false;
        };
        if !conn.server_owns_history()
            || conn.origin_config.bouncer_control
            || !conn.enabled_caps.contains("draft/chathistory")
        {
            return false;
        }
        let limit =
            crate::irc::chathistory::clamp_limit(1000, conn.isupport_parsed.chathistory_max());
        self.history_discovery
            .entry(conn_id.to_string())
            .or_insert_with(|| HistoryDiscovery {
                pending: None,
                discovery_started: false,
                upper_ms: chrono::Utc::now().timestamp_millis().saturating_add(5_000),
                limit,
                queue: VecDeque::new(),
                active: HashSet::new(),
                hydration_attempts: HashMap::new(),
                seen: HashSet::new(),
                finished: false,
                incomplete: false,
                attempts: 0,
                retry_at: Instant::now(),
            });
        true
    }

    pub(crate) fn start_history_discovery(&mut self, conn_id: &str) {
        if !self.ensure_history_discovery(conn_id) {
            return;
        }
        let discovery = self.history_discovery.get_mut(conn_id).unwrap();
        if discovery.discovery_started {
            return;
        }
        discovery.discovery_started = true;
        if let Some(conn) = self.state.connections.get(conn_id) {
            discovery.limit =
                crate::irc::chathistory::clamp_limit(1000, conn.isupport_parsed.chathistory_max());
        }
        self.send_history_targets(conn_id);
    }

    pub(crate) fn queue_connect_history(&mut self, conn_id: &str, target: &str) -> bool {
        if self.config.display.backlog_lines == 0 || !self.ensure_history_discovery(conn_id) {
            return false;
        }
        let discovery = self.history_discovery.get_mut(conn_id).unwrap();
        if !discovery
            .queue
            .iter()
            .any(|name| name.eq_ignore_ascii_case(target))
        {
            if self
                .state
                .connections
                .get(conn_id)
                .is_some_and(|conn| !conn.chathistory.any_in_flight(target))
            {
                discovery.hydration_attempts.remove(target);
            }
            discovery.queue.push_back(target.to_string());
        }
        self.tick_history_discovery();
        true
    }

    fn send_history_targets(&mut self, conn_id: &str) {
        let Some(discovery) = self.history_discovery.get_mut(conn_id) else {
            return;
        };
        if discovery.finished || discovery.pending.is_some() || Instant::now() < discovery.retry_at
        {
            return;
        }
        if discovery.attempts >= 3 {
            discovery.finished = true;
            if let Some(conn) = self.state.connections.get(conn_id) {
                let buffer_id = make_buffer_id(conn_id, &conn.label);
                crate::irc::events::emit(
                    &mut self.state,
                    &buffer_id,
                    "Bouncer conversation discovery failed after three attempts; reconnect to retry.",
                );
            }
            return;
        }
        let command = irc::proto::Command::Raw(
            "CHATHISTORY".into(),
            vec![
                "TARGETS".into(),
                format!(
                    "timestamp={}",
                    crate::irc::chathistory::rfc3339_millis(discovery.upper_ms)
                ),
                "timestamp=1970-01-01T00:00:00.000Z".into(),
                discovery.limit.to_string(),
            ],
        );
        discovery.attempts += 1;
        discovery.retry_at = Instant::now() + Duration::from_secs(5);
        if self
            .irc_handles
            .get(conn_id)
            .is_some_and(|handle| handle.sender().send(command).is_ok())
        {
            discovery.pending = Some(Instant::now());
        }
    }

    pub(crate) fn receive_history_targets(&mut self, conn_id: &str, batch: &BatchInfo) {
        let Some(discovery) = self.history_discovery.get_mut(conn_id) else {
            return;
        };
        if discovery
            .pending
            .is_none_or(|started| batch.started_at < started)
        {
            return;
        }
        let Some(targets) = decode_targets(batch, discovery.upper_ms) else {
            return;
        };
        discovery.pending = None;
        discovery.attempts = 0;
        discovery.retry_at = Instant::now();
        if batch
            .opener_tags
            .as_ref()
            .is_some_and(|tags| tags.iter().any(|tag| tag.0 == "draft/chathistory-end"))
        {
            discovery.finished = true;
        }
        let mut report_incomplete = false;
        if let Some(oldest) = targets.iter().map(|(_, time)| *time).min() {
            let overlapping_upper = oldest.saturating_add(1);
            if !discovery.finished && overlapping_upper == discovery.upper_ms
                && targets.len() >= discovery.limit && !discovery.incomplete {
                discovery.incomplete = true;
                report_incomplete = true;
            }
            discovery.upper_ms = if overlapping_upper < discovery.upper_ms {
                overlapping_upper
            } else {
                oldest
            };
        } else {
            discovery.finished = true;
        }
        for (target, _) in targets {
            if !discovery.seen.insert(target.to_ascii_lowercase()) {
                continue;
            }
            let Some(conn) = self.state.connections.get(conn_id) else {
                return;
            };
            if conn.chathistory.was_closed(&target) {
                continue;
            }
            let buffer_id = make_buffer_id(conn_id, &target);
            let channel = crate::irc::formatting::is_channel(&target);
            if !self.state.buffers.contains_key(&buffer_id) {
                self.state.add_buffer_with_focus(
                    Buffer::empty(
                        conn_id,
                        if channel {
                            BufferType::Channel
                        } else {
                            BufferType::Query
                        },
                        &target,
                    ),
                    false,
                );
            }
            discovery.queue.push_back(target);
        }
        if report_incomplete && let Some(conn) = self.state.connections.get(conn_id) {
            let buffer_id = make_buffer_id(conn_id, &conn.label);
            crate::irc::events::emit(&mut self.state, &buffer_id,
                "Some conversations may be missing because the bouncer's history limit was reached. Open a known conversation by name to retrieve its history.");
        }
        self.tick_history_discovery();
    }

    pub(crate) fn tick_history_discovery(&mut self) {
        let ids: Vec<_> = self.history_discovery.keys().cloned().collect();
        for conn_id in ids {
            let total_pending: usize = self
                .state
                .connections
                .values()
                .filter(|conn| conn.server_owns_history())
                .map(|conn| conn.chathistory.pending_count())
                .sum();
            let Some(conn) = self.state.connections.get_mut(&conn_id) else {
                self.history_discovery.remove(&conn_id);
                continue;
            };
            if conn.status != crate::state::connection::ConnectionStatus::Connected {
                continue;
            }
            let discovery = self.history_discovery.get_mut(&conn_id).unwrap();
            if discovery
                .pending
                .is_some_and(|started| started.elapsed() > Duration::from_secs(35))
            {
                discovery.pending = None;
            }
            let completed: Vec<_> = discovery
                .active
                .iter()
                .filter(|target| !conn.chathistory.any_in_flight(target))
                .cloned()
                .collect();
            for target in completed {
                discovery.active.remove(&target);
                if conn.chathistory.last_request_succeeded(&target) != Some(true)
                    && discovery
                        .hydration_attempts
                        .get(&target)
                        .copied()
                        .unwrap_or(0)
                        < 3
                {
                    conn.chathistory.clear_connect_gapfilled(&target);
                    discovery.queue.push_back(target);
                }
            }
            let available = 2_usize
                .saturating_sub(conn.chathistory.pending_count())
                .min(8_usize.saturating_sub(total_pending));
            let targets: Vec<_> = (0..available)
                .filter_map(|_| discovery.queue.pop_front())
                .collect();
            self.send_history_targets(&conn_id);
            for target in targets {
                self.send_queued_hydration(&conn_id, target);
            }
        }
    }

    fn send_queued_hydration(&mut self, conn_id: &str, target: String) {
        if !self
            .state
            .buffers
            .contains_key(&make_buffer_id(conn_id, &target))
        {
            return;
        }
        let Some(conn) = self.state.connections.get(conn_id) else {
            return;
        };
        if self.config.display.backlog_lines == 0 || conn.chathistory.is_connect_gapfilled(&target)
        {
            return;
        }
        if conn.chathistory.any_in_flight(&target) {
            self.history_discovery
                .get_mut(conn_id)
                .unwrap()
                .queue
                .push_back(target);
            return;
        }
        let attempts = self
            .history_discovery
            .get_mut(conn_id)
            .unwrap()
            .hydration_attempts
            .entry(target.clone())
            .or_default();
        if *attempts >= 3 {
            return;
        }
        *attempts += 1;
        if self.send_connect_gapfill(conn_id, &target) {
            self.history_discovery
                .get_mut(conn_id)
                .unwrap()
                .active
                .insert(target);
        } else {
            self.history_discovery
                .get_mut(conn_id)
                .unwrap()
                .queue
                .push_back(target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let config = toml::from_str("label='Bouncer'\naddress='bnc.example.org'\nport=6697\ntls=true\nchannels=[]\nbouncer_network_id='42'").unwrap();
        app.setup_connection("account", &config);
        let conn = app.state.connections.get_mut("account").unwrap();
        conn.status = crate::state::connection::ConnectionStatus::Connected;
        conn.enabled_caps.insert("draft/chathistory".into());
        app.irc_handles.insert(
            "account".into(),
            crate::irc::IrcHandle::new(
                "account".into(),
                crate::irc::IrcSender::capturing(0),
                None,
                None,
            ),
        );
        app
    }

    #[tokio::test]
    async fn short_and_oversized_target_pages_continue_until_explicit_end() {
        let mut app = app();
        app.start_history_discovery("account");
        let upper = app.history_discovery["account"].upper_ms;
        assert!(upper > chrono::Utc::now().timestamp_millis() + 4_000);
        app.receive_history_targets(
            "account",
            &batch(&[":bnc CHATHISTORY TARGETS First 2024-01-01T00:00:02.000Z"]),
        );
        assert!(!app.history_discovery["account"].finished);
        app.history_discovery.get_mut("account").unwrap().limit = 1;
        let mut page = batch(&[
            ":bnc CHATHISTORY TARGETS Second 2024-01-01T00:00:01.000Z",
            ":bnc CHATHISTORY TARGETS Third 2024-01-01T00:00:00.000Z",
        ]);
        page.opener_tags = Some(vec![irc::proto::message::Tag(
            "draft/chathistory-end".into(),
            None,
        )]);
        app.receive_history_targets("account", &page);
        assert!(app.history_discovery["account"].finished);
        assert!(app.state.buffers.contains_key("account/third"));
    }

    #[tokio::test]
    async fn retained_queries_use_the_same_bounded_queue_before_targets_start() {
        let mut app = app();
        for target in ["First", "Second", "Third"] {
            app.state
                .add_buffer_with_focus(Buffer::empty("account", BufferType::Query, target), false);
        }
        app.gapfill_active_buffer_on_connect("account");
        assert_eq!(
            app.state.connections["account"].chathistory.pending_count(),
            2
        );
        assert_eq!(app.history_discovery["account"].queue.len(), 1);
        assert!(!app.history_discovery["account"].discovery_started);
        app.start_history_discovery("account");
        assert!(app.history_discovery["account"].pending.is_some());
        assert_eq!(app.history_discovery["account"].queue.len(), 1);
    }

    #[tokio::test]
    async fn failed_hydration_retries_without_confusing_timeout_with_empty_history() {
        let mut app = app();
        app.start_history_discovery("account");
        app.receive_history_targets(
            "account",
            &batch(&[":bnc CHATHISTORY TARGETS Peer 2024-01-01T00:00:00.000Z"]),
        );
        assert_eq!(
            app.history_discovery["account"].hydration_attempts["Peer"],
            1
        );
        app.state
            .connections
            .get_mut("account")
            .unwrap()
            .chathistory
            .clear_stale(Duration::ZERO);
        app.tick_history_discovery();
        assert_eq!(
            app.history_discovery["account"].hydration_attempts["Peer"],
            2
        );
        assert!(
            app.state.connections["account"]
                .chathistory
                .any_in_flight("Peer")
        );
        app.state
            .connections
            .get_mut("account")
            .unwrap()
            .chathistory
            .complete_target("Peer", 0, None, true);
        app.tick_history_discovery();
        assert!(app.history_discovery["account"].active.is_empty());
        assert!(app.history_discovery["account"].queue.is_empty());
        assert_eq!(
            app.history_discovery["account"].hydration_attempts["Peer"],
            2
        );
    }

    #[tokio::test]
    async fn hydration_queue_keeps_two_requests_in_flight_per_connection() {
        let mut app = app();
        app.start_history_discovery("account");
        app.receive_history_targets(
            "account",
            &batch(&[
                ":bnc CHATHISTORY TARGETS First 2024-01-01T00:00:00.000Z",
                ":bnc CHATHISTORY TARGETS Second 2024-01-01T00:00:01.000Z",
                ":bnc CHATHISTORY TARGETS Third 2024-01-01T00:00:02.000Z",
            ]),
        );
        assert_eq!(
            app.state.connections["account"].chathistory.pending_count(),
            2
        );
        assert_eq!(app.history_discovery["account"].queue.len(), 1);
        app.state
            .connections
            .get_mut("account")
            .unwrap()
            .chathistory
            .complete_target("First", 0, None, true);
        app.tick_history_discovery();
        assert_eq!(
            app.state.connections["account"].chathistory.pending_count(),
            2
        );
        assert!(
            app.state.connections["account"]
                .chathistory
                .any_in_flight("Third")
        );
        assert!(app.history_discovery["account"].queue.is_empty());
    }

    #[tokio::test]
    async fn timed_out_discovery_retries_the_same_window_with_a_bound() {
        let mut app = app();
        app.start_history_discovery("account");
        let original = app.irc_handles["account"].sender().captured()[0].clone();
        for attempt in 1..=3 {
            let discovery = app.history_discovery.get_mut("account").unwrap();
            discovery.pending = Some(Instant::now().checked_sub(Duration::from_secs(36)).unwrap());
            discovery.retry_at = Instant::now();
            app.tick_history_discovery();
            assert_eq!(
                app.history_discovery["account"].attempts,
                (attempt + 1).min(3)
            );
        }
        let commands = app.irc_handles["account"].sender().captured();
        assert_eq!(commands.len(), 3);
        assert!(commands.iter().all(|command| command == &original));
        assert!(app.history_discovery["account"].finished);
    }

    #[tokio::test]
    async fn full_target_pages_advance_and_deduplicate_overlapping_rows() {
        let mut app = app();
        app.start_history_discovery("account");
        app.history_discovery.get_mut("account").unwrap().limit = 1;
        let page = batch(&[":bnc CHATHISTORY TARGETS Peer 2024-01-01T00:00:00.000Z"]);
        app.receive_history_targets("account", &page);
        assert_eq!(app.history_discovery["account"].upper_ms, 1_704_067_200_001);
        let overlap = batch(&[":bnc CHATHISTORY TARGETS Peer 2024-01-01T00:00:00.000Z"]);
        app.receive_history_targets("account", &overlap);
        assert_eq!(app.history_discovery["account"].upper_ms, 1_704_067_200_000);
        assert!(app.history_discovery["account"].incomplete);
        assert_eq!(app.history_discovery["account"].seen.len(), 1);
        assert_eq!(app.history_discovery["account"].active.len(), 1);
        app.receive_history_targets("account", &batch(&[
            ":bnc CHATHISTORY TARGETS Older 2023-01-01T00:00:00.000Z",
        ]));
        app.receive_history_targets("account", &batch(&[
            ":bnc CHATHISTORY TARGETS Older 2023-01-01T00:00:00.000Z",
        ]));
        assert_eq!(app.history_discovery["account"].seen.len(), 2);
        assert!(app.state.buffers.contains_key("account/older"));
        assert_eq!(app.state.buffers.values().flat_map(|buffer| &buffer.messages)
            .filter(|message| message.text.contains("Some conversations may be missing")).count(), 1);
        let empty = batch(&[]);
        app.receive_history_targets("account", &empty);
        assert!(app.history_discovery["account"].finished);
    }

    #[tokio::test]
    async fn overlapping_pages_below_the_limit_do_not_warn() {
        let mut app = app();
        app.start_history_discovery("account");
        app.history_discovery.get_mut("account").unwrap().limit = 2;
        for _ in 0..2 {
            app.receive_history_targets("account", &batch(&[
                ":bnc CHATHISTORY TARGETS Peer 2024-01-01T00:00:00.000Z",
            ]));
        }
        app.receive_history_targets("account", &batch(&[]));
        assert!(app.history_discovery["account"].finished);
        assert!(!app.history_discovery["account"].incomplete);
    }

    #[tokio::test]
    async fn explicit_end_of_targets_does_not_report_a_limit_gap() {
        let mut app = app();
        app.start_history_discovery("account");
        app.history_discovery.get_mut("account").unwrap().limit = 1;
        app.receive_history_targets("account", &batch(&[
            ":bnc CHATHISTORY TARGETS Peer 2024-01-01T00:00:00.000Z",
        ]));
        let mut page = batch(&[":bnc CHATHISTORY TARGETS Peer 2024-01-01T00:00:00.000Z"]);
        page.opener_tags = Some(vec![irc::proto::message::Tag("draft/chathistory-end".into(), None)]);
        app.receive_history_targets("account", &page);
        assert!(app.history_discovery["account"].finished);
        assert!(!app.history_discovery["account"].incomplete);
    }

    fn batch(lines: &[&str]) -> BatchInfo {
        BatchInfo {
            message_order: Vec::new(),
            redaction_refs: Vec::new(),
            batch_type: "DRAFT/CHATHISTORY-TARGETS".into(),
            params: Vec::new(),
            messages: lines.iter().map(|line| line.parse().unwrap()).collect(),
            dropped_messages: 0,
            started_at: Instant::now(),
            opener_tags: None,
        }
    }

    #[test]
    fn targets_accept_both_channel_and_query_rows() {
        let batch = batch(&[
            ":bnc CHATHISTORY TARGETS #chat 2024-01-01T00:00:00.000Z",
            ":bnc CHATHISTORY TARGETS Peer 2024-01-01T00:00:01.000Z",
        ]);
        assert_eq!(
            decode_targets(&batch, 1_800_000_000_000),
            Some(vec![
                ("#chat".into(), 1_704_067_200_000),
                ("Peer".into(), 1_704_067_201_000)
            ])
        );
    }

    #[test]
    fn targets_reject_truncation_and_rows_outside_the_requested_window() {
        let mut batch = batch(&[":bnc CHATHISTORY TARGETS Peer 2024-01-01T00:00:00.000Z"]);
        assert!(decode_targets(&batch, 1_700_000_000_000).is_none());
        batch.dropped_messages = 1;
        assert!(decode_targets(&batch, 1_800_000_000_000).is_none());
    }

    #[test]
    fn malformed_targets_do_not_partially_commit_a_page() {
        let batch = batch(&[
            ":bnc CHATHISTORY TARGETS Peer 2024-01-01T00:00:00.000Z",
            ":bnc CHATHISTORY TARGETS Other invalid-time",
        ]);
        assert!(decode_targets(&batch, 1_800_000_000_000).is_none());
    }
}

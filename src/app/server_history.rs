use std::time::{Duration, Instant};

use crate::irc::chathistory::Direction;
use crate::state::buffer::{Buffer, BufferType};
use crate::web::protocol::{WebEvent, WireMessage};

use super::App;

pub struct PendingHistoryPage {
    buffer_id: String,
    request_target: String,
    network_scope: Option<String>,
    server_history_available: bool,
    session_id: String,
    limit: usize,
    before: Option<i64>,
    cursor_precision_ms: i64,
    before_message_id: Option<u64>,
    started: Instant,
}

fn memory_page(
    buffer: &Buffer,
    request: &PendingHistoryPage,
    extractor: Option<&crate::web::preview::WebPreviewExtractor>,
) -> (Vec<WireMessage>, bool) {
    let boundary = request
        .before_message_id
        .and_then(|id| buffer.messages.iter().position(|message| message.id == id));
    let rows: Vec<_> = buffer
        .messages
        .iter()
        .enumerate()
        .filter(|(index, message)| {
            !super::backlog::is_synthetic_row(message)
                && boundary.map_or_else(
                    || {
                        request.before.is_none_or(|before| {
                            message
                                .timestamp
                                .timestamp_millis()
                                .div_euclid(request.cursor_precision_ms)
                                <= before.div_euclid(request.cursor_precision_ms)
                        })
                    },
                    |boundary| *index < boundary,
                )
        })
        .map(|(_, message)| message)
        .collect();
    let skip = request.before.filter(|_| boundary.is_none()).map_or_else(
        || rows.len().saturating_sub(request.limit),
        |before| {
            let older = rows.partition_point(|message| {
                message
                    .timestamp
                    .timestamp_millis()
                    .div_euclid(request.cursor_precision_ms)
                    < before.div_euclid(request.cursor_precision_ms)
            });
            let mut start = older.saturating_sub(request.limit);
            while start > 0
                && rows[start - 1]
                    .timestamp
                    .timestamp_millis()
                    .div_euclid(request.cursor_precision_ms)
                    == rows[start]
                        .timestamp
                        .timestamp_millis()
                        .div_euclid(request.cursor_precision_ms)
            {
                start -= 1;
            }
            start
        },
    );
    let messages = rows
        .into_iter()
        .skip(skip)
        .map(|message| crate::web::snapshot::message_to_wire(message, extractor))
        .collect();
    (messages, skip > 0)
}

impl App {
    pub(crate) fn rekey_pending_history(&mut self, old_id: &str, new_id: &str) {
        for page in &mut self.pending_history_pages {
            if page.buffer_id == old_id {
                new_id.clone_into(&mut page.buffer_id);
            }
        }
    }

    pub(crate) fn release_web_history(&mut self, session_id: &str) {
        let Some(buffer_id) = self.state.web_history_buffers.remove(session_id) else {
            return;
        };
        if !self
            .state
            .web_history_buffers
            .values()
            .any(|id| id == &buffer_id)
            && !(self.state.active_buffer_id.as_deref() == Some(&buffer_id)
                && (self.scroll_offset > 0 || self.log_browser_mode))
        {
            self.state.collapse_buffer_backlog(&buffer_id);
        }
    }

    pub(crate) fn fetch_server_history_page(
        &mut self,
        buffer_id: &str,
        limit: u32,
        before: Option<i64>,
        before_message_id: Option<u64>,
        session_id: &str,
    ) {
        let cursor_precision_ms = if before_message_id.is_none()
            && before.is_some_and(|timestamp| timestamp < 10_000_000_000)
        {
            1000
        } else {
            1
        };
        let before = before.map(|timestamp| timestamp.saturating_mul(cursor_precision_ms));
        let request = PendingHistoryPage {
            buffer_id: buffer_id.to_string(),
            request_target: self
                .state
                .buffers
                .get(buffer_id)
                .map_or_else(String::new, |buffer| buffer.name.clone()),
            server_history_available: buffer_id
                .split_once('/')
                .and_then(|(id, _)| self.state.connections.get(id))
                .is_some_and(|conn| conn.enabled_caps.contains("draft/chathistory")),
            network_scope: buffer_id
                .split_once('/')
                .and_then(|(id, _)| self.state.connections.get(id))
                .map(|conn| conn.network_key().to_string()),
            session_id: session_id.to_string(),
            limit: limit.clamp(1, 500) as usize,
            before,
            cursor_precision_ms,
            before_message_id,
            started: Instant::now(),
        };
        if before.is_some() && self.state.buffers.contains_key(buffer_id) {
            if self
                .state
                .web_history_buffers
                .get(session_id)
                .is_some_and(|id| id != buffer_id)
            {
                self.release_web_history(session_id);
            }
            self.state
                .web_history_buffers
                .insert(session_id.to_string(), buffer_id.to_string());
            if let Some(buffer) = self.state.buffers.get_mut(buffer_id) {
                buffer.pin_backlog = true;
            }
        }
        let Some(buffer) = self.state.buffers.get(buffer_id) else {
            self.reply_server_history_page(&request);
            return;
        };
        let (messages, _) = memory_page(buffer, &request, None);
        let conn_id = buffer.connection_id.clone();
        let target = buffer.name.clone();
        let retained_cursor = before_message_id
            .is_some_and(|id| buffer.messages.iter().any(|message| message.id == id));
        let needs_older_page = messages.is_empty()
            || (!retained_cursor
                && before.is_some_and(|before| {
                    messages.iter().all(|message| {
                        message.ts_ms.div_euclid(cursor_precision_ms)
                            >= before.div_euclid(cursor_precision_ms)
                    })
                }));
        let can_request = needs_older_page
            && matches!(buffer.buffer_type, BufferType::Channel | BufferType::Query)
            && buffer.messages.len() < super::backlog::PINNED_BACKLOG_CAP;
        if can_request {
            let pending = self
                .state
                .connections
                .get(&conn_id)
                .is_some_and(|conn| conn.chathistory.any_in_flight(&target));
            let sent = pending
                || if before.is_some() {
                    self.fetch_older_via_chathistory(buffer_id)
                } else {
                    self.request_chathistory_with_limit(
                        &conn_id,
                        &target,
                        Direction::Latest,
                        None,
                        request.limit,
                    )
                };
            if sent {
                self.pending_history_pages
                    .retain(|page| page.session_id != session_id || page.buffer_id != buffer_id);
                self.pending_history_pages.push(request);
                return;
            }
        }
        self.reply_server_history_page(&request);
    }

    pub(crate) fn flush_server_history_pages(&mut self) {
        let pages = std::mem::take(&mut self.pending_history_pages);
        for page in pages {
            let pending = self
                .state
                .buffers
                .get(&page.buffer_id)
                .and_then(|buffer| {
                    self.state
                        .connections
                        .get(&buffer.connection_id)
                        .map(|conn| conn.chathistory.any_in_flight(&page.request_target))
                })
                .unwrap_or(false);
            if pending && page.started.elapsed() < Duration::from_secs(35) {
                self.pending_history_pages.push(page);
            } else {
                self.reply_server_history_page(&page);
            }
        }
    }

    fn reply_server_history_page(&self, page: &PendingHistoryPage) {
        let (messages, has_more) = self
            .state
            .buffers
            .get(&page.buffer_id)
            .filter(|buffer| {
                self.state
                    .connections
                    .get(&buffer.connection_id)
                    .is_some_and(|conn| Some(conn.network_key()) == page.network_scope.as_deref())
            })
            .map_or_else(
                || (Vec::new(), false),
                |buffer| {
                    let (messages, more_memory) =
                        memory_page(buffer, page, self.state.web_preview_extractor.as_deref());
                    let more_server = matches!(
                        buffer.buffer_type,
                        BufferType::Channel | BufferType::Query
                    ) && buffer.messages.len()
                        < super::backlog::PINNED_BACKLOG_CAP
                        && self
                            .state
                            .connections
                            .get(&buffer.connection_id)
                            .is_some_and(|conn| {
                                (conn.enabled_caps.contains("draft/chathistory")
                                        || (page.server_history_available && conn.status
                                            != crate::state::connection::ConnectionStatus::Connected))
                                    && !conn.chathistory.is_before_exhausted(&buffer.name)
                            });
                    (messages, more_memory || more_server)
                },
            );
        self.broadcast_web(WebEvent::Messages {
            buffer_id: page.buffer_id.clone(),
            messages,
            has_more,
            session_id: Some(page.session_id.clone()),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let server = toml::from_str(
            "label = 'Bouncer'\naddress = 'bnc.example.org'\nport = 6697\ntls = true\nchannels = []\nbouncer_network_id = '42'",
        ).unwrap();
        app.setup_connection("account", &server);
        app.state
            .add_buffer(Buffer::for_test("account", BufferType::Channel, "#test"));
        app.state
            .connections
            .get_mut("account")
            .unwrap()
            .enabled_caps
            .insert("draft/chathistory".into());
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
    async fn memory_cursor_keeps_equal_timestamps_with_nonmonotonic_ids() {
        let mut app = app();
        let mut rows = Vec::new();
        for text in ["first", "second", "third"] {
            let mut message = crate::state::events::tests::make_test_message(&mut app.state, text);
            message.timestamp = chrono::DateTime::from_timestamp_millis(1000).unwrap();
            rows.push(message);
        }
        rows[0].id = 30;
        rows[1].id = 20;
        rows[2].id = 10;
        app.state
            .buffers
            .get_mut("account/#test")
            .unwrap()
            .messages
            .extend(rows);
        let mut web = app.web_broadcaster.subscribe();
        app.fetch_server_history_page("account/#test", 1, Some(1000), Some(10), "browser");
        let WebEvent::Messages {
            messages,
            has_more,
            session_id,
            ..
        } = web.try_recv().unwrap()
        else {
            panic!("expected page")
        };
        assert_eq!(messages[0].text, "second");
        assert!(has_more);
        assert_eq!(session_id.as_deref(), Some("browser"));
        app.fetch_server_history_page("account/#test", 1, Some(1000), Some(20), "browser");
        let WebEvent::Messages { messages, .. } = web.try_recv().unwrap() else {
            panic!("expected page")
        };
        assert_eq!(messages[0].text, "first");
    }

    #[tokio::test]
    async fn server_page_completes_browser_request_without_a_log_writer() {
        let mut app = app();
        let mut web = app.web_broadcaster.subscribe();
        app.fetch_server_history_page("account/#test", 100, None, None, "browser");
        assert!(web.try_recv().is_err());
        assert_eq!(app.pending_history_pages.len(), 1);
        let batch = crate::irc::batch::BatchInfo {
            batch_type: "CHATHISTORY".into(),
            params: vec!["#test".into()],
            started_at: Instant::now(),
            opener_tags: None,
            dropped_messages: 0,
            messages: vec![
                "@time=2024-01-01T00:00:00.000Z;msgid=m1 :peer!u@h PRIVMSG #test :from server"
                    .parse()
                    .unwrap(),
            ],
        };
        crate::irc::batch::process_completed_batch(&mut app.state, "account", &batch, true);
        app.flush_server_history_pages();
        let WebEvent::Messages {
            messages,
            session_id,
            ..
        } = web.try_recv().unwrap()
        else {
            panic!("expected page")
        };
        assert_eq!(messages[0].text, "from server");
        assert_eq!(session_id.as_deref(), Some("browser"));
        assert!(app.pending_history_pages.is_empty());
        app.flush_server_history_pages();
        assert!(web.try_recv().is_err());
    }

    #[tokio::test]
    async fn existing_sqlite_history_is_neither_loaded_nor_deleted() {
        let mut app = app();
        app.storage = Some(crate::storage::Storage::in_memory());
        let scope = app.state.connections["account"].network_key().to_string();
        app.storage.as_ref().unwrap().db.lock().unwrap().execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, text) VALUES ('stored-history', ?1, '#test', 1, 'message', 'stale local history')",
            [&scope],
        ).unwrap();
        app.state
            .connections
            .get_mut("account")
            .unwrap()
            .enabled_caps
            .clear();
        app.load_backlog("account/#test");
        assert!(app.state.buffers["account/#test"].messages.is_empty());
        let mut web = app.web_broadcaster.subscribe();
        app.fetch_server_history_page("account/#test", 100, None, None, "browser");
        let WebEvent::Messages {
            messages, has_more, ..
        } = web.try_recv().unwrap()
        else {
            panic!("expected page")
        };
        assert!(messages.is_empty());
        assert!(!has_more);
        let count: i64 = app
            .storage
            .as_ref()
            .unwrap()
            .db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn volatile_mentions_follow_the_seven_day_retention_policy() {
        let mut app = app();
        let now = chrono::Utc::now();
        for age_days in [8, 1] {
            let mut message =
                crate::state::events::tests::make_test_message(&mut app.state, "mention");
            message.timestamp = now - chrono::Duration::days(age_days);
            let wire = crate::web::snapshot::message_to_wire(&message, None);
            app.record_mention("account/#test", &wire);
        }
        assert_eq!(app.volatile_mentions.len(), 2);
        app.last_mention_purge = Instant::now()
            .checked_sub(Duration::from_secs(3601))
            .unwrap();
        app.maybe_purge_old_mentions();
        assert_eq!(app.volatile_mentions.len(), 1);
        assert_eq!(
            app.volatile_mentions[0].1.timestamp,
            (now - chrono::Duration::days(1)).timestamp()
        );
    }

    #[tokio::test]
    async fn pending_history_and_readers_follow_buffer_renames() {
        let mut app = app();
        app.fetch_server_history_page("account/#test", 20, None, None, "browser");
        app.state
            .web_history_buffers
            .insert("browser".into(), "account/#test".into());
        let mut buffer = app.state.buffers.shift_remove("account/#test").unwrap();
        buffer.id = "account/#renamed".into();
        buffer.name = "#renamed".into();
        app.state.buffers.insert(buffer.id.clone(), buffer);
        app.state
            .rekey_buffer_state("account/#test", "account/#renamed");
        app.drain_pending_buffer_rekeys();
        assert_eq!(app.state.web_history_buffers["browser"], "account/#renamed");
        assert_eq!(app.pending_history_pages[0].buffer_id, "account/#renamed");
        let mut web = app.web_broadcaster.subscribe();
        app.flush_server_history_pages();
        assert!(web.try_recv().is_err());
        let batch = crate::irc::batch::BatchInfo {
            batch_type: "CHATHISTORY".into(),
            params: vec!["#test".into()],
            started_at: Instant::now(),
            opener_tags: None,
            dropped_messages: 0,
            messages: vec!["@time=2024-01-01T00:00:00.000Z;msgid=renamed :peer!u@h PRIVMSG #test :renamed history".parse().unwrap()],
        };
        crate::irc::batch::process_completed_batch(&mut app.state, "account", &batch, true);
        app.flush_server_history_pages();
        let WebEvent::Messages {
            buffer_id,
            messages,
            ..
        } = web.try_recv().unwrap()
        else {
            panic!("expected history response");
        };
        assert_eq!(buffer_id, "account/#renamed");
        assert_eq!(messages[0].text, "renamed history");
    }

    #[tokio::test]
    async fn memory_only_web_pagination_pins_the_cursor_before_live_rows_arrive() {
        let mut app = app();
        app.state.scrollback_limit = 1;
        let message = crate::state::events::tests::make_test_message(&mut app.state, "cursor");
        app.state.add_message("account/#test", message);
        app.fetch_server_history_page("account/#test", 20, Some(i64::MAX), None, "browser");
        assert!(app.pending_history_pages.is_empty());
        let message = crate::state::events::tests::make_test_message(&mut app.state, "new");
        app.state.add_message("account/#test", message);
        assert!(app.state.buffers["account/#test"].pin_backlog);
        assert_eq!(
            app.state.buffers["account/#test"].messages[0].text,
            "cursor"
        );
        app.release_web_history("browser");
        assert_eq!(app.state.buffers["account/#test"].messages.len(), 1);
    }

    #[tokio::test]
    async fn disconnected_web_history_timeout_remains_retryable() {
        let mut app = app();
        let mut web = app.web_broadcaster.subscribe();
        app.fetch_server_history_page("account/#test", 20, None, None, "browser");
        let conn = app.state.connections.get_mut("account").unwrap();
        conn.enabled_caps.clear();
        conn.status = crate::state::connection::ConnectionStatus::Disconnected;
        app.pending_history_pages[0].started =
            Instant::now().checked_sub(Duration::from_secs(36)).unwrap();
        app.flush_server_history_pages();
        let WebEvent::Messages { has_more, .. } = web.try_recv().unwrap() else {
            panic!("expected history response");
        };
        assert!(has_more);
        assert!(app.pending_history_pages.is_empty());
    }

    #[tokio::test]
    async fn switching_one_web_session_preserves_another_readers_backlog() {
        let mut app = app();
        app.state.scrollback_limit = 1;
        app.state.active_buffer_id = Some("account/#test".into());
        app.state.add_buffer_with_focus(
            Buffer::for_test("account", BufferType::Query, "peer"),
            false,
        );
        for text in ["older", "current"] {
            let message = crate::state::events::tests::make_test_message(&mut app.state, text);
            app.state
                .buffers
                .get_mut("account/#test")
                .unwrap()
                .messages
                .push_back(message);
        }
        app.state
            .buffers
            .get_mut("account/#test")
            .unwrap()
            .pin_backlog = true;
        for session in ["first", "second"] {
            app.state
                .web_history_buffers
                .insert(session.into(), "account/#test".into());
        }
        app.handle_web_command(
            crate::web::protocol::WebCommand::SwitchBuffer {
                buffer_id: "account/peer".into(),
            },
            "first",
        );
        assert_eq!(app.state.active_buffer_id.as_deref(), Some("account/peer"));
        assert!(app.state.buffers["account/#test"].pin_backlog);
        assert_eq!(app.state.buffers["account/#test"].messages.len(), 2);
        app.release_web_history("second");
        assert!(!app.state.buffers["account/#test"].pin_backlog);
        assert_eq!(app.state.buffers["account/#test"].messages.len(), 1);
    }

    #[tokio::test]
    async fn web_history_releases_memory_after_the_last_reader_leaves() {
        let mut app = app();
        app.state.scrollback_limit = 1;
        for text in ["older", "middle", "current"] {
            let message = crate::state::events::tests::make_test_message(&mut app.state, text);
            app.state
                .buffers
                .get_mut("account/#test")
                .unwrap()
                .messages
                .push_back(message);
        }
        app.state
            .buffers
            .get_mut("account/#test")
            .unwrap()
            .pin_backlog = true;
        app.fetch_server_history_page("account/#test", 1, Some(i64::MAX), None, "first");
        app.fetch_server_history_page("account/#test", 1, Some(i64::MAX), None, "second");
        app.collapse_backlog_if_at_bottom();
        assert_eq!(app.state.buffers["account/#test"].messages.len(), 3);
        app.handle_web_command(
            crate::web::protocol::WebCommand::CollapseBacklog {
                buffer_id: "account/#test".into(),
            },
            "first",
        );
        assert!(app.state.buffers["account/#test"].pin_backlog);
        app.handle_web_command(crate::web::protocol::WebCommand::WebDisconnect, "second");
        assert!(!app.state.buffers["account/#test"].pin_backlog);
        assert_eq!(app.state.buffers["account/#test"].messages.len(), 1);
        assert!(app.state.web_history_buffers.is_empty());
    }

    #[tokio::test]
    async fn web_collapse_preserves_history_being_read_in_the_terminal() {
        let mut app = app();
        app.state.active_buffer_id = Some("account/#test".into());
        app.scroll_offset = 5;
        app.state
            .buffers
            .get_mut("account/#test")
            .unwrap()
            .pin_backlog = true;
        app.state
            .web_history_buffers
            .insert("browser".into(), "account/#test".into());
        app.release_web_history("browser");
        assert!(app.state.buffers["account/#test"].pin_backlog);
        app.scroll_offset = 0;
        app.collapse_backlog_if_at_bottom();
        assert!(!app.state.buffers["account/#test"].pin_backlog);
    }

    #[tokio::test]
    async fn reopening_query_reloads_history_after_pagination_exhaustion() {
        let mut app = app();
        app.state
            .add_buffer(Buffer::for_test("account", BufferType::Query, "peer"));
        app.load_backlog("account/peer");
        app.state
            .connections
            .get_mut("account")
            .unwrap()
            .chathistory
            .complete_target("peer", 0, None, true);
        let message = crate::state::events::tests::make_test_message(&mut app.state, "current");
        app.state.add_message("account/peer", message);
        assert!(app.fetch_older_via_chathistory("account/peer"));
        app.state
            .connections
            .get_mut("account")
            .unwrap()
            .chathistory
            .complete_target("peer", 0, Some((1000, None)), true);
        assert!(
            app.state.connections["account"]
                .chathistory
                .is_before_exhausted("peer")
        );
        app.state.remove_buffer("account/peer");
        app.state
            .add_buffer(Buffer::for_test("account", BufferType::Query, "peer"));
        app.load_backlog("account/peer");
        let history = &app.state.connections["account"].chathistory;
        assert!(!history.is_before_exhausted("peer"));
        assert!(history.oldest_fetched("peer").is_none());
        assert!(history.requested_limit("peer", Direction::Latest).is_some());
    }

    #[tokio::test]
    async fn automatic_history_respects_configured_backlog_size() {
        let mut app = app();
        app.state
            .add_buffer(Buffer::for_test("account", BufferType::Query, "peer"));
        app.config.display.backlog_lines = 0;
        app.load_backlog("account/peer");
        assert!(
            !app.state.connections["account"]
                .chathistory
                .any_in_flight("peer")
        );
        app.config.display.backlog_lines = 7;
        app.load_backlog("account/peer");
        assert_eq!(
            app.state.connections["account"]
                .chathistory
                .requested_limit("peer", Direction::Latest),
            Some(7)
        );
    }

    #[tokio::test]
    async fn explicit_web_history_still_works_with_automatic_backlog_disabled() {
        let mut app = app();
        app.config.display.backlog_lines = 0;
        app.fetch_server_history_page("account/#test", 13, None, None, "browser");
        assert_eq!(
            app.state.connections["account"]
                .chathistory
                .requested_limit("#test", Direction::Latest),
            Some(13)
        );
    }

    #[tokio::test]
    async fn dcc_history_stays_local_on_a_bouncer_connection() {
        let mut app = app();
        app.storage = Some(crate::storage::Storage::in_memory());
        app.state.add_buffer(Buffer::for_test(
            "account",
            BufferType::DccChat,
            "=dcc-peer",
        ));
        let (log_tx, mut log_rx) = tokio::sync::mpsc::channel(8);
        app.state.log_tx = Some(log_tx);
        let message =
            crate::state::events::tests::make_test_message(&mut app.state, "direct peer message");
        app.state.add_message("account/=dcc-peer", message);
        assert_eq!(log_rx.try_recv().unwrap().text, "direct peer message");
        assert!(!app.server_owns_buffer_history("account/=dcc-peer"));
        let message = crate::state::events::tests::make_test_message(&mut app.state, "dcc mention");
        app.record_mention(
            "account/=dcc-peer",
            &crate::web::snapshot::message_to_wire(&message, None),
        );
        let mentions = crate::storage::query::get_unread_mentions(
            &app.storage.as_ref().unwrap().db.lock().unwrap(),
        )
        .unwrap();
        assert_eq!(mentions.len(), 1);
        assert!(!App::mention_uses_server_history(
            &mentions[0].network,
            &mentions[0].buffer
        ));
        let scope = app.state.connections["account"].network_key().to_string();
        app.storage.as_ref().unwrap().db.lock().unwrap().execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, text) VALUES ('stored-dcc', ?1, '=dcc-peer', 1, 'message', 'older direct message')",
            [&scope],
        ).unwrap();
        app.load_backlog("account/=dcc-peer");
        assert!(
            app.state.buffers["account/=dcc-peer"]
                .messages
                .iter()
                .any(|message| message.text == "older direct message")
        );
    }

    #[tokio::test]
    async fn final_browser_page_follows_insert_events_and_preserves_exhaustion() {
        let mut app = app();
        let mut current = crate::state::events::tests::make_test_message(&mut app.state, "current");
        current.timestamp = chrono::DateTime::from_timestamp_millis(1_800_000_000_000).unwrap();
        let cursor = current.id;
        app.state.add_message("account/#test", current);
        app.state.pending_web_events.clear();
        let mut web = app.web_broadcaster.subscribe();
        app.fetch_server_history_page(
            "account/#test",
            100,
            Some(1_800_000_000_000),
            Some(cursor),
            "browser",
        );
        let batch = crate::irc::batch::BatchInfo {
            batch_type: "CHATHISTORY".into(),
            params: vec!["#test".into()],
            started_at: Instant::now(),
            opener_tags: None,
            dropped_messages: 0,
            messages: vec![
                "@time=2024-01-01T00:00:00.000Z;msgid=m1 :peer!u@h PRIVMSG #test :older"
                    .parse()
                    .unwrap(),
            ],
        };
        crate::irc::batch::process_completed_batch(&mut app.state, "account", &batch, true);
        app.drain_pending_web_events();
        let mut inserted = false;
        let mut completed = false;
        while let Ok(event) = web.try_recv() {
            match event {
                WebEvent::InsertMessage { .. } => {
                    assert!(!completed);
                    inserted = true;
                }
                WebEvent::Messages { has_more, .. } => {
                    assert!(inserted);
                    assert!(!has_more);
                    completed = true;
                }
                _ => {}
            }
        }
        assert!(completed);
    }

    #[tokio::test]
    async fn legacy_cursors_keep_whole_timestamp_groups() {
        for cursor in [1_700_000_000_000, 1_700_000_000] {
            let mut app = app();
            app.state
                .connections
                .get_mut("account")
                .unwrap()
                .enabled_caps
                .clear();
            for (index, offset) in [-1000, 0, 0, 0].into_iter().enumerate() {
                let mut message = crate::state::events::tests::make_test_message(
                    &mut app.state,
                    &format!("row-{index}"),
                );
                message.timestamp =
                    chrono::DateTime::from_timestamp_millis(1_700_000_000_000 + offset).unwrap();
                app.state
                    .buffers
                    .get_mut("account/#test")
                    .unwrap()
                    .messages
                    .push_back(message);
            }
            let mut web = app.web_broadcaster.subscribe();
            app.fetch_server_history_page("account/#test", 1, Some(cursor), None, "legacy");
            let WebEvent::Messages {
                messages, has_more, ..
            } = web.try_recv().unwrap()
            else {
                panic!("expected page")
            };
            assert_eq!(
                messages
                    .iter()
                    .map(|message| message.text.as_str())
                    .collect::<Vec<_>>(),
                ["row-0", "row-1", "row-2", "row-3"]
            );
            assert!(!has_more);
        }
    }

    #[tokio::test]
    async fn late_history_does_not_repin_a_collapsed_buffer() {
        let mut app = app();
        app.state.scrollback_limit = 1;
        let mut message = crate::state::events::tests::make_test_message(&mut app.state, "current");
        message.timestamp = chrono::DateTime::from_timestamp_millis(1_800_000_000_000).unwrap();
        app.state.add_message("account/#test", message);
        assert!(app.fetch_older_via_chathistory("account/#test"));
        assert!(app.state.buffers["account/#test"].pin_backlog);
        app.state.collapse_buffer_backlog("account/#test");
        let batch = crate::irc::batch::BatchInfo {
            batch_type: "CHATHISTORY".into(),
            params: vec!["#test".into()],
            started_at: Instant::now(),
            opener_tags: None,
            dropped_messages: 0,
            messages: vec![
                "@time=2024-01-01T00:00:00.000Z;msgid=m1 :peer!u@h PRIVMSG #test :older"
                    .parse()
                    .unwrap(),
            ],
        };
        crate::irc::batch::process_completed_batch(&mut app.state, "account", &batch, true);
        let buffer = &app.state.buffers["account/#test"];
        assert!(!buffer.pin_backlog);
        assert_eq!(buffer.messages.len(), 1);
        assert_eq!(buffer.messages[0].text, "current");
        assert!(
            !app.state.connections["account"]
                .chathistory
                .is_before_exhausted("#test")
        );
        assert!(
            app.state.connections["account"]
                .chathistory
                .oldest_fetched("#test")
                .is_none()
        );
        assert!(app.fetch_older_via_chathistory("account/#test"));
    }

    #[tokio::test]
    async fn background_channel_waits_for_names_before_loading_history() {
        let mut app = app();
        app.config.display.backlog_lines = 9;
        let active = app.state.active_buffer_id.clone();
        app.load_backlog("account/#test");
        assert!(
            !app.state.connections["account"]
                .chathistory
                .any_in_flight("#test")
        );
        assert!(
            !app.state.connections["account"]
                .chathistory
                .is_connect_gapfilled("#test")
        );
        app.gapfill_active_channel_on_join("account", "#test");
        assert_eq!(
            app.state.connections["account"]
                .chathistory
                .requested_limit("#test", Direction::Latest),
            Some(9)
        );
        assert_eq!(app.state.active_buffer_id, active);
    }

    #[tokio::test]
    async fn incomplete_before_page_retries_from_the_original_memory_anchor() {
        let mut app = app();
        let mut message = crate::state::events::tests::make_test_message(&mut app.state, "current");
        message.timestamp = chrono::DateTime::from_timestamp_millis(1_800_000_000_000).unwrap();
        app.state.add_message("account/#test", message);
        assert!(app.fetch_older_via_chathistory("account/#test"));
        let before = app.irc_handles["account"]
            .sender()
            .captured()
            .last()
            .unwrap()
            .clone();
        let batch = crate::irc::batch::BatchInfo {
            batch_type: "CHATHISTORY".into(),
            params: vec!["#test".into()],
            started_at: Instant::now(),
            opener_tags: None,
            dropped_messages: 0,
            messages: vec!["@time=2024-01-01T00:00:00.000Z;msgid=m1 :peer!u@h PRIVMSG #test :incomplete older page".parse().unwrap()],
        };
        crate::irc::batch::process_completed_batch(&mut app.state, "account", &batch, false);
        assert_eq!(app.state.buffers["account/#test"].messages.len(), 1);
        assert!(app.fetch_older_via_chathistory("account/#test"));
        assert_eq!(
            app.irc_handles["account"]
                .sender()
                .captured()
                .last()
                .unwrap(),
            &before
        );
    }

    #[tokio::test]
    async fn reconnect_gapfills_all_retained_bouncer_queries_once() {
        let mut app = app();
        app.state.active_buffer_id = None;
        for peer in ["first", "second"] {
            let mut buffer = Buffer::for_test("account", BufferType::Query, peer);
            let mut message =
                crate::state::events::tests::make_test_message(&mut app.state, "before disconnect");
            message.timestamp = chrono::DateTime::from_timestamp_millis(1_700_000_000_000).unwrap();
            buffer.messages.push_back(message);
            app.state.add_buffer(buffer);
        }
        app.gapfill_active_buffer_on_connect("account");
        for peer in ["first", "second"] {
            assert_eq!(
                app.state.connections["account"]
                    .chathistory
                    .in_flight_direction(peer),
                Some(Direction::After)
            );
        }
        assert!(
            !app.state.connections["account"]
                .chathistory
                .any_in_flight("#test")
        );
        let sent = app.irc_handles["account"].sender().captured().len();
        app.gapfill_active_buffer_on_connect("account");
        assert_eq!(app.irc_handles["account"].sender().captured().len(), sent);
    }

    #[tokio::test]
    async fn reopening_mentions_restores_volatile_history_without_sqlite() {
        let mut app = app();
        app.state.buffers.shift_remove(App::MENTIONS_BUFFER_ID);
        let message =
            crate::state::events::tests::make_test_message(&mut app.state, "remember this mention");
        app.record_mention(
            "account/#test",
            &crate::web::snapshot::message_to_wire(&message, None),
        );
        assert!(app.storage.is_none());
        for _ in 0..2 {
            app.create_mentions_buffer();
            let buffer = &app.state.buffers[App::MENTIONS_BUFFER_ID];
            assert_eq!(buffer.messages.len(), 1);
            assert!(buffer.messages[0].text.contains("remember this mention"));
            assert!(buffer.messages[0].text.contains("Bouncer"));
            app.state.buffers.shift_remove(App::MENTIONS_BUFFER_ID);
        }
    }

    #[tokio::test]
    async fn timed_out_browser_request_gets_one_reply() {
        let mut app = app();
        let mut web = app.web_broadcaster.subscribe();
        app.fetch_server_history_page("account/#test", 100, None, None, "browser");
        app.pending_history_pages[0].started =
            Instant::now().checked_sub(Duration::from_secs(36)).unwrap();
        app.flush_server_history_pages();
        assert!(matches!(web.try_recv().unwrap(), WebEvent::Messages { .. }));
        assert!(app.pending_history_pages.is_empty());
        app.flush_server_history_pages();
        assert!(web.try_recv().is_err());
    }
}

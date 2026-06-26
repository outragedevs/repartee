use std::sync::Arc;
use std::time::Instant;

use chrono::{Local, Utc};
use tokio::time::Duration;

use crate::state::buffer::{BufferType, Message, MessageType};

use super::App;

/// How long a CHATHISTORY request may sit in-flight before a sweep releases it.
///
/// Longer than the 60s batch timeout (`crate::irc::batch`), so an opened-but-slow
/// batch is always cleared by `purge_expired_batches` first; only requests the
/// server never answered with a batch (FAIL/error numeric/silent drop) reach
/// this fallback.
const CHATHISTORY_REQUEST_TIMEOUT_SECS: u64 = 90;

impl App {
    /// Tick the netsplit state and emit batched netsplit/netjoin messages.
    pub(crate) fn handle_netsplit_tick(&mut self) {
        let messages = self.state.netsplit_state.tick();
        for msg in messages {
            for buffer_id in &msg.buffer_ids {
                let id = self.state.next_message_id();
                self.state.add_message(
                    buffer_id,
                    Message {
                        id,
                        timestamp: Utc::now(),
                        message_type: MessageType::Event,
                        nick: None,
                        nick_mode: None,
                        text: msg.text.clone(),
                        highlight: false,
                        event_key: Some("netsplit".to_string()),
                        event_params: None,
                        log_msg_id: None,
                        log_ref_id: None,
                        tags: None,
                    },
                );
            }
        }
    }

    /// Process any batches that have been open too long (e.g. dropped `-BATCH`).
    ///
    /// Expired batches are passed through `process_completed_batch` so their
    /// buffered JOIN/PART/QUIT/NICK messages still mutate `Buffer.users`. If
    /// we silently dropped them, channels would carry stale nicks for users
    /// who quit inside a netsplit batch that never closed.
    pub(crate) fn purge_expired_batches(&mut self) {
        // (conn_id, ref_tag, batch). The ref tag lets us match a nested
        // `draft/multiline` child to its parent batch within this sweep.
        let mut to_replay: Vec<(String, String, crate::irc::batch::BatchInfo)> = Vec::new();
        for (conn_id, tracker) in &mut self.batch_trackers {
            for (ref_tag, batch) in tracker.purge_expired() {
                to_replay.push((conn_id.clone(), ref_tag, batch));
            }
        }

        // Fold expired nested `draft/multiline` children into their parent batch
        // BEFORE processing parents. A nested multiline (its opener carried
        // `@batch=<parent>`, e.g. a multiline message inside a CHATHISTORY batch)
        // must never be dispatched live as a backlog row. The child is YOUNGER
        // than its parent, so if the child expired its parent did too — they
        // purge together in THIS sweep — so fold the child's reassembled
        // (truncated) synthetic into the parent's messages here, where it will be
        // ingested store-only. If the parent isn't in this sweep (already
        // completed earlier), drop the orphan rather than showing it live.
        let mut nested_children: Vec<usize> = Vec::new();
        for i in 0..to_replay.len() {
            if to_replay[i].2.batch_type != "DRAFT/MULTILINE" {
                continue;
            }
            let Some(parent_ref) = to_replay[i].2.opener_tags.as_ref().and_then(|tags| {
                tags.iter().find(|t| t.0 == "batch").and_then(|t| t.1.clone())
            }) else {
                continue; // top-level multiline → processed normally below
            };
            nested_children.push(i);
            let conn_id = to_replay[i].0.clone();
            let synthetic = match crate::irc::batch::build_multiline_message(
                &self.state,
                &conn_id,
                &to_replay[i].2,
                false,
            ) {
                crate::irc::batch::MultilineOutcome::Message(m) => Some(*m),
                // Malformed (E2E/mixed) or empty nested batch on timeout: drop it
                // (live per-fragment replay of backlog rows would be wrong).
                _ => None,
            };
            if let Some(synthetic) = synthetic {
                if let Some(parent) = to_replay
                    .iter_mut()
                    .find(|(c, r, _)| *c == conn_id && *r == parent_ref)
                {
                    // @time-sorted ingest places it correctly among history rows.
                    parent.2.messages.push(synthetic);
                } else {
                    tracing::warn!(
                        "dropping timed-out nested multiline batch (parent '{parent_ref}' already completed)"
                    );
                }
            }
        }
        // Remove the folded/dropped children (highest index first to stay valid).
        nested_children.sort_unstable();
        for i in nested_children.into_iter().rev() {
            to_replay.remove(i);
        }

        for (conn_id, _ref, batch) in to_replay {
            // Force-completed by timeout (`clean_end = false`): a missing
            // `BATCH -tag` is a transport/server failure, so a short CHATHISTORY
            // batch here must not be treated as genuine end-of-history.
            crate::irc::batch::process_completed_batch(&mut self.state, &conn_id, &batch, false);
        }
    }

    /// Release CHATHISTORY requests stuck in-flight past the request timeout.
    ///
    /// The usual clear path is batch completion (normal `BATCH -tag` or the
    /// batch purge above). A request the server rejects (`FAIL`/error numeric)
    /// or drops without ever opening a batch never reaches it, so without this
    /// sweep `should_request` would suppress all future history for that target
    /// until reconnect. Clearing the marker is treated as a failure, not
    /// end-of-history, so it never marks `BEFORE` exhausted.
    pub(crate) fn purge_stale_chathistory_requests(&mut self) {
        let timeout = Duration::from_secs(CHATHISTORY_REQUEST_TIMEOUT_SECS);
        for (conn_id, conn) in &mut self.state.connections {
            let cleared = conn.chathistory.clear_stale(timeout);
            if !cleared.is_empty() {
                tracing::warn!(
                    %conn_id,
                    targets = ?cleared,
                    "CHATHISTORY request(s) timed out with no batch — releasing in-flight lock"
                );
            }
        }
    }

    /// Run periodic event-message pruning if enough time has elapsed (1 hour).
    pub(crate) fn maybe_purge_old_events(&mut self) {
        let hours = self.config.logging.event_retention_hours;
        if hours == 0 {
            return;
        }
        if self.last_event_purge.elapsed() < Duration::from_hours(1) {
            return;
        }
        self.last_event_purge = Instant::now();

        let Some(storage) = &self.storage else {
            return;
        };
        let db = Arc::clone(&storage.db);
        let encrypt = storage.encrypt;
        tokio::task::spawn_blocking(move || {
            let Ok(conn) = db.lock() else { return };
            let has_fts = !encrypt;
            let removed = crate::storage::db::purge_old_events(&conn, hours, has_fts);
            if removed > 0 {
                tracing::info!(
                    "periodic purge: removed {removed} event messages older than {hours}h"
                );
            }
        });
    }

    /// Purge mentions older than 7 days from DB and in-memory buffer.
    pub(crate) fn maybe_purge_old_mentions(&mut self) {
        if self.last_mention_purge.elapsed() < Duration::from_hours(1) {
            return;
        }
        self.last_mention_purge = Instant::now();

        let seven_days_ago = Utc::now().timestamp() - 7 * 24 * 3600;

        if let Some(storage) = &self.storage {
            let db = Arc::clone(&storage.db);
            tokio::task::spawn_blocking(move || {
                let Ok(conn) = db.lock() else { return };
                if let Ok(removed) =
                    crate::storage::query::purge_old_mentions(&conn, seven_days_ago)
                    && removed > 0
                {
                    tracing::info!("periodic purge: removed {removed} mentions older than 7 days");
                }
            });
        }

        if let Some(buf) = self.state.buffers.get_mut(Self::MENTIONS_BUFFER_ID) {
            let cutoff =
                chrono::DateTime::from_timestamp(seven_days_ago, 0).unwrap_or_else(Utc::now);
            let before = buf.messages.len();
            buf.messages.retain(|m| m.timestamp >= cutoff);
            while buf.messages.len() > 1000 {
                buf.messages.pop_front();
            }
            if buf.messages.len() < before {
                buf.messages.shrink_to(buf.messages.len());
            }
        }
    }

    /// Check if the local date has changed (midnight) and insert a
    /// "Day changed" marker in all chat buffers — like irssi/weechat.
    pub(crate) fn check_day_changed(&mut self) {
        let today = Local::now().date_naive();
        if today == self.last_day {
            return;
        }
        self.last_day = today;

        let separator_text = super::backlog::format_date_separator(today);
        let buffer_ids: Vec<String> = self
            .state
            .buffers
            .iter()
            .filter(|(_, buf)| {
                matches!(
                    buf.buffer_type,
                    BufferType::Channel
                        | BufferType::Query
                        | BufferType::DccChat
                        | BufferType::Server
                )
            })
            .map(|(id, _)| id.clone())
            .collect();

        for buf_id in buffer_ids {
            let id = self.state.next_message_id();
            let event_param = separator_text.clone();
            self.state.add_local_message(
                &buf_id,
                Message {
                    id,
                    timestamp: Utc::now(),
                    message_type: MessageType::Event,
                    nick: None,
                    nick_mode: None,
                    text: separator_text.clone(),
                    highlight: false,
                    event_key: Some("date_separator".to_string()),
                    event_params: Some(vec![event_param]),
                    log_msg_id: None,
                    log_ref_id: None,
                    tags: None,
                },
            );
        }
    }

    /// Send IRC PING every 30s per connection to measure lag.
    pub(crate) fn measure_lag(&mut self) {
        let now = Instant::now();
        let conn_ids: Vec<String> = self.irc_handles.keys().cloned().collect();
        for conn_id in conn_ids {
            let is_connected =
                self.state.connections.get(&conn_id).is_some_and(|c| {
                    c.status == crate::state::connection::ConnectionStatus::Connected
                });
            if !is_connected {
                continue;
            }

            // Check for lag timeout (no PONG for 5 minutes)
            if let Some(sent_at) = self.lag_pings.get(&conn_id) {
                let pending = self
                    .state
                    .connections
                    .get(&conn_id)
                    .is_some_and(|c| c.lag_pending);
                if pending && sent_at.elapsed().as_secs() >= 300 {
                    let buf_id = self.state.connections.get(&conn_id).map_or_else(
                        || conn_id.clone(),
                        |c| crate::state::buffer::make_buffer_id(&conn_id, &c.label),
                    );
                    let msg_id = self.state.next_message_id();
                    self.state.add_message(
                        &buf_id,
                        crate::state::buffer::Message {
                            id: msg_id,
                            timestamp: chrono::Utc::now(),
                            message_type: crate::state::buffer::MessageType::Event,
                            nick: None,
                            nick_mode: None,
                            text: format!(
                                "Connection to {conn_id} timed out (no PONG for 5 minutes)"
                            ),
                            highlight: false,
                            tags: None,
                            log_msg_id: None,
                            log_ref_id: None,
                            event_key: None,
                            event_params: Some(Vec::new()),
                        },
                    );
                    if let Some(handle) = self.irc_handles.get(&conn_id) {
                        let _ = handle.sender.send(::irc::proto::Command::QUIT(Some(
                            "Ping timeout".to_string(),
                        )));
                    }
                    continue;
                }
            }

            let should_ping = self
                .lag_pings
                .get(&conn_id)
                .is_none_or(|last| now.duration_since(*last).as_secs() >= 30);

            if should_ping {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .to_string();
                if let Some(handle) = self.irc_handles.get(&conn_id) {
                    let _ = handle
                        .sender
                        .send(::irc::proto::Command::Raw("PING".to_string(), vec![ts]));
                }
                self.lag_pings.insert(conn_id.clone(), now);
                if let Some(conn) = self.state.connections.get_mut(&conn_id) {
                    conn.lag_pending = true;
                }
            }
        }
    }
}

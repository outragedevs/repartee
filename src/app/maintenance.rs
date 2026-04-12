use std::sync::Arc;
use std::time::Instant;

use chrono::{Local, Utc};
use tokio::time::Duration;

use crate::state::buffer::{BufferType, Message, MessageType};

use super::App;

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

    /// Discard any batches that have been open too long (e.g. dropped `-BATCH`).
    pub(crate) fn purge_expired_batches(&mut self) {
        for tracker in self.batch_trackers.values_mut() {
            tracker.purge_expired();
        }
    }

    /// Run periodic event-message pruning if enough time has elapsed (1 hour).
    pub(crate) fn maybe_purge_old_events(&mut self) {
        let hours = self.config.logging.event_retention_hours;
        if hours == 0 {
            return;
        }
        if self.last_event_purge.elapsed() < Duration::from_secs(3600) {
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
        if self.last_mention_purge.elapsed() < Duration::from_secs(3600) {
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
                    event_params: None,
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

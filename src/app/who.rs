use std::collections::HashSet;
use std::time::Instant;

use tokio::time::Duration;

use super::App;

impl App {
    /// Queue a channel for batched auto-WHO + auto-MODE after joining.
    pub(crate) fn queue_channel_query(&mut self, conn_id: &str, channel: String) {
        tracing::trace!(conn_id, %channel, "queue_channel_query");
        self.channel_query_queues
            .entry(conn_id.to_string())
            .or_default()
            .push_back(channel);

        if !self.channel_query_in_flight.contains_key(conn_id) {
            self.send_channel_query_batch(conn_id);
        }
    }

    /// Send the next batch of WHO + MODE queries for a connection.
    pub(crate) fn send_channel_query_batch(&mut self, conn_id: &str) {
        /// Max channels per WHO command. `IRCnet` ircd 2.12 silently drops
        /// targets beyond ~11 in comma-separated WHO. Use 5 for safety.
        const MAX_WHO_TARGETS: usize = 5;

        let queue = match self.channel_query_queues.get_mut(conn_id) {
            Some(q) if !q.is_empty() => q,
            _ => {
                self.channel_query_in_flight.remove(conn_id);
                self.channel_query_sent_at.remove(conn_id);
                return;
            }
        };

        let has_whox = self
            .state
            .connections
            .get(conn_id)
            .is_some_and(|c| c.isupport_parsed.has_whox());

        // WHO overhead: "WHO " (4) + " %tcuihnfar,NNN" (~16 for WHOX) + "\r\n" (2)
        let who_overhead = if has_whox { 22 } else { 6 };
        let who_budget = 512 - who_overhead;

        // MODE overhead: "MODE " (5) + "\r\n" (2)
        let mode_budget = 512 - 7;

        // Use the smaller budget so both commands fit their channels.
        let budget = who_budget.min(mode_budget);

        let mut batch = Vec::new();
        let mut len = 0;

        while let Some(ch) = queue.front() {
            if batch.len() >= MAX_WHO_TARGETS {
                break;
            }
            let add = if batch.is_empty() {
                ch.len()
            } else {
                1 + ch.len() // comma + channel name
            };
            if len + add > budget && !batch.is_empty() {
                break;
            }
            len += add;
            batch.push(queue.pop_front().expect("front() was Some"));
        }

        if batch.is_empty() {
            self.channel_query_in_flight.remove(conn_id);
            return;
        }

        let Some(handle) = self.irc_handles.get(conn_id) else {
            self.channel_query_in_flight.remove(conn_id);
            return;
        };

        // Track in-flight channels for RPL_ENDOFWHO completion.
        let batch_set: HashSet<String> = batch.iter().cloned().collect();
        self.channel_query_in_flight
            .insert(conn_id.to_string(), batch_set);
        self.channel_query_sent_at
            .insert(conn_id.to_string(), Instant::now());

        // Mark all batch channels as silent (no display for auto-WHO replies).
        if let Some(conn) = self.state.connections.get_mut(conn_id) {
            for ch in &batch {
                conn.silent_who_channels.insert(ch.clone());
            }
        }

        // Send batched WHO (single command, comma-separated channels).
        let chanlist = batch.join(",");
        tracing::trace!(conn_id, %chanlist, has_whox, "send_channel_query_batch: sending WHO+MODE");
        if has_whox {
            let token = crate::irc::events::next_who_token(&mut self.state, conn_id);
            let fields = format!("{},{token}", crate::constants::WHOX_FIELDS);
            tracing::trace!(conn_id, %chanlist, %fields, "WHOX command");
            let _ = handle.sender.send(::irc::proto::Command::Raw(
                "WHO".to_string(),
                vec![chanlist.clone(), fields],
            ));
        } else {
            tracing::trace!(conn_id, %chanlist, "standard WHO (no WHOX)");
            let _ = handle
                .sender
                .send(::irc::proto::Command::WHO(Some(chanlist.clone()), None));
        }

        // Send MODE query for channel modes.
        let multi_mode = self
            .state
            .connections
            .get(conn_id)
            .is_some_and(|c| c.isupport_parsed.supports_multi_target_mode());
        if multi_mode {
            let _ = handle.sender.send(::irc::proto::Command::Raw(
                "MODE".to_string(),
                vec![chanlist],
            ));
        } else {
            for ch in &batch {
                let _ = handle.sender.send(::irc::proto::Command::Raw(
                    "MODE".to_string(),
                    vec![ch.clone()],
                ));
            }
        }
    }

    /// Handle `RPL_ENDOFWHO` for batch tracking.
    pub(crate) fn handle_who_batch_complete(&mut self, conn_id: &str, target: &str) {
        tracing::trace!(conn_id, %target, "handle_who_batch_complete");
        let Some(in_flight) = self.channel_query_in_flight.get_mut(conn_id) else {
            tracing::trace!(conn_id, "no in-flight batch for this connection");
            return;
        };

        in_flight.remove(target);

        if target.contains(',') {
            for ch in target.split(',') {
                in_flight.remove(ch);
            }
        }

        tracing::trace!(
            conn_id,
            remaining = in_flight.len(),
            "in-flight after removal"
        );

        if in_flight.is_empty() {
            let remaining_queued = self
                .channel_query_queues
                .get(conn_id)
                .map_or(0, std::collections::VecDeque::len);
            tracing::trace!(conn_id, remaining_queued, "batch complete, sending next");
            let conn_id = conn_id.to_string();
            self.channel_query_in_flight.remove(&conn_id);
            self.send_channel_query_batch(&conn_id);
        }
    }

    /// Detect stale WHO batches where the server silently dropped some targets.
    pub(crate) fn check_stale_who_batches(&mut self) {
        let stale_conns: Vec<String> = self
            .channel_query_sent_at
            .iter()
            .filter(|(_, sent_at)| sent_at.elapsed() > Duration::from_secs(30))
            .map(|(conn_id, _)| conn_id.clone())
            .collect();

        for conn_id in stale_conns {
            if let Some(stale) = self.channel_query_in_flight.remove(&conn_id) {
                tracing::warn!(
                    %conn_id,
                    stale_channels = ?stale,
                    "WHO batch timed out — server likely dropped targets, moving on"
                );
                if let Some(conn) = self.state.connections.get_mut(&conn_id) {
                    for ch in &stale {
                        conn.silent_who_channels.remove(ch.as_str());
                    }
                }
            }
            self.channel_query_sent_at.remove(&conn_id);
            self.send_channel_query_batch(&conn_id);
        }
    }
}

use chrono::Utc;

use crate::state::buffer::{ActivityLevel, Buffer, BufferType, Message, MessageType};

use super::App;

impl App {
    pub(crate) fn mention_uses_server_history(network: &str, buffer: &str) -> bool {
        crate::config::network_scope::is_bouncer_scope(network) && !buffer.starts_with('=')
    }

    pub(crate) fn mention_target(&self, network: &str) -> Option<(String, String)> {
        if let Some(connection) = self.state.connections.values().find(|connection| {
            connection
                .network_scope
                .as_deref()
                .map_or(connection.id == network, |scope| scope == network)
        }) {
            return Some((connection.id.clone(), connection.label.clone()));
        }
        if let Some((id, server)) = self.config.servers.iter().find(|(id, server)| {
            if server.bouncer_control || server.bouncer_network_id.is_some() {
                crate::config::network_scope::network_scope(id, server, &self.config.general.username) == network
            } else {
                id.as_str() == network
            }
        }) {
            if self.state.connections.contains_key(id) {
                return None;
            }
            return Some((id.clone(), server.label.clone()));
        }
        if crate::config::network_scope::is_bouncer_scope(network)
            || self
                .state
                .connections
                .get(network)
                .is_some_and(|connection| connection.network_scope.is_some())
            || self
                .config
                .servers
                .get(network)
                .is_some_and(|server| server.bouncer_control || server.bouncer_network_id.is_some())
        {
            return None;
        }
        Some((network.to_string(), network.to_string()))
    }

    /// Buffer ID for the mentions aggregation buffer.
    pub const MENTIONS_BUFFER_ID: &'static str = "_mentions";

    /// Create the mentions buffer if it doesn't already exist.
    pub(crate) fn create_mentions_buffer(&mut self) {
        if self.state.buffers.contains_key(Self::MENTIONS_BUFFER_ID) {
            return;
        }
        let buf = Buffer {
            id: Self::MENTIONS_BUFFER_ID.to_string(),
            connection_id: String::new(),
            buffer_type: BufferType::Mentions,
            name: "Mentions".to_string(),
            messages: std::collections::VecDeque::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: Utc::now(),
            topic: None,
            topic_set_by: None,
            users: std::collections::HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: std::collections::HashMap::new(),
            last_speakers: Vec::new(),
            peer_handle: None,
            log_total_lines: None,
            log_oldest_ts: None,
            log_newest_ts: None,
            history_exhausted: false,
            log_initial_loaded: false,
            pin_backlog: false,
            metadata: crate::irc::metadata::Flags::default(),
        };
        self.state
            .buffers
            .insert(Self::MENTIONS_BUFFER_ID.to_string(), buf);
        self.load_mentions_history();
    }

    /// Load recent mentions from DB into the mentions buffer (7 days, max 1000).
    pub(crate) fn load_mentions_history(&mut self) {
        let seven_days_ago = chrono::Utc::now().timestamp() - 7 * 24 * 3600;
        let mut rows = self.storage.as_ref().and_then(|storage| {
            let db = storage.db.lock().ok()?;
            crate::storage::query::load_recent_mentions(&db, seven_days_ago, 1000).ok()
        }).unwrap_or_default();
        rows.retain(|row| !Self::mention_uses_server_history(&row.network, &row.buffer));
        rows.extend(self.volatile_mentions.iter().filter(|(_, mention, _)| mention.timestamp >= seven_days_ago)
            .map(|(network, mention, _)| crate::storage::types::MentionRow {
                id: mention.id,
                timestamp: mention.timestamp,
                network: network.clone(),
                buffer: crate::web::snapshot::split_buffer_id(&mention.buffer_id).1.to_string(),
                channel: mention.channel.clone(),
                nick: mention.nick.clone(),
                text: mention.text.clone(),
            }));
        rows.sort_by_key(|row| row.timestamp);
        if rows.len() > 1000 {
            rows.drain(..rows.len() - 1000);
        }
        let identities: std::collections::HashMap<_, _> = self.volatile_mentions.iter()
            .filter_map(|(_, mention, identity)| identity.as_ref().map(|identity| (mention.id, identity.clone())))
            .collect();
        let origins: std::collections::HashMap<_, _> = rows.iter().filter_map(|row| {
            self.mention_target(&row.network).map(|(id, _)| (row.id, (id, row.channel.clone(), row.nick.clone())))
        }).collect();
        for row in &mut rows {
            row.network = self.mention_target(&row.network).map_or_else(
                || "Previous bouncer network".to_string(),
                |(_, label)| label,
            );
        }
        // Pre-allocate message IDs before borrowing buffers mutably.
        let base_id = self.state.message_counter + 1;
        self.state.message_counter += rows.len() as u64;
        let mut messages = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            let mut message = Self::mention_row_to_message(
                row,
                base_id + i as u64,
                self.config.display.nick_color_saturation,
                self.config.display.nick_color_lightness,
            );
            if let Some(identity) = identities.get(&row.id) {
                message.redaction_ref = Some(identity.clone());
                if let Ok(id) = serde_json::to_string(&identity.key) {
                    message.tags = Some(std::collections::HashMap::from([("msgid".into(), id)]));
                }
            }
            if let Some((id, target, source)) = origins.get(&row.id) {
                self.state.mark_metadata_origin(id, target, source, &mut message);
            }
            if self.state.metadata_prepare_row(Self::MENTIONS_BUFFER_ID, &mut message).is_some() {
                messages.push(message);
            }
        }
        if let Some(buf) = self.state.buffers.get_mut(Self::MENTIONS_BUFFER_ID) {
            buf.messages.extend(messages);
        }
    }

    /// Convert a `MentionRow` to a `Message` for the mentions buffer.
    pub(crate) fn mention_row_to_message(
        row: &crate::storage::types::MentionRow,
        id: u64,
        nick_sat: f32,
        nick_lit: f32,
    ) -> Message {
        let ts =
            chrono::DateTime::from_timestamp(row.timestamp, 0).unwrap_or_else(chrono::Utc::now);
        let datetime = ts
            .with_timezone(&chrono::Local)
            .format("%Y/%m/%d %H:%M:%S")
            .to_string();
        let text = crate::state::mention_format::format_mention_line(
            &datetime,
            &row.network,
            &row.channel,
            &row.nick,
            &row.text,
            nick_sat,
            nick_lit,
        );
        Message {
            redaction_ref: None,
            redaction_msgid: None,
            log_key: None,
            id,
            timestamp: ts,
            message_type: MessageType::MentionLog,
            nick: None,
            nick_mode: None,
            text,
            highlight: true,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
            wire_origin: None,
            translation_suffix_at: None,
        }
    }
}

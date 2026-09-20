use crate::irc::isupport::casefold;
use crate::irc::metadata::{Flags, Key};
use super::AppState;

impl AppState {
    pub fn metadata_flags(&self, conn_id: &str, target: &str) -> Flags {
        let Some(conn) = self.connections.get(conn_id).filter(|conn| conn.origin_config.bouncer_network_id.is_some() && !conn.origin_config.bouncer_control) else { return Flags::default(); };
        if self.metadata_casemappings.get(conn.network_key()).is_some_and(|mapping| mapping != conn.isupport_parsed.casemapping()) { return Flags::default(); }
        self.bouncer_metadata.get(&(conn.network_key().to_string(), casefold(target, conn.isupport_parsed.casemapping()))).copied().unwrap_or_default()
    }

    pub(crate) fn reconcile_metadata_mapping(&mut self, scope: &str, mapping: &str) {
        if self.metadata_casemappings.insert(scope.to_string(), mapping.to_string()).is_some_and(|old| old != mapping) {
            self.bouncer_metadata.retain(|(network, _), _| network != scope);
        }
    }

    pub(crate) fn refresh_metadata_buffers(&mut self, conn_id: &str) {
        let updates: Vec<_> = self.buffers.values().filter(|buffer| buffer.connection_id == conn_id
            && matches!(buffer.buffer_type, super::buffer::BufferType::Channel | super::buffer::BufferType::Query))
            .map(|buffer| (buffer.id.clone(), self.metadata_flags(conn_id, &buffer.name))).collect();
        for (id, flags) in updates {
            let buffer = self.buffers.get_mut(&id).unwrap();
            if buffer.metadata == flags { continue; }
            buffer.metadata = flags;
            self.pending_web_events.push(crate::web::protocol::WebEvent::BufferMetadataChanged {
                buffer_id: id, pinned: flags.pinned, muted: flags.muted, blocked: flags.blocked,
            });
        }
    }

    pub(crate) fn reconcile_metadata_key(&mut self, conn_id: &str, key: Key, seen: &std::collections::HashSet<String>) {
        let Some(conn) = self.connections.get(conn_id) else { return; };
        let scope = conn.network_key().to_string();
        for ((network, target), flags) in &mut self.bouncer_metadata {
            if *network == scope && !seen.contains(target) { flags.set(key, false); }
        }
        self.refresh_metadata_buffers(conn_id);
    }

    pub(crate) fn set_metadata(&mut self, conn_id: &str, target: &str, key: Key, value: bool) {
        let Some(conn) = self.connections.get(conn_id).filter(|conn| conn.origin_config.bouncer_network_id.is_some() && !conn.origin_config.bouncer_control) else { return; };
        let mapping = conn.isupport_parsed.casemapping();
        let target = casefold(target, mapping);
        let flags = self.bouncer_metadata.entry((conn.network_key().to_string(), target.clone())).or_default();
        flags.set(key, value);
        let flags = *flags;
        for buffer in self.buffers.values_mut().filter(|buffer| buffer.connection_id == conn_id && matches!(buffer.buffer_type, super::buffer::BufferType::Channel | super::buffer::BufferType::Query) && casefold(&buffer.name, mapping) == target) {
            if buffer.metadata == flags { continue; }
            buffer.metadata = flags;
            self.pending_web_events.push(crate::web::protocol::WebEvent::BufferMetadataChanged {
                buffer_id: buffer.id.clone(), pinned: flags.pinned, muted: flags.muted, blocked: flags.blocked,
            });
        }
    }
}

static SCOPE_TAG: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| format!("{}/metadata-scope", crate::constants::APP_NAME));
static TARGET_TAG: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| format!("{}/metadata-target", crate::constants::APP_NAME));
static SOURCE_TAG: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| format!("{}/metadata-source", crate::constants::APP_NAME));

impl AppState {
    pub(crate) fn metadata_policy(&self, conn_id: &str, target: &str, source: Option<&str>) -> Flags {
        let target_flags = self.metadata_flags(conn_id, target);
        let source_flags = source.map_or_else(Flags::default, |source| self.metadata_flags(conn_id, source));
        Flags { pinned: target_flags.pinned, muted: target_flags.muted || source_flags.muted, blocked: target_flags.blocked || source_flags.blocked }
    }

    pub(crate) fn metadata_row_policy(&self, buffer_id: &str, message: &super::buffer::Message) -> Flags {
        use super::buffer::{BufferType, MessageType};
        if message.message_type == MessageType::MentionLog || (message.message_type == MessageType::Event && message.event_key.as_deref() == Some("invite")) {
            let Some(tags) = &message.tags else { return Flags::default(); };
            let Some(scope) = tags.get(SCOPE_TAG.as_str()) else { return Flags::default(); };
            let Some((id, _)) = self.connections.iter().find(|(_, conn)| conn.network_key() == scope) else { return Flags::default(); };
            return self.metadata_policy(id, tags.get(TARGET_TAG.as_str()).map_or("", String::as_str), tags.get(SOURCE_TAG.as_str()).map(String::as_str));
        }
        if !matches!(message.message_type, MessageType::Message | MessageType::Action | MessageType::Notice) { return Flags::default(); }
        let Some((conn_id, fallback)) = buffer_id.split_once('/') else { return Flags::default(); };
        let target = self.buffers.get(buffer_id).map_or(fallback, |buffer| buffer.name.as_str());
        let target = if self.buffers.get(buffer_id).is_some_and(|buffer| buffer.buffer_type == BufferType::Special && buffer.name == "*search*") {
            message.tags.as_ref().and_then(|tags| tags.get(super::buffer::SEARCH_TARGET_TAG.as_str())).map_or(target, String::as_str)
        } else { target };
        self.metadata_policy(conn_id, target, message.nick.as_deref())
    }

    pub(crate) fn metadata_prepare_row(&self, buffer_id: &str, message: &mut super::buffer::Message) -> Option<super::buffer::ActivityLevel> {
        let flags = self.metadata_row_policy(buffer_id, message);
        if flags.blocked { return None; }
        if flags.muted { message.highlight = false; }
        Some(if flags.muted { super::buffer::ActivityLevel::Activity } else { super::buffer::ActivityLevel::Mention })
    }

    pub(crate) fn mark_metadata_origin(&self, conn_id: &str, target: &str, source: &str, message: &mut super::buffer::Message) {
        let Some(conn) = self.connections.get(conn_id).filter(|conn| conn.server_owns_history()) else { return; };
        let tags = message.tags.get_or_insert_with(std::collections::HashMap::new);
        tags.insert(SCOPE_TAG.clone(), conn.network_key().to_string());
        tags.insert(TARGET_TAG.clone(), target.to_string());
        tags.insert(SOURCE_TAG.clone(), source.to_string());
    }

    pub(crate) fn purge_metadata_blocked_rows(&mut self) {
        for (buffer_id, nicks) in self.typing.snapshot() {
            let Some(buffer) = self.buffers.get(&buffer_id) else { continue; };
            let blocked: Vec<_> = nicks.into_iter().filter(|nick| self.metadata_policy(&buffer.connection_id, &buffer.name, Some(nick)).blocked).collect();
            let mut changed = false;
            for nick in blocked { changed |= self.typing.clear(&buffer_id, &nick); }
            if changed { crate::irc::events::push_typing_web_event(self, &buffer_id); }
        }
        let removals: Vec<_> = self.buffers.iter().filter_map(|(id, buffer)| {
            let ids: std::collections::HashSet<_> = buffer.messages.iter().filter(|message| self.metadata_row_policy(id, message).blocked).map(|message| message.id).collect();
            (!ids.is_empty()).then(|| (id.clone(), ids))
        }).collect();
        for (buffer_id, ids) in removals {
            if let Some(buffer) = self.buffers.get_mut(&buffer_id) {
                if !self.read_activity.contains_key(&buffer_id) {
                    let removed_unread = buffer.messages.iter().rev().take(buffer.unread_count as usize).filter(|message| ids.contains(&message.id)).count();
                    buffer.unread_count = buffer.unread_count.saturating_sub(u32::try_from(removed_unread).unwrap_or(u32::MAX));
                    if buffer.unread_count == 0 { buffer.activity = super::buffer::ActivityLevel::None; }
                    self.pending_web_events.push(crate::web::protocol::WebEvent::ActivityChanged {
                        buffer_id: buffer_id.clone(), activity: buffer.activity as u8, unread_count: buffer.unread_count,
                    });
                }
                buffer.messages.retain(|message| !ids.contains(&message.id));
            }
            self.pending_web_events.retain(|event| !matches!(event,
                crate::web::protocol::WebEvent::NewMessage { buffer_id: id, message }
                | crate::web::protocol::WebEvent::MentionAlert { buffer_id: id, message }
                | crate::web::protocol::WebEvent::InsertMessage { buffer_id: id, message, .. }
                if *id == buffer_id && ids.contains(&message.id)));
            if let Some(activity) = self.read_activity.get_mut(&buffer_id) {
                activity.unread.retain(|id, _| !ids.contains(id));
                activity.origins.retain(|id, _| !ids.contains(id));
                self.refresh_read_activity(&buffer_id);
            }
            self.pending_web_events.push(crate::web::protocol::WebEvent::DeleteMessages { buffer_id, message_ids: ids.into_iter().collect() });
        }
    }
}

use std::collections::HashMap;

use super::AppState;
use super::buffer::{ActivityLevel, Message};

#[derive(Default)]
pub(super) struct ReadActivity {
    pub(super) through: Option<i64>,
    pub(super) unread: HashMap<u64, (i64, ActivityLevel)>,
}

impl AppState {
    pub(crate) fn reset_connection_read_markers(&mut self, conn_id: &str) {
        for (buffer_id, state) in &mut self.read_activity {
            if buffer_id
                .split_once('/')
                .is_some_and(|(id, _)| id == conn_id)
            {
                state.through = None;
            }
        }
    }

    pub(crate) fn uses_read_markers(&self, buffer_id: &str) -> bool {
        self.buffer_uses_server_history(buffer_id)
            && buffer_id
                .split_once('/')
                .and_then(|(id, _)| self.connections.get(id))
                .is_some_and(|conn| {
                    !conn.origin_config.bouncer_control
                        && (conn.enabled_caps.contains("draft/read-marker")
                            || conn.enabled_caps.contains("soju.im/read"))
                })
    }

    pub(super) fn message_already_read(&self, buffer_id: &str, message: &Message) -> bool {
        self.read_activity
            .get(buffer_id)
            .and_then(|state| state.through)
            .is_some_and(|through| message.timestamp.timestamp_millis() <= through)
    }

    pub(super) fn record_read_activity(
        &mut self,
        buffer_id: &str,
        message: &Message,
        level: ActivityLevel,
    ) {
        if level == ActivityLevel::None || self.message_already_read(buffer_id, message) {
            return;
        }
        self.read_activity
            .entry(buffer_id.to_string())
            .or_default()
            .unread
            .insert(message.id, (message.timestamp.timestamp_millis(), level));
    }

    pub(crate) fn activate_connection_read_markers(&mut self, conn_id: &str) {
        let buffers: Vec<_> = self
            .buffers
            .values()
            .filter(|buffer| buffer.connection_id == conn_id && self.uses_read_markers(&buffer.id))
            .map(|buffer| buffer.id.clone())
            .collect();
        for id in buffers {
            self.refresh_read_activity(&id);
        }
    }

    pub(super) fn prune_read_activity(&mut self, buffer_id: &str) {
        let Some(buffer) = self.buffers.get(buffer_id) else {
            return;
        };
        let Some(state) = self.read_activity.get_mut(buffer_id) else {
            return;
        };
        let ids: std::collections::HashSet<_> =
            buffer.messages.iter().map(|message| message.id).collect();
        state.unread.retain(|id, _| ids.contains(id));
    }

    pub(super) fn refresh_read_activity(&mut self, buffer_id: &str) {
        self.prune_read_activity(buffer_id);
        let Some(buffer) = self.buffers.get_mut(buffer_id) else {
            return;
        };
        let state = self.read_activity.entry(buffer_id.to_string()).or_default();
        buffer.unread_count = u32::try_from(state.unread.len()).unwrap_or(u32::MAX);
        buffer.activity = state
            .unread
            .values()
            .map(|(_, level)| *level)
            .max()
            .unwrap_or(ActivityLevel::None);
        if buffer.activity == ActivityLevel::None {
            self.activity_order.remove(buffer_id);
        }
        self.pending_web_events
            .push(crate::web::protocol::WebEvent::ActivityChanged {
                buffer_id: buffer_id.to_string(),
                activity: buffer.activity as u8,
                unread_count: buffer.unread_count,
            });
    }

    pub(crate) fn apply_server_read_marker(&mut self, buffer_id: &str, millis: i64) {
        let state = self.read_activity.entry(buffer_id.to_string()).or_default();
        if state.through.is_some_and(|previous| previous >= millis) {
            return;
        }
        state.through = Some(millis);
        state.unread.retain(|_, (time, _)| *time > millis);
        if let Some(buffer) = self.buffers.get_mut(buffer_id)
            && let Some(timestamp) = chrono::DateTime::from_timestamp_millis(millis)
        {
            buffer.last_read = timestamp;
        }
        self.refresh_read_activity(buffer_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::buffer::{Buffer, BufferType};

    fn state() -> AppState {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let config = toml::from_str("label='Bouncer'\naddress='bnc.example.org'\nport=6697\ntls=true\nchannels=[]\nbouncer_network_id='42'").unwrap();
        app.setup_connection("account", &config);
        app.state
            .connections
            .get_mut("account")
            .unwrap()
            .enabled_caps
            .insert("draft/read-marker".into());
        app.state
            .add_buffer_with_focus(Buffer::empty("account", BufferType::Query, "Peer"), false);
        app.state
    }

    fn deliver(state: &mut AppState, time: i64, level: ActivityLevel) {
        let mut message = crate::state::events::tests::make_test_message(state, "unread");
        message.timestamp = chrono::DateTime::from_timestamp_millis(time).unwrap();
        message.highlight = level == ActivityLevel::Mention;
        state.add_transient_message_with_activity("account/peer", message, level);
    }

    #[tokio::test]
    async fn runtime_marker_activation_preserves_preexisting_unread_activity() {
        let mut state = state();
        state
            .connections
            .get_mut("account")
            .unwrap()
            .enabled_caps
            .clear();
        deliver(&mut state, 1000, ActivityLevel::Mention);
        deliver(&mut state, 2000, ActivityLevel::Activity);
        deliver(&mut state, 3000, ActivityLevel::Activity);
        crate::irc::events::handle_cap_ack(&mut state, "account", Some("draft/read-marker"), None);
        assert_eq!(state.buffers["account/peer"].unread_count, 3);
        assert_eq!(
            state.buffers["account/peer"].activity,
            ActivityLevel::Mention
        );
        state.apply_server_read_marker("account/peer", 1000);
        assert_eq!(state.buffers["account/peer"].unread_count, 2);
        assert_eq!(
            state.buffers["account/peer"].activity,
            ActivityLevel::Activity
        );
    }

    #[tokio::test]
    async fn runtime_marker_activation_does_not_restore_locally_cleared_messages() {
        let mut state = state();
        state
            .connections
            .get_mut("account")
            .unwrap()
            .enabled_caps
            .clear();
        deliver(&mut state, 1000, ActivityLevel::Mention);
        state.clear_activity("account/peer");
        deliver(&mut state, 2000, ActivityLevel::Activity);
        crate::irc::events::handle_cap_ack(&mut state, "account", Some("draft/read-marker"), None);
        assert_eq!(state.buffers["account/peer"].unread_count, 1);
        assert_eq!(
            state.buffers["account/peer"].activity,
            ActivityLevel::Activity
        );
    }

    #[tokio::test]
    async fn partial_read_removes_old_mentions_without_clearing_later_activity() {
        let mut state = state();
        deliver(&mut state, 1000, ActivityLevel::Mention);
        deliver(&mut state, 2000, ActivityLevel::Activity);
        deliver(&mut state, 3000, ActivityLevel::Activity);
        assert_eq!(state.buffers["account/peer"].unread_count, 3);
        state.apply_server_read_marker("account/peer", 1000);
        assert_eq!(state.buffers["account/peer"].unread_count, 2);
        assert_eq!(
            state.buffers["account/peer"].activity,
            ActivityLevel::Activity
        );
        let mut history = crate::state::events::tests::make_test_message(&mut state, "history");
        history.timestamp = chrono::DateTime::from_timestamp_millis(4000).unwrap();
        state
            .buffers
            .get_mut("account/peer")
            .unwrap()
            .messages
            .push_back(history);
        state.apply_server_read_marker("account/peer", 3000);
        assert_eq!(state.buffers["account/peer"].unread_count, 0);
        assert_eq!(state.buffers["account/peer"].activity, ActivityLevel::None);
    }

    #[tokio::test]
    async fn delayed_read_messages_do_not_restore_activity_or_alerts() {
        let mut state = state();
        state.apply_server_read_marker("account/peer", 2000);
        state.pending_web_events.clear();
        deliver(&mut state, 1000, ActivityLevel::Mention);
        deliver(&mut state, 2000, ActivityLevel::Mention);
        assert_eq!(state.buffers["account/peer"].unread_count, 0);
        assert!(
            !state
                .pending_web_events
                .iter()
                .any(|event| matches!(event, crate::web::protocol::WebEvent::MentionAlert { .. }))
        );
        deliver(&mut state, 2001, ActivityLevel::Activity);
        assert_eq!(state.buffers["account/peer"].unread_count, 1);
        state.apply_server_read_marker("account/peer", 1000);
        assert_eq!(state.read_activity["account/peer"].through, Some(2000));
        assert_eq!(state.buffers["account/peer"].unread_count, 1);
    }

    #[tokio::test]
    async fn marker_before_buffer_creation_suppresses_late_unread_and_close_discards_it() {
        let mut state = state();
        state.remove_buffer("account/peer");
        state.apply_server_read_marker("account/peer", 2000);
        state.add_buffer_with_focus(Buffer::empty("account", BufferType::Query, "Peer"), false);
        deliver(&mut state, 1000, ActivityLevel::Activity);
        assert_eq!(state.buffers["account/peer"].unread_count, 0);
        state.remove_buffer("account/peer");
        assert!(!state.read_activity.contains_key("account/peer"));
    }

    #[tokio::test]
    async fn unread_tracking_stays_within_retained_scrollback() {
        let mut state = state();
        state.scrollback_limit = 2;
        for time in 1..=10 {
            deliver(&mut state, time, ActivityLevel::Activity);
        }
        assert_eq!(state.read_activity["account/peer"].unread.len(), 2);
        assert_eq!(state.buffers["account/peer"].unread_count, 2);
        state.apply_server_read_marker("account/peer", 9);
        assert_eq!(state.buffers["account/peer"].unread_count, 1);
    }
}

use std::collections::HashMap;

use super::AppState;
use super::buffer::{ActivityLevel, BufferType, Message, MessageType};

#[derive(Default)]
pub(super) struct ReadActivity {
    pub(super) through: Option<i64>,
    pub(super) origins: HashMap<u64, String>,
    pub(super) unread: HashMap<u64, (Option<i64>, ActivityLevel, u64)>,
}

fn server_time(message: &Message) -> Option<i64> {
    message
        .tags
        .as_ref()?
        .get("time")
        .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
        .map(|time| time.timestamp_millis())
}

impl AppState {
    pub(crate) fn reset_connection_read_markers(&mut self, conn_id: &str) {
        self.read_identities.remove(conn_id);
        let buffers: Vec<_> = self
            .read_activity
            .keys()
            .filter(|buffer_id| {
                buffer_id
                    .split_once('/')
                    .is_some_and(|(id, _)| id == conn_id)
            })
            .cloned()
            .collect();
        for buffer_id in buffers {
            self.read_activity.remove(&buffer_id);
            self.refresh_read_activity(&buffer_id);
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
        let Some(time) = server_time(message) else {
            return false;
        };
        let origin = self
            .read_activity
            .get(buffer_id)
            .and_then(|state| state.origins.get(&message.id))
            .map_or(buffer_id, String::as_str);
        self.read_activity
            .get(origin)
            .and_then(|state| state.through)
            .is_some_and(|through| time <= through)
    }

    pub(super) fn record_read_origin(&mut self, buffer_id: &str, message: &Message, origin: &str) {
        if self.buffer_uses_server_history(buffer_id) {
            self.read_activity
                .entry(buffer_id.to_string())
                .or_default()
                .origins
                .insert(message.id, origin.to_string());
        }
    }

    pub(crate) fn visible_read_markers(
        &self,
        buffer_id: &str,
        message_id: u64,
    ) -> Option<HashMap<String, i64>> {
        let buffer = self.buffers.get(buffer_id)?;
        let position = buffer
            .messages
            .iter()
            .position(|message| message.id == message_id)?;
        let mut markers = HashMap::<String, i64>::new();
        for message in buffer.messages.range(..=position) {
            let Some(time) = message
                .tags
                .as_ref()
                .and_then(|tags| tags.get("time"))
                .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
            else {
                continue;
            };
            let origin = self
                .read_activity
                .get(buffer_id)
                .and_then(|state| state.origins.get(&message.id))
                .map_or(buffer_id, String::as_str);
            let through = markers
                .entry(origin.to_string())
                .or_insert_with(|| time.timestamp_millis());
            *through = (*through).max(time.timestamp_millis());
        }
        Some(markers)
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
        self.activity_counter = self.activity_counter.saturating_add(1);
        let order = self.activity_counter;
        self.read_activity
            .entry(buffer_id.to_string())
            .or_default()
            .unread
            .entry(message.id)
            .and_modify(|entry| {
                entry.0 = server_time(message);
                entry.1 = level;
            })
            .or_insert_with(|| (server_time(message), level, order));
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

    pub(super) fn record_history_read_activity(
        &mut self,
        buffer_id: &str,
        message: &Message,
        own_nick: Option<&str>,
    ) {
        if !self.buffer_uses_server_history(buffer_id)
            || (!self.uses_read_markers(buffer_id)
                && self.active_buffer_id.as_deref() == Some(buffer_id))
            || self.is_own_history_message(buffer_id, message, own_nick)
        {
            return;
        }
        let mentioned = message.highlight
            || own_nick
                .filter(|nick| !nick.is_empty())
                .is_some_and(|nick| {
                    crate::irc::formatting::strip_irc_formatting(&message.text)
                        .to_lowercase()
                        .contains(&nick.to_lowercase())
                });
        let level = match message.message_type {
            MessageType::Message | MessageType::Action => {
                if mentioned
                    || self
                        .buffers
                        .get(buffer_id)
                        .is_some_and(|buffer| buffer.buffer_type == BufferType::Query)
                {
                    ActivityLevel::Mention
                } else {
                    ActivityLevel::Activity
                }
            }
            MessageType::Notice => {
                if mentioned {
                    ActivityLevel::Mention
                } else {
                    ActivityLevel::Activity
                }
            }
            MessageType::Event => ActivityLevel::Events,
            MessageType::MentionLog => ActivityLevel::None,
        };
        if !self.message_already_read(buffer_id, message) {
            self.record_activity(buffer_id, level);
        }
        self.record_read_activity(buffer_id, message, level);
    }

    pub(super) fn finish_history_read_activity(&mut self, buffer_id: &str) {
        if self.uses_read_markers(buffer_id) {
            self.refresh_read_activity(buffer_id);
        } else if self.buffer_uses_server_history(buffer_id) {
            self.prune_read_activity(buffer_id);
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
        state.origins.retain(|id, _| ids.contains(id));
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
            .map(|(_, level, _)| *level)
            .max()
            .unwrap_or(ActivityLevel::None);
        if let Some(order) = state
            .unread
            .values()
            .filter(|(_, level, _)| *level == buffer.activity)
            .map(|(_, _, order)| *order)
            .min()
        {
            self.activity_order.insert(buffer_id.to_string(), order);
        } else {
            self.activity_order.remove(buffer_id);
        }
        self.pending_web_events
            .push(crate::web::protocol::WebEvent::ActivityChanged {
                buffer_id: buffer_id.to_string(),
                activity: buffer.activity as u8,
                unread_count: buffer.unread_count,
            });
    }

    pub(crate) fn clear_visible_read_rows(&mut self, buffer_id: &str, message_id: u64) {
        let Some(buffer) = self.buffers.get(buffer_id) else {
            return;
        };
        let Some(position) = buffer
            .messages
            .iter()
            .position(|message| message.id == message_id)
        else {
            return;
        };
        let ids: std::collections::HashSet<_> = buffer
            .messages
            .range(..=position)
            .map(|message| message.id)
            .collect();
        if let Some(state) = self.read_activity.get_mut(buffer_id) {
            let before = state.unread.len();
            state.unread.retain(|id, _| !ids.contains(id));
            if state.unread.len() != before {
                self.refresh_read_activity(buffer_id);
            }
        }
    }

    pub(crate) fn apply_server_read_marker(&mut self, buffer_id: &str, millis: i64) {
        let state = self.read_activity.entry(buffer_id.to_string()).or_default();
        if state.through.is_some_and(|previous| previous >= millis) {
            return;
        }
        state.through = Some(millis);
        let mut changed = Vec::new();
        for (id, state) in &mut self.read_activity {
            let before = state.unread.len();
            state.unread.retain(|message_id, (time, _, _)| {
                state
                    .origins
                    .get(message_id)
                    .map_or(id.as_str(), String::as_str)
                    != buffer_id
                    || time.is_none_or(|time| time > millis)
            });
            if state.unread.len() != before && id != buffer_id {
                changed.push(id.clone());
            }
        }
        for id in changed {
            self.refresh_read_activity(&id);
        }
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
        message.tags = Some(HashMap::from([(
            "time".into(),
            message.timestamp.to_rfc3339(),
        )]));
        message.highlight = level == ActivityLevel::Mention;
        state.add_transient_message_with_activity("account/peer", message, level);
    }

    #[tokio::test]
    async fn partial_read_reorders_remaining_activity_by_arrival() {
        let mut state = state();
        state.add_buffer_with_focus(Buffer::empty("account", BufferType::Query, "Other"), false);
        deliver(&mut state, 1000, ActivityLevel::Mention);
        let mut other = crate::state::events::tests::make_test_message(&mut state, "other");
        other.timestamp = chrono::DateTime::from_timestamp_millis(2000).unwrap();
        other.tags = Some(HashMap::from([(
            "time".into(),
            other.timestamp.to_rfc3339(),
        )]));
        state.add_transient_message_with_activity("account/other", other, ActivityLevel::Activity);
        deliver(&mut state, 3000, ActivityLevel::Activity);
        assert_eq!(
            state.next_activity_buffer().as_deref(),
            Some("account/peer")
        );
        state.apply_server_read_marker("account/peer", 1000);
        assert_eq!(
            state.next_activity_buffer().as_deref(),
            Some("account/other")
        );
        state.apply_server_read_marker("account/other", 2000);
        assert_eq!(
            state.next_activity_buffer().as_deref(),
            Some("account/peer")
        );
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

    fn history_row(state: &mut AppState, millis: i64) -> Message {
        let mut message = crate::state::events::tests::make_test_message(state, "history");
        message.timestamp = chrono::DateTime::from_timestamp_millis(millis).unwrap();
        message.tags = Some(HashMap::from([(
            "time".into(),
            message.timestamp.to_rfc3339(),
        )]));
        message.nick = Some("Peer".into());
        message
    }

    #[tokio::test]
    async fn server_history_tracks_unread_rows_without_playback_notifications() {
        let mut state = state();
        state.apply_server_read_marker("account/peer", 1000);
        let mut rows: Vec<_> = [500, 1000, 2000, 3000]
            .into_iter()
            .map(|time| history_row(&mut state, time))
            .collect();
        let mut own = history_row(&mut state, 3500);
        own.nick = Some(state.connections["account"].nick.clone());
        rows.push(own);
        state.pending_web_events.clear();
        state.surface_history_page("account/peer", rows.clone(), false);
        assert_eq!(state.buffers["account/peer"].unread_count, 2);
        assert_eq!(
            state.buffers["account/peer"].activity,
            ActivityLevel::Mention
        );
        state.surface_history_page("account/peer", rows, false);
        assert_eq!(state.buffers["account/peer"].unread_count, 2);
        let older = history_row(&mut state, 1500);
        state.surface_history_page("account/peer", vec![older], true);
        assert_eq!(state.buffers["account/peer"].unread_count, 3);
        state.apply_server_read_marker("account/peer", 2500);
        assert_eq!(state.buffers["account/peer"].unread_count, 1);
        assert!(!state.pending_web_events.iter().any(|event| matches!(
            event,
            crate::web::protocol::WebEvent::MentionAlert { .. }
                | crate::web::protocol::WebEvent::NewMessage { .. }
        )));
    }

    #[tokio::test]
    async fn marker_arriving_after_initial_hydration_reconciles_unread_rows() {
        let mut state = state();
        let rows: Vec<_> = [1000, 2000, 3000]
            .into_iter()
            .map(|time| history_row(&mut state, time))
            .collect();
        state.surface_history_page("account/peer", rows, false);
        assert_eq!(state.buffers["account/peer"].unread_count, 3);
        state.apply_server_read_marker("account/peer", 2000);
        assert_eq!(state.buffers["account/peer"].unread_count, 1);
    }

    #[tokio::test]
    async fn capability_loss_does_not_restore_read_messages_as_unread() {
        let mut state = state();
        state.apply_server_read_marker("account/peer", 2000);
        crate::irc::events::handle_cap_del(&mut state, "account", Some("draft/read-marker"), None);
        state.pending_web_events.clear();
        deliver(&mut state, 1000, ActivityLevel::Mention);
        assert_eq!(state.buffers["account/peer"].unread_count, 0);
        assert_eq!(state.buffers["account/peer"].activity, ActivityLevel::None);
        assert!(
            !state
                .pending_web_events
                .iter()
                .any(|event| matches!(event, crate::web::protocol::WebEvent::MentionAlert { .. }))
        );
        deliver(&mut state, 3000, ActivityLevel::Activity);
        assert_eq!(state.buffers["account/peer"].unread_count, 1);
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
    async fn marker_survives_buffer_closure_without_retaining_unread_rows() {
        let mut state = state();
        state.remove_buffer("account/peer");
        state.apply_server_read_marker("account/peer", 2000);
        state.add_buffer_with_focus(Buffer::empty("account", BufferType::Query, "Peer"), false);
        deliver(&mut state, 1000, ActivityLevel::Activity);
        assert_eq!(state.buffers["account/peer"].unread_count, 0);
        deliver(&mut state, 3000, ActivityLevel::Activity);
        state.remove_buffer("account/peer");
        assert_eq!(state.read_activity["account/peer"].through, Some(2000));
        assert!(state.read_activity["account/peer"].unread.is_empty());
        state.add_buffer_with_focus(Buffer::empty("account", BufferType::Query, "Peer"), false);
        assert_eq!(
            state.buffers["account/peer"].last_read.timestamp_millis(),
            2000
        );
        let rows = [1000, 2000, 3000]
            .into_iter()
            .map(|time| history_row(&mut state, time))
            .collect();
        state.surface_history_page("account/peer", rows, false);
        assert_eq!(state.buffers["account/peer"].unread_count, 1);
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
    #[tokio::test]
    async fn server_markers_never_clear_untimed_rows_with_skewed_local_clocks() {
        let mut state = state();
        state.apply_server_read_marker("account/peer", 2000);
        for time_tag in [None, Some("invalid")] {
            let mut row = history_row(&mut state, 1000);
            row.tags = time_tag.map(|time| HashMap::from([("time".into(), time.into())]));
            state.add_transient_message_with_activity("account/peer", row, ActivityLevel::Mention);
        }
        assert_eq!(state.buffers["account/peer"].unread_count, 2);
        state.apply_server_read_marker("account/peer", 3000);
        assert_eq!(state.buffers["account/peer"].unread_count, 2);
        let last = state.buffers["account/peer"].messages.back().unwrap().id;
        state.clear_visible_read_rows("account/peer", last);
        assert_eq!(state.buffers["account/peer"].unread_count, 0);
    }
    #[tokio::test]
    async fn authenticated_history_identity_needs_no_channel_membership() {
        let mut state = state();
        let nick = state.connections["account"].nick.clone();
        for wire in [
            format!(":bnc 900 {nick} {nick}!user@host upstream-account :Logged in"),
            format!(":lurker.bouncer 900 {nick} {nick}!user@host bouncer-login :Logged in"),
        ] {
            crate::irc::events::handle_irc_message(&mut state, "account", &wire.parse().unwrap());
        }
        assert!(state.buffers.values().all(|buffer| buffer.users.is_empty()));
        let mut own = history_row(&mut state, 1000);
        own.nick = Some("EarlierNick".into());
        own.tags
            .as_mut()
            .unwrap()
            .insert("account".into(), "upstream-account".into());
        let mut other = history_row(&mut state, 2000);
        other.nick = Some(nick.clone());
        other
            .tags
            .as_mut()
            .unwrap()
            .insert("account".into(), "another-account".into());
        state.surface_history_page("account/peer", vec![own.clone(), other], false);
        assert_eq!(state.buffers["account/peer"].unread_count, 1);
        crate::irc::events::handle_irc_message(
            &mut state,
            "account",
            &format!(":{nick}!user@host ACCOUNT *").parse().unwrap(),
        );
        assert!(state.is_own_history_message("account/peer", &own, Some(&nick)));
        crate::irc::events::handle_irc_message(
            &mut state,
            "account",
            &format!(":{nick}!user@host ACCOUNT upstream-account")
                .parse()
                .unwrap(),
        );
        assert!(state.is_own_history_message("account/peer", &own, Some(&nick)));
        crate::irc::events::handle_irc_message(
            &mut state,
            "account",
            &format!(":bnc 901 {nick} {nick}!user@host :Logged out")
                .parse()
                .unwrap(),
        );
        assert!(state.is_own_history_message("account/peer", &own, Some(&nick)));
    }
    #[tokio::test]
    async fn own_history_survives_account_switch_and_logout() {
        let mut state = state();
        let nick = state.connections["account"].nick.clone();
        for account in ["first-account", "second-account", "*"] {
            crate::irc::events::handle_irc_message(
                &mut state,
                "account",
                &format!(":{nick}!user@host ACCOUNT {account}")
                    .parse()
                    .unwrap(),
            );
        }
        let rows = ["first-account", "second-account", "unrelated-account"]
            .into_iter()
            .enumerate()
            .map(|(index, account)| {
                let mut message = history_row(&mut state, i64::try_from(index).unwrap() + 1000);
                message.nick = Some("EarlierNick".into());
                message
                    .tags
                    .as_mut()
                    .unwrap()
                    .insert("account".into(), account.into());
                message
            })
            .collect();
        state.surface_history_page("account/peer", rows, false);
        assert_eq!(state.buffers["account/peer"].unread_count, 1);
    }
    #[tokio::test]
    async fn removing_connection_releases_retained_markers_and_identity() {
        let mut state = state();
        let nick = state.connections["account"].nick.clone();
        state.record_read_account("account", &nick, Some("own-account"));
        state.apply_server_read_marker("account/peer", 2000);
        state.remove_buffer("account/peer");
        assert!(state.read_activity.contains_key("account/peer"));
        assert!(state.read_identities.contains_key("account"));
        state.apply_server_read_marker("other/peer", 1000);
        state.remove_connection("account");
        assert!(
            !state
                .read_activity
                .keys()
                .any(|id| id.starts_with("account/"))
        );
        assert!(!state.read_identities.contains_key("account"));
        assert_eq!(state.read_activity["other/peer"].through, Some(1000));
        assert!(!state.connections.contains_key("account"));
    }
    #[tokio::test]
    async fn deleted_bouncer_networks_release_read_state_each_time() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let config = toml::from_str("label='Bouncer'\naddress='127.0.0.1'\nport=1\ntls=false\nchannels=[]\nbouncer_control=true").unwrap();
        app.setup_connection("account", &config);
        app.state.connections.get_mut("account").unwrap().status =
            crate::state::connection::ConnectionStatus::Connected;
        app.bouncer_networks.insert(
            "account".into(),
            crate::irc::bouncer::NetworkRegistry::default(),
        );
        for netid in 1..=8 {
            assert!(
                app.handle_bouncer_network_message(
                    "account",
                    &format!("BOUNCER NETWORK {netid} name=Temporary")
                        .parse()
                        .unwrap()
                )
            );
            let child = app.bouncer_children.keys().next().unwrap().clone();
            let nick = app.state.connections[&child].nick.clone();
            app.state
                .record_read_account(&child, &nick, Some("own-account"));
            let buffer_id = format!("{child}/peer");
            app.state
                .add_buffer_with_focus(Buffer::empty(&child, BufferType::Query, "Peer"), false);
            app.state.apply_server_read_marker(&buffer_id, 2000);
            app.state.remove_buffer(&buffer_id);
            assert!(app.state.read_activity.contains_key(&buffer_id));
            assert!(app.handle_bouncer_network_message(
                "account",
                &format!("BOUNCER NETWORK {netid} *").parse().unwrap()
            ));
            assert!(
                !app.state
                    .read_activity
                    .keys()
                    .any(|id| id.starts_with(&format!("{child}/")))
            );
            assert!(!app.state.read_identities.contains_key(&child));
            assert!(!app.read_markers.contains_key(&child));
            assert!(app.bouncer_children.is_empty());
        }
    }
}

use super::AppState;
use super::buffer::ActivityLevel;

impl AppState {
    pub(super) fn register_activity(&mut self, buffer_id: &str) {
        self.activity_counter = self.activity_counter.saturating_add(1);
        self.activity_order
            .insert(buffer_id.to_string(), self.activity_counter);
    }

    pub(super) fn record_activity(&mut self, buffer_id: &str, level: ActivityLevel) {
        if self
            .buffers
            .get(buffer_id)
            .is_some_and(|buffer| level > buffer.activity)
        {
            self.register_activity(buffer_id);
        }
    }

    pub fn rekey_activity(&mut self, old_id: &str, new_id: &str) {
        if old_id == new_id {
            return;
        }
        if let Some(mut state) = self.read_activity.remove(old_id) {
            let destination = self.read_activity.entry(new_id.to_string()).or_default();
            destination.unread.extend(std::mem::take(&mut state.unread));
            if let Some(timestamp) = destination.through.and_then(chrono::DateTime::from_timestamp_millis)
                && let Some(buffer) = self.buffers.get_mut(new_id)
            {
                buffer.last_read = timestamp;
            }
            if state.through.is_some() {
                self.read_activity.insert(old_id.to_string(), state);
            }
        }
        if let Some(order) = self.activity_order.remove(old_id) {
            self.activity_order.insert(new_id.to_string(), order);
        } else if self
            .buffers
            .get(new_id)
            .is_some_and(|buffer| buffer.activity == ActivityLevel::None)
        {
            self.activity_order.remove(new_id);
        }
    }

    pub fn clear_activity(&mut self, buffer_id: &str) {
        if self.uses_read_markers(buffer_id) { return; }
        self.activity_order.remove(buffer_id);
        if let Some(state) = self.read_activity.get_mut(buffer_id) {
            state.unread.clear();
        }
        if let Some(buffer) = self.buffers.get_mut(buffer_id) {
            buffer.activity = ActivityLevel::None;
            buffer.unread_count = 0;
        }
    }

    pub fn next_activity_buffer(&self) -> Option<String> {
        self.buffers
            .values()
            .enumerate()
            .filter(|(_, buffer)| {
                buffer.activity != ActivityLevel::None
                    && self.active_buffer_id.as_deref() != Some(buffer.id.as_str())
            })
            .min_by_key(|(index, buffer)| {
                (
                    std::cmp::Reverse(buffer.activity),
                    self.activity_order
                        .get(&buffer.id)
                        .copied()
                        .unwrap_or(u64::MAX),
                    *index,
                )
            })
            .map(|(_, buffer)| buffer.id.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::buffer::{Buffer, BufferType};
    use crate::state::events::tests::make_test_message;

    fn state() -> AppState {
        let mut state = AppState::new();
        for name in ["#current", "#older", "#newer", "#events", "#mention"] {
            state.add_buffer(Buffer::for_test("net", BufferType::Channel, name));
        }
        state.set_active_buffer("net/#current");
        state
    }

    fn deliver(state: &mut AppState, id: &str, level: ActivityLevel) {
        let message = make_test_message(state, "activity");
        state.add_transient_message_with_activity(id, message, level);
    }

    #[test]
    fn chooses_priority_then_oldest_unread_activity() {
        let mut state = state();
        deliver(&mut state, "net/#events", ActivityLevel::Events);
        deliver(&mut state, "net/#older", ActivityLevel::Activity);
        deliver(&mut state, "net/#newer", ActivityLevel::Activity);
        deliver(&mut state, "net/#older", ActivityLevel::Activity);
        deliver(&mut state, "net/#mention", ActivityLevel::Mention);
        for expected in ["net/#mention", "net/#older", "net/#newer", "net/#events"] {
            assert_eq!(state.next_activity_buffer().as_deref(), Some(expected));
            state.set_active_buffer(expected);
        }
        assert!(state.next_activity_buffer().is_none());
        assert!(state.activity_order.is_empty());
    }

    #[test]
    fn escalation_starts_a_new_position_within_the_higher_priority() {
        let mut state = state();
        deliver(&mut state, "net/#older", ActivityLevel::Events);
        deliver(&mut state, "net/#newer", ActivityLevel::Activity);
        deliver(&mut state, "net/#older", ActivityLevel::Activity);
        deliver(&mut state, "net/#newer", ActivityLevel::Events);
        assert_eq!(state.next_activity_buffer().as_deref(), Some("net/#newer"));
        state.clear_activity("net/#newer");
        assert_eq!(state.next_activity_buffer().as_deref(), Some("net/#older"));
        deliver(&mut state, "net/#newer", ActivityLevel::Activity);
        assert_eq!(state.next_activity_buffer().as_deref(), Some("net/#older"));
    }

    #[test]
    fn closing_and_renaming_buffers_retains_only_live_activity_order() {
        let mut state = state();
        state.set_activity("net/#older", ActivityLevel::Activity);
        state.set_activity("net/#newer", ActivityLevel::Activity);
        let mut renamed = state.buffers.shift_remove("net/#older").unwrap();
        renamed.id = "net/#renamed".into();
        renamed.name = "#renamed".into();
        state.buffers.insert(renamed.id.clone(), renamed);
        state.rekey_buffer_state("net/#older", "net/#renamed");
        assert_eq!(
            state.next_activity_buffer().as_deref(),
            Some("net/#renamed")
        );
        assert!(!state.activity_order.contains_key("net/#older"));
        state.remove_buffer("net/#renamed");
        assert_eq!(state.next_activity_buffer().as_deref(), Some("net/#newer"));
        assert!(!state.activity_order.contains_key("net/#renamed"));
    }

    #[test]
    fn closing_active_buffer_marks_both_fallback_paths_read() {
        for has_previous in [true, false] {
            let mut state = state();
            state.set_active_buffer("net/#older");
            if !has_previous {
                state.previous_buffer_id = None;
            }
            let fallback = if has_previous {
                "net/#current".to_string()
            } else {
                state
                    .sorted_buffer_ids()
                    .into_iter()
                    .find(|id| id != "net/#older")
                    .unwrap()
            };
            deliver(&mut state, &fallback, ActivityLevel::Activity);
            state.remove_buffer("net/#older");
            assert_eq!(state.active_buffer_id.as_deref(), Some(fallback.as_str()));
            assert_eq!(state.buffers[&fallback].activity, ActivityLevel::None);
            assert!(!state.activity_order.contains_key(&fallback));
            state.set_active_buffer("net/#newer");
            assert!(state.next_activity_buffer().is_none());
        }
    }

    #[test]
    fn repeated_aggregated_mentions_keep_their_original_position() {
        let mut state = state();
        let mut mentions = Buffer::for_test("net", BufferType::Mentions, "mentions");
        mentions.id = "_mentions".into();
        state.add_buffer(mentions);
        let first = make_test_message(&mut state, "first mention");
        state.add_mention_to_buffer(first);
        deliver(&mut state, "net/#mention", ActivityLevel::Mention);
        let second = make_test_message(&mut state, "second mention");
        state.add_mention_to_buffer(second);
        assert_eq!(state.next_activity_buffer().as_deref(), Some("_mentions"));
        state.set_active_buffer("_mentions");
        assert_eq!(
            state.next_activity_buffer().as_deref(),
            Some("net/#mention")
        );
    }

    #[test]
    fn active_and_missing_buffers_never_become_shortcut_targets() {
        let mut state = state();
        deliver(&mut state, "net/#current", ActivityLevel::Mention);
        state.set_activity("missing", ActivityLevel::Mention);
        assert!(state.next_activity_buffer().is_none());
        assert!(state.activity_order.is_empty());
    }
}

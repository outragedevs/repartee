use std::collections::HashMap;
use std::time::{Duration, Instant};

const REQUEST_TTL: Duration = Duration::from_mins(1);
const MAX_REQUESTS: usize = 512;

struct Request {
    buffer_id: String,
    started: Instant,
    completed: bool,
}

#[derive(Default)]
pub struct Labels {
    requests: HashMap<String, Request>,
}

impl Labels {
    pub fn register(&mut self, buffer_id: String, now: Instant) -> Option<String> {
        self.expire(now);
        if self.requests.len() >= MAX_REQUESTS {
            self.requests.retain(|_, request| !request.completed);
        }
        if self.requests.len() >= MAX_REQUESTS {
            return None;
        }
        let label = uuid::Uuid::new_v4().simple().to_string();
        self.requests.insert(
            label.clone(),
            Request {
                buffer_id,
                started: now,
                completed: false,
            },
        );
        Some(label)
    }

    pub fn resolve(&mut self, label: &str, now: Instant) -> Option<String> {
        self.expire(now);
        self.requests
            .get(label)
            .map(|request| request.buffer_id.clone())
    }

    pub fn finish(&mut self, label: &str) {
        if let Some(request) = self.requests.get_mut(label) {
            request.completed = true;
        }
    }

    pub fn remove(&mut self, label: &str) {
        self.requests.remove(label);
    }

    #[cfg(test)]
    pub fn pending_count(&self) -> usize {
        self.requests
            .values()
            .filter(|request| !request.completed)
            .count()
    }

    #[cfg(test)]
    pub fn is_pending(&self, label: &str) -> bool {
        self.requests
            .get(label)
            .is_some_and(|request| !request.completed)
    }

    pub fn expire(&mut self, now: Instant) {
        self.requests
            .retain(|_, request| now.saturating_duration_since(request.started) < REQUEST_TTL);
    }
}

pub fn message_label(message: &irc::proto::Message) -> Option<&str> {
    message
        .tags
        .as_ref()?
        .iter()
        .find(|tag| tag.0 == "label")?
        .1
        .as_deref()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_context_survives_buffered_payloads_but_expires() {
        let mut labels = Labels::default();
        let now = Instant::now();
        let label = labels.register("network/#origin".into(), now).unwrap();
        assert!(label.len() <= 64);
        labels.finish(&label);
        assert!(labels.requests[&label].completed);
        assert_eq!(
            labels.resolve(&label, now).as_deref(),
            Some("network/#origin")
        );
        assert!(labels.resolve(&label, now + REQUEST_TTL).is_none());
    }

    #[test]
    fn pending_requests_are_bounded_and_finished_slots_are_reclaimed() {
        let mut labels = Labels::default();
        let now = Instant::now();
        let first = labels.register("origin".into(), now).unwrap();
        for _ in 1..MAX_REQUESTS {
            assert!(labels.register("origin".into(), now).is_some());
        }
        assert!(labels.register("origin".into(), now).is_none());
        labels.finish(&first);
        let next = labels.register("next".into(), now).unwrap();
        assert_ne!(first, next);
        labels.remove(&next);
        assert!(labels.resolve(&next, now).is_none());
    }
}

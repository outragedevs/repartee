//! Who is currently typing, in which buffer.
//!
//! Ephemeral by design: never written to `SQLite`, never part of the
//! detach/reattach snapshot. On reattach the map is empty and refills from live
//! traffic within seconds.
//!
//! Lives beside `Buffer` rather than inside it: `Buffer` has no constructor and
//! is built from struct literals in ~39 places, mostly test fixtures. It is also
//! the better boundary — typing is session state, not buffer content.

use std::collections::HashMap;
use std::time::Instant;

use crate::irc::typing::TypingState;

/// One peer's typing state in one buffer.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct TypingEntry {
    pub state: TypingState,
    /// When this state was last refreshed — the TTL clock (spec §1.2).
    pub since: Instant,
}

/// `buffer_id -> nick -> entry`. Buffer ids are `{conn_id}/{name}`
/// (`src/state/buffer.rs:213`), which is what makes connection-scoped cleanup
/// a prefix match.
#[derive(Debug, Default)]
pub struct TypingTracker {
    entries: HashMap<String, HashMap<String, TypingEntry>>,
}

#[allow(dead_code)]
impl TypingTracker {
    /// Record a peer's typing state.
    ///
    /// Returns `true` when the **visible set** of typing nicks changed — that is
    /// what decides whether the web clients need a push. Refreshing a nick who is
    /// already shown as typing changes nothing on screen.
    pub fn set(&mut self, buffer_id: &str, nick: &str, state: TypingState, now: Instant) -> bool {
        if state == TypingState::Done {
            return self.clear(buffer_id, nick);
        }
        self.entries
            .entry(buffer_id.to_string())
            .or_default()
            .insert(nick.to_string(), TypingEntry { state, since: now })
            .is_none()
    }

    /// Stop showing `nick` as typing in `buffer_id`. Returns `true` if they were.
    pub fn clear(&mut self, buffer_id: &str, nick: &str) -> bool {
        let Some(buf) = self.entries.get_mut(buffer_id) else {
            return false;
        };
        let removed = buf.remove(nick).is_some();
        if buf.is_empty() {
            self.entries.remove(buffer_id);
        }
        removed
    }

    /// Stop showing `nick` as typing anywhere **on one connection** — they quit,
    /// or changed nick.
    ///
    /// Scoped deliberately: a QUIT belongs to a single connection, and the same
    /// nick may well be typing on another network. Returns the buffers changed.
    pub fn clear_nick_on_connection(&mut self, conn_id: &str, nick: &str) -> Vec<String> {
        let prefix = format!("{conn_id}/");
        let mut changed = Vec::new();
        self.entries.retain(|buffer_id, buf| {
            if buffer_id.starts_with(&prefix) && buf.remove(nick).is_some() {
                changed.push(buffer_id.clone());
            }
            !buf.is_empty()
        });
        changed
    }

    /// Drop a closed buffer's state entirely. Returns `true` if there was any.
    pub fn remove_buffer(&mut self, buffer_id: &str) -> bool {
        self.entries.remove(buffer_id).is_some()
    }

    /// Forget everything — used when `typing.show` is switched off. Returns every
    /// buffer that had state, so the web clients can be told to clear it.
    pub fn clear_all(&mut self) -> Vec<String> {
        let changed: Vec<String> = self.entries.keys().cloned().collect();
        self.entries.clear();
        changed
    }

    /// Expire entries past their spec TTL (6s active, 30s paused).
    /// Returns the buffers whose visible set changed.
    pub fn expire(&mut self, now: Instant) -> Vec<String> {
        let mut changed = Vec::new();
        self.entries.retain(|buffer_id, buf| {
            let before = buf.len();
            buf.retain(|_, e| {
                e.state
                    .ttl()
                    .is_some_and(|ttl| now.duration_since(e.since) < ttl)
            });
            if buf.len() != before {
                changed.push(buffer_id.clone());
            }
            !buf.is_empty()
        });
        changed
    }

    /// Nicks currently typing in `buffer_id`, sorted so the status line does not
    /// reorder itself between frames.
    #[must_use]
    pub fn nicks(&self, buffer_id: &str) -> Vec<&str> {
        let Some(buf) = self.entries.get(buffer_id) else {
            return Vec::new();
        };
        let mut nicks: Vec<&str> = buf.keys().map(String::as_str).collect();
        nicks.sort_unstable();
        nicks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // A fixed origin: every test is deterministic, no wall clock anywhere.
    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn set_reports_a_visible_change_only_for_new_nicks() {
        let mut tr = TypingTracker::default();
        let now = t0();
        assert!(tr.set("net/#rust", "alice", TypingState::Active, now));
        // Refreshing an existing typer does not change what is displayed.
        assert!(!tr.set("net/#rust", "alice", TypingState::Active, now + Duration::from_secs(3)));
        assert!(tr.set("net/#rust", "bob", TypingState::Active, now));
    }

    #[test]
    fn done_removes_the_nick() {
        let mut tr = TypingTracker::default();
        let now = t0();
        tr.set("net/#rust", "alice", TypingState::Active, now);
        assert!(tr.set("net/#rust", "alice", TypingState::Done, now));
        assert!(tr.nicks("net/#rust").is_empty());
        // Done for someone who was not typing is not a visible change.
        assert!(!tr.set("net/#rust", "carol", TypingState::Done, now));
    }

    #[test]
    fn active_expires_after_six_seconds_paused_after_thirty() {
        let mut tr = TypingTracker::default();
        let now = t0();
        tr.set("net/#rust", "alice", TypingState::Active, now);
        tr.set("net/#rust", "bob", TypingState::Paused, now);

        assert!(tr.expire(now + Duration::from_secs(5)).is_empty());
        assert_eq!(tr.nicks("net/#rust"), vec!["alice", "bob"]);

        assert_eq!(tr.expire(now + Duration::from_secs(6)), vec!["net/#rust"]);
        assert_eq!(tr.nicks("net/#rust"), vec!["bob"]);

        assert_eq!(tr.expire(now + Duration::from_secs(30)), vec!["net/#rust"]);
        assert!(tr.nicks("net/#rust").is_empty());
    }

    #[test]
    fn clear_removes_one_nick_in_one_buffer() {
        let mut tr = TypingTracker::default();
        tr.set("net/#rust", "alice", TypingState::Active, t0());
        assert!(tr.clear("net/#rust", "alice"));
        assert!(!tr.clear("net/#rust", "alice"));
    }

    #[test]
    fn clear_nick_is_scoped_to_one_connection() {
        // The same nick can be typing on two networks. A QUIT on one must not
        // clear the indicator on the other.
        let mut tr = TypingTracker::default();
        let now = t0();
        tr.set("libera/#rust", "alice", TypingState::Active, now);
        tr.set("oftc/#rust", "alice", TypingState::Active, now);
        tr.set("libera/#tokio", "alice", TypingState::Active, now);

        let mut affected = tr.clear_nick_on_connection("libera", "alice");
        affected.sort();
        assert_eq!(affected, vec!["libera/#rust", "libera/#tokio"]);
        // The other network is untouched.
        assert_eq!(tr.nicks("oftc/#rust"), vec!["alice"]);
    }

    #[test]
    fn nicks_are_sorted_so_the_status_line_does_not_jitter() {
        let mut tr = TypingTracker::default();
        let now = t0();
        tr.set("net/#rust", "carol", TypingState::Active, now);
        tr.set("net/#rust", "alice", TypingState::Active, now);
        tr.set("net/#rust", "bob", TypingState::Active, now);
        assert_eq!(tr.nicks("net/#rust"), vec!["alice", "bob", "carol"]);
    }

    #[test]
    fn remove_buffer_drops_everything_for_it() {
        // Closing and reopening a query inside the 30s paused TTL must not
        // resurrect a stale typer.
        let mut tr = TypingTracker::default();
        tr.set("net/alice", "alice", TypingState::Paused, t0());
        assert!(tr.remove_buffer("net/alice"));
        assert!(tr.nicks("net/alice").is_empty());
        assert!(!tr.remove_buffer("net/alice"));
    }

    #[test]
    fn clear_all_reports_every_affected_buffer() {
        let mut tr = TypingTracker::default();
        let now = t0();
        tr.set("net/#rust", "alice", TypingState::Active, now);
        tr.set("net/#tokio", "bob", TypingState::Active, now);
        let mut cleared = tr.clear_all();
        cleared.sort();
        assert_eq!(cleared, vec!["net/#rust", "net/#tokio"]);
        assert!(tr.nicks("net/#rust").is_empty());
    }
}

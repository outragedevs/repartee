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
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TypingEntry {
    pub state: TypingState,
    /// When this state was last refreshed — the TTL clock (spec §1.2).
    pub since: Instant,
    /// The nick as the network spelled it, for display. The map key is this
    /// folded to lowercase — see [`TypingTracker`].
    pub nick: String,
}

/// `buffer_id -> nick -> entry`. Buffer ids are `{conn_id}/{name}`
/// (`src/state/buffer.rs:213`), which is what makes connection-scoped cleanup
/// a prefix match.
///
/// The nick key is **lowercase**, like `Buffer.users`: IRC nicks are
/// case-insensitive and the clears do not all come from the same place the
/// entries do. An entry is created from a TAGMSG's prefix (server-canonical
/// case), but `handle_kick` clears with the KICK's `<user>` *parameter* —
/// whatever case the kicker typed. Matching on exact case would leave a kicked
/// user's indicator up for the full TTL. The entry carries the display spelling
/// so the status line still shows the nick as the network sent it.
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
    /// already shown as typing changes nothing on screen (unless the network has
    /// started spelling them differently, which does).
    pub fn set(&mut self, buffer_id: &str, nick: &str, state: TypingState, now: Instant) -> bool {
        if state == TypingState::Done {
            return self.clear(buffer_id, nick);
        }
        let previous = self.entries.entry(buffer_id.to_string()).or_default().insert(
            nick.to_lowercase(),
            TypingEntry {
                state,
                since: now,
                nick: nick.to_string(),
            },
        );
        previous.is_none_or(|p| p.nick != nick)
    }

    /// Stop showing `nick` as typing in `buffer_id`. Returns `true` if they were.
    pub fn clear(&mut self, buffer_id: &str, nick: &str) -> bool {
        let Some(buf) = self.entries.get_mut(buffer_id) else {
            return false;
        };
        let removed = buf.remove(&nick.to_lowercase()).is_some();
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
        let nick_lower = nick.to_lowercase();
        let mut changed = Vec::new();
        self.entries.retain(|buffer_id, buf| {
            if buffer_id.starts_with(&prefix) && buf.remove(&nick_lower).is_some() {
                changed.push(buffer_id.clone());
            }
            !buf.is_empty()
        });
        changed
    }

    /// Forget every peer typing anywhere **on one connection** — it dropped.
    ///
    /// No `done` can arrive over a socket that is gone, so without this the
    /// indicators would sit there for the full TTL (30s for `paused`) and, worse,
    /// survive into the reconnect. Returns the buffers changed, so the caller can
    /// push the cleared set to the web clients.
    pub fn clear_connection(&mut self, conn_id: &str) -> Vec<String> {
        let prefix = format!("{conn_id}/");
        let mut changed = Vec::new();
        self.entries.retain(|buffer_id, _| {
            if buffer_id.starts_with(&prefix) {
                changed.push(buffer_id.clone());
                return false;
            }
            true
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

    /// Nicks currently typing in `buffer_id`, in their display spelling, sorted
    /// so the status line does not reorder itself between frames.
    #[must_use]
    pub fn nicks(&self, buffer_id: &str) -> Vec<&str> {
        let Some(buf) = self.entries.get(buffer_id) else {
            return Vec::new();
        };
        // Ordered by the lowercase key, not the display spelling: `Bob` must not
        // sort ahead of `alice` just because it is capitalised.
        let mut nicks: Vec<(&str, &str)> = buf
            .iter()
            .map(|(key, entry)| (key.as_str(), entry.nick.as_str()))
            .collect();
        nicks.sort_unstable();
        nicks.into_iter().map(|(_, nick)| nick).collect()
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
    fn clear_connection_drops_every_buffer_on_that_connection() {
        // A connection that drops takes its peers' typing state with it: the
        // socket is gone, no `done` will ever arrive, and the indicator would
        // otherwise sit there for the full 30s paused TTL.
        let mut tr = TypingTracker::default();
        let now = t0();
        tr.set("libera/#rust", "alice", TypingState::Active, now);
        tr.set("libera/bob", "bob", TypingState::Paused, now);
        tr.set("oftc/#rust", "carol", TypingState::Active, now);

        let mut cleared = tr.clear_connection("libera");
        cleared.sort();
        assert_eq!(cleared, vec!["libera/#rust", "libera/bob"]);
        assert!(tr.nicks("libera/#rust").is_empty());
        assert!(tr.nicks("libera/bob").is_empty());
        // Another network is untouched — one connection dropping says nothing
        // about the others.
        assert_eq!(tr.nicks("oftc/#rust"), vec!["carol"]);
        // And a second call has nothing left to report.
        assert!(tr.clear_connection("libera").is_empty());
    }

    #[test]
    fn a_nick_is_matched_case_insensitively_but_displayed_as_the_network_sent_it() {
        // IRC nicks are case-insensitive. The tracker is filled from the TAGMSG
        // prefix, but the clears come from elsewhere: a KICK carries whatever
        // case the kicker typed. Matching by exact case would leave the
        // indicator up for someone who has just been kicked out of the channel.
        let mut tr = TypingTracker::default();
        let now = t0();
        tr.set("net/#rust", "Alice", TypingState::Active, now);
        assert_eq!(tr.nicks("net/#rust"), vec!["Alice"]);

        // Refreshing under a different case is the same person, not a second one.
        tr.set("net/#rust", "ALICE", TypingState::Active, now);
        assert_eq!(tr.nicks("net/#rust").len(), 1);

        assert!(tr.clear("net/#rust", "alice"));
        assert!(tr.nicks("net/#rust").is_empty());
    }

    #[test]
    fn clear_nick_on_connection_is_case_insensitive() {
        let mut tr = TypingTracker::default();
        tr.set("libera/#rust", "Alice", TypingState::Active, t0());
        assert_eq!(
            tr.clear_nick_on_connection("libera", "aLiCe"),
            vec!["libera/#rust"]
        );
        assert!(tr.nicks("libera/#rust").is_empty());
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

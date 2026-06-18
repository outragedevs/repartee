//! `IRCv3` `draft/chathistory` request construction and client-side state.
//!
//! repartee treats chathistory as a background *filler* of the `SQLite` store:
//! requests are built here and sent via `Command::Raw`, and the resulting
//! history batches are quietly ingested (see [`crate::irc::batch`]). The UI
//! always reads from `SQLite`, so this module never touches buffers directly.

// Wired into the binary incrementally across the chathistory tasks (request
// state, batch ingest, scroll-up + reconnect triggers). Matches the
// `#[allow(dead_code)]` convention used by `cap.rs` / `isupport.rs`.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

/// Which reference type to anchor a `CHATHISTORY` request with.
///
/// Chosen from the server's `MSGREFTYPES` ISUPPORT token, preferring `msgid`
/// (stable across clock skew) over `timestamp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    MsgId,
    Timestamp,
}

/// A resolved anchor for a `CHATHISTORY` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryRef {
    /// Rendered as `msgid=<id>`.
    MsgId(String),
    /// Rendered as `timestamp=<rfc3339>`.
    Timestamp(String),
    /// Rendered as `*` — "the most recent messages", only valid with `LATEST`.
    Latest,
}

/// Choose the reference type the server accepts, preferring `msgid`.
///
/// Falls back to `Timestamp` when `msgid` is not advertised (or the list is
/// empty), since every chathistory-capable server supports timestamps.
#[must_use]
pub fn pick_ref_type(msgreftypes: &[String]) -> RefKind {
    if msgreftypes.iter().any(|t| t == "msgid") {
        RefKind::MsgId
    } else {
        RefKind::Timestamp
    }
}

/// Clamp a desired page size to the server-advertised maximum (if any),
/// never returning less than 1.
#[must_use]
pub fn clamp_limit(want: usize, server_max: Option<usize>) -> usize {
    want.min(server_max.unwrap_or(want)).max(1)
}

/// Render the wire string for a single-anchor `CHATHISTORY` request,
/// to be sent via `Command::Raw`.
///
/// Examples:
/// - `CHATHISTORY BEFORE #chan msgid=abc 100`
/// - `CHATHISTORY AFTER #chan timestamp=2024-01-01T00:00:00.000Z 50`
/// - `CHATHISTORY LATEST #chan * 100`
#[must_use]
pub fn build_command(
    subcommand: &str,
    target: &str,
    anchor: &HistoryRef,
    limit: usize,
) -> String {
    let anchor_str = match anchor {
        HistoryRef::MsgId(id) => format!("msgid={id}"),
        HistoryRef::Timestamp(ts) => format!("timestamp={ts}"),
        HistoryRef::Latest => "*".to_string(),
    };
    format!("CHATHISTORY {subcommand} {target} {anchor_str} {limit}")
}

/// Direction of a `CHATHISTORY` request, used both to pick the subcommand and
/// to key in-flight tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Older messages (scroll-up).
    Before,
    /// Newer messages (reconnect gap-fill).
    After,
    /// Most recent messages (reconnect with no local anchor).
    Latest,
}

impl Direction {
    /// The `CHATHISTORY` subcommand keyword for this direction.
    #[must_use]
    pub const fn subcommand(self) -> &'static str {
        match self {
            Self::Before => "BEFORE",
            Self::After => "AFTER",
            Self::Latest => "LATEST",
        }
    }
}

/// Per-connection chathistory request state.
///
/// Prevents duplicate in-flight requests and remembers when a target's
/// server-side history has been exhausted, so scroll-up stops hammering the
/// server once the start of history is reached. Targets are tracked
/// case-insensitively.
#[derive(Debug, Clone, Default)]
pub struct HistoryState {
    /// `(target_lower, direction)` → requested limit, for requests currently
    /// awaiting their batch. The stored limit lets batch completion decide
    /// server-side exhaustion (`rows < limit`).
    in_flight: HashMap<(String, Direction), usize>,
    /// Targets (lowercased) whose `BEFORE` history the server has exhausted.
    before_exhausted: HashSet<String>,
}

impl HistoryState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a request in `dir` for `target` should be sent now.
    ///
    /// False if the cap is disabled, an identical request is already in
    /// flight, or (for `Before`) the server already reported exhaustion.
    #[must_use]
    pub fn should_request(&self, target: &str, dir: Direction, cap_enabled: bool) -> bool {
        if !cap_enabled {
            return false;
        }
        let target = target.to_ascii_lowercase();
        if self.in_flight.contains_key(&(target.clone(), dir)) {
            return false;
        }
        if dir == Direction::Before && self.before_exhausted.contains(&target) {
            return false;
        }
        true
    }

    /// Record that a request (with its requested `limit`) is now in flight.
    /// Returns `false` if an identical request was already tracked (caller
    /// should not send a duplicate).
    pub fn mark_in_flight(&mut self, target: &str, dir: Direction, limit: usize) -> bool {
        self.in_flight
            .insert((target.to_ascii_lowercase(), dir), limit)
            .is_none()
    }

    /// Clear an in-flight request (on failure / explicit cancel).
    pub fn clear_in_flight(&mut self, target: &str, dir: Direction) {
        self.in_flight.remove(&(target.to_ascii_lowercase(), dir));
    }

    /// Mark a target's `BEFORE` history as exhausted (start of history reached,
    /// or a server failure), so we stop requesting older messages for it.
    pub fn mark_before_exhausted(&mut self, target: &str) {
        self.before_exhausted.insert(target.to_ascii_lowercase());
    }

    #[must_use]
    pub fn is_before_exhausted(&self, target: &str) -> bool {
        self.before_exhausted
            .contains(&target.to_ascii_lowercase())
    }

    /// Complete all in-flight requests for `target` after its batch arrived
    /// with `rows` total messages.
    ///
    /// Clears the in-flight markers and, for a `BEFORE` request whose batch
    /// came back short (`rows < requested limit`), records server-side
    /// exhaustion so scroll-up stops asking. Returns `true` if a `BEFORE`
    /// request was completed (the caller can then re-paginate / settle).
    pub fn complete_target(&mut self, target: &str, rows: usize) -> bool {
        let target = target.to_ascii_lowercase();
        let mut had_before = false;
        let completed: Vec<(Direction, usize)> = self
            .in_flight
            .iter()
            .filter(|((t, _), _)| *t == target)
            .map(|((_, dir), limit)| (*dir, *limit))
            .collect();
        for (dir, limit) in completed {
            self.in_flight.remove(&(target.clone(), dir));
            if dir == Direction::Before {
                had_before = true;
                if rows < limit {
                    self.before_exhausted.insert(target.clone());
                }
            }
        }
        had_before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_ref_type_prefers_msgid() {
        assert_eq!(
            pick_ref_type(&["timestamp".into(), "msgid".into()]),
            RefKind::MsgId
        );
    }

    #[test]
    fn pick_ref_type_falls_back_to_timestamp() {
        assert_eq!(pick_ref_type(&["timestamp".into()]), RefKind::Timestamp);
        assert_eq!(pick_ref_type(&[]), RefKind::Timestamp);
    }

    #[test]
    fn clamp_limit_respects_server_max() {
        assert_eq!(clamp_limit(200, Some(100)), 100);
        assert_eq!(clamp_limit(50, Some(100)), 50);
    }

    #[test]
    fn clamp_limit_without_server_max() {
        assert_eq!(clamp_limit(200, None), 200);
    }

    #[test]
    fn clamp_limit_never_zero() {
        assert_eq!(clamp_limit(0, Some(100)), 1);
        assert_eq!(clamp_limit(0, None), 1);
    }

    #[test]
    fn build_before_msgid() {
        let cmd = build_command("BEFORE", "#chan", &HistoryRef::MsgId("abc".into()), 100);
        assert_eq!(cmd, "CHATHISTORY BEFORE #chan msgid=abc 100");
    }

    #[test]
    fn build_after_timestamp() {
        let cmd = build_command(
            "AFTER",
            "#chan",
            &HistoryRef::Timestamp("2024-01-01T00:00:00.000Z".into()),
            50,
        );
        assert_eq!(
            cmd,
            "CHATHISTORY AFTER #chan timestamp=2024-01-01T00:00:00.000Z 50"
        );
    }

    #[test]
    fn build_latest_star() {
        let cmd = build_command("LATEST", "#chan", &HistoryRef::Latest, 100);
        assert_eq!(cmd, "CHATHISTORY LATEST #chan * 100");
    }

    #[test]
    fn direction_subcommands() {
        assert_eq!(Direction::Before.subcommand(), "BEFORE");
        assert_eq!(Direction::After.subcommand(), "AFTER");
        assert_eq!(Direction::Latest.subcommand(), "LATEST");
    }

    #[test]
    fn gating_blocks_when_cap_disabled() {
        let st = HistoryState::new();
        assert!(!st.should_request("#chan", Direction::Before, false));
        assert!(st.should_request("#chan", Direction::Before, true));
    }

    #[test]
    fn gating_blocks_in_flight_same_direction() {
        let mut st = HistoryState::new();
        assert!(st.mark_in_flight("#chan", Direction::Before, 200));
        assert!(!st.should_request("#chan", Direction::Before, true));
        // Different direction is still allowed.
        assert!(st.should_request("#chan", Direction::After, true));
        // Marking the same request again reports the duplicate.
        assert!(!st.mark_in_flight("#chan", Direction::Before, 200));
    }

    #[test]
    fn clear_in_flight_reenables() {
        let mut st = HistoryState::new();
        st.mark_in_flight("#chan", Direction::Before, 200);
        st.clear_in_flight("#chan", Direction::Before);
        assert!(st.should_request("#chan", Direction::Before, true));
    }

    #[test]
    fn short_before_batch_marks_exhausted() {
        let mut st = HistoryState::new();
        st.mark_in_flight("#chan", Direction::Before, 100);
        let had_before = st.complete_target("#chan", 30);
        assert!(had_before);
        assert!(st.is_before_exhausted("#chan"));
        assert!(!st.should_request("#chan", Direction::Before, true));
    }

    #[test]
    fn full_before_batch_not_exhausted() {
        let mut st = HistoryState::new();
        st.mark_in_flight("#chan", Direction::Before, 100);
        st.complete_target("#chan", 100);
        assert!(!st.is_before_exhausted("#chan"));
        // In-flight cleared, so a fresh request is allowed again.
        assert!(st.should_request("#chan", Direction::Before, true));
    }

    #[test]
    fn after_batch_never_exhausts_before() {
        let mut st = HistoryState::new();
        st.mark_in_flight("#chan", Direction::After, 100);
        let had_before = st.complete_target("#chan", 0);
        assert!(!had_before);
        assert!(!st.is_before_exhausted("#chan"));
    }

    #[test]
    fn complete_target_clears_in_flight() {
        let mut st = HistoryState::new();
        st.mark_in_flight("#chan", Direction::After, 50);
        st.complete_target("#chan", 50);
        // After completion the AFTER slot is free for a new gap-fill.
        assert!(st.should_request("#chan", Direction::After, true));
    }

    #[test]
    fn target_tracking_is_case_insensitive() {
        let mut st = HistoryState::new();
        st.mark_in_flight("#Chan", Direction::Before, 200);
        assert!(!st.should_request("#chan", Direction::Before, true));
        st.mark_before_exhausted("#CHAN");
        assert!(st.is_before_exhausted("#chan"));
    }
}

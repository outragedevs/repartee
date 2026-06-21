//! `IRCv3` `draft/chathistory` request construction and client-side state.
//!
//! repartee treats chathistory as a background *filler* of the `SQLite` store:
//! requests are built here and sent via `Command::Raw`, and the resulting
//! history batches are quietly ingested (see [`crate::irc::batch`]). The UI
//! always reads from `SQLite`, so this module never touches buffers directly.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

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

/// Format a Unix timestamp in **milliseconds** as an `IRCv3` `server-time`
/// reference (`YYYY-MM-DDTHH:MM:SS.sssZ`) for a `CHATHISTORY` timestamp anchor.
///
/// Millisecond precision matters: anchoring `BEFORE` at a whole second would
/// skip any messages later in that second that we have not fetched yet.
#[must_use]
pub fn rfc3339_millis(unix_ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(unix_ms)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
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
    /// `(target_lower, direction)` → `(requested limit, sent-at)`, for requests
    /// currently awaiting their batch. The stored limit lets batch completion
    /// decide server-side exhaustion (`rows < limit`); the timestamp lets a
    /// periodic sweep release requests the server rejected or dropped without
    /// ever opening a batch (see [`HistoryState::clear_stale`]).
    in_flight: HashMap<(String, Direction), (usize, Instant)>,
    /// Targets (lowercased) whose `BEFORE` history the server has exhausted.
    before_exhausted: HashSet<String>,
    /// Per-target oldest point we have pulled via chathistory: the oldest
    /// server-time (unix **millis**) seen across **all** batch lines (including
    /// event-playback) paired with that line's IRC `@msgid` (if any). The next
    /// `BEFORE` anchors here — by `@msgid` when the server prefers it, else by
    /// the full-precision timestamp — so scroll-up keeps making progress even
    /// through windows that contain only (un-ingested) event-playback lines,
    /// and never skips messages within the boundary second.
    oldest_fetched: HashMap<String, (i64, Option<String>)>,
}

impl HistoryState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a request in `dir` for `target` should be sent now.
    ///
    /// False if the cap is disabled, **any** request for the target is already
    /// in flight, or (for `Before`) the server already reported exhaustion.
    ///
    /// Requests are serialized per target (any direction): the server does not
    /// echo the subcommand on the returned `BATCH`, so a completing batch can't
    /// be matched back to its request. Allowing a `BEFORE` and an
    /// `AFTER`/`LATEST` to be in flight at once would let the first batch to
    /// return clear both and possibly mark `BEFORE` exhausted using the other
    /// request's row count. Serializing keeps completion unambiguous.
    #[must_use]
    pub fn should_request(&self, target: &str, dir: Direction, cap_enabled: bool) -> bool {
        if !cap_enabled {
            return false;
        }
        let target = target.to_ascii_lowercase();
        if self.in_flight.keys().any(|(t, _)| *t == target) {
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
            .insert((target.to_ascii_lowercase(), dir), (limit, Instant::now()))
            .is_none()
    }

    /// Release in-flight markers older than `timeout` and return the affected
    /// targets (deduplicated). Used by a periodic sweep to recover from
    /// CHATHISTORY requests the server rejected (`FAIL`/error numeric) or
    /// dropped without ever opening a batch: their markers would otherwise wedge
    /// the target so `should_request` suppresses all future history requests
    /// until reconnect. A stale request is a failure, NOT end-of-history, so
    /// this only clears the marker — it never marks `BEFORE` exhausted.
    pub fn clear_stale(&mut self, timeout: Duration) -> Vec<String> {
        let stale: Vec<(String, Direction)> = self
            .in_flight
            .iter()
            .filter(|(_, (_, sent_at))| sent_at.elapsed() >= timeout)
            .map(|(key, _)| key.clone())
            .collect();
        let mut targets: Vec<String> = Vec::new();
        for key in stale {
            if !targets.contains(&key.0) {
                targets.push(key.0.clone());
            }
            self.in_flight.remove(&key);
        }
        targets
    }

    /// Whether **any** request (in any direction) for `target` is awaiting its
    /// batch. Used to serialize CHATHISTORY per target so scroll-up
    /// (`BEFORE`) and reconnect gap-fill (`AFTER`/`LATEST`) never overlap.
    #[must_use]
    pub fn any_in_flight(&self, target: &str) -> bool {
        let target = target.to_ascii_lowercase();
        self.in_flight.keys().any(|(t, _)| *t == target)
    }

    /// The direction of the (single, serialized) in-flight request for
    /// `target`, if any. Read at batch completion to decide whether the
    /// just-fetched rows need splicing into the live buffer (`AFTER`/`LATEST`
    /// gap-fill) or surface through normal pagination (`BEFORE` scroll-back).
    #[must_use]
    pub fn in_flight_direction(&self, target: &str) -> Option<Direction> {
        let target = target.to_ascii_lowercase();
        self.in_flight
            .keys()
            .find(|(t, _)| *t == target)
            .map(|(_, dir)| *dir)
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

    /// Oldest point pulled so far for `target`: `(unix_millis, msgid?)`, if any.
    #[must_use]
    pub fn oldest_fetched(&self, target: &str) -> Option<(i64, Option<String>)> {
        self.oldest_fetched
            .get(&target.to_ascii_lowercase())
            .cloned()
    }

    /// Complete all in-flight requests for `target` after its batch arrived
    /// with `rows` total messages whose oldest line was `oldest` —
    /// `(unix_millis, msgid?)`.
    ///
    /// Clears the in-flight markers, advances the per-target oldest-fetched
    /// watermark (so the next `BEFORE` keeps making progress), and — for a
    /// `BEFORE` request whose batch came back short (`rows < requested limit`)
    /// — records server-side exhaustion so scroll-up stops asking.
    ///
    /// `clean_end` is `true` when the batch closed normally (`BATCH -tag`) and
    /// `false` when it was force-completed by the timeout purge. A timed-out
    /// batch is a transport/server failure, not proof that history ended, so a
    /// short timed-out `BEFORE` batch still clears in-flight (allowing a retry)
    /// but is NEVER marked exhausted — otherwise a single dropped `BATCH -tag`
    /// would wedge the target with no older history until reconnect.
    pub fn complete_target(
        &mut self,
        target: &str,
        rows: usize,
        oldest: Option<(i64, Option<String>)>,
        clean_end: bool,
    ) {
        let target = target.to_ascii_lowercase();

        let completed: Vec<(Direction, usize)> = self
            .in_flight
            .keys()
            .filter(|(t, _)| *t == target)
            .map(|(_, dir)| *dir)
            .map(|dir| (dir, self.in_flight[&(target.clone(), dir)].0))
            .collect();

        // Only a BEFORE completion advances the scroll-back watermark: it tracks
        // how far back scroll-up has reached. An AFTER/LATEST gap-fill pulls
        // RECENT messages near the reconnect gap, so seeding the watermark from
        // it would make the next BEFORE anchor there and replay already-local
        // history page by page (scrollback appears stuck).
        let advances_watermark = completed.iter().any(|(dir, _)| *dir == Direction::Before);
        if advances_watermark
            && let Some((ts, msgid)) = oldest
        {
            self.oldest_fetched
                .entry(target.clone())
                // Keep whichever line is older; take its msgid alongside.
                .and_modify(|cur| {
                    if ts < cur.0 {
                        *cur = (ts, msgid.clone());
                    }
                })
                .or_insert((ts, msgid));
        }
        for (dir, limit) in completed {
            self.in_flight.remove(&(target.clone(), dir));
            if clean_end && dir == Direction::Before && rows < limit {
                self.mark_before_exhausted(&target);
            }
        }
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
    fn gating_serializes_requests_per_target() {
        let mut st = HistoryState::new();
        assert!(st.mark_in_flight("#chan", Direction::Before, 200));
        assert!(!st.should_request("#chan", Direction::Before, true));
        // A different direction is blocked too: requests are serialized per
        // target so a returning batch is never mis-attributed.
        assert!(!st.should_request("#chan", Direction::After, true));
        assert!(!st.should_request("#chan", Direction::Latest, true));
        // A different target is unaffected.
        assert!(st.should_request("#other", Direction::After, true));
        // Marking the same request again reports the duplicate.
        assert!(!st.mark_in_flight("#chan", Direction::Before, 200));
        // After completion the target frees up for any direction again.
        st.complete_target("#chan", 200, Some((1000, None)), true);
        assert!(st.should_request("#chan", Direction::After, true));
    }

    #[test]
    fn any_in_flight_spans_directions() {
        let mut st = HistoryState::new();
        assert!(!st.any_in_flight("#chan"));
        st.mark_in_flight("#chan", Direction::After, 50);
        assert!(st.any_in_flight("#chan"));
        assert!(!st.any_in_flight("#other"));
        st.complete_target("#chan", 50, Some((1000, None)), true);
        assert!(!st.any_in_flight("#chan"));
    }

    #[test]
    fn short_before_batch_marks_exhausted() {
        let mut st = HistoryState::new();
        st.mark_in_flight("#chan", Direction::Before, 100);
        st.complete_target("#chan", 30, Some((1000, None)), true);
        assert!(st.is_before_exhausted("#chan"));
        assert!(!st.should_request("#chan", Direction::Before, true));
    }

    #[test]
    fn clear_stale_releases_in_flight_after_timeout() {
        // A request that the server rejected (FAIL/error numeric) or silently
        // dropped never completes a batch, so its in-flight marker would wedge
        // the target forever. A periodic stale sweep must release it WITHOUT
        // marking history exhausted (a failure isn't end-of-history). A zero
        // timeout makes the just-marked request immediately stale.
        let mut st = HistoryState::new();
        st.mark_in_flight("#chan", Direction::Before, 200);
        assert!(!st.should_request("#chan", Direction::Before, true));

        let cleared = st.clear_stale(std::time::Duration::ZERO);

        assert_eq!(cleared, vec!["#chan".to_string()]);
        assert!(
            st.should_request("#chan", Direction::Before, true),
            "retry allowed once the stuck marker is cleared"
        );
        assert!(
            !st.is_before_exhausted("#chan"),
            "a failed request must not exhaust history"
        );
    }

    #[test]
    fn in_flight_direction_reports_the_pending_request() {
        // Requests are serialized per target, so at most one direction is in
        // flight. The batch-completion path reads it (before clearing) to know
        // whether to splice an AFTER/LATEST gap-fill into the live buffer.
        let mut st = HistoryState::new();
        assert_eq!(st.in_flight_direction("#chan"), None);
        st.mark_in_flight("#chan", Direction::After, 50);
        assert_eq!(st.in_flight_direction("#chan"), Some(Direction::After));
        assert_eq!(st.in_flight_direction("#CHAN"), Some(Direction::After));
        assert_eq!(st.in_flight_direction("#other"), None);
    }

    #[test]
    fn clear_stale_keeps_fresh_in_flight() {
        // A request still within its timeout window is left alone — a batch may
        // still be on its way.
        let mut st = HistoryState::new();
        st.mark_in_flight("#chan", Direction::After, 200);
        let cleared = st.clear_stale(std::time::Duration::from_secs(90));
        assert!(cleared.is_empty());
        assert!(!st.should_request("#chan", Direction::After, true));
    }

    #[test]
    fn timed_out_short_before_batch_stays_fetchable() {
        // A CHATHISTORY batch that times out (its closing `BATCH -tag` never
        // arrived) with fewer rows than requested is a transport/server
        // failure, NOT proof that history ended. It must clear the in-flight
        // marker (so scroll-up can retry) WITHOUT marking BEFORE exhausted —
        // otherwise the target is wedged with no older history until reconnect.
        let mut st = HistoryState::new();
        st.mark_in_flight("#chan", Direction::Before, 100);
        st.complete_target("#chan", 30, Some((1000, None)), false);
        assert!(
            !st.is_before_exhausted("#chan"),
            "a timed-out short batch must not exhaust history"
        );
        assert!(
            st.should_request("#chan", Direction::Before, true),
            "in-flight cleared so a later scroll-up retries the fetch"
        );
    }

    #[test]
    fn full_before_batch_not_exhausted() {
        let mut st = HistoryState::new();
        st.mark_in_flight("#chan", Direction::Before, 100);
        st.complete_target("#chan", 100, Some((1000, None)), true);
        assert!(!st.is_before_exhausted("#chan"));
        // In-flight cleared, so a fresh request is allowed again.
        assert!(st.should_request("#chan", Direction::Before, true));
    }

    #[test]
    fn after_batch_never_exhausts_before() {
        let mut st = HistoryState::new();
        st.mark_in_flight("#chan", Direction::After, 100);
        st.complete_target("#chan", 0, Some((1000, None)), true);
        assert!(!st.is_before_exhausted("#chan"));
    }

    #[test]
    fn complete_target_clears_in_flight() {
        let mut st = HistoryState::new();
        st.mark_in_flight("#chan", Direction::After, 50);
        st.complete_target("#chan", 50, Some((1000, None)), true);
        // After completion the AFTER slot is free for a new gap-fill.
        assert!(st.should_request("#chan", Direction::After, true));
    }

    #[test]
    fn after_gapfill_does_not_seed_before_watermark() {
        // A reconnect AFTER gap-fill pulls recent messages near the reconnect
        // gap. Its oldest line must NOT become the BEFORE scroll-back watermark,
        // or the next BEFORE would anchor there and replay already-local history
        // page by page (scrollback appears stuck).
        let mut st = HistoryState::new();
        st.mark_in_flight("#chan", Direction::After, 100);
        st.complete_target("#chan", 100, Some((9000, Some("recent".into()))), true);
        assert_eq!(
            st.oldest_fetched("#chan"),
            None,
            "AFTER/LATEST must not seed the BEFORE watermark"
        );
    }

    #[test]
    fn oldest_fetched_advances_to_minimum() {
        let mut st = HistoryState::new();
        assert_eq!(st.oldest_fetched("#chan"), None);
        st.mark_in_flight("#chan", Direction::Before, 100);
        st.complete_target("#chan", 100, Some((5000, Some("m5".into()))), true);
        assert_eq!(st.oldest_fetched("#chan"), Some((5000, Some("m5".into()))));
        // A later, older page lowers the watermark (and adopts its msgid); a
        // newer one does not raise it.
        st.mark_in_flight("#chan", Direction::Before, 100);
        st.complete_target("#chan", 100, Some((3000, Some("m3".into()))), true);
        assert_eq!(st.oldest_fetched("#chan"), Some((3000, Some("m3".into()))));
        st.mark_in_flight("#chan", Direction::Before, 100);
        st.complete_target("#chan", 100, Some((9000, Some("m9".into()))), true);
        assert_eq!(st.oldest_fetched("#chan"), Some((3000, Some("m3".into()))));
    }

    #[test]
    fn rfc3339_millis_format() {
        // Input is milliseconds: 2024-01-01T00:00:00.000Z
        assert_eq!(rfc3339_millis(1_704_067_200_000), "2024-01-01T00:00:00.000Z");
        // Subsecond precision is preserved (not floored to the second).
        assert_eq!(rfc3339_millis(1_704_067_200_500), "2024-01-01T00:00:00.500Z");
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

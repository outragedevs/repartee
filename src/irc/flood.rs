// Flood protection — antiflood detection for CTCP, per-nick tilde rate,
// PM tilde storm, duplicate text, and nick changes.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant};

/// Result of a flood check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloodResult {
    /// Message is allowed through.
    Allow,
    /// Flood just triggered — caller should show ONE notification.
    Triggered,
    /// Already blocking — suppress silently (no notification).
    Blocked,
}

impl FloodResult {
    /// Returns `true` if the message should be suppressed.
    pub const fn suppressed(self) -> bool {
        matches!(self, Self::Triggered | Self::Blocked)
    }
}

// === Constants ===

const CTCP_THRESHOLD: usize = 5;
const CTCP_WINDOW: Duration = Duration::from_secs(5);
const CTCP_BLOCK: Duration = Duration::from_secs(60);

// Per-nick tilde rate limit — blocks only the offending nick.
const TILDE_NICK_THRESHOLD: usize = 5;
const TILDE_NICK_WINDOW: Duration = Duration::from_secs(5);
const TILDE_NICK_BLOCK: Duration = Duration::from_secs(60);

// PM tilde storm — many different ~ nicks PMing us = botnet.
const PM_STORM_THRESHOLD: usize = 6;
const PM_STORM_WINDOW: Duration = Duration::from_secs(5);
const PM_STORM_BLOCK: Duration = Duration::from_secs(60);

const DUP_MIN_IN_WINDOW: usize = 5;
const DUP_THRESHOLD: usize = 3;
const DUP_WINDOW: Duration = Duration::from_secs(5);
const DUP_BLOCK: Duration = Duration::from_secs(60);

const NICK_THRESHOLD: usize = 5;
const NICK_WINDOW: Duration = Duration::from_secs(3);
const NICK_BLOCK: Duration = Duration::from_secs(60);

// === Per-connection state ===

/// Tracks flood detection state for a single IRC connection.
pub struct FloodState {
    // CTCP flood (global — CTCPs are rare)
    ctcp_times: Vec<Instant>,
    ctcp_blocked_until: Option<Instant>,

    // Per-nick tilde rate limit — only blocks the flooding nick
    tilde_nick_times: HashMap<u64, Vec<Instant>>,   // nick_hash -> timestamps
    tilde_nick_blocked: HashMap<u64, Instant>,       // nick_hash -> blocked_until

    // PM tilde storm — tracks unique ~ nicks PMing us
    pm_storm_nicks: Vec<(u64, Instant)>,             // (nick_hash, timestamp)
    pm_storm_blocked_until: Option<Instant>,

    // Duplicate text flood — per-text-hash
    msg_window: Vec<(u64, Instant)>,
    blocked_texts: HashMap<u64, Instant>,

    // Nick change flood (per buffer)
    nick_times: HashMap<String, Vec<Instant>>,
    nick_blocked_until: HashMap<String, Instant>,
}

impl FloodState {
    /// Create a new, empty flood detection state.
    pub fn new() -> Self {
        Self {
            ctcp_times: Vec::new(),
            ctcp_blocked_until: None,
            tilde_nick_times: HashMap::new(),
            tilde_nick_blocked: HashMap::new(),
            pm_storm_nicks: Vec::new(),
            pm_storm_blocked_until: None,
            msg_window: Vec::new(),
            blocked_texts: HashMap::new(),
            nick_times: HashMap::new(),
            nick_blocked_until: HashMap::new(),
        }
    }

    /// Check for CTCP flood.
    pub fn check_ctcp_flood(&mut self, now: Instant) -> FloodResult {
        if let Some(until) = self.ctcp_blocked_until {
            if now < until {
                self.ctcp_blocked_until = Some(now + CTCP_BLOCK);
                return FloodResult::Blocked;
            }
            self.ctcp_blocked_until = None;
        }

        self.ctcp_times.push(now);
        let count = prune_window(&mut self.ctcp_times, now, CTCP_WINDOW);

        if count >= CTCP_THRESHOLD {
            self.ctcp_blocked_until = Some(now + CTCP_BLOCK);
            self.ctcp_times.clear();
            return FloodResult::Triggered;
        }

        FloodResult::Allow
    }

    /// Per-nick tilde rate limit. Blocks only the specific nick that floods.
    /// Replaces the old global tilde counter that caused collateral damage.
    pub fn check_tilde_nick_flood(&mut self, nick: &str, now: Instant) -> FloodResult {
        let nick_hash = hash_text(nick);

        // Already blocked for this nick?
        if let Some(&until) = self.tilde_nick_blocked.get(&nick_hash) {
            if now < until {
                self.tilde_nick_blocked.insert(nick_hash, now + TILDE_NICK_BLOCK);
                return FloodResult::Blocked;
            }
            self.tilde_nick_blocked.remove(&nick_hash);
        }

        let times = self.tilde_nick_times.entry(nick_hash).or_default();
        times.push(now);
        let count = prune_window(times, now, TILDE_NICK_WINDOW);

        if count >= TILDE_NICK_THRESHOLD {
            self.tilde_nick_blocked.insert(nick_hash, now + TILDE_NICK_BLOCK);
            times.clear();
            return FloodResult::Triggered;
        }

        // Periodic cleanup of idle nicks (avoid unbounded growth).
        // Prune timestamps inside each Vec before checking emptiness,
        // otherwise stale entries with expired-but-non-empty Vecs survive.
        if self.tilde_nick_times.len() > 200 {
            let cutoff = now.checked_sub(TILDE_NICK_WINDOW).unwrap_or(now);
            self.tilde_nick_times.retain(|_, v| {
                v.retain(|t| *t >= cutoff);
                !v.is_empty()
            });
            self.tilde_nick_blocked.retain(|_, until| *until > now);
        }

        FloodResult::Allow
    }

    /// PM tilde storm: detects many different `~` ident nicks `PM`ing us.
    /// Counts unique nick hashes in a sliding window. When 6+ unique `~`
    /// nicks PM us within 5 seconds, blocks ALL incoming `~` PMs for 60s.
    /// Channel messages are never affected.
    pub fn check_pm_tilde_storm(&mut self, nick: &str, now: Instant) -> FloodResult {
        // Already in storm block?
        if let Some(until) = self.pm_storm_blocked_until {
            if now < until {
                self.pm_storm_blocked_until = Some(now + PM_STORM_BLOCK);
                return FloodResult::Blocked;
            }
            self.pm_storm_blocked_until = None;
        }

        let nick_hash = hash_text(nick);
        self.pm_storm_nicks.push((nick_hash, now));

        // Prune entries outside the window
        let cutoff = now.checked_sub(PM_STORM_WINDOW).unwrap_or(now);
        self.pm_storm_nicks.retain(|(_, t)| *t >= cutoff);

        // Count unique nicks in the window
        let unique = unique_count(&self.pm_storm_nicks);
        if unique >= PM_STORM_THRESHOLD {
            self.pm_storm_blocked_until = Some(now + PM_STORM_BLOCK);
            self.pm_storm_nicks.clear();
            return FloodResult::Triggered;
        }

        FloodResult::Allow
    }

    /// Check for duplicate text flood. Only for channel messages.
    pub fn check_duplicate_flood(
        &mut self,
        text: &str,
        is_channel: bool,
        now: Instant,
    ) -> FloodResult {
        if !is_channel || text.is_empty() {
            return FloodResult::Allow;
        }

        let hash = hash_text(text);

        if let Some(&until) = self.blocked_texts.get(&hash)
            && now < until
        {
            self.blocked_texts.insert(hash, now + DUP_BLOCK);
            return FloodResult::Blocked;
        }

        self.msg_window.push((hash, now));

        let cutoff = now.checked_sub(DUP_WINDOW).unwrap_or(now);
        self.msg_window.retain(|(_, t)| *t >= cutoff);

        if self.msg_window.len() >= DUP_MIN_IN_WINDOW {
            let dupes = self.msg_window.iter().filter(|(h, _)| *h == hash).count();
            if dupes >= DUP_THRESHOLD {
                self.blocked_texts.insert(hash, now + DUP_BLOCK);
                return FloodResult::Triggered;
            }
        }

        if self.blocked_texts.len() > 50 {
            self.blocked_texts.retain(|_, until| *until > now);
        }

        FloodResult::Allow
    }

    /// Check for nick change flood in a specific buffer.
    pub fn should_suppress_nick_flood(&mut self, buffer_id: &str, now: Instant) -> bool {
        // Already blocked — extend without re-allocating the key
        if let Some(until) = self.nick_blocked_until.get_mut(buffer_id) {
            if now < *until {
                *until = now + NICK_BLOCK;
                return true;
            }
            self.nick_blocked_until.remove(buffer_id);
        }

        let times = self.nick_times.entry(buffer_id.to_string()).or_default();
        times.push(now);
        prune_window(times, now, NICK_WINDOW);

        if times.len() >= NICK_THRESHOLD {
            self.nick_blocked_until
                .insert(buffer_id.to_string(), now + NICK_BLOCK);
            times.clear();
            return true;
        }

        // Periodic cleanup of idle buffers (avoid unbounded growth).
        if self.nick_times.len() > 200 {
            let cutoff = now.checked_sub(NICK_WINDOW).unwrap_or(now);
            self.nick_times.retain(|_, v| {
                v.retain(|t| *t >= cutoff);
                !v.is_empty()
            });
            self.nick_blocked_until.retain(|_, until| *until > now);
        }

        false
    }

    /// Remove all tracking data for a buffer that has been closed.
    pub fn remove_buffer(&mut self, buffer_id: &str) {
        self.nick_times.remove(buffer_id);
        self.nick_blocked_until.remove(buffer_id);
    }
}

impl Default for FloodState {
    fn default() -> Self {
        Self::new()
    }
}

/// Hash a string into a u64 fingerprint for flood tracking.
fn hash_text(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Count unique u64 values in a slice of `(hash, timestamp)` pairs.
fn unique_count(entries: &[(u64, Instant)]) -> usize {
    // For small windows (<50 entries), linear scan is faster than HashSet.
    let mut seen: Vec<u64> = Vec::with_capacity(entries.len());
    for &(hash, _) in entries {
        if !seen.contains(&hash) {
            seen.push(hash);
        }
    }
    seen.len()
}

/// Remove timestamps older than `window` from the front of `times`.
/// Returns the number of remaining entries.
pub fn prune_window(times: &mut Vec<Instant>, now: Instant, window: Duration) -> usize {
    let cutoff = now.checked_sub(window).unwrap_or(now);
    let mut i = 0;
    while i < times.len() && times[i] < cutoff {
        i += 1;
    }
    if i > 0 {
        times.drain(..i);
    }
    times.len()
}

// === Tests ===

#[cfg(test)]
mod tests {
    use super::*;

    // --- CTCP flood ---

    #[test]
    fn ctcp_under_threshold_passes() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..4 {
            let t = now + Duration::from_millis(i * 100);
            assert_eq!(
                state.check_ctcp_flood(t),
                FloodResult::Allow,
                "request {i} should pass"
            );
        }
    }

    #[test]
    fn ctcp_at_threshold_triggers() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..4 {
            assert_eq!(
                state.check_ctcp_flood(now + Duration::from_millis(i * 100)),
                FloodResult::Allow
            );
        }
        assert_eq!(
            state.check_ctcp_flood(now + Duration::from_millis(400)),
            FloodResult::Triggered
        );
    }

    #[test]
    fn ctcp_block_extends_on_continued_flood() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..5 {
            state.check_ctcp_flood(now + Duration::from_millis(i * 100));
        }
        assert_eq!(
            state.check_ctcp_flood(now + Duration::from_secs(30)),
            FloodResult::Blocked
        );
    }

    #[test]
    fn ctcp_block_expires() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..5 {
            state.check_ctcp_flood(now + Duration::from_millis(i * 100));
        }
        assert_eq!(
            state.check_ctcp_flood(now + Duration::from_secs(61)),
            FloodResult::Allow
        );
    }

    #[test]
    fn ctcp_outside_window_does_not_trigger() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..5 {
            let t = now + Duration::from_secs(i * 3);
            assert_eq!(state.check_ctcp_flood(t), FloodResult::Allow);
        }
    }

    // --- Per-nick tilde rate limit ---

    #[test]
    fn tilde_nick_under_threshold_passes() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..4 {
            assert_eq!(
                state.check_tilde_nick_flood("jim", now + Duration::from_millis(i * 100)),
                FloodResult::Allow,
                "message {i} from jim should pass"
            );
        }
    }

    #[test]
    fn tilde_nick_at_threshold_blocks_only_that_nick() {
        let mut state = FloodState::new();
        let now = Instant::now();
        // jim sends 5 messages in 5s — triggers block
        for i in 0..5 {
            state.check_tilde_nick_flood("jim", now + Duration::from_millis(i * 100));
        }
        // jim is now blocked
        assert_eq!(
            state.check_tilde_nick_flood("jim", now + Duration::from_secs(1)),
            FloodResult::Blocked
        );
        // ripsum is NOT blocked — different nick
        assert_eq!(
            state.check_tilde_nick_flood("ripsum", now + Duration::from_secs(1)),
            FloodResult::Allow
        );
    }

    #[test]
    fn tilde_nick_block_expires() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..5 {
            state.check_tilde_nick_flood("jim", now + Duration::from_millis(i * 100));
        }
        assert_eq!(
            state.check_tilde_nick_flood("jim", now + Duration::from_secs(61)),
            FloodResult::Allow,
        );
    }

    #[test]
    fn tilde_nick_different_nicks_independent() {
        let mut state = FloodState::new();
        let now = Instant::now();
        // 4 messages from jim, 4 from alice — neither hits threshold
        for i in 0..4 {
            let t = now + Duration::from_millis(i * 100);
            assert_eq!(state.check_tilde_nick_flood("jim", t), FloodResult::Allow);
            assert_eq!(state.check_tilde_nick_flood("alice", t), FloodResult::Allow);
        }
    }

    #[test]
    fn tilde_nick_outside_window_does_not_trigger() {
        let mut state = FloodState::new();
        let now = Instant::now();
        // Spread 5 messages over 10 seconds (window is 5s)
        for i in 0..5 {
            let t = now + Duration::from_secs(i * 3);
            assert_eq!(
                state.check_tilde_nick_flood("jim", t),
                FloodResult::Allow
            );
        }
    }

    // --- PM tilde storm ---

    #[test]
    fn pm_storm_under_threshold_passes() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..5 {
            assert_eq!(
                state.check_pm_tilde_storm(
                    &format!("bot{i}"),
                    now + Duration::from_millis(i * 100)
                ),
                FloodResult::Allow,
                "PM from bot{i} should pass"
            );
        }
    }

    #[test]
    fn pm_storm_at_threshold_triggers() {
        let mut state = FloodState::new();
        let now = Instant::now();
        // 5 unique nicks PM us — still under threshold (6)
        for i in 0..5 {
            assert_eq!(
                state.check_pm_tilde_storm(
                    &format!("bot{i}"),
                    now + Duration::from_millis(i * 100)
                ),
                FloodResult::Allow,
            );
        }
        // 6th unique nick triggers
        assert_eq!(
            state.check_pm_tilde_storm("bot5", now + Duration::from_millis(500)),
            FloodResult::Triggered
        );
    }

    #[test]
    fn pm_storm_same_nick_does_not_inflate_count() {
        let mut state = FloodState::new();
        let now = Instant::now();
        // Same nick PMs 10 times — only 1 unique nick, never triggers storm
        for i in 0..10 {
            assert_eq!(
                state.check_pm_tilde_storm("spammer", now + Duration::from_millis(i * 50)),
                FloodResult::Allow,
                "same nick repeat {i} should not trigger storm"
            );
        }
    }

    #[test]
    fn pm_storm_block_expires() {
        let mut state = FloodState::new();
        let now = Instant::now();
        // Trigger storm block
        for i in 0..6 {
            state.check_pm_tilde_storm(
                &format!("bot{i}"),
                now + Duration::from_millis(i * 100),
            );
        }
        // Still blocked at 30s (extends block to 30s + 60s = 90s)
        assert_eq!(
            state.check_pm_tilde_storm("late_bot", now + Duration::from_secs(30)),
            FloodResult::Blocked
        );
        // Still blocked at 61s (90s hasn't passed yet)
        assert_eq!(
            state.check_pm_tilde_storm("another", now + Duration::from_secs(61)),
            FloodResult::Blocked
        );
        // Expired after 122s (61s extended to 121s, 122 > 121)
        assert_eq!(
            state.check_pm_tilde_storm("legit_user", now + Duration::from_secs(122)),
            FloodResult::Allow
        );
    }

    #[test]
    fn pm_storm_outside_window_does_not_trigger() {
        let mut state = FloodState::new();
        let now = Instant::now();
        // 6 unique nicks but spread over 12 seconds (window is 5s)
        for i in 0..6 {
            assert_eq!(
                state.check_pm_tilde_storm(
                    &format!("user{i}"),
                    now + Duration::from_secs(i * 3)
                ),
                FloodResult::Allow,
                "user{i} at {i}*3s should pass"
            );
        }
    }

    // --- Duplicate text ---

    #[test]
    fn duplicate_non_channel_ignored() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..10 {
            assert_eq!(
                state.check_duplicate_flood(
                    "same text",
                    false,
                    now + Duration::from_millis(i * 100)
                ),
                FloodResult::Allow
            );
        }
    }

    #[test]
    fn duplicate_empty_text_ignored() {
        let mut state = FloodState::new();
        assert_eq!(
            state.check_duplicate_flood("", true, Instant::now()),
            FloodResult::Allow
        );
    }

    #[test]
    fn duplicate_below_window_size_passes() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..4 {
            assert_eq!(
                state.check_duplicate_flood("spam", true, now + Duration::from_millis(i * 100)),
                FloodResult::Allow
            );
        }
    }

    #[test]
    fn duplicate_at_threshold_triggers() {
        let mut state = FloodState::new();
        let now = Instant::now();
        assert_eq!(state.check_duplicate_flood("spam", true, now), FloodResult::Allow);
        assert_eq!(
            state.check_duplicate_flood("other1", true, now + Duration::from_millis(100)),
            FloodResult::Allow
        );
        assert_eq!(
            state.check_duplicate_flood("spam", true, now + Duration::from_millis(200)),
            FloodResult::Allow
        );
        assert_eq!(
            state.check_duplicate_flood("other2", true, now + Duration::from_millis(300)),
            FloodResult::Allow
        );
        assert_eq!(
            state.check_duplicate_flood("spam", true, now + Duration::from_millis(400)),
            FloodResult::Triggered
        );
    }

    #[test]
    fn duplicate_blocked_text_stays_blocked() {
        let mut state = FloodState::new();
        let now = Instant::now();
        assert_eq!(state.check_duplicate_flood("spam", true, now), FloodResult::Allow);
        assert_eq!(
            state.check_duplicate_flood("a", true, now + Duration::from_millis(100)),
            FloodResult::Allow
        );
        assert_eq!(
            state.check_duplicate_flood("spam", true, now + Duration::from_millis(200)),
            FloodResult::Allow
        );
        assert_eq!(
            state.check_duplicate_flood("b", true, now + Duration::from_millis(300)),
            FloodResult::Allow
        );
        assert_eq!(
            state.check_duplicate_flood("spam", true, now + Duration::from_millis(400)),
            FloodResult::Triggered
        );
        assert_eq!(
            state.check_duplicate_flood("spam", true, now + Duration::from_secs(10)),
            FloodResult::Blocked
        );
    }

    #[test]
    fn duplicate_different_texts_pass() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..10 {
            assert_eq!(
                state.check_duplicate_flood(
                    &format!("unique msg {i}"),
                    true,
                    now + Duration::from_millis(i * 100)
                ),
                FloodResult::Allow
            );
        }
    }

    // --- Nick change flood ---

    #[test]
    fn nick_under_threshold_passes() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..4 {
            assert!(
                !state.should_suppress_nick_flood("conn/chan", now + Duration::from_millis(i * 100))
            );
        }
    }

    #[test]
    fn nick_at_threshold_triggers() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..5 {
            let result = state
                .should_suppress_nick_flood("conn/#channel", now + Duration::from_millis(i * 100));
            if i < 4 {
                assert!(!result, "nick change {i} should pass");
            } else {
                assert!(result, "nick change {i} should trigger");
            }
        }
    }

    #[test]
    fn nick_different_buffers_independent() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..4 {
            assert!(
                !state.should_suppress_nick_flood("buf_a", now + Duration::from_millis(i * 100))
            );
        }
        assert!(!state.should_suppress_nick_flood("buf_b", now + Duration::from_millis(500)));
    }

    #[test]
    fn nick_block_extends_on_continued_flood() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..5 {
            state.should_suppress_nick_flood("buf", now + Duration::from_millis(i * 100));
        }
        assert!(state.should_suppress_nick_flood("buf", now + Duration::from_secs(30)));
    }

    #[test]
    fn nick_block_expires() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..5 {
            state.should_suppress_nick_flood("buf", now + Duration::from_millis(i * 100));
        }
        assert!(!state.should_suppress_nick_flood("buf", now + Duration::from_secs(61)));
    }

    #[test]
    fn nick_outside_window_does_not_trigger() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..5 {
            let t = now + Duration::from_millis(i * 1500);
            assert!(
                !state.should_suppress_nick_flood("buf", t),
                "nick change at {i}*1.5s should pass"
            );
        }
    }

    // --- Utility ---

    #[test]
    fn prune_window_removes_old_entries() {
        let now = Instant::now();
        let mut times = vec![
            now.checked_sub(Duration::from_secs(10)).unwrap(),
            now.checked_sub(Duration::from_secs(8)).unwrap(),
            now.checked_sub(Duration::from_secs(3)).unwrap(),
            now.checked_sub(Duration::from_secs(1)).unwrap(),
            now,
        ];
        let count = prune_window(&mut times, now, Duration::from_secs(5));
        assert_eq!(count, 3);
    }

    #[test]
    fn prune_window_empty_vec() {
        let mut times: Vec<Instant> = Vec::new();
        let count = prune_window(&mut times, Instant::now(), Duration::from_secs(5));
        assert_eq!(count, 0);
    }

    #[test]
    fn prune_window_all_recent() {
        let now = Instant::now();
        let mut times = vec![now, now, now];
        let count = prune_window(&mut times, now, Duration::from_secs(5));
        assert_eq!(count, 3);
    }

    #[test]
    fn prune_window_all_expired() {
        let now = Instant::now();
        let mut times = vec![
            now.checked_sub(Duration::from_secs(20)).unwrap(),
            now.checked_sub(Duration::from_secs(15)).unwrap(),
            now.checked_sub(Duration::from_secs(10)).unwrap(),
        ];
        let count = prune_window(&mut times, now, Duration::from_secs(5));
        assert_eq!(count, 0);
    }

    #[test]
    fn unique_count_deduplicates() {
        let now = Instant::now();
        let entries = vec![
            (1, now),
            (2, now),
            (1, now), // duplicate
            (3, now),
            (2, now), // duplicate
        ];
        assert_eq!(unique_count(&entries), 3);
    }

    #[test]
    fn default_impl_matches_new() {
        let a = FloodState::new();
        let b = FloodState::default();
        assert!(a.ctcp_times.is_empty());
        assert!(b.ctcp_times.is_empty());
        assert!(a.ctcp_blocked_until.is_none());
        assert!(b.ctcp_blocked_until.is_none());
    }

    #[test]
    fn remove_buffer_cleans_nick_tracking() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..3 {
            state.should_suppress_nick_flood("conn/#old", now + Duration::from_millis(i * 100));
        }
        assert!(state.nick_times.contains_key("conn/#old"));

        state.remove_buffer("conn/#old");
        assert!(!state.nick_times.contains_key("conn/#old"));
        assert!(!state.nick_blocked_until.contains_key("conn/#old"));
    }
}

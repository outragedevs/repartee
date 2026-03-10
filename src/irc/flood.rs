// Flood protection — antiflood detection for CTCP, tilde-ident, duplicate text, and nick changes.

use std::collections::HashMap;
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

// === Constants (proven thresholds from kokoirc/erssi) ===

const CTCP_THRESHOLD: usize = 5;
const CTCP_WINDOW: Duration = Duration::from_secs(5);
const CTCP_BLOCK: Duration = Duration::from_secs(60);

const TILDE_THRESHOLD: usize = 5;
const TILDE_WINDOW: Duration = Duration::from_secs(5);
const TILDE_BLOCK: Duration = Duration::from_secs(60);

const DUP_MIN_IN_WINDOW: usize = 5; // need 5+ msgs in window before checking dups
const DUP_THRESHOLD: usize = 3; // 3 identical out of those = flood
const DUP_WINDOW: Duration = Duration::from_secs(5);
const DUP_BLOCK: Duration = Duration::from_secs(60);

const NICK_THRESHOLD: usize = 5;
const NICK_WINDOW: Duration = Duration::from_secs(3);
const NICK_BLOCK: Duration = Duration::from_secs(60);

// === Per-connection state ===

/// Tracks flood detection state for a single IRC connection.
pub struct FloodState {
    // CTCP flood
    ctcp_times: Vec<Instant>,
    ctcp_blocked_until: Option<Instant>,

    // Tilde (~ident) flood
    tilde_times: Vec<Instant>,
    tilde_blocked_until: Option<Instant>,

    // Duplicate text flood
    msg_window: Vec<(String, Instant)>, // (text, timestamp)
    blocked_texts: HashMap<String, Instant>, // text -> blocked_until

    // Nick change flood (per buffer)
    nick_times: HashMap<String, Vec<Instant>>, // buffer_id -> timestamps
    nick_blocked_until: HashMap<String, Instant>, // buffer_id -> blocked_until
}

impl FloodState {
    /// Create a new, empty flood detection state.
    pub fn new() -> Self {
        Self {
            ctcp_times: Vec::new(),
            ctcp_blocked_until: None,
            tilde_times: Vec::new(),
            tilde_blocked_until: None,
            msg_window: Vec::new(),
            blocked_texts: HashMap::new(),
            nick_times: HashMap::new(),
            nick_blocked_until: HashMap::new(),
        }
    }

    /// Check for CTCP flood. Returns whether the message is allowed, just triggered, or already blocked.
    pub fn check_ctcp_flood(&mut self, now: Instant) -> FloodResult {
        // If currently blocked, extend the block and suppress silently
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

    /// Check for tilde (~ident) flood. Returns whether the message is allowed, just triggered, or already blocked.
    pub fn check_tilde_flood(&mut self, now: Instant) -> FloodResult {
        // If currently blocked, extend the block and suppress silently
        if let Some(until) = self.tilde_blocked_until {
            if now < until {
                self.tilde_blocked_until = Some(now + TILDE_BLOCK);
                return FloodResult::Blocked;
            }
            self.tilde_blocked_until = None;
        }

        self.tilde_times.push(now);
        let count = prune_window(&mut self.tilde_times, now, TILDE_WINDOW);

        if count >= TILDE_THRESHOLD {
            self.tilde_blocked_until = Some(now + TILDE_BLOCK);
            self.tilde_times.clear();
            return FloodResult::Triggered;
        }

        FloodResult::Allow
    }

    /// Check for duplicate text flood. Returns whether the message is allowed, just triggered, or already blocked.
    /// Only checks duplicates for channel messages (`is_channel = true`).
    pub fn check_duplicate_flood(&mut self, text: &str, is_channel: bool, now: Instant) -> FloodResult {
        if !is_channel || text.is_empty() {
            return FloodResult::Allow;
        }

        // Check if this exact text is already blocked — suppress silently
        if let Some(&until) = self.blocked_texts.get(text)
            && now < until
        {
            self.blocked_texts.insert(text.to_string(), now + DUP_BLOCK);
            return FloodResult::Blocked;
        }

        // Add to sliding message window
        self.msg_window.push((text.to_string(), now));

        // Prune old entries
        let cutoff = now.checked_sub(DUP_WINDOW).unwrap_or(now);
        self.msg_window.retain(|(_, t)| *t >= cutoff);

        // Only analyze when enough messages in window
        if self.msg_window.len() >= DUP_MIN_IN_WINDOW {
            let dupes = self.msg_window.iter().filter(|(t, _)| t == text).count();
            if dupes >= DUP_THRESHOLD {
                self.blocked_texts.insert(text.to_string(), now + DUP_BLOCK);
                return FloodResult::Triggered;
            }
        }

        // Clean expired blocked texts periodically
        if self.blocked_texts.len() > 50 {
            self.blocked_texts.retain(|_, until| *until > now);
        }

        FloodResult::Allow
    }

    /// Check for nick change flood in a specific buffer.
    /// Returns `true` if nick change display should be suppressed.
    pub fn should_suppress_nick_flood(&mut self, buffer_id: &str, now: Instant) -> bool {
        // Check if currently blocked for this buffer
        if let Some(&until) = self.nick_blocked_until.get(buffer_id)
            && now < until
        {
            // Extend block silently
            self.nick_blocked_until
                .insert(buffer_id.to_string(), now + NICK_BLOCK);
            return true;
        }

        // Track nick change timestamp
        let times = self
            .nick_times
            .entry(buffer_id.to_string())
            .or_default();
        times.push(now);
        prune_window(times, now, NICK_WINDOW);

        if times.len() >= NICK_THRESHOLD {
            self.nick_blocked_until
                .insert(buffer_id.to_string(), now + NICK_BLOCK);
            times.clear();
            return true;
        }

        false
    }
}

impl Default for FloodState {
    fn default() -> Self {
        Self::new()
    }
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

    #[test]
    fn new_state_has_no_blocks() {
        let state = FloodState::new();
        assert!(state.ctcp_blocked_until.is_none());
        assert!(state.tilde_blocked_until.is_none());
        assert!(state.msg_window.is_empty());
        assert!(state.blocked_texts.is_empty());
        assert!(state.nick_times.is_empty());
        assert!(state.nick_blocked_until.is_empty());
    }

    #[test]
    fn ctcp_under_threshold_passes() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..4 {
            let t = now + Duration::from_millis(i * 100);
            assert_eq!(state.check_ctcp_flood(t), FloodResult::Allow, "request {i} should pass");
        }
    }

    #[test]
    fn ctcp_at_threshold_triggers() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..4 {
            let t = now + Duration::from_millis(i * 100);
            assert_eq!(state.check_ctcp_flood(t), FloodResult::Allow);
        }
        // 5th request triggers — should be Triggered (not Blocked)
        assert_eq!(state.check_ctcp_flood(now + Duration::from_millis(400)), FloodResult::Triggered);
    }

    #[test]
    fn ctcp_block_extends_on_continued_flood() {
        let mut state = FloodState::new();
        let now = Instant::now();
        // Trigger the block
        for i in 0..5 {
            state.check_ctcp_flood(now + Duration::from_millis(i * 100));
        }
        // Subsequent requests during block should be Blocked (silent), not Triggered
        assert_eq!(state.check_ctcp_flood(now + Duration::from_secs(30)), FloodResult::Blocked);
    }

    #[test]
    fn ctcp_block_expires() {
        let mut state = FloodState::new();
        let now = Instant::now();
        // Trigger the block
        for i in 0..5 {
            state.check_ctcp_flood(now + Duration::from_millis(i * 100));
        }
        // After 61 seconds, should no longer be blocked
        assert_eq!(state.check_ctcp_flood(now + Duration::from_secs(61)), FloodResult::Allow);
    }

    #[test]
    fn ctcp_outside_window_does_not_trigger() {
        let mut state = FloodState::new();
        let now = Instant::now();
        // Spread 5 requests over 10 seconds (window is 5s)
        for i in 0..5 {
            let t = now + Duration::from_secs(i * 3);
            assert_eq!(state.check_ctcp_flood(t), FloodResult::Allow);
        }
    }

    #[test]
    fn tilde_under_threshold_passes() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..4 {
            assert_eq!(state.check_tilde_flood(now + Duration::from_millis(i * 100)), FloodResult::Allow);
        }
    }

    #[test]
    fn tilde_at_threshold_triggers() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..5 {
            let result = state.check_tilde_flood(now + Duration::from_millis(i * 100));
            if i < 4 {
                assert_eq!(result, FloodResult::Allow);
            } else {
                assert_eq!(result, FloodResult::Triggered);
            }
        }
    }

    #[test]
    fn tilde_block_expires() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..5 {
            state.check_tilde_flood(now + Duration::from_millis(i * 100));
        }
        assert_eq!(state.check_tilde_flood(now + Duration::from_secs(61)), FloodResult::Allow);
    }

    #[test]
    fn duplicate_non_channel_ignored() {
        let mut state = FloodState::new();
        let now = Instant::now();
        // Private messages should never trigger duplicate detection
        for i in 0..10 {
            assert_eq!(state.check_duplicate_flood(
                "same text",
                false,
                now + Duration::from_millis(i * 100)
            ), FloodResult::Allow);
        }
    }

    #[test]
    fn duplicate_empty_text_ignored() {
        let mut state = FloodState::new();
        let now = Instant::now();
        assert_eq!(state.check_duplicate_flood("", true, now), FloodResult::Allow);
    }

    #[test]
    fn duplicate_below_window_size_passes() {
        let mut state = FloodState::new();
        let now = Instant::now();
        // 4 messages isn't enough to trigger analysis (need DUP_MIN_IN_WINDOW=5)
        for i in 0..4 {
            assert_eq!(state.check_duplicate_flood(
                "spam",
                true,
                now + Duration::from_millis(i * 100)
            ), FloodResult::Allow);
        }
    }

    #[test]
    fn duplicate_at_threshold_triggers() {
        let mut state = FloodState::new();
        let now = Instant::now();
        // 5 messages, 3 of which are identical -> should trigger on the 5th
        assert_eq!(state.check_duplicate_flood("spam", true, now), FloodResult::Allow);
        assert_eq!(state.check_duplicate_flood("other1", true, now + Duration::from_millis(100)), FloodResult::Allow);
        assert_eq!(state.check_duplicate_flood("spam", true, now + Duration::from_millis(200)), FloodResult::Allow);
        assert_eq!(state.check_duplicate_flood("other2", true, now + Duration::from_millis(300)), FloodResult::Allow);
        // 5th msg, 3rd "spam" — window now has 5 msgs and 3 are "spam"
        assert_eq!(state.check_duplicate_flood(
            "spam",
            true,
            now + Duration::from_millis(400)
        ), FloodResult::Triggered);
    }

    #[test]
    fn duplicate_blocked_text_stays_blocked() {
        let mut state = FloodState::new();
        let now = Instant::now();
        // Trigger block
        assert_eq!(state.check_duplicate_flood("spam", true, now), FloodResult::Allow);
        assert_eq!(state.check_duplicate_flood("a", true, now + Duration::from_millis(100)), FloodResult::Allow);
        assert_eq!(state.check_duplicate_flood("spam", true, now + Duration::from_millis(200)), FloodResult::Allow);
        assert_eq!(state.check_duplicate_flood("b", true, now + Duration::from_millis(300)), FloodResult::Allow);
        assert_eq!(state.check_duplicate_flood(
            "spam",
            true,
            now + Duration::from_millis(400)
        ), FloodResult::Triggered);
        // Now "spam" is blocked — subsequent messages should be Blocked (silent)
        assert_eq!(state.check_duplicate_flood(
            "spam",
            true,
            now + Duration::from_secs(10)
        ), FloodResult::Blocked);
    }

    #[test]
    fn duplicate_different_texts_pass() {
        let mut state = FloodState::new();
        let now = Instant::now();
        // All different messages should pass even with many in window
        for i in 0..10 {
            assert_eq!(state.check_duplicate_flood(
                &format!("unique msg {i}"),
                true,
                now + Duration::from_millis(i * 100)
            ), FloodResult::Allow);
        }
    }

    #[test]
    fn nick_under_threshold_passes() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..4 {
            assert!(!state.should_suppress_nick_flood(
                "conn/chan",
                now + Duration::from_millis(i * 100)
            ));
        }
    }

    #[test]
    fn nick_at_threshold_triggers() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..5 {
            let result = state.should_suppress_nick_flood(
                "conn/#channel",
                now + Duration::from_millis(i * 100),
            );
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
        // Fill buffer A almost to threshold
        for i in 0..4 {
            assert!(!state.should_suppress_nick_flood(
                "buf_a",
                now + Duration::from_millis(i * 100)
            ));
        }
        // Buffer B should be unaffected
        assert!(!state.should_suppress_nick_flood(
            "buf_b",
            now + Duration::from_millis(500)
        ));
    }

    #[test]
    fn nick_block_extends_on_continued_flood() {
        let mut state = FloodState::new();
        let now = Instant::now();
        // Trigger block
        for i in 0..5 {
            state.should_suppress_nick_flood("buf", now + Duration::from_millis(i * 100));
        }
        // Should still be blocked after 30s
        assert!(state.should_suppress_nick_flood("buf", now + Duration::from_secs(30)));
    }

    #[test]
    fn nick_block_expires() {
        let mut state = FloodState::new();
        let now = Instant::now();
        for i in 0..5 {
            state.should_suppress_nick_flood("buf", now + Duration::from_millis(i * 100));
        }
        // After 61 seconds, should no longer be blocked
        assert!(!state.should_suppress_nick_flood("buf", now + Duration::from_secs(61)));
    }

    #[test]
    fn nick_outside_window_does_not_trigger() {
        let mut state = FloodState::new();
        let now = Instant::now();
        // Spread 5 nick changes over 6 seconds (window is 3s)
        for i in 0..5 {
            let t = now + Duration::from_millis(i * 1500);
            assert!(
                !state.should_suppress_nick_flood("buf", t),
                "nick change at {i}*1.5s should pass"
            );
        }
    }

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
        assert_eq!(count, 3); // only the last 3 are within the 5s window
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
    fn default_impl_matches_new() {
        let a = FloodState::new();
        let b = FloodState::default();
        assert!(a.ctcp_times.is_empty());
        assert!(b.ctcp_times.is_empty());
        assert!(a.ctcp_blocked_until.is_none());
        assert!(b.ctcp_blocked_until.is_none());
    }
}

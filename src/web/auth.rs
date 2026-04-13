use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::constants::APP_NAME;

/// Rate limiter that tracks failed login attempts per IP with exponential backoff.
pub struct RateLimiter {
    attempts: HashMap<String, AttemptState>,
}

struct AttemptState {
    failures: u32,
    last_attempt: Instant,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            attempts: HashMap::new(),
        }
    }

    /// Check if an IP is currently blocked. Returns the remaining lockout duration if blocked.
    pub fn check(&self, ip: &str) -> Option<Duration> {
        let state = self.attempts.get(ip)?;
        if state.failures == 0 {
            return None;
        }
        let lockout = lockout_duration(state.failures);
        let elapsed = state.last_attempt.elapsed();
        if elapsed < lockout {
            Some(lockout.saturating_sub(elapsed))
        } else {
            None
        }
    }

    /// Record a failed login attempt for an IP.
    pub fn record_failure(&mut self, ip: &str) {
        let state = self
            .attempts
            .entry(ip.to_string())
            .or_insert_with(|| AttemptState {
                failures: 0,
                last_attempt: Instant::now(),
            });
        state.failures = state.failures.saturating_add(1);
        state.last_attempt = Instant::now();
    }

    /// Reset failure count for an IP (on successful login).
    pub fn record_success(&mut self, ip: &str) {
        self.attempts.remove(ip);
    }

    /// Remove expired entries (entries whose lockout has fully elapsed).
    pub fn purge_expired(&mut self) {
        self.attempts.retain(|_, state| {
            let lockout = lockout_duration(state.failures);
            state.last_attempt.elapsed() < lockout
        });
    }
}

/// Exponential backoff: 1s, 2s, 4s, 8s, 16s, 32s, max 60s.
fn lockout_duration(failures: u32) -> Duration {
    let secs = 1u64
        .checked_shl(failures.saturating_sub(1))
        .unwrap_or(60)
        .min(60);
    Duration::from_secs(secs)
}

/// Session token store.
pub struct SessionStore {
    sessions: HashMap<String, Session>,
    max_age: Duration,
}

pub struct Session {
    pub created_at: Instant,
    pub ip: String,
}

impl SessionStore {
    /// Create with a custom session duration.
    pub fn with_hours(hours: u32) -> Self {
        Self {
            sessions: HashMap::new(),
            max_age: Duration::from_secs(u64::from(hours) * 3600),
        }
    }

    /// Create a new session and return the token.
    pub fn create(&mut self, ip: &str) -> String {
        let token = generate_token();
        self.sessions.insert(
            token.clone(),
            Session {
                created_at: Instant::now(),
                ip: ip.to_string(),
            },
        );
        token
    }

    /// Validate a session token. Returns the session if valid and not expired.
    pub fn validate(&self, token: &str, ip: &str) -> Option<&Session> {
        let session = self.sessions.get(token)?;
        if session.created_at.elapsed() > self.max_age {
            return None;
        }
        if session.ip != ip {
            return None;
        }
        Some(session)
    }

    /// Remove expired sessions.
    pub fn purge_expired(&mut self) {
        let max_age = self.max_age;
        self.sessions
            .retain(|_, s| s.created_at.elapsed() < max_age);
    }
}

pub fn session_cookie_name() -> String {
    format!("{}_web_session", APP_NAME.to_lowercase())
}

/// Generate a cryptographically random 32-byte hex session token.
fn generate_token() -> String {
    use rand::RngExt;
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    hex::encode(bytes)
}

/// Constant-time password comparison to prevent timing attacks.
///
/// Uses HMAC-SHA256 to ensure comparison time is independent of where
/// the strings first differ.
#[must_use]
pub fn verify_password(provided: &str, expected: &str) -> bool {
    use hmac::Mac;
    type HmacSha256 = hmac::Hmac<sha2::Sha256>;

    if expected.is_empty() {
        return false;
    }

    let mut mac =
        HmacSha256::new_from_slice(b"repartee-password-verify").expect("HMAC accepts any key");
    mac.update(expected.as_bytes());
    let expected_tag = mac.finalize().into_bytes();

    let mut mac2 =
        HmacSha256::new_from_slice(b"repartee-password-verify").expect("HMAC accepts any key");
    mac2.update(provided.as_bytes());

    mac2.verify(&expected_tag).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_allows_first_attempt() {
        let limiter = RateLimiter::new();
        assert!(limiter.check("1.2.3.4").is_none());
    }

    #[test]
    fn rate_limiter_blocks_after_failure() {
        let mut limiter = RateLimiter::new();
        limiter.record_failure("1.2.3.4");
        // Should be blocked for ~1s after first failure.
        assert!(limiter.check("1.2.3.4").is_some());
    }

    #[test]
    fn rate_limiter_resets_on_success() {
        let mut limiter = RateLimiter::new();
        limiter.record_failure("1.2.3.4");
        limiter.record_failure("1.2.3.4");
        limiter.record_success("1.2.3.4");
        assert!(limiter.check("1.2.3.4").is_none());
    }

    #[test]
    fn rate_limiter_exponential_backoff() {
        assert_eq!(lockout_duration(1).as_secs(), 1);
        assert_eq!(lockout_duration(2).as_secs(), 2);
        assert_eq!(lockout_duration(3).as_secs(), 4);
        assert_eq!(lockout_duration(4).as_secs(), 8);
        assert_eq!(lockout_duration(7).as_secs(), 60); // capped at 60
        assert_eq!(lockout_duration(100).as_secs(), 60); // capped at 60
    }

    #[test]
    fn session_store_create_and_validate() {
        let mut store = SessionStore::with_hours(24);
        let token = store.create("1.2.3.4");
        assert_eq!(token.len(), 64); // 32 bytes = 64 hex chars
        assert!(store.validate(&token, "1.2.3.4").is_some());
        assert!(store.validate(&token, "5.6.7.8").is_none());
        assert!(store.validate("invalid-token", "1.2.3.4").is_none());
    }

    #[test]
    fn verify_password_constant_time() {
        assert!(verify_password("secret", "secret"));
        assert!(!verify_password("wrong", "secret"));
        assert!(!verify_password("secret", ""));
        assert!(!verify_password("", "secret"));
        assert!(!verify_password("", ""));
    }

    #[test]
    fn generate_token_unique() {
        let t1 = generate_token();
        let t2 = generate_token();
        assert_ne!(t1, t2);
    }
}

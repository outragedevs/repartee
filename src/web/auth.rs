use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};

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

// ---------------------------------------------------------------------------
// Session store — file-backed, hashed tokens.
// ---------------------------------------------------------------------------

/// One persisted web session.
///
/// The raw token never lives on disk — only its HMAC is stored. The only way
/// to "use" a session is to present the raw token in a cookie; the server
/// HMACs it and looks for a match.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedSession {
    /// HMAC-SHA256 of the raw token under the server-wide session secret.
    pub token_hash: [u8; 32],
    /// Unix seconds when the session was created.
    pub created_at: i64,
    /// Unix seconds when the session was last validated.
    pub last_used: i64,
    /// User-agent reported at login. Display only — not used for validation.
    pub user_agent: String,
    /// Optional human label (future: "iPhone Safari", manually set).
    pub label: Option<String>,
}

/// Wire format for the on-disk session file.
#[derive(Debug, Serialize, Deserialize)]
struct PersistedFile {
    version: u8,
    sessions: Vec<PersistedSession>,
}

/// File-backed session store.
///
/// In-memory map of `token_hash -> PersistedSession`, persisted to
/// `~/.repartee/web_sessions.bin` via `postcard`. Saves are debounced to
/// avoid a disk write on every WS heartbeat — `record_use` only flushes
/// after `SAVE_DEBOUNCE_SECS` of dirty state.
pub struct SessionStore {
    sessions: HashMap<[u8; 32], PersistedSession>,
    /// HMAC key (32 bytes) used to hash raw tokens.
    secret: Vec<u8>,
    /// Maximum age of a session before `validate` rejects it.
    max_age: Duration,
    /// File on disk where sessions are persisted. `None` = in-memory only
    /// (used by tests that don't need the round-trip).
    path: Option<PathBuf>,
    /// `true` if there are unsaved changes.
    dirty: bool,
    /// Last save instant — used to debounce `record_use` writes.
    last_saved: Instant,
}

const SAVE_DEBOUNCE: Duration = Duration::from_mins(1);
const FILE_VERSION: u8 = 1;

#[derive(Debug, Clone)]
#[expect(
    dead_code,
    reason = "session metadata is exposed for future /sessions UI; reading happens via serde"
)]
pub struct Session {
    pub created_at: i64,
    pub last_used: i64,
    pub user_agent: String,
}

impl SessionStore {
    /// Create an empty in-memory store with a custom session lifetime.
    /// Used by tests; production callers should use [`load`].
    #[must_use]
    pub fn new(secret: Vec<u8>, max_age: Duration) -> Self {
        Self {
            sessions: HashMap::new(),
            secret,
            max_age,
            path: None,
            dirty: false,
            last_saved: Instant::now(),
        }
    }

    /// Convenience constructor: lifetime in days.
    #[must_use]
    pub fn with_days(secret: Vec<u8>, days: u32) -> Self {
        Self::new(secret, Duration::from_secs(u64::from(days) * 86_400))
    }

    /// Load (or create) a file-backed store at `path` with `days` lifetime.
    ///
    /// Missing or corrupt files yield an empty store — corruption is logged
    /// but not surfaced as an error (better to lose all sessions than refuse
    /// to start the server).
    ///
    /// Currently never errors, but the signature returns `Result` so future
    /// fail-loud behaviour (e.g. refuse to start when the file exists but is
    /// unreadable) doesn't need a downstream change.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "Result reserved for future strict mode; keeps call sites stable"
    )]
    pub fn load(path: &Path, secret: Vec<u8>, days: u32) -> Result<Self> {
        let max_age = Duration::from_secs(u64::from(days) * 86_400);
        let mut store = Self {
            sessions: HashMap::new(),
            secret,
            max_age,
            path: Some(path.to_path_buf()),
            dirty: false,
            last_saved: Instant::now(),
        };
        if !path.exists() {
            return Ok(store);
        }
        if let Err(e) = crate::fs_secure::restrict_path(path, 0o600) {
            tracing::warn!("failed to secure session file {}: {e}", path.display());
        }
        match std::fs::read(path) {
            Ok(bytes) => match postcard::from_bytes::<PersistedFile>(&bytes) {
                Ok(file) if file.version == FILE_VERSION => {
                    for session in file.sessions {
                        store.sessions.insert(session.token_hash, session);
                    }
                    let now = unix_now();
                    store
                        .sessions
                        .retain(|_, s| s.last_used + max_age.as_secs().cast_signed() > now);
                }
                Ok(_) => tracing::warn!("session file has unknown version, starting fresh"),
                Err(e) => tracing::warn!("session file unreadable ({e}), starting fresh"),
            },
            Err(e) => tracing::warn!("could not read session file: {e}"),
        }
        Ok(store)
    }

    /// Create a new session. Returns the **raw** token to send in the cookie;
    /// only its HMAC is stored.
    pub fn create(&mut self, user_agent: &str) -> String {
        use rand::RngExt;
        let mut bytes = [0u8; 32];
        rand::rng().fill(&mut bytes);
        let raw = hex::encode(bytes);
        let token_hash = self.hash(&raw);
        let now = unix_now();
        self.sessions.insert(
            token_hash,
            PersistedSession {
                token_hash,
                created_at: now,
                last_used: now,
                user_agent: user_agent.to_string(),
                label: None,
            },
        );
        self.dirty = true;
        // Flush immediately on create — losing a fresh login is the worst
        // possible outcome for UX.
        self.flush_if_needed(true);
        raw
    }

    /// Validate a raw token against the store. Returns the session if found
    /// and not expired, and bumps `last_used`. Otherwise returns `None`.
    pub fn validate(&mut self, raw_token: &str) -> Option<Session> {
        let hash = self.hash(raw_token);
        let now = unix_now();
        let max = self.max_age.as_secs().cast_signed();
        let session = self.sessions.get_mut(&hash)?;
        if session.last_used + max <= now {
            self.sessions.remove(&hash);
            self.dirty = true;
            self.flush_if_needed(false);
            return None;
        }
        session.last_used = now;
        let snapshot = Session {
            created_at: session.created_at,
            last_used: session.last_used,
            user_agent: session.user_agent.clone(),
        };
        self.dirty = true;
        self.flush_if_needed(false);
        Some(snapshot)
    }

    /// Revoke a single session by its raw token.
    pub fn revoke(&mut self, raw_token: &str) -> bool {
        let hash = self.hash(raw_token);
        let removed = self.sessions.remove(&hash).is_some();
        if removed {
            self.dirty = true;
            self.flush_if_needed(true);
        }
        removed
    }

    /// Wipe all sessions (logout-everywhere).
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "exposed for future /sessions logout-all endpoint")
    )]
    pub fn revoke_all(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        self.sessions.clear();
        self.dirty = true;
        self.flush_if_needed(true);
    }

    /// Drop sessions whose `last_used + max_age` has passed.
    pub fn purge_expired(&mut self) {
        let now = unix_now();
        let max = self.max_age.as_secs().cast_signed();
        let before = self.sessions.len();
        self.sessions.retain(|_, s| s.last_used + max > now);
        if self.sessions.len() != before {
            self.dirty = true;
            self.flush_if_needed(true);
        }
    }

    /// Number of active sessions in memory (for tests / debugging).
    #[must_use]
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used by tests and future /sessions list endpoint")
    )]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    #[must_use]
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used by tests and future /sessions list endpoint")
    )]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    fn hash(&self, raw_token: &str) -> [u8; 32] {
        use hmac::Mac;
        type HmacSha256 = hmac::Hmac<sha2::Sha256>;
        let mut mac = HmacSha256::new_from_slice(&self.secret).expect("HMAC accepts any key");
        mac.update(raw_token.as_bytes());
        mac.finalize().into_bytes().into()
    }

    /// Write the store to disk, atomically.
    /// `force` bypasses the debounce window; otherwise no-op if last save
    /// happened less than `SAVE_DEBOUNCE` ago.
    fn flush_if_needed(&mut self, force: bool) {
        if !self.dirty {
            return;
        }
        let Some(ref path) = self.path else {
            self.dirty = false;
            return;
        };
        if !force && self.last_saved.elapsed() < SAVE_DEBOUNCE {
            return;
        }
        let file = PersistedFile {
            version: FILE_VERSION,
            sessions: self.sessions.values().cloned().collect(),
        };
        let bytes = match postcard::to_allocvec(&file) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("failed to serialize session file: {e}");
                return;
            }
        };
        if let Some(parent) = path.parent()
            && let Err(e) = crate::fs_secure::create_dir_all(parent, 0o700)
        {
            tracing::warn!("failed to create session dir: {e}");
            return;
        }
        if let Err(e) = crate::fs_secure::write_file(path, bytes, 0o600) {
            tracing::warn!("failed to write session file: {e}");
            return;
        }
        self.dirty = false;
        self.last_saved = Instant::now();
    }

    /// Force-flush any pending changes to disk. Used at shutdown and tests.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "called from tests; future shutdown hook will call it")
    )]
    pub fn flush(&mut self) {
        self.flush_if_needed(true);
    }
}

fn unix_now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

pub fn session_cookie_name() -> String {
    format!("{}_web_session", APP_NAME.to_lowercase())
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

    fn test_secret() -> Vec<u8> {
        vec![0x42; 32]
    }

    #[test]
    fn rate_limiter_allows_first_attempt() {
        let limiter = RateLimiter::new();
        assert!(limiter.check("1.2.3.4").is_none());
    }

    #[test]
    fn rate_limiter_blocks_after_failure() {
        let mut limiter = RateLimiter::new();
        limiter.record_failure("1.2.3.4");
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
        assert_eq!(lockout_duration(7).as_secs(), 60);
        assert_eq!(lockout_duration(100).as_secs(), 60);
    }

    #[test]
    fn create_returns_unique_raw_tokens() {
        let mut store = SessionStore::with_days(test_secret(), 90);
        let t1 = store.create("ua-1");
        let t2 = store.create("ua-2");
        assert_eq!(t1.len(), 64);
        assert_ne!(t1, t2);
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn raw_token_never_appears_in_persisted_session() {
        let mut store = SessionStore::with_days(test_secret(), 90);
        let raw = store.create("ua");
        let persisted = store.sessions.values().next().unwrap();
        // The raw token (hex) cannot appear inside the persisted struct's bytes.
        let bytes = postcard::to_allocvec(&PersistedFile {
            version: FILE_VERSION,
            sessions: vec![persisted.clone()],
        })
        .unwrap();
        assert!(
            !bytes.windows(raw.len()).any(|w| w == raw.as_bytes()),
            "raw token found in serialised bytes"
        );
    }

    #[test]
    fn validate_accepts_correct_token() {
        let mut store = SessionStore::with_days(test_secret(), 90);
        let raw = store.create("ua");
        assert!(store.validate(&raw).is_some());
    }

    #[test]
    fn validate_rejects_wrong_token() {
        let mut store = SessionStore::with_days(test_secret(), 90);
        let _raw = store.create("ua");
        assert!(store.validate("0".repeat(64).as_str()).is_none());
    }

    #[test]
    fn validate_bumps_last_used() {
        let mut store = SessionStore::with_days(test_secret(), 90);
        let raw = store.create("ua");
        let before = store
            .sessions
            .values()
            .next()
            .unwrap()
            .last_used;
        std::thread::sleep(Duration::from_millis(1100));
        store.validate(&raw).unwrap();
        let after = store.sessions.values().next().unwrap().last_used;
        assert!(after > before, "last_used should advance");
    }

    #[test]
    fn revoke_removes_session() {
        let mut store = SessionStore::with_days(test_secret(), 90);
        let raw = store.create("ua");
        assert!(store.revoke(&raw));
        assert!(store.validate(&raw).is_none());
        assert!(!store.revoke(&raw));
    }

    #[test]
    fn revoke_all_clears_everything() {
        let mut store = SessionStore::with_days(test_secret(), 90);
        store.create("a");
        store.create("b");
        store.revoke_all();
        assert!(store.is_empty());
    }

    #[test]
    fn rotating_secret_invalidates_existing_sessions() {
        let mut store = SessionStore::with_days(test_secret(), 90);
        let raw = store.create("ua");
        // Move every persisted entry across to a store with a different
        // secret. The raw token now hashes to a different key, so lookup
        // misses.
        let mut store2 = SessionStore::with_days(vec![0x99; 32], 90);
        for (key, value) in store.sessions.drain() {
            store2.sessions.insert(key, value);
        }
        assert!(store2.validate(&raw).is_none());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web_sessions.bin");
        let raw = {
            let mut s = SessionStore::load(&path, test_secret(), 90).unwrap();
            let r = s.create("Mozilla/5.0");
            s.flush();
            r
        };
        let mut s2 = SessionStore::load(&path, test_secret(), 90).unwrap();
        assert_eq!(s2.len(), 1);
        let session = s2.validate(&raw).unwrap();
        assert_eq!(session.user_agent, "Mozilla/5.0");
    }

    #[test]
    fn load_creates_empty_store_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does_not_exist.bin");
        let s = SessionStore::load(&path, test_secret(), 90).unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn load_drops_expired_sessions_silently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("expired.bin");
        // Write a file with one session whose last_used is two centuries ago.
        let stale = PersistedSession {
            token_hash: [1; 32],
            created_at: 0,
            last_used: 0,
            user_agent: String::new(),
            label: None,
        };
        let bytes = postcard::to_allocvec(&PersistedFile {
            version: FILE_VERSION,
            sessions: vec![stale],
        })
        .unwrap();
        std::fs::write(&path, bytes).unwrap();
        let s = SessionStore::load(&path, test_secret(), 1).unwrap();
        assert!(s.is_empty(), "expired sessions must be pruned at load");
    }

    #[test]
    fn purge_expired_removes_old_entries() {
        let mut store = SessionStore::with_days(test_secret(), 1);
        let _ = store.create("ua");
        // Force last_used into the distant past.
        for s in store.sessions.values_mut() {
            s.last_used = 0;
        }
        store.purge_expired();
        assert!(store.is_empty());
    }

    #[test]
    fn debounced_save_does_not_write_on_every_validate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("debounce.bin");
        let mut store = SessionStore::load(&path, test_secret(), 90).unwrap();
        let raw = store.create("ua"); // forced flush
        let mtime_before = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        for _ in 0..5 {
            store.validate(&raw);
        }
        let mtime_after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(
            mtime_before, mtime_after,
            "validate should not flush within debounce window"
        );
    }

    #[test]
    fn verify_password_constant_time() {
        assert!(verify_password("secret", "secret"));
        assert!(!verify_password("wrong", "secret"));
        assert!(!verify_password("secret", ""));
        assert!(!verify_password("", "secret"));
        assert!(!verify_password("", ""));
    }
}

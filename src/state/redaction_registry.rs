use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, OnceLock, Weak};
use sha2::{Digest, Sha256};

pub type Key = (String, String, String);

#[derive(Debug)]
pub struct Identity {
    pub key: Key,
    notice: OnceLock<String>,
}

impl Identity {
    pub fn notice(&self) -> Option<&str> {
        self.notice.get().map(String::as_str)
    }
}

pub struct Registry {
    identities: HashMap<Key, Weak<Identity>>,
    recent: VecDeque<Arc<Identity>>,
    recent_limit: usize,
    operations: usize,
    archived: Option<rusqlite::Connection>,
    archived_scopes: HashSet<String>,
    archive_failed: Cell<bool>,
}

impl Registry {
    pub fn new(recent_limit: usize) -> Self {
        Self {
            identities: HashMap::new(),
            recent: VecDeque::new(),
            recent_limit,
            operations: 0,
            archived: None,
            archived_scopes: HashSet::new(),
            archive_failed: Cell::new(false),
        }
    }

    #[cfg(test)]
    pub fn deletion_count(&self) -> usize {
        self.identities.values().filter_map(Weak::upgrade)
            .filter(|identity| identity.notice().is_some()).count()
    }

    pub fn get(&self, key: &Key) -> Option<Arc<Identity>> {
        let retained = self.identities.get(key).and_then(Weak::upgrade);
        if retained.as_ref().is_some_and(|identity| identity.notice().is_some()) {
            return retained;
        }
        let archived = self.archived.as_ref().map_or(Ok(false), |db| {
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM deletions WHERE scope = ?1 AND id = ?2)",
                rusqlite::params![Sha256::digest(key.0.as_bytes()).as_slice(), Self::fingerprint(key).as_slice()],
                |row| row.get::<_, bool>(0),
            )
        });
        let notice = match archived {
            Ok(true) => Some("Message deleted"),
            Ok(false) if !self.archive_failed.get() => None,
            result => {
                if let Err(error) = result {
                    self.archive_error(&error);
                }
                Some("Message unavailable: deletion tracking failed")
            }
        };
        notice.map_or(retained, |notice| Some(Arc::new(Identity {
            key: key.clone(), notice: OnceLock::from(notice.to_string()),
        })))
    }

    fn archive_error(&self, error: &rusqlite::Error) {
        if !self.archive_failed.replace(true) {
            tracing::error!(%error, "Temporary deletion registry failed; hiding unverified messages");
        }
    }

    fn archive(&mut self, key: &Key) -> rusqlite::Result<()> {
        if self.archived.is_none() {
            let db = rusqlite::Connection::open("")?;
            db.execute_batch(
                "PRAGMA page_size = 4096;
                 PRAGMA cache_size = -256;
                 PRAGMA cache_spill = ON;
                 PRAGMA mmap_size = 0;
                 PRAGMA max_page_count = 16384;
                 CREATE TABLE deletions (scope BLOB, id BLOB, PRIMARY KEY(scope, id)) WITHOUT ROWID;",
            )?;
            self.archived = Some(db);
        }
        if let Some(db) = &self.archived {
            db.execute("INSERT OR IGNORE INTO deletions VALUES (?1, ?2)",
                rusqlite::params![Sha256::digest(key.0.as_bytes()).as_slice(), Self::fingerprint(key).as_slice()])?;
            self.archived_scopes.insert(key.0.clone());
        }
        Ok(())
    }

    pub fn track(&mut self, key: Key) -> Arc<Identity> {
        self.operations += 1;
        if self.operations >= 256 {
            self.identities
                .retain(|_, identity| identity.strong_count() > 0);
            self.operations = 0;
        }
        if let Some(identity) = self.get(&key) {
            self.identities.insert(key, Arc::downgrade(&identity));
            return identity;
        }
        let identity = Arc::new(Identity {
            key: key.clone(),
            notice: OnceLock::new(),
        });
        self.identities.insert(key, Arc::downgrade(&identity));
        identity
    }

    pub fn redact(&mut self, key: Key, notice: String) -> Arc<Identity> {
        let identity = self.track(key);
        if identity.notice.set(notice).is_ok() {
            self.recent.push_back(Arc::clone(&identity));
            while self.recent.len() > self.recent_limit {
                if let Some(expired) = self.recent.pop_front() {
                    if let Err(error) = self.archive(&expired.key) {
                        self.archive_error(&error);
                    }
                    if Arc::strong_count(&expired) == 1 {
                        self.identities.remove(&expired.key);
                    }
                }
            }
        }
        identity
    }

    fn fingerprint(key: &Key) -> [u8; 32] {
        let mut digest = Sha256::new();
        for part in [&key.1, &key.2] {
            digest.update(part.len().to_be_bytes());
            digest.update(part.as_bytes());
        }
        digest.finalize().into()
    }

    pub fn retain_scopes(&mut self, active: &std::collections::HashSet<&str>) {
        if let Some(db) = &self.archived {
            for scope in self.archived_scopes.iter().filter(|scope| !active.contains(scope.as_str())) {
                if let Err(error) = db.execute("DELETE FROM deletions WHERE scope = ?1",
                    [Sha256::digest(scope.as_bytes()).as_slice()]) {
                    self.archive_error(&error);
                }
            }
        }
        self.archived_scopes.retain(|scope| active.contains(scope.as_str()));
        if active.is_empty() {
            self.archived = None;
            self.archive_failed.set(false);
        }
        self.recent
            .retain(|identity| active.contains(identity.key.0.as_str()));
        self.identities
            .retain(|key, identity| active.contains(key.0.as_str()) && identity.strong_count() > 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: &str) -> Key {
        ("account".into(), "#channel".into(), id.into())
    }

    #[test]
    fn pending_copy_keeps_deletion_after_recent_cache_eviction() {
        let mut registry = Registry::new(2);
        let worker = registry.track(key("pending"));
        let display = Arc::clone(&worker);
        registry.redact(key("pending"), "deleted".into());
        for id in 0..10_000 {
            registry.redact(key(&id.to_string()), "another deletion".into());
        }
        assert_eq!(worker.notice(), Some("deleted"));
        assert_eq!(display.notice(), Some("deleted"));
        assert_eq!(registry.track(key("pending")).notice(), Some("deleted"));
        assert_eq!(registry.recent.len(), 2);
        assert!(registry.identities.len() <= 3);
    }

    #[test]
    fn live_and_history_references_share_confirmation_and_first_notice() {
        let mut registry = Registry::new(2);
        let history = registry.track(key("message"));
        let live = registry.track(key("message"));
        assert!(Arc::ptr_eq(&history, &live));
        assert!(history.notice().is_none());
        registry.redact(key("message"), "first actor".into());
        registry.redact(key("message"), "second actor".into());
        assert_eq!(history.notice(), Some("first actor"));
        assert_eq!(live.notice(), Some("first actor"));
        assert_eq!(registry.recent.len(), 1);
    }

    #[test]
    fn abandoned_references_are_collected_and_scope_replacement_is_isolated() {
        let mut registry = Registry::new(2);
        let old_worker = registry.track(key("pending"));
        registry.redact(key("pending"), "old account deletion".into());
        registry.retain_scopes(&std::collections::HashSet::new());
        assert!(registry.identities.is_empty());
        assert!(registry.recent.is_empty());
        assert_eq!(old_worker.notice(), Some("old account deletion"));
        assert!(registry.track(key("pending")).notice().is_none());
        for id in 0..10_000 {
            registry.track(key(&id.to_string()));
        }
        assert!(registry.identities.len() <= 256);
    }
    #[test]
    fn unseen_later_replay_retains_deletion_after_cache_eviction() {
        let mut registry = Registry::new(2);
        registry.redact(key("old"), "Deleted by actor".into());
        for id in 0..10_000 {
            registry.redact(key(&id.to_string()), "newer deletion".into());
        }
        assert_eq!(registry.track(key("old")).notice(), Some("Message deleted"));
        let other_target = ("account".into(), "#other".into(), "old".into());
        assert!(registry.track(other_target).notice().is_none());
        registry.retain_scopes(&HashSet::new());
        assert!(registry.track(key("old")).notice().is_none());
    }

    #[test]
    fn archive_spills_beyond_its_bounded_cache_without_storing_message_content() {
        let mut registry = Registry::new(2);
        for id in 0..10_000 {
            registry.redact(key(&id.to_string()), "private actor and reason".into());
        }
        let db = registry.archived.as_ref().unwrap();
        let pages: i64 = db.query_row("PRAGMA page_count", [], |row| row.get(0)).unwrap();
        let cache: i64 = db.query_row("PRAGMA cache_size", [], |row| row.get(0)).unwrap();
        assert_eq!(cache, -256);
        assert!(pages > 64);
        let (count, min_length, max_length): (i64, i64, i64) = db.query_row(
            "SELECT count(*), min(length(scope) + length(id)), max(length(scope) + length(id)) FROM deletions",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!(count, 9_998);
        assert_eq!((min_length, max_length), (64, 64));
        assert_eq!(registry.get(&key("0")).unwrap().notice(), Some("Message deleted"));
        assert!(registry.get(&key("not-deleted")).is_none());
        registry.retain_scopes(&HashSet::new());
        assert!(registry.archived.is_none());
    }

    #[test]
    fn archive_write_failure_hides_unverified_bodies_until_scope_reset() {
        let mut registry = Registry::new(0);
        registry.redact(key("first"), "deleted".into());
        registry.archived.as_ref().unwrap().execute_batch("PRAGMA query_only = ON").unwrap();
        registry.redact(key("lost"), "deleted".into());
        assert!(registry.archive_failed.get());
        assert_eq!(registry.get(&key("lost")).unwrap().notice(),
            Some("Message unavailable: deletion tracking failed"));
        assert_eq!(registry.get(&key("first")).unwrap().notice(), Some("Message deleted"));
        registry.retain_scopes(&HashSet::new());
        assert!(registry.get(&key("lost")).is_none());
    }

    #[test]
    fn archive_read_failure_does_not_restore_retained_unverified_body() {
        let mut registry = Registry::new(0);
        let pending = registry.track(key("pending"));
        registry.redact(key("first"), "deleted".into());
        registry.archived.as_ref().unwrap().execute_batch("DROP TABLE deletions").unwrap();
        assert!(pending.notice().is_none());
        assert_eq!(registry.get(&key("pending")).unwrap().notice(),
            Some("Message unavailable: deletion tracking failed"));
        assert!(registry.archive_failed.get());
    }

}

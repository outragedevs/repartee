//! URL shortener integration (shr.al-compatible).
//!
//! Two responsibilities live here: detecting candidate URLs in chat
//! text (over the user-configured length threshold) and caching the
//! results of recent shortenings so the same link doesn't hit the API
//! twice. The HTTP wrapper that actually talks to the API lives in
//! `client.rs`; the orchestration that decides *when* to shorten
//! (live incoming vs outgoing vs backlog) lives in `app/`.

mod client;

pub use client::{ShrinkClient, ShrinkError};

use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;

/// One URL that was successfully shortened. Empty `original` and
/// `shortened` are never produced — both fields are non-empty on
/// every constructed value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlShortening {
    /// The URL as it appeared in the original text.
    pub original: String,
    /// What `original` was replaced with — typically
    /// `https://shr.al/<7-char-slug>`.
    pub shortened: String,
}

/// Mirrors the URL pattern in `image_preview::detect` deliberately —
/// the two pipelines must agree on what counts as a URL so the
/// shortener and the preview extractor never disagree on token
/// boundaries (a stray trailing `]` consumed by one but not the other
/// produces stale links).
static URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"https?://[^\s<>"')\]]+"#).expect("URL_RE is a valid regex")
});

/// Return every distinct URL in `text` whose length (including the
/// `http(s)://` scheme prefix) is at least `min_length`. The result
/// preserves first-appearance order so callers can rebuild the
/// substituted text deterministically.
#[must_use]
pub fn find_long_urls(text: &str, min_length: usize) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for m in URL_RE.find_iter(text) {
        let url = m.as_str();
        if url.len() < min_length {
            continue;
        }
        if seen.insert(url.to_string()) {
            out.push(url.to_string());
        }
    }
    out
}

/// Extract the host portion of a URL, stripped of `www.` and port.
/// Used to render the `[host]` hint after a shortened link in
/// incoming chat messages. Returns `None` for malformed inputs.
#[must_use]
pub fn host_of(url: &str) -> Option<String> {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = without_scheme.split(['/', '?', '#']).next()?;
    if host.is_empty() {
        return None;
    }
    let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
    let host = host.strip_prefix("www.").unwrap_or(host);
    Some(host.to_ascii_lowercase())
}

/// Bounded LRU cache mapping original URL → most-recent shortening.
///
/// Each `get` promotes the entry to most-recently-used; `insert` over
/// the cap drops the least-recently-used. Storage is `IndexMap` so
/// promotion is a single `swap_remove_entry` + `insert` without
/// touching the rest of the entries.
pub struct ShrinkCache {
    map: IndexMap<String, UrlShortening>,
    cap: usize,
}

impl ShrinkCache {
    /// Create a cache with the given upper bound on entry count.
    /// A `cap` of 0 silently behaves as 1 — the user-facing `/set`
    /// validation should keep this from happening in practice.
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Self {
            map: IndexMap::new(),
            cap: cap.max(1),
        }
    }

    /// Look up `url`, promoting the entry to most-recently-used on hit.
    pub fn get(&mut self, url: &str) -> Option<UrlShortening> {
        let v = self.map.swap_remove(url)?;
        self.map.insert(url.to_string(), v.clone());
        Some(v)
    }

    /// Insert a shortening, evicting the oldest entry when at capacity.
    /// Updating an existing key counts as a refresh (re-inserted at the
    /// MRU end) and does not evict anything.
    pub fn insert(&mut self, url: String, shortening: UrlShortening) {
        if self.map.contains_key(&url) {
            self.map.swap_remove(&url);
        } else if self.map.len() >= self.cap {
            self.map.shift_remove_index(0);
        }
        self.map.insert(url, shortening);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(original: &str, shortened: &str) -> UrlShortening {
        UrlShortening {
            original: original.to_string(),
            shortened: shortened.to_string(),
        }
    }

    #[test]
    fn find_long_urls_filters_by_length() {
        // http://x.com → 12 chars (below threshold)
        // https://example.com/very/long/path?x=1&y=2 → 42 chars (above)
        let text = "short http://x.com long https://example.com/very/long/path?x=1&y=2";
        let urls = find_long_urls(text, 30);
        assert_eq!(urls, vec!["https://example.com/very/long/path?x=1&y=2"]);
    }

    #[test]
    fn find_long_urls_includes_scheme_in_length() {
        let url = "https://example.com/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        assert_eq!(url.len(), 50);
        // exactly 50 → included with threshold 50
        let urls = find_long_urls(url, 50);
        assert_eq!(urls.len(), 1);
        // threshold one higher → excluded
        let urls = find_long_urls(url, 51);
        assert!(urls.is_empty());
    }

    #[test]
    fn find_long_urls_dedupes_in_appearance_order() {
        let text = "https://example.com/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa first \
                    https://other.com/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb \
                    https://example.com/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa repeat";
        let urls = find_long_urls(text, 50);
        assert_eq!(urls.len(), 2);
        assert!(urls[0].contains("example.com"));
        assert!(urls[1].contains("other.com"));
    }

    #[test]
    fn find_long_urls_excludes_non_http() {
        let text = "ftp://server/very_long_path_that_exceeds_the_threshold_easily";
        let urls = find_long_urls(text, 30);
        assert!(urls.is_empty());
    }

    #[test]
    fn find_long_urls_trims_trailing_brackets() {
        // The regex deliberately stops at `)` and `]` so a URL inside
        // parentheses doesn't pick up the closing bracket.
        let text = "see (https://example.com/path/with-long-name-aaaaaaaaaaaaaa) for details";
        let urls = find_long_urls(text, 40);
        assert_eq!(urls.len(), 1);
        assert!(!urls[0].ends_with(')'));
    }

    #[test]
    fn host_of_extracts_basics() {
        assert_eq!(host_of("https://example.com/path"), Some("example.com".into()));
        assert_eq!(host_of("http://www.example.com/"), Some("example.com".into()));
        assert_eq!(host_of("https://Sub.Example.COM"), Some("sub.example.com".into()));
        assert_eq!(host_of("https://example.com:8443/x"), Some("example.com".into()));
    }

    #[test]
    fn host_of_handles_malformed() {
        assert_eq!(host_of("not-a-url"), None);
        assert_eq!(host_of("ftp://example.com"), None);
        assert_eq!(host_of("https://"), None);
    }

    #[test]
    fn cache_returns_none_on_miss() {
        let mut c = ShrinkCache::new(10);
        assert!(c.get("https://x").is_none());
    }

    #[test]
    fn cache_returns_value_on_hit() {
        let mut c = ShrinkCache::new(10);
        c.insert("https://x.com/long".into(), sh("https://x.com/long", "https://shr.al/a"));
        assert_eq!(c.get("https://x.com/long").unwrap().shortened, "https://shr.al/a");
    }

    #[test]
    fn cache_evicts_oldest_when_full() {
        let mut c = ShrinkCache::new(2);
        c.insert("a".into(), sh("a", "https://shr.al/1"));
        c.insert("b".into(), sh("b", "https://shr.al/2"));
        c.insert("c".into(), sh("c", "https://shr.al/3"));
        // a was oldest → evicted
        assert!(c.get("a").is_none());
        assert!(c.get("b").is_some());
        assert!(c.get("c").is_some());
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn cache_get_promotes_to_most_recent() {
        let mut c = ShrinkCache::new(2);
        c.insert("a".into(), sh("a", "https://shr.al/1"));
        c.insert("b".into(), sh("b", "https://shr.al/2"));
        // Touch `a` → now `b` is oldest
        let _ = c.get("a");
        c.insert("c".into(), sh("c", "https://shr.al/3"));
        // b was promoted to oldest by `a`'s promotion → b evicted
        assert!(c.get("a").is_some());
        assert!(c.get("b").is_none());
        assert!(c.get("c").is_some());
    }

    #[test]
    fn cache_update_does_not_evict() {
        let mut c = ShrinkCache::new(2);
        c.insert("a".into(), sh("a", "https://shr.al/1"));
        c.insert("b".into(), sh("b", "https://shr.al/2"));
        // Re-insert same key with a new shortening — should refresh,
        // not evict the other key.
        c.insert("a".into(), sh("a", "https://shr.al/9"));
        assert_eq!(c.len(), 2);
        assert_eq!(c.get("a").unwrap().shortened, "https://shr.al/9");
        assert!(c.get("b").is_some());
    }

    #[test]
    fn cache_cap_zero_treated_as_one() {
        let mut c = ShrinkCache::new(0);
        c.insert("a".into(), sh("a", "https://shr.al/1"));
        assert_eq!(c.len(), 1);
        c.insert("b".into(), sh("b", "https://shr.al/2"));
        assert_eq!(c.len(), 1);
        assert!(c.get("a").is_none());
        assert!(c.get("b").is_some());
    }
}

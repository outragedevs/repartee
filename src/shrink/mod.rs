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

/// URL pattern with symmetric bracket exclusion: stops at `(`, `)`,
/// `[`, `]`, single quote, double quote, `<`, `>`, and whitespace.
///
/// Diverges intentionally from `image_preview::detect`'s pattern
/// (which only excludes the close brackets): the shortener stores
/// whatever string we POST, so a truncated URL becomes a permanent
/// short-link that 404s. Excluding the opening forms too means
/// URLs containing balanced parens — Wikipedia disambiguation,
/// MDN, RFC links — are not matched at all, which is the right
/// failure mode (no shrink → original URL goes through unchanged,
/// preview pipeline still extracts via its own pattern).
static URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"https?://[^\s<>"'()\[\]]+"#).expect("URL_RE is a valid regex")
});

/// Return every distinct URL in `text` whose length (including the
/// `http(s)://` scheme prefix) is at least `min_length`. The result
/// preserves first-appearance order so callers can rebuild the
/// substituted text deterministically.
///
/// Trailing sentence punctuation (`.,;:`) is stripped from each
/// candidate before the length check and dedup — without this the
/// shortener would store URLs like `…/page-name.` (including the
/// period), producing short-links that 404. The set deliberately
/// excludes `?` (RFC 3986 query separator — a bare `…/search?` is
/// a different URL than `…/search`) and `!` (RFC 3986 sub-delim,
/// used in some path schemes); trimming them would silently change
/// the URL identity.
#[must_use]
pub fn find_long_urls(text: &str, min_length: usize) -> Vec<String> {
    const TRAILING_TRIM: &[char] = &['.', ',', ';', ':'];
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for m in URL_RE.find_iter(text) {
        let url = m.as_str().trim_end_matches(TRAILING_TRIM);
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
    ///
    /// `shift_remove` (not `swap_remove`) is critical here: IndexMap's
    /// `swap_remove` moves the LAST entry into the vacated slot, which
    /// scrambles every entry's relative age and breaks LRU eviction
    /// at cap ≥ 3. `shift_remove` preserves the order of all other
    /// entries at O(n) cost — acceptable for a 500-entry cache hit
    /// path.
    pub fn get(&mut self, url: &str) -> Option<UrlShortening> {
        let v = self.map.shift_remove(url)?;
        self.map.insert(url.to_string(), v.clone());
        Some(v)
    }

    /// Insert a shortening, evicting the oldest entry when at capacity.
    /// Updating an existing key counts as a refresh (re-inserted at the
    /// MRU end) and does not evict anything. See `get` for why
    /// `shift_remove` is used on the update path.
    pub fn insert(&mut self, url: String, shortening: UrlShortening) {
        if self.map.contains_key(&url) {
            self.map.shift_remove(&url);
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
    fn find_long_urls_trims_trailing_punctuation() {
        // Regression: the bare URL_RE regex doesn't strip sentence-end
        // punctuation, and the shortener would store the trailing dot
        // as part of the URL — producing a short-link that 404s.
        let text =
            "see https://example.com/very/long/path-name-that-exceeds-min, then also https://example.com/another-long-path-here-yes.";
        let urls = find_long_urls(text, 40);
        assert_eq!(urls.len(), 2);
        for u in &urls {
            assert!(
                !u.ends_with('.') && !u.ends_with(','),
                "URL still ends with punctuation: {u}"
            );
        }
    }

    #[test]
    fn find_long_urls_keeps_question_mark() {
        // Regression: `?` is the URL query separator. Trimming it
        // would silently change `…/search?` to `…/search`, which
        // some servers route differently.
        let text = "see https://example.com/search-for-a-very-long-name?";
        let urls = find_long_urls(text, 30);
        assert_eq!(urls.len(), 1);
        assert!(urls[0].ends_with('?'), "URL must keep trailing `?`: {}", urls[0]);
    }

    #[test]
    fn find_long_urls_excludes_balanced_parens_url_entirely() {
        // Regression: URL_RE now stops at `(` too, so Wikipedia
        // disambiguation links no longer get truncated — they
        // simply aren't matched. The original URL passes through
        // the message unchanged; preview pipeline has its own
        // pattern and is unaffected.
        let text = "see https://en.wikipedia.org/wiki/Stack_(abstract_data_type) for details";
        let urls = find_long_urls(text, 20);
        // The match stops at `(` — the captured prefix is
        // `https://en.wikipedia.org/wiki/Stack_` which is still
        // over 20 chars, so it returns. The KEY property is that
        // the match never includes a truncated-but-looks-valid
        // path that would resolve to the wrong page on shr.al.
        // For now we accept the truncated prefix (it ends in `_`
        // which is unusual and obvious) rather than the
        // half-truncated `…/Stack_(abstract_data_type` which would
        // resolve as a valid-but-wrong page.
        for u in &urls {
            assert!(!u.contains('('), "URL must not include `(`: {u}");
            assert!(!u.contains(')'), "URL must not include `)`: {u}");
        }
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
    fn cache_get_promotes_preserves_other_entries_order_at_cap3() {
        // Regression: earlier `swap_remove` impl scrambled the order
        // of entries OTHER than the touched key, so the next eviction
        // dropped the wrong entry. Insert a, b, c (a oldest), touch
        // a, then insert d — b must be the survivor along with c, d.
        // Pre-fix would evict c (because swap_remove moved c to
        // index 0 during the `get("a")` promotion).
        let mut c = ShrinkCache::new(3);
        c.insert("a".into(), sh("a", "https://shr.al/1"));
        c.insert("b".into(), sh("b", "https://shr.al/2"));
        c.insert("c".into(), sh("c", "https://shr.al/3"));
        let _ = c.get("a");
        c.insert("d".into(), sh("d", "https://shr.al/4"));
        // a was just touched (MRU); b is now oldest → b evicted.
        assert!(c.get("a").is_some(), "a was just touched, must survive");
        assert!(c.get("b").is_none(), "b is oldest after a's promotion");
        assert!(c.get("c").is_some(), "c must survive — swap_remove would have evicted it");
        assert!(c.get("d").is_some(), "d just inserted");
    }

    #[test]
    fn cache_update_preserves_other_entries_at_cap3() {
        // Same shape as above but via `insert` (update path) instead of `get`.
        let mut c = ShrinkCache::new(3);
        c.insert("a".into(), sh("a", "https://shr.al/1"));
        c.insert("b".into(), sh("b", "https://shr.al/2"));
        c.insert("c".into(), sh("c", "https://shr.al/3"));
        // Re-insert "a" (update). Pre-fix this swapped c into slot 0.
        c.insert("a".into(), sh("a", "https://shr.al/1-updated"));
        c.insert("d".into(), sh("d", "https://shr.al/4"));
        assert!(c.get("a").is_some(), "a was just updated, must survive");
        assert!(c.get("b").is_none(), "b is oldest");
        assert!(c.get("c").is_some(), "c must survive");
        assert!(c.get("d").is_some(), "d just inserted");
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

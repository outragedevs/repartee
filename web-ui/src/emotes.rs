//! Built-in emote name whitelist, embedded at build time (see `build.rs`).
//!
//! Available synchronously from the first render — no manifest fetch, no race
//! against backlog rendering.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

include!(concat!(env!("OUT_DIR"), "/emote_names.rs"));

/// Set view over [`EMOTE_NAMES`] for O(1) membership checks.
static EMOTE_SET: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| EMOTE_NAMES.iter().copied().collect());

/// Any tag name (Polish stem or English alias) -> Polish stem (= GIF file name).
static STEM_MAP: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| EMOTE_STEM.iter().copied().collect());

/// Whether `name` is a known built-in emote (either language).
#[must_use]
pub fn is_emote(name: &str) -> bool {
    EMOTE_SET.contains(name)
}

/// The Polish stem (GIF file name) for a tag in either language; empty if unknown.
#[must_use]
pub fn stem_for(name: &str) -> &'static str {
    STEM_MAP.get(name).copied().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_names_present_and_sorted() {
        assert!(
            EMOTE_NAMES.len() >= 180,
            "expected full GG7 set, got {}",
            EMOTE_NAMES.len()
        );
        assert!(
            EMOTE_NAMES.windows(2).all(|w| w[0] <= w[1]),
            "must be sorted"
        );
        assert!(is_emote("usmiech"));
        assert!(!is_emote("definitely_not_an_emote"));
    }

    #[test]
    fn english_aliases_resolve_to_stem() {
        assert!(is_emote("smile"), "English alias is a known emote");
        assert!(is_emote("usmiech"), "Polish stem is a known emote");
        assert_eq!(stem_for("smile"), "usmiech");
        assert_eq!(stem_for("usmiech"), "usmiech");
        assert_eq!(stem_for("definitely_not_an_emote"), "");
    }
}

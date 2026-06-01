//! Built-in emote name whitelist, embedded at build time (see `build.rs`).
//!
//! Available synchronously from the first render — no manifest fetch, no race
//! against backlog rendering.

use std::collections::HashSet;
use std::sync::LazyLock;

include!(concat!(env!("OUT_DIR"), "/emote_names.rs"));

/// Set view over [`EMOTE_NAMES`] for O(1) membership checks.
static EMOTE_SET: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| EMOTE_NAMES.iter().copied().collect());

/// Whether `name` is a known built-in emote.
#[must_use]
pub fn is_emote(name: &str) -> bool {
    EMOTE_SET.contains(name)
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
}

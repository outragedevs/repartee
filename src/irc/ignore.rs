// User ignore management — wildcard pattern matching against nick!ident@host masks.

use std::collections::HashMap;
use std::sync::Mutex;

use regex::Regex;

use crate::config::{IgnoreEntry, IgnoreLevel};

/// Thread-safe cache of compiled regexes keyed by wildcard pattern.
static REGEX_CACHE: std::sync::LazyLock<Mutex<HashMap<String, Regex>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Get a compiled regex for a wildcard pattern, using a cache to avoid recompilation.
fn cached_wildcard_regex(pattern: &str) -> Regex {
    let mut cache = REGEX_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache
        .entry(pattern.to_string())
        .or_insert_with(|| wildcard_to_regex(pattern))
        .clone()
}

/// Convert a simple wildcard pattern (`*` and `?`) to a case-insensitive regex.
///
/// - `*` matches any number of characters (including zero)
/// - `?` matches exactly one character
pub fn wildcard_to_regex(pattern: &str) -> Regex {
    let mut escaped = String::with_capacity(pattern.len() * 2 + 4);
    escaped.push('^');
    for ch in pattern.chars() {
        match ch {
            '*' => escaped.push_str(".*"),
            '?' => escaped.push('.'),
            // Escape regex metacharacters
            '.' | '+' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            _ => escaped.push(ch),
        }
    }
    escaped.push('$');
    Regex::new(&format!("(?i){escaped}")).expect("wildcard pattern should produce valid regex")
}

/// Build a `nick!ident@host` mask from components.
///
/// Missing ident or hostname are replaced with `*`.
pub fn build_mask(nick: &str, ident: Option<&str>, hostname: Option<&str>) -> String {
    format!(
        "{}!{}@{}",
        nick,
        ident.unwrap_or("*"),
        hostname.unwrap_or("*")
    )
}

/// Check if an event should be ignored based on the ignore list.
///
/// Logic:
/// 1. For each ignore entry, check if the level matches (entry has `All` or contains the level).
/// 2. Pattern check: if the mask contains `!`, match against the full `nick!ident@host`;
///    otherwise match against the nick only.
/// 3. Channel restriction: if the entry has a channels list, only match if the current channel
///    is in that list (case-insensitive).
/// 4. Returns `true` on first match.
pub fn should_ignore(
    ignores: &[IgnoreEntry],
    nick: &str,
    ident: Option<&str>,
    hostname: Option<&str>,
    level: &IgnoreLevel,
    channel: Option<&str>,
) -> bool {
    if ignores.is_empty() {
        return false;
    }

    let full_mask = build_mask(nick, ident, hostname);

    for entry in ignores {
        // Level check: entry must include ALL or the specific level
        if !entry
            .levels
            .iter()
            .any(|l| matches!(l, IgnoreLevel::All) || l == level)
        {
            continue;
        }

        // Pattern check: bare nick vs full mask (uses cached compiled regex)
        let re = cached_wildcard_regex(&entry.mask);
        let matched = if entry.mask.contains('!') {
            re.is_match(&full_mask)
        } else {
            re.is_match(nick)
        };

        if !matched {
            continue;
        }

        // Channel restriction
        if let Some(ref entry_channels) = entry.channels
            && !entry_channels.is_empty()
        {
            match channel {
                Some(ch) => {
                    let ch_lower = ch.to_lowercase();
                    if !entry_channels
                        .iter()
                        .any(|ec| ec.to_lowercase() == ch_lower)
                    {
                        continue;
                    }
                }
                None => continue,
            }
        }

        return true;
    }

    false
}

// === Tests ===

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{IgnoreEntry, IgnoreLevel};

    // --- wildcard_to_regex tests ---

    #[test]
    fn wildcard_star_matches_anything() {
        let re = wildcard_to_regex("*");
        assert!(re.is_match("anything"));
        assert!(re.is_match(""));
        assert!(re.is_match("hello world"));
    }

    #[test]
    fn wildcard_question_matches_single_char() {
        let re = wildcard_to_regex("a?c");
        assert!(re.is_match("abc"));
        assert!(re.is_match("axc"));
        assert!(!re.is_match("ac"));
        assert!(!re.is_match("abbc"));
    }

    #[test]
    fn wildcard_case_insensitive() {
        let re = wildcard_to_regex("Hello*");
        assert!(re.is_match("hello world"));
        assert!(re.is_match("HELLO"));
        assert!(re.is_match("HeLLo"));
    }

    #[test]
    fn wildcard_escapes_metacharacters() {
        let re = wildcard_to_regex("user.name+tag");
        assert!(re.is_match("user.name+tag"));
        assert!(!re.is_match("userXname+tag")); // dot should be literal
    }

    #[test]
    fn wildcard_complex_pattern() {
        let re = wildcard_to_regex("*!*@*.spam.host");
        assert!(re.is_match("nick!user@anything.spam.host"));
        assert!(!re.is_match("nick!user@other.host"));
    }

    #[test]
    fn wildcard_exact_match() {
        let re = wildcard_to_regex("exactname");
        assert!(re.is_match("exactname"));
        assert!(re.is_match("ExactName")); // case insensitive
        assert!(!re.is_match("exactname2"));
        assert!(!re.is_match("xexactname"));
    }

    // --- build_mask tests ---

    #[test]
    fn build_mask_all_parts() {
        assert_eq!(
            build_mask("nick", Some("user"), Some("host.net")),
            "nick!user@host.net"
        );
    }

    #[test]
    fn build_mask_missing_ident() {
        assert_eq!(
            build_mask("nick", None, Some("host.net")),
            "nick!*@host.net"
        );
    }

    #[test]
    fn build_mask_missing_hostname() {
        assert_eq!(build_mask("nick", Some("user"), None), "nick!user@*");
    }

    #[test]
    fn build_mask_missing_both() {
        assert_eq!(build_mask("nick", None, None), "nick!*@*");
    }

    // --- should_ignore tests ---

    fn make_entry(
        mask: &str,
        levels: Vec<IgnoreLevel>,
        channels: Option<Vec<&str>>,
    ) -> IgnoreEntry {
        IgnoreEntry {
            mask: mask.to_string(),
            levels,
            channels: channels.map(|v| v.into_iter().map(str::to_string).collect()),
        }
    }

    #[test]
    fn empty_ignores_returns_false() {
        assert!(!should_ignore(
            &[],
            "nick",
            None,
            None,
            &IgnoreLevel::Msgs,
            None
        ));
    }

    #[test]
    fn ignores_matching_nick_pattern() {
        let ignores = vec![make_entry("spammer*", vec![IgnoreLevel::All], None)];
        assert!(should_ignore(
            &ignores,
            "spammer123",
            Some("user"),
            Some("host"),
            &IgnoreLevel::Msgs,
            None
        ));
    }

    #[test]
    fn ignores_full_mask_pattern() {
        let ignores = vec![make_entry(
            "*!*@spam.host",
            vec![IgnoreLevel::Msgs, IgnoreLevel::Notices],
            None,
        )];
        assert!(should_ignore(
            &ignores,
            "anyone",
            Some("user"),
            Some("spam.host"),
            &IgnoreLevel::Msgs,
            None
        ));
    }

    #[test]
    fn does_not_ignore_wrong_level() {
        let ignores = vec![make_entry("spammer", vec![IgnoreLevel::Msgs], None)];
        assert!(!should_ignore(
            &ignores,
            "spammer",
            None,
            None,
            &IgnoreLevel::Joins,
            None
        ));
    }

    #[test]
    fn all_level_matches_everything() {
        let ignores = vec![make_entry("spammer", vec![IgnoreLevel::All], None)];
        assert!(should_ignore(
            &ignores,
            "spammer",
            None,
            None,
            &IgnoreLevel::Joins,
            None
        ));
        assert!(should_ignore(
            &ignores,
            "spammer",
            None,
            None,
            &IgnoreLevel::Msgs,
            None
        ));
        assert!(should_ignore(
            &ignores,
            "spammer",
            None,
            None,
            &IgnoreLevel::Ctcps,
            None
        ));
    }

    #[test]
    fn channel_restriction_matches() {
        let ignores = vec![make_entry(
            "spammer",
            vec![IgnoreLevel::All],
            Some(vec!["#general"]),
        )];
        // Matches in the restricted channel
        assert!(should_ignore(
            &ignores,
            "spammer",
            None,
            None,
            &IgnoreLevel::Msgs,
            Some("#general")
        ));
    }

    #[test]
    fn channel_restriction_case_insensitive() {
        let ignores = vec![make_entry(
            "spammer",
            vec![IgnoreLevel::All],
            Some(vec!["#General"]),
        )];
        assert!(should_ignore(
            &ignores,
            "spammer",
            None,
            None,
            &IgnoreLevel::Msgs,
            Some("#general")
        ));
    }

    #[test]
    fn channel_restriction_no_match() {
        let ignores = vec![make_entry(
            "spammer",
            vec![IgnoreLevel::All],
            Some(vec!["#general"]),
        )];
        // Does not match in a different channel
        assert!(!should_ignore(
            &ignores,
            "spammer",
            None,
            None,
            &IgnoreLevel::Msgs,
            Some("#other")
        ));
    }

    #[test]
    fn channel_restriction_none_channel() {
        let ignores = vec![make_entry(
            "spammer",
            vec![IgnoreLevel::All],
            Some(vec!["#general"]),
        )];
        // No current channel => does not match
        assert!(!should_ignore(
            &ignores,
            "spammer",
            None,
            None,
            &IgnoreLevel::Msgs,
            None
        ));
    }

    #[test]
    fn no_channel_restriction_matches_anywhere() {
        let ignores = vec![make_entry("spammer", vec![IgnoreLevel::All], None)];
        assert!(should_ignore(
            &ignores,
            "spammer",
            None,
            None,
            &IgnoreLevel::Msgs,
            Some("#anychannel")
        ));
        assert!(should_ignore(
            &ignores,
            "spammer",
            None,
            None,
            &IgnoreLevel::Msgs,
            None
        ));
    }

    #[test]
    fn nick_pattern_does_not_match_full_mask() {
        // Pattern without '!' should only match against nick, not full mask
        let ignores = vec![make_entry("gooduser", vec![IgnoreLevel::All], None)];
        assert!(should_ignore(
            &ignores,
            "gooduser",
            Some("spam"),
            Some("bad.host"),
            &IgnoreLevel::Msgs,
            None
        ));
    }

    #[test]
    fn full_mask_pattern_requires_matching_host() {
        let ignores = vec![make_entry("*!*@bad.host", vec![IgnoreLevel::All], None)];
        // Should match when hostname matches
        assert!(should_ignore(
            &ignores,
            "anyone",
            Some("user"),
            Some("bad.host"),
            &IgnoreLevel::Msgs,
            None
        ));
        // Should not match when hostname differs
        assert!(!should_ignore(
            &ignores,
            "anyone",
            Some("user"),
            Some("good.host"),
            &IgnoreLevel::Msgs,
            None
        ));
    }

    #[test]
    fn multiple_entries_first_match_wins() {
        let ignores = vec![
            make_entry("good*", vec![IgnoreLevel::Msgs], None),
            make_entry("*", vec![IgnoreLevel::All], None),
        ];
        // "gooduser" matches first entry for MSGS
        assert!(should_ignore(
            &ignores,
            "gooduser",
            None,
            None,
            &IgnoreLevel::Msgs,
            None
        ));
        // "baduser" only matches second entry
        assert!(should_ignore(
            &ignores,
            "baduser",
            None,
            None,
            &IgnoreLevel::Joins,
            None
        ));
    }

    #[test]
    fn pattern_does_not_match() {
        let ignores = vec![make_entry("specific_nick", vec![IgnoreLevel::All], None)];
        assert!(!should_ignore(
            &ignores,
            "other_nick",
            None,
            None,
            &IgnoreLevel::Msgs,
            None
        ));
    }

    #[test]
    fn wildcard_in_mask_components() {
        let ignores = vec![make_entry(
            "nick?!~*@192.168.*",
            vec![IgnoreLevel::All],
            None,
        )];
        assert!(should_ignore(
            &ignores,
            "nickA",
            Some("~user"),
            Some("192.168.1.1"),
            &IgnoreLevel::Msgs,
            None
        ));
        assert!(!should_ignore(
            &ignores,
            "nickAB",
            Some("~user"),
            Some("192.168.1.1"),
            &IgnoreLevel::Msgs,
            None
        ));
    }

    #[test]
    fn empty_channels_list_treated_as_no_restriction() {
        let ignores = vec![IgnoreEntry {
            mask: "spammer".to_string(),
            levels: vec![IgnoreLevel::All],
            channels: Some(Vec::new()),
        }];
        // Empty channels vec should not restrict
        assert!(should_ignore(
            &ignores,
            "spammer",
            None,
            None,
            &IgnoreLevel::Msgs,
            Some("#anywhere")
        ));
    }
}

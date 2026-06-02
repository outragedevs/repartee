//! UI-agnostic tokenizer that splits a message into text runs and `:name:` emotes.
//!
//! Matching rule: a colon, then a run of `[a-z0-9_]`, then a colon, where the inner
//! name is a known emote. `:)`, `:D`, and unknown `:foo:` stay as plain text.
//!
//! The web frontend re-implements the same rule in `web-ui/src/format.rs`
//! (`emotify_spans_with`) because the WASM crate cannot link this binary. Keep the
//! two matching rules in lockstep; both have unit tests over the same cases.

use std::ops::Range;

/// A contiguous slice of the input: either literal text or a resolved emote name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Byte range into the source string.
    Text(Range<usize>),
    /// A known emote name (without colons or `.gif`).
    Emote(String),
}

/// Tokenize using the embedded registry as the whitelist.
#[must_use]
pub fn tokenize(text: &str) -> Vec<Segment> {
    tokenize_with(text, super::contains)
}

/// Tokenize using a caller-supplied whitelist predicate (used in tests).
pub fn tokenize_with(text: &str, is_known: impl Fn(&str) -> bool) -> Vec<Segment> {
    let bytes = text.as_bytes();
    let mut out: Vec<Segment> = Vec::new();
    let mut text_start = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b':' {
            // Find the closing colon of a candidate name.
            let name_start = i + 1;
            let mut j = name_start;
            while j < bytes.len() && is_name_byte(bytes[j]) {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b':' && j > name_start {
                let name = &text[name_start..j];
                if is_known(name) {
                    if text_start < i {
                        out.push(Segment::Text(text_start..i));
                    }
                    out.push(Segment::Emote(name.to_owned()));
                    i = j + 1;
                    text_start = i;
                    continue;
                }
            }
        }
        i += 1;
    }
    if text_start < bytes.len() {
        out.push(Segment::Text(text_start..bytes.len()));
    }
    out
}

const fn is_name_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test helper: a fixed whitelist so tests don't depend on the embedded set.
    fn known(name: &str) -> bool {
        matches!(name, "usmiech" | "smutny" | "lol")
    }

    fn seg(text: &str) -> Vec<Segment> {
        tokenize_with(text, known)
    }

    #[test]
    fn plain_text_has_no_emotes() {
        assert_eq!(seg("hello world"), vec![Segment::Text(0..11)]);
    }

    #[test]
    fn single_known_emote() {
        // "a :lol: b"
        assert_eq!(
            seg("a :lol: b"),
            vec![
                Segment::Text(0..2),
                Segment::Emote("lol".into()),
                Segment::Text(7..9)
            ]
        );
    }

    #[test]
    fn unknown_token_stays_text() {
        assert_eq!(seg(":nope:"), vec![Segment::Text(0..6)]);
    }

    #[test]
    fn smiley_does_not_trigger() {
        // ":)" and ":D" must remain plain text.
        assert_eq!(seg(":) :D"), vec![Segment::Text(0..5)]);
    }

    #[test]
    fn adjacent_emotes() {
        assert_eq!(
            seg(":lol::smutny:"),
            vec![
                Segment::Emote("lol".into()),
                Segment::Emote("smutny".into())
            ]
        );
    }

    #[test]
    fn emote_at_start_and_end() {
        assert_eq!(
            seg(":usmiech:x:lol:"),
            vec![
                Segment::Emote("usmiech".into()),
                Segment::Text(9..10),
                Segment::Emote("lol".into())
            ]
        );
    }

    #[test]
    fn lone_colons() {
        assert_eq!(seg("::"), vec![Segment::Text(0..2)]);
        assert_eq!(seg("a::b"), vec![Segment::Text(0..4)]);
    }

    #[test]
    fn names_with_underscores_and_digits() {
        fn known2(n: &str) -> bool {
            n == "je_pizze" || n == "8p"
        }
        assert_eq!(
            tokenize_with(":je_pizze:", known2),
            vec![Segment::Emote("je_pizze".into())]
        );
    }
}

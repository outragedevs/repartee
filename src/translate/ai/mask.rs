use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

static URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i:https?)://[^\s<>\"'\x00-\x1f]+(?:\s*\[[A-Za-z0-9.-]+\.[A-Za-z]{2,}\])?"#)
        .expect("valid regex")
});
static CHANNEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^\w#])(#[^\s,:\x00-\x1f]+)").expect("valid regex"));
static NORM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:din(?:\s+en)?\s+vde|din|vde|iso|iec|en|ieee|rfc)\s*\d+(?:[/-]\d+)*\b")
        .expect("valid regex")
});
static BRAND_MODEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:shure|roland|doepfer|behringer|yamaha|korg|akai|arturia|focusrite|rme|motu|sennheiser|akg|rode|neumann|presonus|tascam|boss|fender|gibson|ibanez|moog|ubiquiti|unifi|mikrotik|fritzbox|fritz|raspberry|arduino|esp|beaglebone|intel|amd|nvidia|ryzen|xeon|epyc|threadripper|radeon|geforce|quadro|thinkpad|latitude|optiplex|proliant|poweredge|cisco|netgear|zyxel|dlink|samsung|crucial|kingston|seagate|toshiba|supermicro|asrock|gigabyte)\s*-?\s*[a-z]*\d+[a-z]*\b",
    )
    .expect("valid regex")
});
static MODEL_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b[a-z]{1,6}\d{1,4}[a-z]{0,3}\b").expect("valid regex"));
static PLACEHOLDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)__([A-Z]+\d+)__").expect("valid regex"));
static BARE_PLACEHOLDER_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b([A-Z]+\d+)\b").expect("valid regex"));

const UNIT_BLACKLIST: &str = "gb tb mb kb pb gib mib kib ghz mhz khz hz gbit mbit kbit gbps mbps kbps eur usd pln chf mm cm km kg mg ml cl dl std min sek h2o co2 mp3 mp4 flac x86 x64 i18n l10n a11y utf8 utf16 sha1 sha256 md5 rgb rgba hdmi vga dvi usb3 usb2 ipv4 ipv6 http2 http3 tls12 tls13 wifi6 wifi5 cat5 cat6 cat7";
const MIRC_CONTROLS: &str = "\u{2}\u{3}\u{4}\u{f}\u{16}\u{1d}\u{1f}";

#[derive(Debug)]
pub struct MaskedText {
    pub text: String,
    mapping: HashMap<String, String>,
    expected: HashMap<String, usize>,
    literal_expected: HashMap<String, usize>,
}

#[derive(Debug, Default)]
pub struct UnmaskReport {
    pub missing: Vec<String>,
    pub mangled: Vec<String>,
    pub extra: Vec<String>,
    pub count_mismatch: Vec<String>,
}

impl UnmaskReport {
    pub const fn failed(&self) -> bool {
        !self.missing.is_empty()
            || !self.mangled.is_empty()
            || !self.extra.is_empty()
            || !self.count_mismatch.is_empty()
    }
}

struct MaskState {
    mapping: HashMap<String, String>,
    seen: HashMap<(char, String), String>,
    counters: HashMap<char, usize>,
    reserved: HashSet<String>,
}

impl MaskState {
    fn new(text: &str) -> Self {
        Self {
            mapping: HashMap::new(),
            seen: HashMap::new(),
            counters: HashMap::new(),
            reserved: reserved_keys(text),
        }
    }

    fn take(&mut self, kind: char, original: &str) -> String {
        let identity = (kind, original.to_string());
        if let Some(key) = self.seen.get(&identity) {
            return format!("__{key}__");
        }
        let counter = self.counters.entry(kind).or_default();
        let key = loop {
            *counter += 1;
            let candidate = format!("{kind}{counter}");
            if self.reserved.insert(candidate.clone()) {
                break candidate;
            }
        };
        self.mapping.insert(key.clone(), original.to_string());
        self.seen.insert(identity, key.clone());
        format!("__{key}__")
    }
}

pub fn mask(text: &str, known_nicks: &[String]) -> MaskedText {
    let literal_expected = count_literal_placeholders(text);
    let mut state = MaskState::new(text);
    let mut output = replace_regex(text, &URL, 'U', &mut state, |_| true);
    output = replace_channel(&output, &mut state);
    output = replace_regex(&output, &NORM, 'T', &mut state, |_| true);
    output = replace_regex(&output, &BRAND_MODEL, 'T', &mut state, |_| true);
    output = replace_regex(&output, &MODEL_CODE, 'T', &mut state, |value| {
        value.len() >= 4
            && !UNIT_BLACKLIST
                .split_whitespace()
                .any(|unit| unit.eq_ignore_ascii_case(value))
            && !value.contains("__")
    });

    let mut nicks: Vec<&str> = known_nicks
        .iter()
        .map(String::as_str)
        .filter(|nick| !nick.is_empty())
        .collect();
    nicks.sort_unstable_by_key(|nick| std::cmp::Reverse(nick.len()));
    nicks.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    for nick in nicks {
        output = replace_nick(&output, nick, &mut state);
    }
    output = mask_formatting(&output, &mut state);

    let expected = count_placeholders(&output);
    MaskedText {
        text: output,
        mapping: state.mapping,
        expected,
        literal_expected,
    }
}

pub fn unmask(masked: &MaskedText, output: &str) -> (String, UnmaskReport) {
    let mut text = output.to_string();
    let mut report = UnmaskReport::default();

    for (key, original) in &masked.mapping {
        let expected = masked.expected.get(key).copied().unwrap_or_default();
        let exact = format!("__{key}__");
        let mut count = text.matches(&exact).count();
        if count > 0 {
            text = text.replace(&exact, original);
        }

        let mut altered = 0;
        for (open, close) in [
            ("__", "__"),
            ("«", "»"),
            ("[[", "]]"),
            ("{{", "}}"),
            ("<", ">"),
            ("**", "**"),
            ("[", "]"),
            ("⟦", "⟧"),
            ("_", "_"),
            ("*", "*"),
        ] {
            let pattern = Regex::new(&format!(
                r"(?i){}\s*{}\s*{}",
                regex::escape(open),
                regex::escape(key),
                regex::escape(close)
            ))
            .expect("escaped placeholder regex");
            let hits = pattern.find_iter(&text).count();
            if hits > 0 {
                text = pattern.replace_all(&text, original).into_owned();
                altered += hits;
            }
        }
        let (bare, bare_count) = replace_bounded_key(&text, key, original);
        text = bare;
        altered += bare_count;
        count += altered;

        if expected > 0 && count == 0 {
            report.missing.push(key.clone());
        }
        if altered > 0 && expected > 0 {
            report.mangled.push(key.clone());
        }
        if expected > 0 && count != expected {
            report.count_mismatch.push(key.clone());
        }
    }

    let remaining = count_literal_placeholders(&text);
    for (key, expected) in &masked.literal_expected {
        if remaining.get(key).copied().unwrap_or_default() != *expected {
            report.count_mismatch.push(key.clone());
        }
    }
    for capture in PLACEHOLDER.captures_iter(&text) {
        let literal = capture.get(0).map_or("", |matched| matched.as_str());
        let key = capture
            .get(1)
            .map_or_else(String::new, |matched| matched.as_str().to_ascii_uppercase());
        if !masked.mapping.contains_key(&key) && !masked.literal_expected.contains_key(literal) {
            report.extra.push(key);
        }
    }

    (text, report)
}

fn mask_formatting(text: &str, state: &mut MaskState) -> String {
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    while cursor < text.len() {
        let ch = text[cursor..]
            .chars()
            .next()
            .expect("cursor at char boundary");
        if MIRC_CONTROLS.contains(ch) {
            let end = formatting_end(text, cursor, ch);
            output.push_str(&state.take('F', &text[cursor..end]));
            cursor = end;
        } else {
            output.push(ch);
            cursor += ch.len_utf8();
        }
    }
    output
}

fn formatting_end(text: &str, start: usize, control: char) -> usize {
    let bytes = text.as_bytes();
    let mut end = start + control.len_utf8();
    let (digits, radix) = match control {
        '\u{3}' => (2, 10),
        '\u{4}' => (6, 16),
        _ => return end,
    };
    end = consume_digits(bytes, end, digits, radix);
    if bytes.get(end) == Some(&b',') {
        let background = consume_digits(bytes, end + 1, digits, radix);
        if background > end + 1 {
            end = background;
        }
    }
    end
}

fn consume_digits(bytes: &[u8], mut at: usize, limit: usize, radix: u32) -> usize {
    let start = at;
    while at < bytes.len() && at - start < limit && char::from(bytes[at]).is_digit(radix) {
        at += 1;
    }
    at
}

fn replace_regex(
    text: &str,
    pattern: &Regex,
    kind: char,
    state: &mut MaskState,
    accept: impl Fn(&str) -> bool,
) -> String {
    let mut output = String::with_capacity(text.len());
    let mut end = 0;
    for matched in pattern.find_iter(text) {
        output.push_str(&text[end..matched.start()]);
        let value = matched.as_str();
        if accept(value) {
            output.push_str(&state.take(kind, value));
        } else {
            output.push_str(value);
        }
        end = matched.end();
    }
    output.push_str(&text[end..]);
    output
}

fn replace_channel(text: &str, state: &mut MaskState) -> String {
    CHANNEL
        .replace_all(text, |captures: &regex::Captures<'_>| {
            let whole = captures.get(0).map_or("", |m| m.as_str());
            let channel = captures.get(1).map_or("", |m| m.as_str());
            let prefix_len = whole.len().saturating_sub(channel.len());
            format!("{}{}", &whole[..prefix_len], state.take('C', channel))
        })
        .into_owned()
}

fn replace_nick(text: &str, nick: &str, state: &mut MaskState) -> String {
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    while cursor < text.len() {
        let Some(end) = cursor
            .checked_add(nick.len())
            .filter(|&end| end <= text.len())
        else {
            output.push_str(&text[cursor..]);
            break;
        };
        let candidate = text.get(cursor..end);
        if candidate.is_some_and(|value| value.eq_ignore_ascii_case(nick))
            && boundary_before(text, cursor)
            && boundary_after(text, end)
        {
            output.push_str(&state.take('N', candidate.unwrap_or_default()));
            cursor = end;
        } else {
            let ch = text[cursor..]
                .chars()
                .next()
                .expect("cursor at char boundary");
            output.push(ch);
            cursor += ch.len_utf8();
        }
    }
    output
}

fn replace_bounded_key(text: &str, key: &str, replacement: &str) -> (String, usize) {
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut count = 0;
    while cursor < text.len() {
        let Some(end) = cursor
            .checked_add(key.len())
            .filter(|&end| end <= text.len())
        else {
            output.push_str(&text[cursor..]);
            break;
        };
        if text
            .get(cursor..end)
            .is_some_and(|value| value.eq_ignore_ascii_case(key))
            && simple_boundary_before(text, cursor)
            && simple_boundary_after(text, end)
        {
            output.push_str(replacement);
            cursor = end;
            count += 1;
        } else {
            let ch = text[cursor..]
                .chars()
                .next()
                .expect("cursor at char boundary");
            output.push(ch);
            cursor += ch.len_utf8();
        }
    }
    (output, count)
}

fn boundary_before(text: &str, at: usize) -> bool {
    at == 0
        || text[..at]
            .chars()
            .next_back()
            .is_none_or(|ch| !is_nick_char(ch))
}

fn boundary_after(text: &str, at: usize) -> bool {
    at == text.len() || text[at..].chars().next().is_none_or(|ch| !is_nick_char(ch))
}

fn simple_boundary_before(text: &str, at: usize) -> bool {
    at == 0
        || text[..at]
            .chars()
            .next_back()
            .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_')
}

fn simple_boundary_after(text: &str, at: usize) -> bool {
    at == text.len()
        || text[at..]
            .chars()
            .next()
            .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_')
}

fn is_nick_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || "_[]\\`^{}|-".contains(ch)
}

fn count_placeholders(text: &str) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for capture in PLACEHOLDER.captures_iter(text) {
        if let Some(key) = capture.get(1) {
            *counts.entry(key.as_str().to_ascii_uppercase()).or_default() += 1;
        }
    }
    counts
}

fn count_literal_placeholders(text: &str) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for matched in PLACEHOLDER.find_iter(text) {
        *counts.entry(matched.as_str().to_string()).or_default() += 1;
    }
    counts
}

fn reserved_keys(text: &str) -> HashSet<String> {
    PLACEHOLDER
        .captures_iter(text)
        .chain(BARE_PLACEHOLDER_KEY.captures_iter(text))
        .filter_map(|capture| capture.get(1))
        .map(|key| key.as_str().to_ascii_uppercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_and_restores_nicks_urls_channels_and_technical_ids() {
        let masked = mask(
            "alice: https://example.com on #rust with DIN VDE 701/702",
            &["alice".to_string()],
        );
        let (restored, report) = unmask(&masked, &masked.text);
        assert_eq!(
            (restored, report.failed()),
            (
                "alice: https://example.com on #rust with DIN VDE 701/702".to_string(),
                false
            )
        );
    }

    #[test]
    fn reports_a_mangled_placeholder_even_when_it_can_recover_it() {
        let masked = mask("hello alice", &["alice".to_string()]);
        let output = masked.text.replace("__N1__", "[[N1]]");
        let (_, report) = unmask(&masked, &output);
        assert_eq!(report.mangled, vec!["N1"]);
    }

    #[test]
    fn reports_a_missing_placeholder() {
        let masked = mask("hello alice", &["alice".to_string()]);
        let (_, report) = unmask(&masked, "cześć");
        assert_eq!(report.missing, vec!["N1"]);
    }

    #[test]
    fn does_not_mask_a_nick_inside_another_word() {
        let masked = mask("malice alice", &["alice".to_string()]);
        assert_eq!(masked.text, "malice __N1__");
    }

    #[test]
    fn masks_and_restores_one_and_two_character_nicks() {
        let masked = mask("k told ab", &["k".to_string(), "ab".to_string()]);
        let (restored, report) = unmask(&masked, &masked.text);
        assert_eq!(
            (masked.text, restored, report.failed()),
            (
                "__N2__ told __N1__".to_string(),
                "k told ab".to_string(),
                false
            )
        );
    }

    #[test]
    fn formatting_boundaries_follow_the_translated_span() {
        let masked = mask("\u{2}hello world\u{2}", &[]);
        let (translated, report) = unmask(&masked, "__F1__dłuższe tłumaczenie tekstu__F1__");
        assert_eq!(
            (translated, report.failed()),
            ("\u{2}dłuższe tłumaczenie tekstu\u{2}".to_string(), false)
        );
    }

    #[test]
    fn formatting_adjacent_to_protected_tokens_remains_independent() {
        let masked = mask("\u{2}#c https://example.com\u{2}", &[]);
        assert_eq!(masked.text, "__F1____C1__ __U1____F1__");
        let (restored, report) = unmask(&masked, &masked.text);
        assert_eq!(
            (restored, report.failed()),
            ("\u{2}#c https://example.com\u{2}".to_string(), false)
        );
    }

    #[test]
    fn masks_short_and_long_channel_names_completely() {
        let long = "#abcdefghijklmnopqrstuvwxyz123456789";
        let source = format!("join #c and {long}");
        let masked = mask(&source, &[]);
        assert_eq!(masked.text, "join __C1__ and __C2__");
    }

    #[test]
    fn preserves_case_distinct_urls_independently() {
        let source = "https://example.com/Foo https://example.com/foo";
        let masked = mask(source, &[]);
        assert_eq!(masked.text, "__U1__ __U2__");
        let (restored, report) = unmask(&masked, &masked.text);
        assert_eq!((restored.as_str(), report.failed()), (source, false));
    }

    #[test]
    fn masks_urls_with_case_insensitive_schemes() {
        let source = "HTTPS://Example.com/X HtTp://example.com/y";
        let masked = mask(source, &[]);
        assert_eq!(masked.text, "__U1__ __U2__");
        let (restored, report) = unmask(&masked, &masked.text);
        assert_eq!((restored.as_str(), report.failed()), (source, false));
    }

    #[test]
    fn preserves_case_distinct_irc_entities_independently() {
        let source = "Alice ALICE #Rust #rust";
        let masked = mask(source, &["alice".to_string()]);
        assert_eq!(masked.text, "__N1__ __N2__ __C1__ __C2__");
        let (restored, report) = unmask(&masked, &masked.text);
        assert_eq!((restored.as_str(), report.failed()), (source, false));
    }

    #[test]
    fn generated_placeholders_do_not_collide_with_source_tokens() {
        let source = "__N1__ N2 [[N3]] hello alice";
        let masked = mask(source, &["alice".to_string()]);
        assert_eq!(masked.text, "__N1__ N2 [[N3]] hello __N4__");
        let (restored, report) = unmask(&masked, &masked.text);
        assert_eq!((restored.as_str(), report.failed()), (source, false));
    }

    #[test]
    fn changing_a_literal_placeholder_is_reported() {
        let masked = mask("keep __N1__", &[]);
        let (_, report) = unmask(&masked, "zachowaj __n1__");
        assert!(report.failed());
    }
}

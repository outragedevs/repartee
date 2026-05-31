/// A styled text segment for rendering in the web UI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StyledSpan {
    pub text: String,
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
    /// When `Some`, this span is a clickable link. `chat_view` wraps the
    /// span text in `<a href={link} target="_blank" rel=...>`; left-click
    /// opens in a new tab and right-click yields the browser's native
    /// "open in new window" context menu.
    pub link: Option<String>,
    /// When `Some`, this span is an emote and should render as `<img>`; `text`
    /// holds the original `:name:` token (used as alt text / fallback).
    pub emote_name: Option<String>,
}

impl StyledSpan {
    /// Generate a CSS `style` string for this span.
    pub fn css(&self) -> String {
        let mut s = String::new();
        let mut sep = "";
        if let Some(ref fg) = self.fg {
            s.push_str("color:");
            s.push_str(fg);
            sep = ";";
        }
        if let Some(ref bg) = self.bg {
            s.push_str(sep);
            s.push_str("background:");
            s.push_str(bg);
            sep = ";";
        }
        if self.bold {
            s.push_str(sep);
            s.push_str("font-weight:bold");
            sep = ";";
        }
        if self.italic {
            s.push_str(sep);
            s.push_str("font-style:italic");
            sep = ";";
        }
        if self.underline {
            s.push_str(sep);
            s.push_str("text-decoration:underline");
            sep = ";";
        }
        if self.dim {
            s.push_str(sep);
            s.push_str("opacity:0.5");
        }
        s
    }

    /// Returns true if this span has any styling.
    pub fn has_style(&self) -> bool {
        self.fg.is_some()
            || self.bg.is_some()
            || self.bold
            || self.italic
            || self.underline
            || self.dim
    }
}

/// Parse irssi/mIRC format strings into styled spans.
///
/// Supported formats:
/// - `%ZRRGGBB` — 24-bit hex foreground
/// - `%zRRGGBB` — 24-bit hex background
/// - `%N` / `%n` — reset all formatting
/// - `%_` — bold toggle, `%u` — underline, `%i` — italic, `%d` — dim
/// - mIRC `\x02` bold, `\x03` color, `\x04` hex color, `\x0F` reset
/// - mIRC `\x1D` italic, `\x1E` strikethrough, `\x1F` underline
pub fn parse_format(text: &str) -> Vec<StyledSpan> {
    let mut spans = Vec::new();
    let mut current = String::new();
    let mut fg: Option<String> = None;
    let mut bg: Option<String> = None;
    let mut bold = false;
    let mut italic = false;
    let mut underline = false;
    let mut dim = false;

    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    macro_rules! flush {
        () => {
            if !current.is_empty() {
                spans.push(StyledSpan {
                    text: std::mem::take(&mut current),
                    fg: fg.clone(),
                    bg: bg.clone(),
                    bold,
                    italic,
                    underline,
                    dim,
                    link: None,
                    emote_name: None,
                });
            }
        };
    }

    macro_rules! reset {
        () => {
            fg = None;
            bg = None;
            bold = false;
            italic = false;
            underline = false;
            dim = false;
        };
    }

    while i < len {
        match chars[i] {
            // irssi format codes
            '%' if i + 1 < len => match chars[i + 1] {
                'Z' if i + 8 <= len => {
                    flush!();
                    let hex: String = chars[i + 2..i + 8].iter().collect();
                    fg = Some(format!("#{hex}"));
                    i += 8;
                }
                'z' if i + 8 <= len => {
                    flush!();
                    let hex: String = chars[i + 2..i + 8].iter().collect();
                    bg = Some(format!("#{hex}"));
                    i += 8;
                }
                'N' | 'n' => {
                    flush!();
                    reset!();
                    i += 2;
                }
                '_' => {
                    flush!();
                    bold = !bold;
                    i += 2;
                }
                'u' | 'U' => {
                    flush!();
                    underline = !underline;
                    i += 2;
                }
                'i' | 'I' => {
                    flush!();
                    italic = !italic;
                    i += 2;
                }
                'd' => {
                    flush!();
                    dim = !dim;
                    i += 2;
                }
                '%' => {
                    current.push('%');
                    i += 2;
                }
                c => {
                    // irssi single-letter color codes (%k %r %g %y %b %m %c %w + uppercase)
                    if let Some(hex) = irssi_color(c) {
                        flush!();
                        fg = Some(hex.to_string());
                        i += 2;
                    } else {
                        // Unknown %X — keep literal
                        current.push('%');
                        current.push(c);
                        i += 2;
                    }
                }
            },

            // mIRC bold
            '\x02' => {
                flush!();
                bold = !bold;
                i += 1;
            }

            // mIRC color: \x03[fg[,bg]]
            '\x03' => {
                flush!();
                i += 1;
                let mut fg_str = String::new();
                while i < len && chars[i].is_ascii_digit() && fg_str.len() < 2 {
                    fg_str.push(chars[i]);
                    i += 1;
                }
                if fg_str.is_empty() {
                    fg = None;
                    bg = None;
                } else {
                    fg = fg_str
                        .parse::<u8>()
                        .ok()
                        .and_then(mirc_color)
                        .map(String::from);
                    if i < len && chars[i] == ',' {
                        i += 1;
                        let mut bg_str = String::new();
                        while i < len && chars[i].is_ascii_digit() && bg_str.len() < 2 {
                            bg_str.push(chars[i]);
                            i += 1;
                        }
                        bg = bg_str
                            .parse::<u8>()
                            .ok()
                            .and_then(mirc_color)
                            .map(String::from);
                    }
                }
            }

            // mIRC hex color: \x04RRGGBB
            '\x04' => {
                flush!();
                i += 1;
                if i + 6 <= len && chars[i..i + 6].iter().all(|c| c.is_ascii_hexdigit()) {
                    let hex: String = chars[i..i + 6].iter().collect();
                    fg = Some(format!("#{hex}"));
                    i += 6;
                }
            }

            // mIRC reset
            '\x0F' => {
                flush!();
                reset!();
                i += 1;
            }

            // mIRC reverse (ignore — just skip)
            '\x16' => {
                i += 1;
            }

            // mIRC italic
            '\x1D' => {
                flush!();
                italic = !italic;
                i += 1;
            }

            // mIRC strikethrough (render as dim)
            '\x1E' => {
                flush!();
                dim = !dim;
                i += 1;
            }

            // mIRC underline
            '\x1F' => {
                flush!();
                underline = !underline;
                i += 1;
            }

            ch => {
                current.push(ch);
                i += 1;
            }
        }
    }

    flush!();
    spans
}

/// Walk every span and split plain-text fragments around URL matches.
///
/// Spans that already carry a `link` (or have `text` with no URL) are
/// returned unchanged. URL fragments inherit the original span's styling
/// and gain `link = Some(url)`.
///
/// The regex matches the same shapes as `src/image_preview/detect::URL_RE`
/// — `https?://` followed by characters up to the first whitespace or a
/// common closing delimiter — so server-side preview extraction and
/// client-side linkification stay in sync without sharing a crate.
#[must_use]
pub fn linkify_spans(spans: Vec<StyledSpan>) -> Vec<StyledSpan> {
    let mut out: Vec<StyledSpan> = Vec::with_capacity(spans.len());
    for span in spans {
        if span.link.is_some() {
            out.push(span);
            continue;
        }
        if !span.text.contains("http") {
            out.push(span);
            continue;
        }
        split_one(&span, &mut out);
    }
    out
}

/// Split any known `:name:` tokens inside text spans into emote spans. Spans that
/// are already links/emotes, or carry no colon, pass through. Uses the embedded
/// emote whitelist ([`crate::emotes::is_emote`]).
///
/// Matching mirrors the native TUI tokenizer (`src/emotes/parse.rs`): `:` +
/// `[a-z0-9_]+` + `:`. Keep the two in lockstep — `emotify_parity_with_rules`
/// guards the shared matching semantics.
#[must_use]
pub fn emotify_spans(spans: Vec<StyledSpan>) -> Vec<StyledSpan> {
    emotify_spans_with(spans, crate::emotes::is_emote)
}

/// `emotify_spans` with an injectable whitelist predicate (for tests).
#[must_use]
pub fn emotify_spans_with(spans: Vec<StyledSpan>, is_known: impl Fn(&str) -> bool) -> Vec<StyledSpan> {
    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        if span.link.is_some() || span.emote_name.is_some() || !span.text.contains(':') {
            out.push(span);
            continue;
        }
        let bytes = span.text.as_bytes();
        let mut text_start = 0usize;
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] == b':' {
                let name_start = i + 1;
                let mut j = name_start;
                while j < bytes.len()
                    && (bytes[j].is_ascii_lowercase()
                        || bytes[j].is_ascii_digit()
                        || bytes[j] == b'_')
                {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b':' && j > name_start {
                    let name = &span.text[name_start..j];
                    if is_known(name) {
                        if text_start < i {
                            out.push(StyledSpan {
                                text: span.text[text_start..i].to_owned(),
                                ..span.clone()
                            });
                        }
                        out.push(StyledSpan {
                            text: span.text[i..=j].to_owned(), // ":name:"
                            emote_name: Some(name.to_owned()),
                            ..StyledSpan::default()
                        });
                        i = j + 1;
                        text_start = i;
                        continue;
                    }
                }
            }
            i += 1;
        }
        if text_start < span.text.len() {
            out.push(StyledSpan { text: span.text[text_start..].to_owned(), ..span.clone() });
        }
    }
    out
}

fn split_one(span: &StyledSpan, out: &mut Vec<StyledSpan>) {
    let text = span.text.as_str();
    let mut cursor = 0usize;
    let mut found_any = false;
    for m in find_url_matches(text) {
        let (start, end) = m;
        if start > cursor {
            out.push(child_span(span, &text[cursor..start], None));
        }
        let url = text[start..end].to_owned();
        out.push(child_span(span, &text[start..end], Some(url)));
        cursor = end;
        found_any = true;
    }
    if found_any {
        if cursor < text.len() {
            out.push(child_span(span, &text[cursor..], None));
        }
    } else {
        out.push(span.clone());
    }
}

fn child_span(parent: &StyledSpan, text: &str, link: Option<String>) -> StyledSpan {
    StyledSpan {
        text: text.to_owned(),
        fg: parent.fg.clone(),
        bg: parent.bg.clone(),
        bold: parent.bold,
        italic: parent.italic,
        underline: parent.underline,
        dim: parent.dim,
        link,
        emote_name: None,
    }
}

/// Hand-rolled URL scanner. We avoid pulling the `regex` crate into the
/// WASM bundle (≈300 KB after gzip) for what amounts to a single linear
/// pass over the text.
fn find_url_matches(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if let Some(end) = match_url(bytes, i) {
            out.push((i, end));
            i = end;
        } else {
            // Advance by one character, not one byte, so we don't split a
            // multi-byte UTF-8 codepoint and corrupt downstream slicing.
            i += utf8_char_len(bytes, i);
        }
    }
    out
}

fn match_url(b: &[u8], i: usize) -> Option<usize> {
    let rem = &b[i..];
    let scheme_len = if rem.starts_with(b"https://") {
        8
    } else if rem.starts_with(b"http://") {
        7
    } else {
        return None;
    };
    // Require at least one host character after the scheme. Otherwise a
    // bare "https://" anywhere in chat would match.
    let mut j = i + scheme_len;
    let start_body = j;
    while j < b.len() && !is_url_terminator(b[j]) {
        j += 1;
    }
    if j == start_body {
        return None;
    }
    Some(j)
}

const fn is_url_terminator(c: u8) -> bool {
    matches!(
        c,
        b' ' | b'\t' | b'\n' | b'\r' | b'<' | b'>' | b'"' | b'\'' | b')' | b']' | b'}'
    )
}

const fn utf8_char_len(b: &[u8], i: usize) -> usize {
    let c = b[i];
    if c < 0x80 {
        1
    } else if c < 0xC0 {
        // Continuation byte — shouldn't happen at scan position. Skip 1
        // to make forward progress.
        1
    } else if c < 0xE0 {
        2
    } else if c < 0xF0 {
        3
    } else {
        4
    }
}

/// irssi single-letter color codes (%k %r %g %m etc).
fn irssi_color(code: char) -> Option<&'static str> {
    match code {
        'k' => Some("#000000"),
        'K' => Some("#555555"),
        'r' => Some("#aa0000"),
        'R' => Some("#ff5555"),
        'g' => Some("#00aa00"),
        'G' => Some("#55ff55"),
        'y' => Some("#aa5500"),
        'Y' => Some("#ffff55"),
        'b' => Some("#0000aa"),
        'B' => Some("#5555ff"),
        'm' => Some("#aa00aa"),
        'M' => Some("#ff55ff"),
        'c' => Some("#00aaaa"),
        'C' => Some("#55ffff"),
        'w' => Some("#aaaaaa"),
        'W' => Some("#ffffff"),
        _ => None,
    }
}

/// Convert a mIRC color code (0-15) to a CSS hex color.
fn mirc_color(code: u8) -> Option<&'static str> {
    match code {
        0 => Some("#ffffff"),
        1 => Some("#000000"),
        2 => Some("#00007f"),
        3 => Some("#009300"),
        4 => Some("#ff0000"),
        5 => Some("#7f0000"),
        6 => Some("#9c009c"),
        7 => Some("#fc7f00"),
        8 => Some("#ffff00"),
        9 => Some("#00fc00"),
        10 => Some("#009393"),
        11 => Some("#00ffff"),
        12 => Some("#0000fc"),
        13 => Some("#ff00ff"),
        14 => Some("#7f7f7f"),
        15 => Some("#d2d2d2"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> StyledSpan {
        StyledSpan {
            text: text.to_owned(),
            ..StyledSpan::default()
        }
    }

    fn styled(text: &str) -> StyledSpan {
        StyledSpan {
            text: text.to_owned(),
            fg: Some("#ff0000".to_owned()),
            bold: true,
            ..StyledSpan::default()
        }
    }

    #[test]
    fn linkify_no_url_returns_unchanged() {
        let spans = vec![plain("just a normal message")];
        let out = linkify_spans(spans.clone());
        assert_eq!(out, spans);
    }

    fn known(name: &str) -> bool {
        matches!(name, "usmiech" | "lol")
    }

    #[test]
    fn splits_known_emote_into_emote_span() {
        let out = emotify_spans_with(vec![plain("hi :lol: x")], known);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].text, "hi ");
        assert_eq!(out[1].emote_name.as_deref(), Some("lol"));
        assert_eq!(out[1].text, ":lol:");
        assert_eq!(out[2].text, " x");
    }

    #[test]
    fn unknown_and_smileys_untouched() {
        let out = emotify_spans_with(vec![plain(":) :nope:")], known);
        assert_eq!(out.len(), 1);
        assert!(out[0].emote_name.is_none());
        assert_eq!(out[0].text, ":) :nope:");
    }

    #[test]
    fn no_match_is_noop() {
        let out = emotify_spans_with(vec![plain(":zzz:")], known);
        assert_eq!(out.len(), 1);
        assert!(out[0].emote_name.is_none());
    }

    #[test]
    fn styled_span_keeps_style_on_surrounding_text() {
        let out = emotify_spans_with(vec![styled("a :lol: b")], known);
        assert_eq!(out.len(), 3);
        assert!(out[0].bold && out[0].fg.is_some()); // surrounding text keeps style
        assert!(out[1].emote_name.is_some());
        assert!(!out[1].bold); // emote span itself is unstyled
        assert!(out[2].bold);
    }

    #[test]
    fn link_span_is_not_emotified() {
        let link =
            StyledSpan { text: ":lol:".to_owned(), link: Some("x".to_owned()), ..StyledSpan::default() };
        let out = emotify_spans_with(vec![link], known);
        assert_eq!(out.len(), 1);
        assert!(out[0].emote_name.is_none());
    }

    #[test]
    fn emotify_spans_uses_embedded_whitelist() {
        // The zero-arg entry point uses the build-time embedded set.
        let out = emotify_spans(vec![plain(":usmiech:")]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].emote_name.as_deref(), Some("usmiech"));
    }

    #[test]
    fn linkify_single_url_in_middle() {
        let out = linkify_spans(vec![plain("see https://example.com/x.png please")]);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].text, "see ");
        assert!(out[0].link.is_none());
        assert_eq!(out[1].text, "https://example.com/x.png");
        assert_eq!(out[1].link.as_deref(), Some("https://example.com/x.png"));
        assert_eq!(out[2].text, " please");
        assert!(out[2].link.is_none());
    }

    #[test]
    fn linkify_url_at_start() {
        let out = linkify_spans(vec![plain("https://a.example/x more text")]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].text, "https://a.example/x");
        assert_eq!(out[0].link.as_deref(), Some("https://a.example/x"));
        assert_eq!(out[1].text, " more text");
    }

    #[test]
    fn linkify_url_at_end() {
        let out = linkify_spans(vec![plain("text then https://a.example/x")]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].text, "text then ");
        assert_eq!(out[1].link.as_deref(), Some("https://a.example/x"));
    }

    #[test]
    fn linkify_two_urls_with_text_between() {
        let out = linkify_spans(vec![plain(
            "a https://a.example/x b https://b.example/y c",
        )]);
        let urls: Vec<_> = out.iter().filter_map(|s| s.link.clone()).collect();
        assert_eq!(
            urls,
            vec![
                "https://a.example/x".to_owned(),
                "https://b.example/y".to_owned(),
            ]
        );
    }

    #[test]
    fn linkify_preserves_styling_on_each_fragment() {
        let out = linkify_spans(vec![styled("see https://x.example/p ok")]);
        // Every fragment must inherit fg + bold from the styled parent.
        for span in &out {
            assert_eq!(span.fg.as_deref(), Some("#ff0000"));
            assert!(span.bold);
        }
    }

    #[test]
    fn linkify_strips_trailing_paren() {
        // The detector regex stops URLs at ')'. In real chat, "(see https://x.example/p)"
        // should produce "https://x.example/p" without the closing paren.
        let out = linkify_spans(vec![plain("(https://x.example/p)")]);
        let url = out.iter().find_map(|s| s.link.clone()).unwrap();
        assert_eq!(url, "https://x.example/p");
    }

    #[test]
    fn linkify_skips_bare_scheme() {
        // "https://" with nothing after must not match — would be confusing
        // to render an empty <a>.
        let out = linkify_spans(vec![plain("the prefix https:// alone")]);
        assert!(out.iter().all(|s| s.link.is_none()));
    }

    #[test]
    fn linkify_ignores_already_linked_span() {
        let already = StyledSpan {
            text: "click me".to_owned(),
            link: Some("https://x.example/p".to_owned()),
            ..StyledSpan::default()
        };
        let out = linkify_spans(vec![already.clone()]);
        assert_eq!(out, vec![already]);
    }

    #[test]
    fn linkify_handles_multibyte_text_around_url() {
        // Cyrillic text around URL — must not panic on UTF-8 boundary.
        let out = linkify_spans(vec![plain("привет https://a.example/x мир")]);
        let url = out.iter().find_map(|s| s.link.clone()).unwrap();
        assert_eq!(url, "https://a.example/x");
        // Reconstructed text must equal input (no bytes lost or duplicated).
        let reassembled: String = out.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(reassembled, "привет https://a.example/x мир");
    }

    #[test]
    fn linkify_handles_http_scheme_too() {
        let out = linkify_spans(vec![plain("http://insecure.example/x")]);
        assert_eq!(out[0].link.as_deref(), Some("http://insecure.example/x"));
    }

    #[test]
    fn linkify_does_not_match_ftp_or_other_schemes() {
        let out = linkify_spans(vec![plain("ftp://example.com/x")]);
        assert!(out.iter().all(|s| s.link.is_none()));
    }

    #[test]
    fn parse_format_default_link_is_none() {
        let spans = parse_format("plain text");
        assert!(spans.iter().all(|s| s.link.is_none()));
    }
}

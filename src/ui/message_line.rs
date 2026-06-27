use unicode_width::UnicodeWidthStr;

use crate::config::AppConfig;
use crate::state::buffer::{Message, MessageType};
use crate::theme::{StyledSpan, parse_format_string, resolve_abstractions};
use crate::ui::styled_text::styled_spans_to_line;
use ratatui::style::Color;
use ratatui::text::Line;

/// Render a single message into a themed ratatui Line.
///
/// When `emote_sizing` is `Some` (emotes enabled, render=Graphical, terminal
/// supports graphics), known `:name:` tokens in the message body are rewritten
/// to placeholders sized per the emote's cell footprint so the wrapper reserves
/// the right number of cells for the inline image; otherwise the literal
/// `:name:` text is kept.
pub fn render_message(
    msg: &Message,
    is_own: bool,
    theme: &crate::theme::ThemeFile,
    config: &AppConfig,
    nick_fg_override: Option<Color>,
    emote_sizing: Option<crate::ui::emote_layout::EmoteSizing>,
) -> Line<'static> {
    let abstracts = &theme.abstracts;

    // MentionLog: pre-formatted line — render text as-is, no timestamp/nick column.
    // Emotes are still rewritten so mention rows match the channel/web rendering.
    if msg.message_type == MessageType::MentionLog {
        let body = emotify_message_text(&msg.text, emote_sizing);
        let spans = parse_format_string(&body, &[]);
        return styled_spans_to_line(&spans);
    }

    // 1. Format timestamp
    let ts = msg
        .timestamp
        .with_timezone(&chrono::Local)
        .format(&config.general.timestamp_format)
        .to_string();
    let ts_format = abstracts
        .get("timestamp")
        .cloned()
        .unwrap_or_else(|| "$*".to_string());
    let ts_resolved = resolve_abstractions(&ts_format, abstracts, 0);
    let ts_spans = parse_format_string(&ts_resolved, &[&ts]);

    // 2. Build message spans based on type
    let msg_spans = if msg.message_type == MessageType::Event {
        render_event(msg, theme)
    } else {
        render_chat_message(msg, is_own, theme, config, nick_fg_override, emote_sizing)
    };

    // 3. Combine: timestamp + space + message
    let separator = StyledSpan {
        text: " ".to_string(),
        fg: None,
        bg: None,
        bold: false,
        italic: false,
        underline: false,
        dim: false,
    };

    let mut all_spans = ts_spans;
    all_spans.push(separator);
    all_spans.extend(msg_spans);

    styled_spans_to_line(&all_spans)
}

fn render_event(msg: &Message, theme: &crate::theme::ThemeFile) -> Vec<StyledSpan> {
    let events = &theme.formats.events;
    let abstracts = &theme.abstracts;

    if let Some(event_key) = &msg.event_key
        && let Some(format) = events.get(event_key)
    {
        let resolved = resolve_abstractions(format, abstracts, 0);
        let params: Vec<&str> = msg
            .event_params
            .as_ref()
            .map(|p| p.iter().map(String::as_str).collect())
            .unwrap_or_default();
        return parse_format_string(&resolved, &params);
    }
    // Fallback: parse text directly (may contain inline format codes).
    parse_format_string(&msg.text, &[])
}

fn render_chat_message(
    msg: &Message,
    is_own: bool,
    theme: &crate::theme::ThemeFile,
    config: &AppConfig,
    nick_fg_override: Option<Color>,
    emote_sizing: Option<crate::ui::emote_layout::EmoteSizing>,
) -> Vec<StyledSpan> {
    let abstracts = &theme.abstracts;
    let messages = &theme.formats.messages;

    let nick = msg.nick.as_deref().unwrap_or("");
    let nick_mode = msg.nick_mode.as_deref().unwrap_or("");
    let nick_width = config.display.nick_column_width as usize;
    let max_len = config.display.nick_max_length as usize;

    // Truncate nick accounting for mode prefix width, so mode+nick fits the column
    let mode_width = nick_mode.width();
    let nick_budget = max_len.saturating_sub(mode_width);
    let mut display_nick = if config.display.nick_truncation {
        super::truncate_with_plus(nick, nick_budget)
    } else {
        nick.to_string()
    };

    // Pad combined mode+nick to fill column width (display columns, not char count)
    let total_len = nick_mode.width() + display_nick.width();
    let pad_size = nick_width.saturating_sub(total_len);

    let padded_nick_mode = match config.display.nick_alignment {
        crate::config::NickAlignment::Right => {
            format!("{}{}", " ".repeat(pad_size), nick_mode)
        }
        crate::config::NickAlignment::Center => {
            let left = pad_size / 2;
            let right = pad_size - left;
            display_nick = format!("{}{}", display_nick, " ".repeat(right));
            format!("{}{}", " ".repeat(left), nick_mode)
        }
        crate::config::NickAlignment::Left => {
            display_nick = format!("{}{}", display_nick, " ".repeat(pad_size));
            nick_mode.to_string()
        }
    };

    // Determine format key
    let format_key = match msg.message_type {
        MessageType::Action => "action",
        MessageType::Notice => "notice",
        _ if is_own => "own_msg",
        _ if msg.highlight => "pubmsg_mention",
        _ => "pubmsg",
    };

    let msg_format = messages
        .get(format_key)
        .cloned()
        .unwrap_or_else(|| "$0 $1".to_string());
    let resolved = resolve_abstractions(&msg_format, abstracts, 0);
    // params: $0=displayNick, $1=text, $2=paddedNickMode
    let body = emotify_message_text(&msg.text, emote_sizing);
    let mut spans = parse_format_string(&resolved, &[&display_nick, &body, &padded_nick_mode]);

    // Apply nick color override: recolor spans containing the nick text.
    // Only applies to pubmsg (not own, mention, highlight, action, notice).
    if let Some(color) = nick_fg_override {
        for span in &mut spans {
            // Match the span that contains exactly the nick text (trimmed).
            // The nick is rendered via {pubnick $0} which creates a separate span.
            // Use ASCII case-insensitive compare — IRC nicks are ASCII, avoids allocation.
            if !span.text.is_empty() && span.text.trim().eq_ignore_ascii_case(&display_nick) {
                span.fg = Some(color);
            }
        }
    }

    spans
}

/// Replace known `:name:` tokens with PUA placeholders so the wrapper reserves
/// cells for the inline image. Each emote's placeholder width is its cell
/// footprint (`sizing.footprint().0`) so wide GIFs reserve more columns. No-op
/// when `sizing` is `None` (text mode) or the text contains no emote. The emote's
/// placeholder index is its position in `emotes::names()` (binary-searchable,
/// matching the animator's lookup).
#[must_use]
fn emotify_message_text(
    text: &str,
    sizing: Option<crate::ui::emote_layout::EmoteSizing>,
) -> String {
    use crate::emotes;
    use crate::emotes::parse::Segment;
    use crate::ui::emote_layout::placeholder_for_emote;

    let Some(sizing) = sizing else {
        return text.to_owned();
    };
    if !text.contains(':') {
        return text.to_owned();
    }
    let segs = emotes::parse::tokenize(text);
    if !segs.iter().any(|s| matches!(s, Segment::Emote(_))) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    for seg in segs {
        match seg {
            Segment::Text(range) => out.push_str(&text[range]),
            Segment::Emote(name) => {
                // Accept either language: resolve PL stem or EN alias to the index.
                if let Some(idx) = emotes::resolve(&name) {
                    let (cols, _rows) = sizing.footprint(idx);
                    out.push_str(&placeholder_for_emote(idx, cols));
                } else {
                    out.push(':');
                    out.push_str(&name);
                    out.push(':');
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod emote_tests {
    use super::emotify_message_text;
    use crate::ui::emote_layout::{EmoteSizing, decode_placeholder_index};

    /// A representative cell size (8×16) with the conservative one-row cap.
    fn sizing() -> EmoteSizing {
        EmoteSizing {
            font_w: 8,
            font_h: 16,
            max_cols: 8,
            max_rows: 1,
        }
    }

    fn placeholder_count(s: &str) -> usize {
        s.chars()
            .filter(|c| decode_placeholder_index(*c).is_some())
            .count()
    }

    #[test]
    fn english_alias_becomes_placeholder() {
        let sizing = sizing();
        let idx = crate::emotes::resolve("smile").expect("smile resolves");
        let expected = usize::from(sizing.footprint(idx).0);
        let out = emotify_message_text("hi :smile: x", Some(sizing));
        assert_eq!(
            placeholder_count(&out),
            expected,
            ":smile: must become a footprint-width placeholder run"
        );
        assert!(!out.contains(":smile:"));
    }

    #[test]
    fn known_token_becomes_placeholder_when_graphical() {
        let sizing = sizing();
        let idx = crate::emotes::resolve("usmiech").expect("usmiech resolves");
        let expected = usize::from(sizing.footprint(idx).0);
        let out = emotify_message_text("hi :usmiech: x", Some(sizing));
        assert_eq!(placeholder_count(&out), expected);
        assert!(!out.contains(":usmiech:"));
        assert!(out.starts_with("hi "));
        assert!(out.ends_with(" x"));
    }

    #[test]
    fn wide_emote_reserves_more_columns_than_a_square_one() {
        // A wide GIF (a banner like 40×18) must reserve strictly more cells than a
        // small square one (:smile:/15×15) — the core of "size to the original".
        let sizing = sizing();
        let square = crate::emotes::resolve("smile").expect("smile");
        let names_len = u32::try_from(crate::emotes::names().len()).unwrap_or(u32::MAX);
        let wide = (0..names_len)
            .find(|&i| {
                crate::emotes::native_size(i).is_some_and(|(w, h)| w >= h.saturating_mul(2))
            })
            .expect("the curated set has at least one wide emote");
        let square_cols = usize::from(sizing.footprint(square).0);
        let wide_cols = usize::from(sizing.footprint(wide).0);
        assert!(
            wide_cols > square_cols,
            "wide emote ({wide_cols} cols) must reserve more than square ({square_cols} cols)"
        );
    }

    #[test]
    fn unchanged_when_not_graphical() {
        assert_eq!(
            emotify_message_text("hi :usmiech: x", None),
            "hi :usmiech: x"
        );
    }

    #[test]
    fn unknown_token_unchanged() {
        assert_eq!(emotify_message_text(":nope: :)", Some(sizing())), ":nope: :)");
    }

    #[test]
    fn no_colon_fast_path() {
        assert_eq!(emotify_message_text("plain text", Some(sizing())), "plain text");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_config;
    use crate::state::buffer::{Message, MessageType};
    use crate::theme::loader::default_theme;
    use chrono::Utc;

    fn test_message(nick: &str, text: &str, msg_type: MessageType) -> Message {
        Message {
            id: 1,
            timestamp: Utc::now(),
            message_type: msg_type,
            nick: Some(nick.to_string()),
            nick_mode: None,
            text: text.to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
        }
    }

    #[test]
    fn render_own_message_contains_nick_and_text() {
        let msg = test_message("me", "hello world", MessageType::Message);
        let theme = default_theme();
        let config = default_config();
        let line = render_message(&msg, true, &theme, &config, None, None);
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(text.contains("me"));
        assert!(text.contains("hello world"));
    }

    #[test]
    fn render_public_message() {
        let msg = test_message("bob", "hi there", MessageType::Message);
        let theme = default_theme();
        let config = default_config();
        let line = render_message(&msg, false, &theme, &config, None, None);
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(text.contains("bob"));
        assert!(text.contains("hi there"));
    }

    #[test]
    fn render_non_ascii_nick_does_not_panic() {
        // Nick "Ñóçk" is 4 chars but 8 bytes — byte-based truncation would panic
        let msg = test_message("Ñóçk", "hola mundo", MessageType::Message);
        let theme = default_theme();
        let mut config = default_config();
        config.display.nick_truncation = true;
        config.display.nick_max_length = 4;
        let line = render_message(&msg, false, &theme, &config, None, None);
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(text.contains("Ñóçk"));
        assert!(text.contains("hola mundo"));
    }

    #[test]
    fn render_non_ascii_nick_truncation() {
        // Nick with 6 multi-byte chars, truncated to 4 chars with '+' indicator
        let msg = test_message("Ñóçkéd", "hi", MessageType::Message);
        let theme = default_theme();
        let mut config = default_config();
        config.display.nick_truncation = true;
        config.display.nick_max_length = 4;
        let line = render_message(&msg, false, &theme, &config, None, None);
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        // Should be truncated to first 3 chars + '+': "Ñóç+"
        assert!(text.contains("Ñóç+"));
        assert!(!text.contains("Ñóçk"));
        assert!(text.contains("hi"));
    }

    #[test]
    fn render_long_ascii_nick_truncation() {
        // "verylongnick" truncated to 7 chars → "verylo+"
        let msg = test_message("verylongnick", "test", MessageType::Message);
        let theme = default_theme();
        let mut config = default_config();
        config.display.nick_truncation = true;
        config.display.nick_max_length = 7;
        let line = render_message(&msg, false, &theme, &config, None, None);
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(text.contains("verylo+"));
        assert!(!text.contains("verylon"));
    }

    #[test]
    fn render_nick_truncation_accounts_for_mode_prefix() {
        // With @ mode prefix (1 char), max_len=8: nick budget = 8-1 = 7
        // "verylongnick" truncated to 7 → "verylo+"
        let mut msg = test_message("verylongnick", "test", MessageType::Message);
        msg.nick_mode = Some("@".to_string());
        let theme = default_theme();
        let mut config = default_config();
        config.display.nick_truncation = true;
        config.display.nick_max_length = 8;
        let line = render_message(&msg, false, &theme, &config, None, None);
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        // Nick should be "verylo+" (7 chars), not "verylon+" (8 chars)
        assert!(text.contains("verylo+"), "expected 'verylo+' in: {text}");
        assert!(
            !text.contains("verylon"),
            "mode prefix should reduce nick budget"
        );

        // Without mode prefix, same nick gets full budget: "verylon+" (8 chars)
        let msg2 = test_message("verylongnick", "test", MessageType::Message);
        let line2 = render_message(&msg2, false, &theme, &config, None, None);
        let text2: String = line2.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(
            text2.contains("verylon+"),
            "without mode, nick should be 'verylon+' in: {text2}"
        );
    }

    #[test]
    fn render_event_message() {
        let msg = Message {
            id: 1,
            timestamp: Utc::now(),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text: "system message".to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
        };
        let theme = default_theme();
        let config = default_config();
        let line = render_message(&msg, false, &theme, &config, None, None);
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(text.contains("system message"));
    }

    #[test]
    fn e2e_trust_notice_renders_full_body_live() {
        // End-to-end regression for the empty-[E2E] bug: a trust-change notice
        // built by the production helper must render its full body on the LIVE
        // path (theme template `[E2E] $*`), not collapse to a bare "[E2E]".
        use crate::e2e::manager::TrustChange;
        let (body, key) = crate::irc::events::trust_change_body(&TrustChange::HandleChanged {
            old_handle: "~r@a.host".to_string(),
            new_handle: "~r@b.host".to_string(),
            fingerprint: [0xCD; 16],
        })
        .expect("HandleChanged must produce a notice");
        let msg = crate::irc::events::e2e_event_message(1, body, key, true);
        let mut theme = default_theme();
        // Mirror themes/default.theme: the live path only collapses to a bare
        // "[E2E]" when the theme actually defines the e2e_* template (which
        // expands `$*` from event_params). The built-in fallback omits it.
        theme
            .formats
            .events
            .insert("e2e_warning".to_string(), "%Ze0af68[E2E]%N $*".to_string());
        let config = default_config();
        let line = render_message(&msg, false, &theme, &config, None, None);
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(
            text.contains("appeared under new handle"),
            "live render lost the notice body: {text:?}"
        );
        assert!(
            text.contains("~r@b.host"),
            "live render lost the new handle: {text:?}"
        );
        // The banner must appear exactly once (template prepends it; the body
        // must not embed a second copy).
        assert_eq!(text.matches("[E2E]").count(), 1, "banner doubled: {text:?}");
    }

    #[test]
    fn render_message_with_nick_color_override() {
        let msg = test_message("alice", "hello", MessageType::Message);
        let mut theme = default_theme();
        // Use a format that creates a separate nick span (like real themes do).
        // %Z7aa2f7 applies a color to the nick, producing its own StyledSpan.
        theme
            .abstracts
            .insert("pubnick".into(), "%Z7aa2f7$*%N".into());
        theme
            .formats
            .messages
            .insert("pubmsg".into(), "{msgnick $2 {pubnick $0}}$1".into());
        let config = default_config();
        let override_color = Color::Rgb(255, 0, 0);
        let line = render_message(&msg, false, &theme, &config, Some(override_color), None);
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(text.contains("alice"));
        // Check that at least one span has the override color
        let has_override = line
            .spans
            .iter()
            .any(|s| s.style.fg == Some(ratatui::style::Color::Rgb(255, 0, 0)));
        assert!(has_override, "nick color override should be applied");
    }
}

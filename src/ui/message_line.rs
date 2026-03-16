use unicode_width::UnicodeWidthStr;

use crate::config::AppConfig;
use crate::state::buffer::{Message, MessageType};
use crate::theme::{StyledSpan, parse_format_string, resolve_abstractions};
use crate::ui::styled_text::styled_spans_to_line;
use ratatui::text::Line;

/// Render a single message into a themed ratatui Line.
pub fn render_message(
    msg: &Message,
    is_own: bool,
    theme: &crate::theme::ThemeFile,
    config: &AppConfig,
) -> Line<'static> {
    let abstracts = &theme.abstracts;

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
        render_chat_message(msg, is_own, theme, config)
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
    // Fallback: parse text directly (may contain inline format codes)
    parse_format_string(&msg.text, &[])
}

fn render_chat_message(
    msg: &Message,
    is_own: bool,
    theme: &crate::theme::ThemeFile,
    config: &AppConfig,
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
    parse_format_string(&resolved, &[&display_nick, &msg.text, &padded_nick_mode])
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
        let line = render_message(&msg, true, &theme, &config);
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(text.contains("me"));
        assert!(text.contains("hello world"));
    }

    #[test]
    fn render_public_message() {
        let msg = test_message("bob", "hi there", MessageType::Message);
        let theme = default_theme();
        let config = default_config();
        let line = render_message(&msg, false, &theme, &config);
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
        let line = render_message(&msg, false, &theme, &config);
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
        let line = render_message(&msg, false, &theme, &config);
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
        let line = render_message(&msg, false, &theme, &config);
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
        let line = render_message(&msg, false, &theme, &config);
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        // Nick should be "verylo+" (7 chars), not "verylon+" (8 chars)
        assert!(text.contains("verylo+"), "expected 'verylo+' in: {text}");
        assert!(
            !text.contains("verylon"),
            "mode prefix should reduce nick budget"
        );

        // Without mode prefix, same nick gets full budget: "verylon+" (8 chars)
        let msg2 = test_message("verylongnick", "test", MessageType::Message);
        let line2 = render_message(&msg2, false, &theme, &config);
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
        let line = render_message(&msg, false, &theme, &config);
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(text.contains("system message"));
    }
}

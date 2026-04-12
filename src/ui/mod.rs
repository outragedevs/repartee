pub mod buffer_list;
pub mod chat_view;
pub mod image_overlay;
pub mod input;
pub mod layout;
pub mod message_line;
pub mod nick_list;
pub mod shell_view;
pub mod splash;
pub mod status_line;
pub mod styled_text;
pub mod topic_bar;

use std::io::{self, Write};

use color_eyre::eyre::Result;
use crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::prelude::*;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub type Tui = Terminal<CrosstermBackend<Box<dyn Write + Send>>>;

/// Set up the terminal for TUI mode (local stdout).
pub fn setup_terminal() -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(Box::new(stdout) as Box<dyn Write + Send>);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// Set up a terminal backed by a socket writer (for remote attach).
pub fn setup_socket_terminal(
    mut writer: Box<dyn Write + Send>,
    cols: u16,
    rows: u16,
) -> Result<Tui> {
    // Send terminal setup sequences through the socket to the shim's stdout.
    execute!(
        writer,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(writer);
    // Use Fixed viewport — Fullscreen would call backend.size() every frame,
    // which queries ioctl on stdout (= /dev/null in the daemon) and gets garbage.
    // Fixed viewport uses the stored area, updated via terminal.resize() on SIGWINCH.
    let terminal = Terminal::with_options(
        backend,
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, cols, rows)),
        },
    )?;
    Ok(terminal)
}

/// Restore the terminal to normal mode.
pub fn restore_terminal(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Truncate a string to fit `max_len` display columns, appending `+` if truncated.
/// Uses `unicode-width` for correct emoji/CJK width measurement.
pub fn truncate_with_plus(s: &str, max_len: usize) -> String {
    if max_len == 0 {
        return String::new();
    }
    if s.width() <= max_len {
        return s.to_string();
    }
    if max_len <= 1 {
        return "+".to_string();
    }
    // Accumulate display width char by char, leaving room for the `+` suffix.
    let budget = max_len - 1;
    let mut used = 0;
    let mut byte_end = 0;
    for (i, ch) in s.char_indices() {
        let w = ch.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        used += w;
        byte_end = i + ch.len_utf8();
    }
    format!("{}+", &s[..byte_end])
}

/// Count visible display width from parsed format spans (ignoring color codes).
/// Uses `unicode-width` for correct emoji/CJK width measurement.
pub fn visible_len(spans: &[crate::theme::StyledSpan]) -> usize {
    spans.iter().map(|s| s.text.width()).sum()
}

// === Mention log line formatting ===

// Theme colors (same hex values used in default.theme).
const COLOR_TIMESTAMP: &str = "6e738d"; // muted gray — matches {timestamp} abstract
const COLOR_NETWORK: &str = "565f89"; // dim gray — matches hostname in join events
const COLOR_CHANNEL: &str = "7aa2f7"; // accent blue — matches {channel} abstract
const COLOR_SEP: &str = "7aa2f7"; // accent blue — nick separator ❯

/// Build a pre-formatted mention log line with irssi `%Z` color codes.
///
/// Layout: `[datetime] [network] [channel] nick❯ text`
///
/// Called from both `irc/events.rs` (live mentions) and `app.rs` (DB reload).
pub fn format_mention_line(
    datetime: &str,
    network: &str,
    channel: &str,
    nick: &str,
    text: &str,
    nick_sat: f32,
    nick_lit: f32,
) -> String {
    let nick_hex = crate::nick_color::nick_color_hex(nick, nick_sat, nick_lit);

    format!(
        "%Z{COLOR_TIMESTAMP}[{datetime}]%N \
         %Z{COLOR_NETWORK}[{network}]%N \
         %Z{COLOR_CHANNEL}[{channel}]%N \
         %Z{nick_hex}%_{nick}%_%N\
         %Z{COLOR_SEP}\u{276F}%N {text}",
    )
}

/// Word-wrap a ratatui `Line` to fit within `width` display columns.
///
/// Continuation lines are indented with `indent` spaces.  Breaks prefer
/// word boundaries (spaces); falls back to char boundaries when a single
/// word exceeds the available width.
///
/// Uses `unicode-width` for correct emoji/CJK column measurement —
/// matching ratatui's internal rendering.
pub fn wrap_line(line: Line<'static>, width: usize, indent: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![line];
    }

    // Flatten to (char, display_width, Style) and measure total width in one pass.
    let styled_chars: Vec<(char, usize, Style)> = line
        .spans
        .iter()
        .flat_map(|span| {
            span.content
                .chars()
                .map(move |ch| (ch, ch.width().unwrap_or(0), span.style))
        })
        .collect();

    let total_width: usize = styled_chars.iter().map(|(_, w, _)| w).sum();
    if total_width <= width {
        return vec![line];
    }

    let mut result: Vec<Line<'static>> = Vec::new();
    let mut pos = 0;
    let mut first_line = true;

    while pos < styled_chars.len() {
        let line_width = if first_line {
            width
        } else {
            width.saturating_sub(indent)
        };

        if line_width == 0 {
            break;
        }

        // Walk forward accumulating display width until we exceed the budget.
        let mut used = 0;
        let mut end = pos;
        while end < styled_chars.len() {
            let w = styled_chars[end].1;
            if used + w > line_width {
                break;
            }
            used += w;
            end += 1;
        }

        // Last chunk — take everything remaining.
        if end >= styled_chars.len() {
            let built = build_line_from_styled_chars(&styled_chars[pos..], !first_line, indent);
            result.push(built);
            break;
        }

        // Try to find a word break point (last space within the chunk).
        let chunk = &styled_chars[pos..end];
        let break_at = chunk.iter().rposition(|(ch, _, _)| *ch == ' ');

        let actual_end = break_at.map_or(end, |break_pos| pos + break_pos + 1);

        let built =
            build_line_from_styled_chars(&styled_chars[pos..actual_end], !first_line, indent);
        result.push(built);

        pos = actual_end;
        first_line = false;

        // Skip leading spaces on continuation lines.
        while pos < styled_chars.len() && styled_chars[pos].0 == ' ' {
            pos += 1;
        }
    }

    if result.is_empty() {
        result.push(line);
    }

    result
}

/// Build a `Line` from a slice of `(char, display_width, Style)` tuples, grouping
/// consecutive chars with the same style into spans.  Prepends `indent` spaces when
/// `is_continuation` is true.
fn build_line_from_styled_chars(
    chars: &[(char, usize, Style)],
    is_continuation: bool,
    indent: usize,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();

    if is_continuation && indent > 0 {
        spans.push(Span::raw(" ".repeat(indent)));
    }

    if chars.is_empty() {
        return Line::from(spans);
    }

    let mut current_text = String::new();
    let mut current_style = chars[0].2;

    for &(ch, _, style) in chars {
        if style != current_style && !current_text.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut current_text),
                current_style,
            ));
            current_style = style;
        }
        current_text.push(ch);
    }

    if !current_text.is_empty() {
        spans.push(Span::styled(current_text, current_style));
    }

    Line::from(spans)
}

/// Install a panic hook that restores the terminal before printing the panic.
pub fn install_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            DisableBracketedPaste
        );
        original_hook(panic_info);
    }));
}

#[cfg(test)]
mod wrap_tests {
    use super::*;
    use ratatui::style::Style;
    use ratatui::text::{Line, Span};

    fn plain_line(text: &str) -> Line<'static> {
        Line::from(text.to_string())
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn short_line_not_wrapped() {
        let line = plain_line("hello world");
        let result = wrap_line(line, 80, 4);
        assert_eq!(result.len(), 1);
        assert_eq!(line_text(&result[0]), "hello world");
    }

    #[test]
    fn wraps_at_word_boundary() {
        let line = plain_line("hello world foo");
        let result = wrap_line(line, 12, 0);
        assert_eq!(result.len(), 2);
        assert_eq!(line_text(&result[0]), "hello world ");
        assert_eq!(line_text(&result[1]), "foo");
    }

    #[test]
    fn continuation_indented() {
        let line = plain_line("hello world foo bar");
        let result = wrap_line(line, 12, 4);
        assert!(result.len() >= 2);
        // Continuation lines start with 4 spaces
        for wrapped in &result[1..] {
            let text = line_text(wrapped);
            assert!(text.starts_with("    "), "expected indent, got: '{text}'");
        }
    }

    #[test]
    fn preserves_styles_across_wrap() {
        let line = Line::from(vec![
            Span::styled("aaa ", Style::default().fg(ratatui::style::Color::Red)),
            Span::styled("bbb ", Style::default().fg(ratatui::style::Color::Blue)),
            Span::styled("ccc", Style::default().fg(ratatui::style::Color::Green)),
        ]);
        let result = wrap_line(line, 5, 0);
        assert!(result.len() >= 2);
        // First line should have red-styled chars
        assert_eq!(
            result[0].spans[0].style.fg,
            Some(ratatui::style::Color::Red)
        );
    }

    #[test]
    fn empty_line_returns_unchanged() {
        let line = plain_line("");
        let result = wrap_line(line, 80, 4);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn zero_width_returns_unchanged() {
        let line = plain_line("hello");
        let result = wrap_line(line, 0, 0);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn wide_emoji_counted_as_double_width() {
        // 😀 (U+1F600) is 2 display columns wide, so "a😀b" = 1+2+1 = 4 columns
        // With width=3, it must wrap.
        let line = plain_line("a😀b");
        let result = wrap_line(line, 3, 0);
        assert!(
            result.len() >= 2,
            "wide emoji should cause wrap at width=3, got {} lines",
            result.len()
        );
    }

    #[test]
    fn truncate_with_plus_respects_wide_char_width() {
        // "hi😀" = 1+1+2 = 4 columns. Truncate to 3 should give "hi+"
        let result = super::truncate_with_plus("hi😀", 3);
        assert_eq!(result, "hi+");
    }

    #[test]
    fn short_line_with_wide_emoji_fits() {
        // 😀 = 2 columns, fits in width=2
        let line = plain_line("😀");
        let result = wrap_line(line, 2, 0);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn cjk_characters_are_double_width() {
        // "中文" = 4 columns (2 each). Should wrap at width=3.
        let line = plain_line("中文test");
        let result = wrap_line(line, 5, 0);
        assert!(
            result.len() >= 2,
            "CJK should cause wrap at width=5, got {} lines",
            result.len()
        );
    }
}

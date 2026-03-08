pub mod buffer_list;
pub mod chat_view;
pub mod image_overlay;
pub mod input;
pub mod layout;
pub mod message_line;
pub mod nick_list;
pub mod status_line;
pub mod styled_text;
pub mod topic_bar;

use std::io;
use color_eyre::eyre::Result;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;

pub type Tui = Terminal<CrosstermBackend<io::Stdout>>;

/// Set up the terminal for TUI mode.
pub fn setup_terminal() -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
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

/// Truncate a string to fit `max_len` visible chars, appending `+` if truncated.
pub fn truncate_with_plus(s: &str, max_len: usize) -> String {
    if max_len == 0 {
        return String::new();
    }
    if s.chars().count() <= max_len {
        return s.to_string();
    }
    if max_len <= 1 {
        return "+".to_string();
    }
    // Use char_indices for correct UTF-8 truncation
    let end = s
        .char_indices()
        .nth(max_len - 1)
        .map_or(s.len(), |(i, _)| i);
    format!("{}+", &s[..end])
}

/// Count visible text length from parsed format spans (ignoring color codes).
pub fn visible_len(spans: &[crate::theme::StyledSpan]) -> usize {
    spans.iter().map(|s| s.text.chars().count()).sum()
}

/// Word-wrap a ratatui `Line` to fit within `width` columns.
///
/// Continuation lines are indented with `indent` spaces.  Breaks prefer
/// word boundaries (spaces); falls back to char boundaries when a single
/// word exceeds the available width.
pub fn wrap_line(line: Line<'static>, width: usize, indent: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![line];
    }

    // Fast path: fits on one line.
    let total_width: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    if total_width <= width {
        return vec![line];
    }

    // Flatten to (char, Style) stream.
    let styled_chars: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|span| span.content.chars().map(move |ch| (ch, span.style)))
        .collect();

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

        let end = (pos + line_width).min(styled_chars.len());

        // Last chunk — take everything remaining.
        if end >= styled_chars.len() {
            let built = build_line_from_styled_chars(&styled_chars[pos..], !first_line, indent);
            result.push(built);
            break;
        }

        // Try to find a word break point (last space within the chunk).
        let chunk = &styled_chars[pos..end];
        let break_at = chunk.iter().rposition(|(ch, _)| *ch == ' ');

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

/// Build a `Line` from a slice of `(char, Style)` pairs, grouping consecutive
/// chars with the same style into spans.  Prepends `indent` spaces when
/// `is_continuation` is true.
fn build_line_from_styled_chars(
    chars: &[(char, Style)],
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
    let mut current_style = chars[0].1;

    for &(ch, style) in chars {
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
    use ratatui::text::{Line, Span};
    use ratatui::style::Style;

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
}

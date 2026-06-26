pub mod buffer_list;
pub mod chat_view;
pub mod emote_layout;
pub mod emote_picker;
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
pub mod wizard;

use std::io::{self, Write};

use color_eyre::eyre::Result;
use crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::prelude::*;
use unicode_segmentation::UnicodeSegmentation;
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

/// Center a `w`×`h` rect inside `area`, clamped to fit. Shared by overlay
/// widgets (emote picker, wizard) so popup placement stays consistent.
#[must_use]
pub fn centered_rect(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
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

    // Flatten to (grapheme, display_width, Style) and measure total width in one
    // pass. Tokenizing by grapheme cluster (not `char`) and measuring each with
    // `UnicodeWidthStr` mirrors how ratatui lays the line out, so multi-codepoint
    // emoji (VS16, flags, skin-tone, ZWJ) get their real on-screen width and are
    // never split across a wrap boundary.
    let styled_chars: Vec<(String, usize, Style)> = line
        .spans
        .iter()
        .flat_map(|span| {
            span.content
                .graphemes(true)
                .map(move |g| (g.to_owned(), UnicodeWidthStr::width(g), span.style))
        })
        .collect();

    // Fast path: a single logical line (no embedded `\n`) that already fits is
    // returned unchanged — preserves the original span structure and keeps the
    // common single-line message path allocation-free.
    let has_newline = styled_chars.iter().any(|(g, _, _)| g == "\n");
    let total_width: usize = styled_chars.iter().map(|(_, w, _)| w).sum();
    if !has_newline && total_width <= width {
        return vec![line];
    }

    // Split on hard `\n` into segments (dropping the newline grapheme) and wrap
    // each segment independently. A hard break starts a NEW logical line, so each
    // segment begins un-indented (its own `first_line`); an empty segment (from
    // consecutive or trailing `\n`) becomes one blank visual line.
    let mut result: Vec<Line<'static>> = Vec::new();
    let mut seg_start = 0;
    let mut i = 0;
    let mut first_segment = true;
    while i <= styled_chars.len() {
        let at_newline = i < styled_chars.len() && styled_chars[i].0 == "\n";
        if at_newline || i == styled_chars.len() {
            let segment = &styled_chars[seg_start..i];
            if segment.is_empty() {
                result.push(Line::default());
            } else {
                // Segments after a hard `\n` are continuations of the same chat
                // message, so their first visual line is indented to the body
                // column. The first segment keeps its real ts+nick prefix at
                // column 0.
                result.extend(wrap_segment(segment, width, indent, !first_segment));
            }
            seg_start = i + 1; // skip the `\n`
            first_segment = false;
        }
        i += 1;
    }

    if result.is_empty() {
        result.push(line);
    }

    result
}

/// Word-wrap a single newline-free segment (a slice of styled graphemes) to
/// `width` columns. Continuation lines are indented by `indent`. This is the
/// per-segment body used by [`wrap_line`] after splitting on hard `\n`.
fn wrap_segment(
    styled_chars: &[(String, usize, Style)],
    width: usize,
    indent: usize,
    start_indented: bool,
) -> Vec<Line<'static>> {
    let mut result: Vec<Line<'static>> = Vec::new();
    let mut pos = 0;
    // `start_indented` indents the segment's FIRST visual line too (used for
    // continuation segments after a hard `\n`).
    let mut first_line = !start_indented;

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

        // Guarantee forward progress. An indivisible grapheme wider than the
        // available width (e.g. a 2-cell emoji or CJK char at width 1) fits
        // nowhere, leaving `end == pos`. Emit it anyway (overflowing the line) so
        // `pos` always advances — otherwise the loop spins forever, growing
        // `result` until the process is OOM-killed.
        if end == pos {
            end = pos + 1;
        }

        // Last chunk — take everything remaining.
        if end >= styled_chars.len() {
            let built = build_line_from_styled_chars(&styled_chars[pos..], !first_line, indent);
            result.push(built);
            break;
        }

        // Try to find a word break point (last space within the chunk).
        let chunk = &styled_chars[pos..end];
        let break_at = chunk.iter().rposition(|(g, _, _)| g == " ");

        let actual_end = break_at.map_or(end, |break_pos| pos + break_pos + 1);
        // Never split a multi-cell emote placeholder run across the boundary.
        let actual_end = avoid_splitting_placeholder(styled_chars, pos, actual_end);

        let built =
            build_line_from_styled_chars(&styled_chars[pos..actual_end], !first_line, indent);
        result.push(built);

        pos = actual_end;
        first_line = false;

        // Skip leading spaces on continuation lines.
        while pos < styled_chars.len() && styled_chars[pos].0 == " " {
            pos += 1;
        }
    }

    if result.is_empty() {
        // Defensive: a non-empty segment always yields ≥1 line above; this only
        // guards a future regression. Rebuild from the slice rather than
        // referencing a `line` binding that does not exist here.
        result.push(build_line_from_styled_chars(styled_chars, false, indent));
    }

    result
}

/// If the wrap boundary `end` falls inside a run of identical emote-placeholder
/// chars, move it back to the run's start so the whole emote wraps as a unit.
/// Returns the original `end` when the run starts at/before `pos` (the emote is
/// wider than the line and cannot be kept whole — avoids a zero-progress loop).
fn avoid_splitting_placeholder(chars: &[(String, usize, Style)], pos: usize, end: usize) -> usize {
    use crate::ui::emote_layout::decode_placeholder_grapheme;
    if end == 0 || end >= chars.len() {
        return end;
    }
    let Some(idx) = decode_placeholder_grapheme(&chars[end].0) else {
        return end;
    };
    // Only a split if the cell just before the boundary is the same placeholder.
    if decode_placeholder_grapheme(&chars[end - 1].0) != Some(idx) {
        return end;
    }
    let mut e = end;
    while e > pos && decode_placeholder_grapheme(&chars[e - 1].0) == Some(idx) {
        e -= 1;
    }
    if e <= pos { end } else { e }
}

/// Build a `Line` from a slice of `(char, display_width, Style)` tuples, grouping
/// consecutive chars with the same style into spans.  Prepends `indent` spaces when
/// `is_continuation` is true.
fn build_line_from_styled_chars(
    chars: &[(String, usize, Style)],
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

    for (g, _, style) in chars {
        if *style != current_style && !current_text.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut current_text),
                current_style,
            ));
            current_style = *style;
        }
        current_text.push_str(g);
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
    fn emote_placeholder_not_split_across_wrap() {
        use crate::ui::emote_layout::{
            EMOTE_COLS, decode_placeholder_index, placeholder_for_index,
        };
        // "aaa" + 2-cell emote placeholder, wrapped at width 4 so the emote would
        // otherwise straddle the boundary (3 text cells + 1 of 2 emote cells).
        let ph = placeholder_for_index(7);
        let line = Line::from(format!("aaa{ph}"));
        let result = wrap_line(line, 4, 0);
        // The placeholder must land entirely on one visual line as a 2-char run,
        // never as a lone 1-char fragment at a line edge.
        for vline in &result {
            let run: usize = vline
                .spans
                .iter()
                .flat_map(|s| s.content.chars())
                .filter(|c| decode_placeholder_index(*c) == Some(7))
                .count();
            assert!(
                run == 0 || run == EMOTE_COLS,
                "placeholder split: run={run} in {result:?}"
            );
        }
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

    // `Line::from(String)` strips newlines (ratatui's `Line::raw`); the real
    // render path keeps `\n` because it builds the line from `Span`s
    // (`styled_spans_to_line` → `Span::styled` → `Line::from(Vec<Span>)`).
    // Mirror that here so `\n` survives into the line under test.
    fn nl_line(text: &str) -> Line<'static> {
        Line::from(vec![Span::raw(text.to_string())])
    }

    #[test]
    fn hard_newline_splits_into_segments() {
        let result = wrap_line(nl_line("alpha\nbeta"), 80, 0);
        assert_eq!(result.len(), 2);
        assert_eq!(line_text(&result[0]), "alpha");
        assert_eq!(line_text(&result[1]), "beta");
    }

    #[test]
    fn blank_segment_becomes_blank_line() {
        let result = wrap_line(nl_line("a\n\nb"), 80, 0);
        assert_eq!(result.len(), 3);
        assert_eq!(line_text(&result[0]), "a");
        assert_eq!(line_text(&result[1]), "");
        assert_eq!(line_text(&result[2]), "b");
    }

    #[test]
    fn trailing_newline_adds_blank_line() {
        let result = wrap_line(nl_line("text\n"), 80, 0);
        assert_eq!(result.len(), 2);
        assert_eq!(line_text(&result[0]), "text");
        assert_eq!(line_text(&result[1]), "");
    }

    #[test]
    fn newline_then_soft_wrap_within_segment() {
        // First segment short; second segment long enough to soft-wrap at 12.
        let result = wrap_line(nl_line("short\nhello world foo bar baz"), 12, 0);
        assert!(result.len() >= 3, "got {result:?}");
        assert_eq!(line_text(&result[0]), "short");
        let rest: String = result[1..].iter().map(line_text).collect();
        assert!(rest.replace(' ', "").contains("helloworldfoobarbaz"));
    }

    #[test]
    fn post_newline_segment_indents_to_body_column() {
        // indent=4 gutter: first logical line at col 0 (has its real prefix),
        // the continuation segment after `\n` is indented to align under it.
        let result = wrap_line(nl_line("first\nsecond"), 80, 4);
        assert_eq!(result.len(), 2);
        assert_eq!(line_text(&result[0]), "first");
        assert_eq!(line_text(&result[1]), "    second");
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

    #[test]
    fn multi_codepoint_emoji_measured_and_kept_whole() {
        // 👨‍👩‍👧 (man-ZWJ-woman-ZWJ-girl) renders as ONE grapheme cluster of
        // display width 2 — the same measurement ratatui uses when it lays the
        // line out. Summing per-`char` widths would count it as 6 columns and
        // could split the ZWJ sequence across wrapped lines.
        let emoji = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let line = Line::from(format!("{emoji}x")); // grapheme width 2 + 1 = 3
        let result = wrap_line(line, 3, 0);
        assert_eq!(
            result.len(),
            1,
            "width-3 budget fits emoji(2)+x(1) on one line, got {}",
            result.len()
        );
        assert_eq!(
            line_text(&result[0]),
            format!("{emoji}x"),
            "emoji grapheme must stay intact, never split"
        );
    }

    #[test]
    fn oversized_grapheme_at_width_one_terminates() {
        // ❤️ is an indivisible 2-cell grapheme; at width 1 it can never "fit", but
        // the wrapper must still make forward progress (emit it, overflowing the
        // line) instead of looping forever. Also covers the pre-existing case of a
        // lone 2-cell char (😀, CJK) at width 1.
        let line = plain_line("a\u{2764}\u{FE0F}b");
        let result = wrap_line(line, 1, 0);
        let joined: String = result.iter().map(line_text).collect();
        assert_eq!(joined, "a\u{2764}\u{FE0F}b", "all content preserved, no hang");
    }

    #[test]
    fn variation_selector_emoji_counts_as_two_columns() {
        // ❤️ is U+2764 + U+FE0F (VS16); as a grapheme it is width 2, the same as
        // ratatui measures it. Summing per-`char` widths yields only 1 (VS16 is
        // width 0), so "a❤️" (3 grapheme columns) would be wrongly kept on one
        // line at width 2. It must wrap to match what ratatui actually draws.
        let line = plain_line("a\u{2764}\u{FE0F}");
        let result = wrap_line(line, 2, 0);
        assert!(
            result.len() >= 2,
            "a + 2-column heart exceeds width 2 and must wrap, got {}",
            result.len()
        );
    }
}

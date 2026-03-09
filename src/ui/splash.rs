use ratatui::prelude::*;
use ratatui::widgets::Block;

use crate::theme::hex_to_color;

/// Bundled ASCII art logo (braille bird + repartee text).
const LOGO: &str = include_str!("../../logo.txt");

/// Colors matching kokoirc's splash theme.
const BIRD_COLOR: &str = "#e0af68";
const TEXT_COLOR: &str = "#7aa2f7";
const DIM_COLOR: &str = "#565f89";
const BG_COLOR: &str = "#1a1b26";

/// Render the splash screen centered on the terminal.
///
/// Lines up to `visible_lines` are revealed; the rest are invisible
/// (drawn in the background color to create a progressive reveal effect).
pub fn render(frame: &mut Frame, visible_lines: usize) {
    let area = frame.area();
    let bg = hex_to_color(BG_COLOR).unwrap_or(Color::Black);

    // Clear entire screen with background.
    frame.render_widget(Block::default().style(Style::default().bg(bg)), area);

    let lines: Vec<&str> = LOGO.lines().collect();
    let total_lines = lines.len();

    // Use the widest line for consistent horizontal centering across all lines.
    #[expect(clippy::cast_possible_truncation, reason = "logo chars ≪ u16::MAX")]
    let max_width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;

    // Vertical centering.
    #[expect(clippy::cast_possible_truncation, reason = "logo lines ≪ u16::MAX")]
    let start_y = area.y + area.height.saturating_sub(total_lines as u16) / 2;
    let x = area.x + area.width.saturating_sub(max_width) / 2;

    let bird_fg = hex_to_color(BIRD_COLOR).unwrap_or(Color::Yellow);
    let text_fg = hex_to_color(TEXT_COLOR).unwrap_or(Color::Blue);
    let dim_fg = hex_to_color(DIM_COLOR).unwrap_or(Color::DarkGray);

    for (i, line_str) in lines.iter().enumerate() {
        #[expect(clippy::cast_possible_truncation, reason = "line index ≪ u16::MAX")]
        let y = start_y + i as u16;
        if y >= area.y + area.height {
            break;
        }

        if i >= visible_lines {
            // Not yet revealed — skip (bg already cleared).
            continue;
        }

        // Color by line: bird braille, repartee text, URL/GitHub info.
        let spans = colorize_line(line_str, i, bird_fg, text_fg, dim_fg, bg);

        let line = Line::from(spans);
        let render_area = Rect {
            x,
            y,
            width: area.width.saturating_sub(x - area.x),
            height: 1,
        };

        frame.render_widget(line, render_area);
    }
}

/// Apply per-character coloring to a logo line.
fn colorize_line<'a>(
    line: &str,
    line_idx: usize,
    bird_fg: Color,
    text_fg: Color,
    dim_fg: Color,
    bg: Color,
) -> Vec<Span<'a>> {
    // Lines 14-15 (0-indexed): URL and GitHub info lines.
    if line_idx == 14 || line_idx == 15 {
        return colorize_mixed_line(line, bird_fg, dim_fg, bg);
    }

    // Lines 8-12 (0-indexed): bird + repartee block text.
    // The repartee text uses block characters (⣶⣿ etc.) that are visually
    // distinct from the bird's braille art. We color them differently.
    if (8..=13).contains(&line_idx) {
        return colorize_mixed_line(line, bird_fg, text_fg, bg);
    }

    // Pure bird lines: everything in bird color.
    let mut spans = Vec::new();
    let mut text = String::new();

    for ch in line.chars() {
        if ch == '\u{2800}' {
            // Braille blank (invisible spacer) — render in bg color.
            if !text.is_empty() {
                spans.push(Span::styled(
                    std::mem::take(&mut text),
                    Style::default().fg(bird_fg).bg(bg),
                ));
            }
            text.push(ch);
            spans.push(Span::styled(
                std::mem::take(&mut text),
                Style::default().fg(bg).bg(bg),
            ));
        } else {
            text.push(ch);
        }
    }

    if !text.is_empty() {
        spans.push(Span::styled(text, Style::default().fg(bird_fg).bg(bg)));
    }

    spans
}

/// Colorize a line that has bird braille on the left and text content on the right.
///
/// The transition point is detected by finding runs of non-braille characters
/// (the repartee text / URL). Braille chars (U+2800..U+28FF) get `bird_fg`,
/// and the rest gets `text_fg`.
fn colorize_mixed_line<'a>(
    line: &str,
    bird_fg: Color,
    text_fg: Color,
    bg: Color,
) -> Vec<Span<'a>> {
    let mut spans = Vec::new();
    let mut current_text = String::new();
    let mut current_is_braille = None;

    for ch in line.chars() {
        let is_braille = ('\u{2800}'..='\u{28FF}').contains(&ch);

        if let Some(prev_braille) = current_is_braille
            && is_braille != prev_braille
            && !current_text.is_empty()
        {
            let fg = if prev_braille {
                if current_text.chars().all(|c| c == '\u{2800}') {
                    bg // invisible spacers
                } else {
                    bird_fg
                }
            } else {
                text_fg
            };
            spans.push(Span::styled(
                std::mem::take(&mut current_text),
                Style::default().fg(fg).bg(bg),
            ));
        }

        current_is_braille = Some(is_braille);
        current_text.push(ch);
    }

    if !current_text.is_empty() {
        let fg = if current_is_braille == Some(true) {
            if current_text.chars().all(|c| c == '\u{2800}') {
                bg
            } else {
                bird_fg
            }
        } else {
            text_fg
        };
        spans.push(Span::styled(
            current_text,
            Style::default().fg(fg).bg(bg),
        ));
    }

    spans
}

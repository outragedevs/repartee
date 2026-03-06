use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;
use crate::state::sorting;
use crate::theme::{hex_to_color, parse_format_string, resolve_abstractions};
use crate::ui::styled_text::styled_spans_to_line;
use super::{truncate_with_plus, visible_len};

/// Render the nick list sidebar. Returns total line count for scroll clamping.
pub fn render(frame: &mut Frame, area: Rect, app: &App, scroll_offset: usize) -> usize {
    let colors = &app.theme.colors;
    let bg = hex_to_color(&colors.bg).unwrap_or(Color::Black);
    let border_color = hex_to_color(&colors.border).unwrap_or(Color::DarkGray);
    let fg_muted = hex_to_color(&colors.fg_muted).unwrap_or(Color::DarkGray);

    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let panel_width = app.config.sidepanel.right.width as usize;

    let Some(buf) = app.state.active_buffer() else {
        return 0;
    };

    let nick_refs: Vec<_> = buf.users.values().collect();
    let sorted = sorting::sort_nicks(&nick_refs, sorting::DEFAULT_PREFIX_ORDER);
    let abstracts = &app.theme.abstracts;
    let nicklist = &app.theme.formats.nicklist;

    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        format!("{} users", sorted.len()),
        Style::default().fg(fg_muted),
    ))];

    for entry in &sorted {
        // Use the highest-rank prefix (first char) for theme format selection.
        // Multi-prefix entries like "@+" should use the op color, not "normal".
        // Away users get dimmed variants (away_op, away_voice, etc.).
        let rank = match entry.prefix.chars().next() {
            Some('~') => "owner",
            Some('&') => "admin",
            Some('@') => "op",
            Some('%') => "halfop",
            Some('+') => "voice",
            _ => "normal",
        };
        let format_key = if entry.away {
            let away_key = format!("away_{rank}");
            if nicklist.contains_key(&away_key) {
                away_key
            } else {
                rank.to_string()
            }
        } else {
            rank.to_string()
        };
        let format = nicklist
            .get(format_key.as_str())
            .cloned()
            .unwrap_or_else(|| " $0".to_string());
        let resolved = resolve_abstractions(&format, abstracts, 0);

        // Measure format overhead (visible chars besides $0 nick)
        let overhead = visible_len(&parse_format_string(&resolved, &[""]));
        let max_nick_len = panel_width.saturating_sub(1 + overhead);
        let display_nick = truncate_with_plus(&entry.nick, max_nick_len);

        let spans = parse_format_string(&resolved, &[&display_nick]);
        lines.push(styled_spans_to_line(&spans));
    }

    let total_lines = lines.len();

    // Apply scroll offset, clamped so last item sits at bottom
    let visible_height = inner.height as usize;
    let max_scroll = total_lines.saturating_sub(visible_height);
    let clamped_offset = scroll_offset.min(max_scroll);
    let visible_lines: Vec<Line> = lines
        .into_iter()
        .skip(clamped_offset)
        .take(visible_height)
        .collect();

    let paragraph = Paragraph::new(visible_lines);
    frame.render_widget(paragraph, inner);

    total_lines
}

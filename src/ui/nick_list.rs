use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;
use crate::state::sorting;
use crate::theme::hex_to_color;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
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

    if let Some(buf) = app.state.active_buffer() {
        let nick_refs: Vec<_> = buf.users.values().collect();
        let sorted = sorting::sort_nicks(&nick_refs, sorting::DEFAULT_PREFIX_ORDER);

        let mut lines: Vec<Line> = vec![Line::from(Span::styled(
            format!("{} users", sorted.len()),
            Style::default().fg(fg_muted),
        ))];

        for entry in &sorted {
            let prefix_style = Style::default()
                .fg(hex_to_color(&colors.accent).unwrap_or(Color::Cyan));
            let nick_style = Style::default()
                .fg(hex_to_color(&colors.fg).unwrap_or(Color::White));
            lines.push(Line::from(vec![
                Span::styled(entry.prefix.clone(), prefix_style),
                Span::styled(entry.nick.clone(), nick_style),
            ]));
        }

        let paragraph = Paragraph::new(lines);
        frame.render_widget(paragraph, inner);
    }
}

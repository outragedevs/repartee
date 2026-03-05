use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;
use crate::state::buffer::ActivityLevel;
use crate::theme::hex_to_color;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let colors = &app.theme.colors;
    let bg = hex_to_color(&colors.bg).unwrap_or(Color::Black);
    let border_color = hex_to_color(&colors.border).unwrap_or(Color::DarkGray);

    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let sorted_ids = app.state.sorted_buffer_ids();
    let active_id = app.state.active_buffer_id.as_deref();

    let items: Vec<Line> = sorted_ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let buf = app.state.buffers.get(id.as_str()).unwrap();
            let num = format!("{}.", i + 1);
            let is_active = active_id == Some(id.as_str());

            let (num_style, name_style) = if is_active {
                (
                    Style::default()
                        .fg(hex_to_color(&colors.accent).unwrap_or(Color::Cyan)),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                let activity_color = match buf.activity {
                    ActivityLevel::None => {
                        hex_to_color(&colors.fg_dim).unwrap_or(Color::DarkGray)
                    }
                    ActivityLevel::Events => {
                        hex_to_color(&colors.fg_muted).unwrap_or(Color::Gray)
                    }
                    _ => hex_to_color(&colors.accent).unwrap_or(Color::Cyan),
                };
                (
                    Style::default()
                        .fg(hex_to_color(&colors.fg_muted).unwrap_or(Color::DarkGray)),
                    Style::default().fg(activity_color),
                )
            };

            Line::from(vec![
                Span::styled(num, num_style),
                Span::raw(" "),
                Span::styled(buf.name.clone(), name_style),
            ])
        })
        .collect();

    let paragraph = Paragraph::new(items);
    frame.render_widget(paragraph, inner);
}

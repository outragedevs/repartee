use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Wrap};

use crate::app::App;
use crate::theme::hex_to_color;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let colors = &app.theme.colors;
    let bg = hex_to_color(&colors.bg).unwrap_or(Color::Black);
    let fg = hex_to_color(&colors.fg).unwrap_or(Color::White);
    let fg_muted = hex_to_color(&colors.fg_muted).unwrap_or(Color::DarkGray);

    if let Some(buf) = app.state.active_buffer() {
        let lines: Vec<Line> = buf
            .messages
            .iter()
            .map(|msg| {
                let timestamp = msg
                    .timestamp
                    .format(&app.config.general.timestamp_format)
                    .to_string();
                let nick = msg.nick.as_deref().unwrap_or("***");

                // Simple rendering: [time] <nick> text
                Line::from(vec![
                    Span::styled(
                        format!("{} ", timestamp),
                        Style::default().fg(fg_muted),
                    ),
                    Span::styled(
                        format!("{}: ", nick),
                        Style::default()
                            .fg(hex_to_color(&colors.accent).unwrap_or(Color::Cyan)),
                    ),
                    Span::styled(msg.text.clone(), Style::default().fg(fg)),
                ])
            })
            .collect();

        let paragraph = Paragraph::new(lines)
            .style(Style::default().bg(bg))
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
    } else {
        let paragraph = Paragraph::new("No active buffer")
            .style(Style::default().fg(fg_muted).bg(bg))
            .alignment(Alignment::Center);
        frame.render_widget(paragraph, area);
    }
}

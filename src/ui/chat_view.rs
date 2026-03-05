use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme::hex_to_color;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let colors = &app.theme.colors;
    let bg = hex_to_color(&colors.bg).unwrap_or(Color::Black);
    let fg_muted = hex_to_color(&colors.fg_muted).unwrap_or(Color::DarkGray);

    if let Some(buf) = app.state.active_buffer() {
        let current_nick = app
            .state
            .connections
            .get(&buf.connection_id)
            .map(|c| c.nick.as_str())
            .unwrap_or("");

        let lines: Vec<Line> = buf
            .messages
            .iter()
            .map(|msg| {
                let is_own = msg.nick.as_deref() == Some(current_nick);
                super::message_line::render_message(msg, is_own, &app.theme, &app.config)
            })
            .collect();

        // Show messages from bottom of visible area
        let visible_height = area.height as usize;
        let skip = if lines.len() > visible_height {
            lines.len() - visible_height
        } else {
            0
        };
        let visible_lines: Vec<Line> = lines.into_iter().skip(skip).collect();

        let paragraph = Paragraph::new(visible_lines).style(Style::default().bg(bg));
        frame.render_widget(paragraph, area);
    } else {
        let paragraph = Paragraph::new("No active buffer")
            .style(Style::default().fg(fg_muted).bg(bg))
            .alignment(Alignment::Center);
        frame.render_widget(paragraph, area);
    }
}

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
            .map_or("", |c| c.nick.as_str());

        let total = buf.messages.len();
        let visible_height = area.height as usize;
        let max_scroll = total.saturating_sub(visible_height);
        let scroll = app.scroll_offset.min(max_scroll);
        let skip = total.saturating_sub(visible_height + scroll);

        let visible_lines: Vec<Line> = buf
            .messages
            .iter()
            .skip(skip)
            .take(visible_height)
            .map(|msg| {
                let is_own = msg.nick.as_deref() == Some(current_nick);
                super::message_line::render_message(msg, is_own, &app.theme, &app.config)
            })
            .collect();

        let paragraph = Paragraph::new(visible_lines).style(Style::default().bg(bg));
        frame.render_widget(paragraph, area);
    } else {
        let paragraph = Paragraph::new("No active buffer")
            .style(Style::default().fg(fg_muted).bg(bg))
            .alignment(Alignment::Center);
        frame.render_widget(paragraph, area);
    }
}

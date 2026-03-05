use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme::hex_to_color;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let colors = &app.theme.colors;
    let bg_alt = hex_to_color(&colors.bg_alt).unwrap_or(Color::Black);
    let fg = hex_to_color(&colors.fg).unwrap_or(Color::White);
    let accent = hex_to_color(&colors.accent).unwrap_or(Color::Cyan);
    let fg_muted = hex_to_color(&colors.fg_muted).unwrap_or(Color::DarkGray);

    let topic_text = if let Some(buf) = app.state.active_buffer() {
        let channel = Span::styled(
            buf.name.clone(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        );
        let separator = Span::styled(" \u{2014} ", Style::default().fg(fg_muted));
        let topic = Span::styled(
            buf.topic.as_deref().unwrap_or("").to_string(),
            Style::default().fg(fg),
        );
        Line::from(vec![channel, separator, topic])
    } else {
        Line::from("")
    };

    let widget = Paragraph::new(topic_text).style(Style::default().bg(bg_alt));
    frame.render_widget(widget, area);
}

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme::hex_to_color;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let colors = &app.theme.colors;
    let fg_muted = hex_to_color(&colors.fg_muted).unwrap_or(Color::DarkGray);
    let accent = hex_to_color(&colors.accent).unwrap_or(Color::Cyan);

    let time = chrono::Local::now()
        .format(&app.config.general.timestamp_format)
        .to_string();

    let conn_nick = app
        .state
        .connections
        .values()
        .next()
        .map(|c| c.nick.as_str())
        .unwrap_or("");
    let chan_name = app
        .state
        .active_buffer()
        .map(|b| b.name.as_str())
        .unwrap_or("");

    let separator = &app.config.statusbar.separator;

    let line = Line::from(vec![
        Span::styled(time, Style::default().fg(fg_muted)),
        Span::styled(separator.clone(), Style::default().fg(fg_muted)),
        Span::styled(conn_nick.to_string(), Style::default().fg(accent)),
        Span::styled(separator.clone(), Style::default().fg(fg_muted)),
        Span::styled(chan_name.to_string(), Style::default().fg(accent)),
    ]);

    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}

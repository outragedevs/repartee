use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme::hex_to_color;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let colors = &app.theme.colors;
    let fg_muted = hex_to_color(&colors.fg_muted).unwrap_or(Color::DarkGray);
    let fg = hex_to_color(&colors.fg).unwrap_or(Color::White);

    let server_label = app
        .state
        .connections
        .values()
        .next()
        .map(|c| c.label.as_str())
        .unwrap_or("");
    let channel_name = app
        .state
        .active_buffer()
        .map(|b| b.name.as_str())
        .unwrap_or("");
    let nick = app
        .state
        .connections
        .values()
        .next()
        .map(|c| c.nick.as_str())
        .unwrap_or("");

    let prompt = app
        .config
        .statusbar
        .prompt
        .replace("$server", server_label)
        .replace("$channel", channel_name)
        .replace("$nick", nick);

    let line = Line::from(vec![
        Span::styled(prompt, Style::default().fg(fg_muted)),
        // Empty cursor placeholder
        Span::styled("\u{2588}", Style::default().fg(fg)),
    ]);

    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}

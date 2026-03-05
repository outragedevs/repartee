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

        // Parse topic through the format string parser to handle IRC colors
        let topic_spans = if let Some(topic_text) = &buf.topic {
            crate::theme::parse_format_string(topic_text, &[])
        } else {
            vec![]
        };
        let topic_line: Vec<Span> = topic_spans
            .iter()
            .map(|s| {
                let mut style = Style::default();
                if let Some(fg_color) = s.fg {
                    style = style.fg(fg_color);
                } else {
                    style = style.fg(fg);
                }
                if let Some(bg_color) = s.bg {
                    style = style.bg(bg_color);
                }
                if s.bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if s.italic {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if s.underline {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                if s.dim {
                    style = style.add_modifier(Modifier::DIM);
                }
                Span::styled(s.text.clone(), style)
            })
            .collect();

        let mut spans = vec![channel, separator];
        spans.extend(topic_line);
        Line::from(spans)
    } else {
        Line::from("")
    };

    let widget = Paragraph::new(topic_text).style(Style::default().bg(bg_alt));
    frame.render_widget(widget, area);
}

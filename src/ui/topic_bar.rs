use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme::hex_to_color;
use crate::ui::styled_text::styled_spans_to_line_with_fg;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let colors = &app.theme.colors;
    let bg_alt = hex_to_color(&colors.bg_alt).unwrap_or(Color::Black);
    let fg = hex_to_color(&colors.fg).unwrap_or(Color::White);
    let accent = hex_to_color(&colors.accent).unwrap_or(Color::Cyan);
    let fg_muted = hex_to_color(&colors.fg_muted).unwrap_or(Color::DarkGray);

    let topic_text = app.state.active_buffer().map_or_else(
        || Line::from(""),
        |buf| {
            let channel = Span::styled(
                buf.name.clone(),
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            );
            let separator = Span::styled(" \u{2014} ", Style::default().fg(fg_muted));

            // Parse topic through the format string parser to handle IRC colors
            let topic_spans = buf.topic.as_ref().map_or_else(Vec::new, |topic_text| {
                crate::theme::parse_format_string(topic_text, &[])
            });
            let topic_line = styled_spans_to_line_with_fg(&topic_spans, fg);

            let mut result_spans = vec![channel, separator];
            result_spans.extend(topic_line.spans);
            Line::from(result_spans)
        },
    );

    let widget = Paragraph::new(topic_text).style(Style::default().bg(bg_alt));
    frame.render_widget(widget, area);
}

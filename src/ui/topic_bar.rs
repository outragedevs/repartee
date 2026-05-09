use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::state::buffer::{Buffer, BufferType};
use crate::theme::hex_to_color;
use crate::ui::styled_text::styled_spans_to_line_with_fg;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let colors = &app.theme.colors;
    let bg_alt = hex_to_color(&colors.bg_alt).unwrap_or(Color::Reset);
    let fg = hex_to_color(&colors.fg).unwrap_or(Color::Reset);
    let accent = hex_to_color(&colors.accent).unwrap_or(Color::Cyan);
    let fg_muted = hex_to_color(&colors.fg_muted).unwrap_or(Color::DarkGray);

    let topic_text = app.state.active_buffer().map_or_else(
        || Line::from(""),
        |buf| {
            if buf.buffer_type == BufferType::Log {
                return render_log_topic(buf, accent, fg, fg_muted);
            }
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

/// Topic-bar content for `BufferType::Log` buffers — shows the network/
/// channel pair, the cached line count and the date range. Computed
/// from the metadata cached on `Buffer` at activation, never queries
/// the database.
fn render_log_topic(buf: &Buffer, accent: Color, fg: Color, fg_muted: Color) -> Line<'static> {
    let net = buf
        .connection_id
        .strip_prefix(App::LOG_CONN_PREFIX)
        .unwrap_or(&buf.connection_id);
    let total = buf.log_total_lines.unwrap_or(0);
    let range = match (buf.log_oldest_ts, buf.log_newest_ts) {
        (Some(o), Some(n)) => format!("{} → {}", format_log_date(o), format_log_date(n)),
        _ => String::from("(empty)"),
    };
    let header = Span::styled(
        format!("\u{1F4DC} Log: {} @ {}", buf.name, net),
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    );
    let sep = Span::styled("  \u{2022}  ", Style::default().fg(fg_muted));
    let lines = Span::styled(
        format!("{total} lines"),
        Style::default().fg(fg),
    );
    let range_span = Span::styled(range, Style::default().fg(fg));
    Line::from(vec![header, sep.clone(), lines, sep, range_span])
}

fn format_log_date(ts: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

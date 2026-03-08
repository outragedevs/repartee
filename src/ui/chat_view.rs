use std::collections::VecDeque;

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

        let total_width = area.width as usize;
        let visible_height = area.height as usize;

        if total_width == 0 || visible_height == 0 {
            return;
        }

        // Calculate indent for wrapped continuation lines:
        // timestamp visual width + separator + nick column + space before text.
        let indent = calculate_wrap_indent(app);

        // Process messages from the end of the buffer, wrapping each into
        // visual lines.  Stop once we have enough to fill the screen plus
        // the current scroll offset.
        let needed = visible_height + app.scroll_offset + visible_height;
        let mut visual_lines: VecDeque<Line<'_>> = VecDeque::new();

        for msg in buf.messages.iter().rev() {
            let is_own = msg.nick.as_deref() == Some(current_nick);
            let line =
                super::message_line::render_message(msg, is_own, &app.theme, &app.config);
            let wrapped = super::wrap_line(line, total_width, indent);

            // Push in reverse so the final deque is in chronological order.
            for wl in wrapped.into_iter().rev() {
                visual_lines.push_front(wl);
            }

            if visual_lines.len() > needed {
                break;
            }
        }

        let total = visual_lines.len();
        let max_scroll = total.saturating_sub(visible_height);
        let scroll = app.scroll_offset.min(max_scroll);
        let skip = total.saturating_sub(visible_height + scroll);

        let visible_lines: Vec<Line<'_>> = visual_lines
            .into_iter()
            .skip(skip)
            .take(visible_height)
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

/// Calculate the wrap-indent width (in columns) for continuation lines.
///
/// This equals the visual width of everything before the message body:
/// `timestamp_visual_width + 1 (separator) + nick_column_width + 1 (space)`.
fn calculate_wrap_indent(app: &App) -> usize {
    // Sample timestamp to get its visual width after theme formatting.
    let ts_sample = chrono::Local::now()
        .format(&app.config.general.timestamp_format)
        .to_string();
    let ts_format = app
        .theme
        .abstracts
        .get("timestamp")
        .cloned()
        .unwrap_or_else(|| "$*".to_string());
    let ts_resolved =
        crate::theme::resolve_abstractions(&ts_format, &app.theme.abstracts, 0);
    let ts_spans = crate::theme::parse_format_string(&ts_resolved, &[&ts_sample]);
    let ts_visual_width: usize = ts_spans.iter().map(|s| s.text.chars().count()).sum();

    // timestamp + " " separator + nick column + " " before text
    ts_visual_width + 1 + app.config.display.nick_column_width as usize + 1
}

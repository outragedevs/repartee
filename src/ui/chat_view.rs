use std::collections::VecDeque;

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme::hex_to_color;

// Wrap-indent is cached on `App::wrap_indent` and recomputed only when
// config or theme changes (see `App::recompute_wrap_indent`).

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    // Delegate to shell renderer for shell buffers.
    if app
        .state
        .active_buffer()
        .is_some_and(|b| b.buffer_type == crate::state::buffer::BufferType::Shell)
    {
        super::shell_view::render(frame, area, app);
        return;
    }

    let colors = &app.theme.colors;
    let bg = hex_to_color(&colors.bg).unwrap_or(Color::Reset);
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

        // Wrap-indent is pre-computed and cached on App.
        let indent = app.wrap_indent;

        // Process messages from the end of the buffer, wrapping each into
        // visual lines.  Stop once we have enough to fill the screen plus
        // the current scroll offset.
        let needed = visible_height + app.scroll_offset;
        let mut visual_lines: VecDeque<Line<'_>> = VecDeque::new();

        for msg in buf.messages.iter().rev() {
            let is_own = msg.nick.as_deref() == Some(current_nick);
            let line = super::message_line::render_message(msg, is_own, &app.theme, &app.config);
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

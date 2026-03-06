use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;
use crate::state::buffer::BufferType;
use crate::theme::{hex_to_color, parse_format_string, resolve_abstractions};
use crate::ui::styled_text::styled_spans_to_line;
use super::{truncate_with_plus, visible_len};

/// Render the buffer list sidebar. Returns total line count for scroll clamping.
pub fn render(frame: &mut Frame, area: Rect, app: &App, scroll_offset: usize) -> usize {
    let colors = &app.theme.colors;
    let bg = hex_to_color(&colors.bg).unwrap_or(Color::Black);
    let border_color = hex_to_color(&colors.border).unwrap_or(Color::DarkGray);

    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let sorted_ids = app.state.sorted_buffer_ids();
    let active_id = app.state.active_buffer_id.as_deref();
    let abstracts = &app.theme.abstracts;
    let sidepanel = &app.theme.formats.sidepanel;
    let panel_width = app.config.sidepanel.left.width as usize;

    let mut lines: Vec<Line> = Vec::new();
    let mut last_conn_id: &str = "";
    let mut ref_num = 1u32;

    for id in &sorted_ids {
        let Some(buf) = app.state.buffers.get(id.as_str()) else {
            continue;
        };

        // Skip default Status buffer
        if buf.connection_id == crate::app::App::DEFAULT_CONN_ID {
            continue;
        }

        // Connection header — render when connection changes.
        // Server-type buffers ARE the header (no separate numbered line).
        if buf.connection_id != last_conn_id {
            last_conn_id = buf.connection_id.as_str();
            let conn_label = app
                .state
                .connections
                .get(&buf.connection_id)
                .map_or(buf.connection_id.as_str(), |c| c.label.as_str());
            let header_fmt = sidepanel
                .get("header")
                .cloned()
                .unwrap_or_else(|| "$0".to_string());
            let resolved = resolve_abstractions(&header_fmt, abstracts, 0);

            let overhead = visible_len(&parse_format_string(&resolved, &[""]));
            let max_label_len = panel_width.saturating_sub(1 + overhead);
            let display_label = truncate_with_plus(conn_label, max_label_len);

            let spans = parse_format_string(&resolved, &[&display_label]);
            lines.push(styled_spans_to_line(&spans));
        }

        // Server buffers don't get a numbered line — they're represented by the header
        if buf.buffer_type == BufferType::Server {
            continue;
        }

        let is_active = active_id == Some(id.as_str());
        let format_key = if is_active {
            "item_selected".to_string()
        } else {
            format!("item_activity_{}", buf.activity as u8)
        };

        let format = sidepanel
            .get(&format_key)
            .or_else(|| sidepanel.get("item"))
            .cloned()
            .unwrap_or_else(|| "$0. $1".to_string());
        let resolved = resolve_abstractions(&format, abstracts, 0);

        let num_str = ref_num.to_string();
        let overhead = visible_len(&parse_format_string(&resolved, &[&num_str, ""]));
        let max_name_len = panel_width.saturating_sub(1 + overhead);
        let display_name = truncate_with_plus(&buf.name, max_name_len);

        let spans = parse_format_string(&resolved, &[&num_str, &display_name]);
        lines.push(styled_spans_to_line(&spans));

        ref_num += 1;
    }

    let total_lines = lines.len();

    // Apply scroll offset, clamped so last item sits at bottom
    let visible_height = inner.height as usize;
    let max_scroll = total_lines.saturating_sub(visible_height);
    let clamped_offset = scroll_offset.min(max_scroll);
    let visible_lines: Vec<Line> = lines
        .into_iter()
        .skip(clamped_offset)
        .take(visible_height)
        .collect();

    let paragraph = Paragraph::new(visible_lines);
    frame.render_widget(paragraph, inner);

    total_lines
}

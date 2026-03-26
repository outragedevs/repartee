use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use super::{truncate_with_plus, visible_len};
use crate::app::App;
use crate::theme::{hex_to_color, parse_format_string, resolve_abstractions};
use crate::ui::styled_text::styled_spans_to_line;

/// Render the buffer list sidebar. Returns total line count for scroll clamping.
#[allow(clippy::too_many_lines)]
pub fn render(frame: &mut Frame, area: Rect, app: &App, scroll_offset: usize) -> usize {
    let colors = &app.theme.colors;
    let bg = hex_to_color(&colors.bg).unwrap_or(Color::Reset);
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
    let mut ref_num = 1u32;

    for id in &sorted_ids {
        let Some(buf) = app.state.buffers.get(id.as_str()) else {
            continue;
        };

        // Skip default Status buffer
        if buf.connection_id == crate::app::App::DEFAULT_CONN_ID {
            continue;
        }

        let is_active = active_id == Some(id.as_str());
        let is_server = buf.buffer_type == crate::state::buffer::BufferType::Server;
        let format_key = if is_active {
            if is_server { "item_server_selected" } else { "item_selected" }.to_string()
        } else if is_server {
            "item_server".to_string()
        } else {
            format!("item_activity_{}", buf.activity as u8)
        };

        let format = sidepanel
            .get(&format_key)
            .or_else(|| sidepanel.get("item"))
            .cloned()
            .unwrap_or_else(|| "$0. $1".to_string());
        let resolved = resolve_abstractions(&format, abstracts, 0);

        // Server buffers display the connection label instead of the buffer name —
        // they serve as both the visual network separator and the status window.
        let display_name = if buf.buffer_type == crate::state::buffer::BufferType::Server {
            app.state
                .connections
                .get(&buf.connection_id)
                .map_or(buf.connection_id.as_str(), |c| c.label.as_str())
                .to_string()
        } else {
            buf.name.clone()
        };

        let num_str = ref_num.to_string();
        let full_spans = parse_format_string(&resolved, &[&num_str, &display_name]);
        let total_visible = visible_len(&full_spans);
        let name_chars = display_name.chars().count();
        let num_chars = num_str.chars().count();
        let overhead = total_visible.saturating_sub(name_chars + num_chars);
        let max_name_len = panel_width.saturating_sub(1 + overhead + num_chars);
        let spans = if name_chars > max_name_len {
            let truncated = truncate_with_plus(&display_name, max_name_len);
            parse_format_string(&resolved, &[&num_str, &truncated])
        } else {
            full_spans
        };
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

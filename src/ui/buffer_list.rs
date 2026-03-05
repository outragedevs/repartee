use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;
use crate::theme::{hex_to_color, parse_format_string, resolve_abstractions};
use crate::ui::styled_text::styled_spans_to_line;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
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
    let max_name_len = (app.config.sidepanel.left.width as usize).saturating_sub(4);

    let mut lines: Vec<Line> = Vec::new();
    let mut last_conn_id = String::new();
    let mut ref_num = 0u32;

    for id in &sorted_ids {
        let buf = match app.state.buffers.get(id.as_str()) {
            Some(b) => b,
            None => continue,
        };

        // Connection header
        if buf.connection_id != last_conn_id {
            last_conn_id.clone_from(&buf.connection_id);
            let conn_label = app
                .state
                .connections
                .get(&buf.connection_id)
                .map(|c| c.label.as_str())
                .unwrap_or(&buf.connection_id);
            let header_fmt = sidepanel
                .get("header")
                .cloned()
                .unwrap_or_else(|| "$0".to_string());
            let resolved = resolve_abstractions(&header_fmt, abstracts, 0);
            let spans = parse_format_string(&resolved, &[conn_label]);
            lines.push(styled_spans_to_line(&spans));
        }

        ref_num += 1;
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

        // Truncate name to fit
        let display_name = if buf.name.len() > max_name_len && max_name_len > 1 {
            format!("{}\u{2026}", &buf.name[..max_name_len - 1])
        } else {
            buf.name.clone()
        };

        let num = ref_num.to_string();
        let spans = parse_format_string(&resolved, &[&num, &display_name]);
        lines.push(styled_spans_to_line(&spans));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

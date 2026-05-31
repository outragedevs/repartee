//! Keyboard + mouse emote picker overlay. Type to filter, arrow keys to move,
//! Enter or click to insert `:name:` at the input cursor, Esc to cancel.
//!
//! v1 renders a filtered grid of `:name:` shortcodes (not animated thumbnails);
//! that keeps the render borrow-simple and works on every terminal. Graphical
//! thumbnails in the grid are a possible future enhancement.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::App;
use crate::theme::hex_to_color;

/// Width in cells of one emote cell in the picker grid (`:name:` + padding).
const CELL_W: u16 = 18;

#[derive(Debug, Default)]
pub enum EmotePickerState {
    #[default]
    Hidden,
    Open {
        /// Current filter text (substring match against emote names).
        filter: String,
        /// Index of the highlighted emote within the filtered list.
        selected: usize,
        /// Registry index + cell rect of each rendered cell, for mouse hit-testing.
        cell_rects: Vec<(u32, Rect)>,
    },
}

impl EmotePickerState {
    #[must_use]
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Open { .. })
    }

    /// Registry indices whose name matches the current filter (all when empty).
    #[must_use]
    pub fn filtered_indices(filter: &str) -> Vec<u32> {
        crate::emotes::names()
            .iter()
            .enumerate()
            .filter(|(_, n)| filter.is_empty() || n.contains(filter))
            .map(|(i, _)| u32::try_from(i).unwrap_or(0))
            .collect()
    }
}

/// Center a `w`×`h` rect inside `area`, clamped to fit.
fn centered_rect(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

/// Render the picker overlay (no-op when hidden). Records the rendered cell rects
/// back onto `app.emote_picker` for mouse hit-testing.
pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let (filter, selected) = match &app.emote_picker {
        EmotePickerState::Open {
            filter, selected, ..
        } => (filter.clone(), *selected),
        EmotePickerState::Hidden => return,
    };

    let colors = &app.theme.colors;
    let bg = hex_to_color(&colors.bg_alt).unwrap_or(ratatui::style::Color::Black);
    let border = hex_to_color(&colors.fg_muted).unwrap_or(ratatui::style::Color::DarkGray);
    let accent = hex_to_color(&colors.accent).unwrap_or(ratatui::style::Color::Cyan);

    let popup = centered_rect(area, (area.width * 7) / 10, (area.height * 7) / 10);
    frame.render_widget(Clear, popup);

    let title = format!(" Emotes  ›  filter: {filter}_ ");
    let block = Block::default()
        .title(Span::styled(title, Style::default().fg(accent)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(bg));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut cell_rects: Vec<(u32, Rect)> = Vec::new();
    if inner.width == 0 || inner.height == 0 {
        store_cell_rects(app, cell_rects);
        return;
    }

    let filtered = EmotePickerState::filtered_indices(&filter);
    if filtered.is_empty() {
        let p = Paragraph::new("(no matching emotes)").style(Style::default().fg(border).bg(bg));
        frame.render_widget(p, inner);
        store_cell_rects(app, cell_rects);
        return;
    }

    let cols = (inner.width / CELL_W).max(1) as usize;
    let rows = inner.height as usize;
    let per_page = cols * rows;
    // Page so the selected cell is always visible.
    let page = selected / per_page;
    let start = page * per_page;
    let names = crate::emotes::names();

    for (vis, &reg_idx) in filtered.iter().enumerate().skip(start).take(per_page) {
        let slot = vis - start;
        let r = slot / cols;
        let c = slot % cols;
        let x = inner.x + u16::try_from(c).unwrap_or(0) * CELL_W;
        let y = inner.y + u16::try_from(r).unwrap_or(0);
        let w = CELL_W.min(inner.x + inner.width - x);
        if y >= inner.y + inner.height {
            break;
        }
        let rect = Rect::new(x, y, w, 1);
        let name = &names[reg_idx as usize];
        let label = format!(":{name}:");
        let style = if vis == selected {
            Style::default()
                .fg(bg)
                .bg(accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(accent).bg(bg)
        };
        frame.render_widget(Paragraph::new(Span::styled(label, style)), rect);
        cell_rects.push((reg_idx, rect));
    }

    store_cell_rects(app, cell_rects);
}

fn store_cell_rects(app: &mut App, rects: Vec<(u32, Rect)>) {
    if let EmotePickerState::Open { cell_rects, .. } = &mut app.emote_picker {
        *cell_rects = rects;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_filters_by_substring() {
        let all = EmotePickerState::filtered_indices("");
        let some = EmotePickerState::filtered_indices("usm");
        assert!(!all.is_empty());
        assert!(some.len() <= all.len());
        assert!(!some.is_empty(), "expected at least :usmiech:");
    }

    #[test]
    fn empty_filter_returns_all() {
        let all = EmotePickerState::filtered_indices("");
        assert_eq!(all.len(), crate::emotes::names().len());
    }
}

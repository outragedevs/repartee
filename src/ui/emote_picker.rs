//! Keyboard + mouse emote picker overlay. Type to filter, arrow keys to move,
//! Enter or click to insert `:name:` at the input cursor, Esc to cancel.
//!
//! On graphics-capable terminals each cell shows a **static** first-frame
//! thumbnail (animating every visible cell would be far too much per-frame work)
//! next to the name; otherwise it falls back to a `:name:` text grid.

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
        /// Number of grid columns from the last render, so Up/Down move by a row.
        cols: usize,
    },
}

impl EmotePickerState {
    #[must_use]
    pub const fn is_open(&self) -> bool {
        matches!(self, Self::Open { .. })
    }

    /// Registry indices whose name matches the current filter (all when empty).
    /// Matching is case-insensitive (emote names are lowercase).
    #[must_use]
    pub fn filtered_indices(filter: &str) -> Vec<u32> {
        let needle = filter.to_ascii_lowercase();
        crate::emotes::names()
            .iter()
            .enumerate()
            .filter(|(i, n)| {
                if needle.is_empty() || n.contains(&needle) {
                    return true;
                }
                // Also match the English alias.
                crate::emotes::english_label(u32::try_from(*i).unwrap_or(0))
                    .is_some_and(|e| e.contains(&needle))
            })
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
#[allow(clippy::too_many_lines, reason = "cohesive single-pass overlay render")]
pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let (filter, selected) = match &app.emote_picker {
        EmotePickerState::Open {
            filter, selected, ..
        } => (filter.clone(), *selected),
        EmotePickerState::Hidden => return,
    };

    let graphical = app.emotes_graphical();
    let colors = &app.theme.colors;
    let bg = hex_to_color(&colors.bg_alt).unwrap_or(ratatui::style::Color::Black);
    let border = hex_to_color(&colors.fg_muted).unwrap_or(ratatui::style::Color::DarkGray);
    let accent = hex_to_color(&colors.accent).unwrap_or(ratatui::style::Color::Cyan);
    // RGB of the popup background, for flattening transparent thumbnail pixels.
    let bg_rgb = crate::theme::hex_to_rgb_or(&colors.bg_alt, (0, 0, 0));

    // 70% of each dimension, computed in u32 to avoid u16 overflow on huge terminals.
    let pw = u16::try_from(u32::from(area.width) * 7 / 10).unwrap_or(area.width);
    let ph = u16::try_from(u32::from(area.height) * 7 / 10).unwrap_or(area.height);
    let popup = centered_rect(area, pw, ph);
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
        store_render_state(app, cell_rects, 1);
        return;
    }

    let filtered = EmotePickerState::filtered_indices(&filter);
    if filtered.is_empty() {
        let p = Paragraph::new("(no matching emotes)").style(Style::default().fg(border).bg(bg));
        frame.render_widget(p, inner);
        store_render_state(app, cell_rects, 1);
        return;
    }

    let cols = (inner.width / CELL_W).max(1) as usize;
    let rows = inner.height as usize;
    let per_page = cols * rows;
    // Page so the selected cell is always visible.
    let page = selected / per_page;
    let start = page * per_page;
    let lang = app.config.emotes.lang.to_registry();

    let emote_w = u16::try_from(crate::ui::emote_layout::EMOTE_COLS).unwrap_or(2);
    for (vis, &reg_idx) in filtered.iter().enumerate().skip(start).take(per_page) {
        let slot = vis - start;
        let row = slot / cols;
        let col = slot % cols;
        let x = inner.x + u16::try_from(col).unwrap_or(0) * CELL_W;
        let y = inner.y + u16::try_from(row).unwrap_or(0);
        let cell_w = CELL_W.min(inner.x + inner.width - x);
        if y >= inner.y + inner.height {
            break;
        }
        let rect = Rect::new(x, y, cell_w, 1);
        let name = crate::emotes::display_name(reg_idx, lang).to_owned();
        let is_sel = vis == selected;
        let style = if is_sel {
            Style::default()
                .fg(bg)
                .bg(accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(accent).bg(bg)
        };

        // Graphical thumbnail + name when there's room; otherwise (text mode, or
        // a cell too narrow for a thumbnail) fall back to the `:name:` label so a
        // cell is never blank-but-clickable.
        if graphical && cell_w > emote_w {
            let img_rect = Rect::new(x, y, emote_w, 1);
            if let Some(proto) = app.emote_animator.thumbnail(&app.picker, reg_idx, bg_rgb) {
                frame.render_stateful_widget(
                    ratatui_image::StatefulImage::default(),
                    img_rect,
                    proto,
                );
            }
            let name_x = x + emote_w + 1;
            let name_w = cell_w.saturating_sub(emote_w + 1);
            frame.render_widget(
                Paragraph::new(Span::styled(name, style)),
                Rect::new(name_x, y, name_w, 1),
            );
        } else {
            frame.render_widget(
                Paragraph::new(Span::styled(format!(":{name}:"), style)),
                rect,
            );
        }
        cell_rects.push((reg_idx, rect));
    }

    store_render_state(app, cell_rects, cols);
}

fn store_render_state(app: &mut App, rects: Vec<(u32, Rect)>, grid_cols: usize) {
    if let EmotePickerState::Open {
        cell_rects, cols, ..
    } = &mut app.emote_picker
    {
        *cell_rects = rects;
        *cols = grid_cols.max(1);
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

    #[test]
    fn filter_matches_english_alias() {
        let by_en = EmotePickerState::filtered_indices("smile");
        let by_pl = EmotePickerState::filtered_indices("usmiech");
        assert!(!by_en.is_empty());
        assert!(
            by_en.iter().any(|i| by_pl.contains(i)),
            "EN and PL filters hit the same emote"
        );
    }
}

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear};

use crate::app::App;
use crate::state::buffer::BufferType;
use crate::theme::hex_to_color;

#[derive(Debug, Clone, Copy, Default)]
#[allow(dead_code)]
#[expect(
    clippy::struct_field_names,
    reason = "_area suffix clarifies these are ratatui Rect regions"
)]
pub struct UiRegions {
    pub buffer_list_area: Option<Rect>,
    pub chat_area: Option<Rect>,
    pub nick_list_area: Option<Rect>,
    pub topic_area: Option<Rect>,
    pub status_area: Option<Rect>,
    pub input_area: Option<Rect>,
}

/// Compute the chat area dimensions without a Frame.
/// Used by the shell subsystem to size PTYs to match the actual render area.
pub fn compute_chat_area_size(
    term_cols: u16,
    term_rows: u16,
    left_visible: bool,
    left_width: u16,
    right_visible: bool,
    right_width: u16,
) -> (u16, u16) {
    // Vertical: topic(1) + main(fill) + bottom(3) = main = rows - 4.
    // Bottom block has 1px top border, so inner is 2 rows (status + input).
    let main_height = term_rows.saturating_sub(4);

    // Horizontal: depends on sidebar visibility.
    let mut chat_width = term_cols;
    if left_visible {
        chat_width = chat_width.saturating_sub(left_width);
    }
    if right_visible {
        chat_width = chat_width.saturating_sub(right_width);
    }

    (chat_width.max(1), main_height.max(1))
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let colors = &app.theme.colors;
    let bg = hex_to_color(&colors.bg).unwrap_or(Color::Reset);
    let bg_alt = hex_to_color(&colors.bg_alt).unwrap_or(Color::Reset);
    let border_color = hex_to_color(&colors.border).unwrap_or(Color::DarkGray);

    // Clear background
    let block = Block::default().style(Style::default().bg(bg));
    frame.render_widget(block, frame.area());

    let config = &app.config;
    let left_width = config.sidepanel.left.width;
    let right_width = config.sidepanel.right.width;
    let left_visible = config.sidepanel.left.visible;

    let show_nicklist = config.sidepanel.right.visible
        && app
            .state
            .active_buffer()
            .is_some_and(|b| b.buffer_type == BufferType::Channel);

    let [topic_area, main_area, bottom_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(3),
    ])
    .areas(frame.area());

    super::topic_bar::render(frame, topic_area, app);

    let mut regions = UiRegions {
        topic_area: Some(topic_area),
        ..Default::default()
    };

    match (left_visible, show_nicklist) {
        (true, true) => {
            let [buf_list_area, chat_area, nick_area] = Layout::horizontal([
                Constraint::Length(left_width),
                Constraint::Fill(1),
                Constraint::Length(right_width),
            ])
            .areas(main_area);
            app.buffer_list_total =
                super::buffer_list::render(frame, buf_list_area, app, app.buffer_list_scroll);
            regions.buffer_list_area = Some(buf_list_area);
            super::chat_view::render(frame, chat_area, app);
            regions.chat_area = Some(chat_area);
            app.nick_list_total =
                super::nick_list::render(frame, nick_area, app, app.nick_list_scroll);
            regions.nick_list_area = Some(nick_area);
        }
        (true, false) => {
            let [buf_list_area, chat_area] =
                Layout::horizontal([Constraint::Length(left_width), Constraint::Fill(1)])
                    .areas(main_area);
            app.buffer_list_total =
                super::buffer_list::render(frame, buf_list_area, app, app.buffer_list_scroll);
            regions.buffer_list_area = Some(buf_list_area);
            super::chat_view::render(frame, chat_area, app);
            regions.chat_area = Some(chat_area);
        }
        (false, true) => {
            let [chat_area, nick_area] =
                Layout::horizontal([Constraint::Fill(1), Constraint::Length(right_width)])
                    .areas(main_area);
            super::chat_view::render(frame, chat_area, app);
            regions.chat_area = Some(chat_area);
            app.nick_list_total =
                super::nick_list::render(frame, nick_area, app, app.nick_list_scroll);
            regions.nick_list_area = Some(nick_area);
        }
        (false, false) => {
            super::chat_view::render(frame, main_area, app);
            regions.chat_area = Some(main_area);
        }
    }

    let bottom_block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(bg_alt));
    let bottom_inner = bottom_block.inner(bottom_area);
    frame.render_widget(bottom_block, bottom_area);

    let [status_area, input_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(bottom_inner);

    super::status_line::render(frame, status_area, app);
    super::input::render(frame, input_area, app);

    regions.status_area = Some(status_area);
    regions.input_area = Some(input_area);

    app.ui_regions = Some(regions);

    // Spell suggestion popup (above input, below image overlay).
    super::input::render_spell_popup(frame, input_area, app);

    // Image preview overlay (drawn last, on top of everything).
    super::image_overlay::render(frame, frame.area(), app);

    // Targeted repaint after image dismiss (Kitty/iTerm2 only).
    // The graphics layer was already cleaned up by escape sequences.
    // Rendering Clear over the popup area forces ratatui's diff to
    // repaint those cells, avoiding a full terminal.clear() flicker.
    if let Some(rect) = app.image_clear_rect.take() {
        frame.render_widget(Clear, rect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_chat_area_both_sidebars() {
        let (cols, rows) = compute_chat_area_size(120, 40, true, 20, true, 20);
        assert_eq!(cols, 80); // 120 - 20 - 20
        assert_eq!(rows, 36); // 40 - 4
    }

    #[test]
    fn compute_chat_area_left_sidebar_only() {
        let (cols, rows) = compute_chat_area_size(120, 40, true, 20, false, 0);
        assert_eq!(cols, 100); // 120 - 20
        assert_eq!(rows, 36);
    }

    #[test]
    fn compute_chat_area_no_sidebars() {
        let (cols, rows) = compute_chat_area_size(120, 40, false, 0, false, 0);
        assert_eq!(cols, 120);
        assert_eq!(rows, 36);
    }

    #[test]
    fn compute_chat_area_tiny_terminal() {
        let (cols, rows) = compute_chat_area_size(10, 5, true, 20, false, 0);
        assert_eq!(cols, 1); // clamped to min 1
        assert_eq!(rows, 1); // 5 - 4 = 1
    }
}

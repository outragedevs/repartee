use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders};

use crate::app::App;
use crate::state::buffer::BufferType;
use crate::theme::hex_to_color;

#[derive(Debug, Clone, Copy, Default)]
#[allow(dead_code)]
#[expect(clippy::struct_field_names, reason = "_area suffix clarifies these are ratatui Rect regions")]
pub struct UiRegions {
    pub buffer_list_area: Option<Rect>,
    pub chat_area: Option<Rect>,
    pub nick_list_area: Option<Rect>,
    pub topic_area: Option<Rect>,
    pub status_area: Option<Rect>,
    pub input_area: Option<Rect>,
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let colors = &app.theme.colors;
    let bg = hex_to_color(&colors.bg).unwrap_or(Color::Black);
    let bg_alt = hex_to_color(&colors.bg_alt).unwrap_or(Color::Black);
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

    let mut main_constraints: Vec<Constraint> = Vec::new();
    if left_visible {
        main_constraints.push(Constraint::Length(left_width));
    }
    main_constraints.push(Constraint::Fill(1));
    if show_nicklist {
        main_constraints.push(Constraint::Length(right_width));
    }

    let main_chunks = Layout::horizontal(main_constraints).split(main_area);
    let mut chunk_idx = 0;

    let mut regions = UiRegions {
        topic_area: Some(topic_area),
        ..Default::default()
    };

    if left_visible {
        let buf_list_area = main_chunks[chunk_idx];
        app.buffer_list_total =
            super::buffer_list::render(frame, buf_list_area, app, app.buffer_list_scroll);
        regions.buffer_list_area = Some(buf_list_area);
        chunk_idx += 1;
    }

    let chat_area = main_chunks[chunk_idx];
    super::chat_view::render(frame, chat_area, app);
    regions.chat_area = Some(chat_area);
    chunk_idx += 1;

    if show_nicklist {
        let nick_area = main_chunks[chunk_idx];
        app.nick_list_total =
            super::nick_list::render(frame, nick_area, app, app.nick_list_scroll);
        regions.nick_list_area = Some(nick_area);
    }

    let _ = chunk_idx;

    let bottom_block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(bg_alt));
    let bottom_inner = bottom_block.inner(bottom_area);
    frame.render_widget(bottom_block, bottom_area);

    let [status_area, input_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(bottom_inner);

    super::status_line::render(frame, status_area, app);
    super::input::render(frame, input_area, app);

    regions.status_area = Some(status_area);
    regions.input_area = Some(input_area);

    app.ui_regions = Some(regions);

    // Image preview overlay (drawn last, on top of everything).
    super::image_overlay::render(frame, frame.area(), app);
}

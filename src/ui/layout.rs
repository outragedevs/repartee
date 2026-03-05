use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders};

use crate::app::App;
use crate::state::buffer::BufferType;
use crate::theme::hex_to_color;

pub fn draw(frame: &mut Frame, app: &App) {
    let colors = &app.theme.colors;
    let bg = hex_to_color(&colors.bg).unwrap_or(Color::Black);
    let border_color = hex_to_color(&colors.border).unwrap_or(Color::DarkGray);

    // Clear background
    let block = Block::default().style(Style::default().bg(bg));
    frame.render_widget(block, frame.area());

    let config = &app.config;
    let left_width = config.sidepanel.left.width;
    let right_width = config.sidepanel.right.width;
    let left_visible = config.sidepanel.left.visible;

    // Check if nicklist should show (only for channel buffers)
    let show_nicklist = config.sidepanel.right.visible
        && app
            .state
            .active_buffer()
            .is_some_and(|b| b.buffer_type == BufferType::Channel);

    // Vertical layout: topic | main | bottom
    let [topic_area, main_area, bottom_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(3), // statusline(1) + input(1) + top border(1)
    ])
    .areas(frame.area());

    // --- Topic bar ---
    super::topic_bar::render(frame, topic_area, app);

    // --- Main area: sidebar | chat | nicklist ---
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

    if left_visible {
        super::buffer_list::render(frame, main_chunks[chunk_idx], app);
        chunk_idx += 1;
    }

    // Chat area
    super::chat_view::render(frame, main_chunks[chunk_idx], app);
    chunk_idx += 1;

    if show_nicklist {
        super::nick_list::render(frame, main_chunks[chunk_idx], app);
    }

    // Suppress unused variable warning when nicklist is conditionally skipped
    let _ = chunk_idx;

    // --- Bottom area: status line + input ---
    let bottom_block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(bg));
    let bottom_inner = bottom_block.inner(bottom_area);
    frame.render_widget(bottom_block, bottom_area);

    let [status_area, input_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(bottom_inner);

    super::status_line::render(frame, status_area, app);
    super::input::render(frame, input_area, app);
}

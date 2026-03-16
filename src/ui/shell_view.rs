use ratatui::prelude::*;

use crate::app::App;

/// Render the active shell buffer by mapping vt100 screen cells to ratatui buffer cells.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let Some(buf) = app.state.active_buffer() else {
        return;
    };
    let Some(shell_id) = app.shell_mgr.session_id_for_buffer(&buf.id) else {
        return;
    };
    let Some(screen) = app.shell_mgr.screen(shell_id) else {
        return;
    };

    let (screen_rows, screen_cols) = screen.size();
    let rows = area.height.min(screen_rows);
    let cols = area.width.min(screen_cols);

    // Log dimension mismatch for debugging full-screen TUI programs.
    if screen_cols != area.width || screen_rows != area.height {
        tracing::debug!(
            screen_cols,
            screen_rows,
            render_width = area.width,
            render_height = area.height,
            "shell_view: PTY/render dimension mismatch"
        );
    }

    let ratatui_buf = frame.buffer_mut();

    // Write every cell 1:1 from vt100 screen to ratatui buffer.
    // Use set_char for single chars and set_symbol only for multi-char graphemes.
    for row in 0..rows {
        for col in 0..cols {
            let x = area.x + col;
            let y = area.y + row;

            let Some(ratatui_cell) = ratatui_buf.cell_mut(Position::new(x, y)) else {
                continue;
            };

            let Some(cell) = screen.cell(row, col) else {
                ratatui_cell.set_char(' ');
                ratatui_cell.set_style(Style::reset());
                continue;
            };

            let ch = cell.contents();
            let fg = vt100_color_to_ratatui(cell.fgcolor());
            let bg = vt100_color_to_ratatui(cell.bgcolor());

            let mut modifier = Modifier::empty();
            if cell.bold() {
                modifier |= Modifier::BOLD;
            }
            if cell.italic() {
                modifier |= Modifier::ITALIC;
            }
            if cell.underline() {
                modifier |= Modifier::UNDERLINED;
            }
            if cell.inverse() {
                modifier |= Modifier::REVERSED;
            }

            let style = Style::new().fg(fg).bg(bg).add_modifier(modifier);

            // Map vt100 cell content to a single char for ratatui.
            let display_char = if ch.is_empty() {
                ' ' // Wide-char continuation or empty cell.
            } else {
                ch.chars().next().unwrap_or(' ')
            };
            ratatui_cell.set_char(display_char);
            ratatui_cell.set_style(style);
        }
    }

    // Position the cursor if the shell is showing it.
    if !screen.hide_cursor() {
        let (crow, ccol) = screen.cursor_position();
        let cx = area.x + ccol;
        let cy = area.y + crow;
        if cx < area.x + area.width && cy < area.y + area.height {
            frame.set_cursor_position(Position::new(cx, cy));
        }
    }
}

/// Map a vt100 color to a ratatui Color.
const fn vt100_color_to_ratatui(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(n) => Color::Indexed(n),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vt100_color_default_maps_to_reset() {
        assert_eq!(vt100_color_to_ratatui(vt100::Color::Default), Color::Reset);
    }

    #[test]
    fn vt100_color_indexed_maps_correctly() {
        assert_eq!(
            vt100_color_to_ratatui(vt100::Color::Idx(196)),
            Color::Indexed(196)
        );
    }

    #[test]
    fn vt100_color_rgb_maps_correctly() {
        assert_eq!(
            vt100_color_to_ratatui(vt100::Color::Rgb(255, 128, 0)),
            Color::Rgb(255, 128, 0)
        );
    }
}

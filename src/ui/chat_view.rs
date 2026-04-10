use std::collections::VecDeque;

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme::hex_to_color;

/// Hard upper bound on how many visual (wrapped) lines a single message can
/// contribute to a frame. Used to cap the render budget so the scroll loop
/// cannot walk the entire buffer when `scroll_offset` exceeds available
/// content. Even a ~1000-char NOTICE on an 80-column terminal wraps to
/// ~13 lines; 16 is a safe over-estimate for realistic IRC workloads.
const MAX_WRAPPED_LINES_PER_MSG: usize = 16;

/// Compute the target size of the render `VecDeque<Line>` for chat view.
///
/// Returns the number of visual lines the render loop should aim to build
/// before breaking. Capped to `buffer_len * MAX_WRAPPED_LINES_PER_MSG` so
/// the loop terminates in O(`buffer_len`) regardless of `scroll_offset`.
/// For empty buffers falls back to `visible_height` so at least a full
/// screen is aimed for.
///
/// This is the minimal guarantee the caller needs: the loop's break
/// condition `visual_lines.len() > needed` will fire in bounded time even
/// under pathological `scroll_offset` values (e.g. after a long mouse-wheel
/// scroll past the top of the buffer).
fn compute_render_budget(
    buffer_len: usize,
    visible_height: usize,
    scroll_offset: usize,
) -> usize {
    let cap = buffer_len
        .saturating_mul(MAX_WRAPPED_LINES_PER_MSG)
        .max(visible_height);
    visible_height.saturating_add(scroll_offset).min(cap)
}

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
        // visual lines. Stop once we have enough to fill the screen plus
        // the current scroll offset. `needed` is capped at
        // `buf.messages.len() * MAX_WRAPPED_LINES_PER_MSG` by
        // `compute_render_budget` so the break condition below fires in
        // O(buf.messages.len()) regardless of how far the user has
        // scroll-wheeled past available content — this is the fix for the
        // v0.8.4 OOM. See docs/superpowers/specs/2026-04-10-v084-oom-fix-design.md.
        let needed = compute_render_budget(buf.messages.len(), visible_height, app.scroll_offset);
        let mut visual_lines: VecDeque<Line<'_>> = VecDeque::new();

        for msg in buf.messages.iter().rev() {
            let is_own = msg.nick.as_deref() == Some(current_nick);
            let nick_fg = if app.config.display.nick_colors && !is_own && !msg.highlight {
                msg.nick.as_deref().map(|n| {
                    crate::nick_color::nick_color(
                        n,
                        app.color_support,
                        app.config.display.nick_color_saturation,
                        app.config.display.nick_color_lightness,
                    )
                })
            } else {
                None
            };
            let line = super::message_line::render_message(msg, is_own, &app.theme, &app.config, nick_fg);
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

#[cfg(test)]
mod tests {
    use super::{MAX_WRAPPED_LINES_PER_MSG, compute_render_budget};

    #[test]
    fn normal_scroll_returns_visible_plus_offset() {
        // Typical case: user scrolled a bit, offset is small compared to buffer cap.
        let got = compute_render_budget(2000, 78, 50);
        assert_eq!(got, 78 + 50);
    }

    #[test]
    fn zero_scroll_returns_visible_height() {
        let got = compute_render_budget(2000, 78, 0);
        assert_eq!(got, 78);
    }

    #[test]
    fn pathological_scroll_is_capped_to_buffer_len_times_max_wraps() {
        // The actual OOM bug: scroll_offset pushed far past available content.
        // `needed` must not exceed buffer_len * MAX_WRAPPED_LINES_PER_MSG —
        // that is the invariant that guarantees the render loop terminates
        // in O(buffer_len) instead of walking every message every frame.
        let buffer_len = 2000;
        let got = compute_render_budget(buffer_len, 78, usize::MAX / 2);
        let expected_cap = buffer_len * MAX_WRAPPED_LINES_PER_MSG;
        assert_eq!(
            got, expected_cap,
            "pathological scroll_offset must be capped to buffer_len * MAX_WRAPPED_LINES_PER_MSG"
        );
    }

    #[test]
    fn empty_buffer_returns_visible_height() {
        // Empty buffer: cap is 0 before the .max(visible_height) floor, but
        // the floor ensures render still targets a full screen.
        let got = compute_render_budget(0, 78, 1000);
        assert_eq!(got, 78);
    }

    #[test]
    fn small_buffer_large_scroll_is_capped_to_buffer_cap() {
        // 10-message buffer cannot produce more than 10 * 16 = 160 visual lines.
        // Even with scroll_offset of 1M, `needed` caps at 160.
        let got = compute_render_budget(10, 78, 1_000_000);
        assert_eq!(got, 10 * MAX_WRAPPED_LINES_PER_MSG);
    }

    #[test]
    fn overflow_safe_on_usize_max_scroll() {
        // visible_height + scroll_offset must not overflow. saturating_add
        // protects the intermediate, then min() with cap brings it down.
        let got = compute_render_budget(100, 78, usize::MAX);
        assert_eq!(got, 100 * MAX_WRAPPED_LINES_PER_MSG);
    }
}

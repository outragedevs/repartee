use std::collections::VecDeque;

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme::hex_to_color;

// Hard upper bound on wrapped lines per message used by `compute_render_budget`
// to cap the render loop's work. A ~1000-char NOTICE on an 80-col terminal
// wraps to ~13 visual lines; 16 is a safe over-estimate for realistic IRC
// traffic. See docs/superpowers/specs/2026-04-10-v084-oom-fix-design.md.
const MAX_WRAPPED_LINES_PER_MSG: usize = 16;

// Compute the target size of the render `VecDeque<Line>` for chat view.
// Capped at `buffer_len * MAX_WRAPPED_LINES_PER_MSG` so the caller's break
// condition `visual_lines.len() > needed` fires in O(buffer_len) regardless
// of `scroll_offset`. Empty buffers fall back to `visible_height`.
fn compute_render_budget(buffer_len: usize, visible_height: usize, scroll_offset: usize) -> usize {
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

        // Walk messages in reverse and wrap each into visual lines. The break
        // below fires in O(buf.messages.len()) via the budget cap — this
        // prevents the v0.8.4 OOM on long mouse-wheel scrolls.
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
            let line =
                super::message_line::render_message(msg, is_own, &app.theme, &app.config, nick_fg);
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
    mod compute_render_budget {
        use super::super::{MAX_WRAPPED_LINES_PER_MSG, compute_render_budget};

        #[test]
        fn returns_visible_plus_offset_for_normal_scroll() {
            // Typical case: user scrolled a bit, offset small vs buffer cap.
            let got = compute_render_budget(2000, 78, 50);
            assert_eq!(
                got, 128,
                "2000-msg buffer with scroll_offset=50 should return visible_height+offset (78+50=128), got {got}"
            );
        }

        #[test]
        fn returns_visible_height_when_scroll_is_zero() {
            let got = compute_render_budget(2000, 78, 0);
            assert_eq!(
                got, 78,
                "zero scroll_offset should return exactly visible_height, got {got}"
            );
        }

        #[test]
        fn caps_at_buffer_times_max_wraps_for_pathological_scroll() {
            // This test locks in the v0.8.4 OOM fix invariant: scroll_offset
            // pushed far past available content must NOT cause the render
            // loop to walk every message per frame. `needed` is bounded by
            // buffer_len * MAX_WRAPPED_LINES_PER_MSG, guaranteeing
            // O(buffer_len) termination.
            let buffer_len = 2000;
            let got = compute_render_budget(buffer_len, 78, usize::MAX / 2);
            let expected = buffer_len * MAX_WRAPPED_LINES_PER_MSG;
            assert_eq!(
                got,
                expected,
                "pathological scroll_offset={} with buffer_len={buffer_len} must cap at buffer_len*MAX_WRAPPED_LINES_PER_MSG={expected}, got {got}",
                usize::MAX / 2
            );
        }

        #[test]
        fn returns_visible_height_for_empty_buffer() {
            // Empty buffer: buffer_cap is 0 before the .max(visible_height)
            // floor. The floor ensures render still targets a full screen
            // even when scroll_offset is large.
            let got = compute_render_budget(0, 78, 1000);
            assert_eq!(
                got, 78,
                "empty buffer with any scroll_offset should fall back to visible_height, got {got}"
            );
        }

        #[test]
        fn caps_at_buffer_cap_for_small_buffer_with_large_scroll() {
            // 10-message buffer cannot produce more than 10*16=160 lines
            // even if the user scrolled a million ticks up.
            let got = compute_render_budget(10, 78, 1_000_000);
            let expected = 10 * MAX_WRAPPED_LINES_PER_MSG;
            assert_eq!(
                got, expected,
                "10-msg buffer with scroll_offset=1M must cap at 10*MAX_WRAPPED_LINES_PER_MSG={expected}, got {got}"
            );
        }

        #[test]
        fn is_overflow_safe_for_usize_max_scroll() {
            // visible_height + scroll_offset must never panic on overflow.
            // saturating_add protects the intermediate, then .min() with
            // the cap brings the final value down to a sane number.
            let got = compute_render_budget(100, 78, usize::MAX);
            let expected = 100 * MAX_WRAPPED_LINES_PER_MSG;
            assert_eq!(
                got, expected,
                "usize::MAX scroll_offset must not overflow and must cap at 100*MAX_WRAPPED_LINES_PER_MSG={expected}, got {got}"
            );
        }
    }
}

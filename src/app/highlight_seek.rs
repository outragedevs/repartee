//! `seek_previous_highlight` / `seek_next_highlight` binding actions: jump the
//! chat view to the nearest loaded highlight above or below what is visible.

use super::App;
use crate::state::buffer::BufferType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekDirection {
    Previous,
    Next,
}

/// The highlight the last seek targeted. It stays the anchor for the next seek
/// only while the view has not been scrolled by anything else, so manual
/// scrolling falls back to the visible edges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSeek {
    buffer_id: String,
    message_id: u64,
    state: SeekState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeekState {
    /// Waiting for the chat renderer, which knows the wrapped line counts, to
    /// scroll the target into view. Requested at this scroll offset.
    Pending { from: usize },
    /// The renderer placed the target at this scroll offset.
    Shown { offset: usize },
}

impl HighlightSeek {
    fn anchors(&self, buffer_id: &str, scroll_offset: usize) -> bool {
        let (SeekState::Pending { from: offset } | SeekState::Shown { offset }) = self.state;
        self.buffer_id == buffer_id && offset == scroll_offset
    }

    /// Message the renderer should scroll to, unless the view moved since the
    /// seek was requested.
    pub fn pending_target(&self, buffer_id: &str, scroll_offset: usize) -> Option<u64> {
        (matches!(self.state, SeekState::Pending { .. }) && self.anchors(buffer_id, scroll_offset))
            .then_some(self.message_id)
    }

    pub const fn is_pending(&self) -> bool {
        matches!(self.state, SeekState::Pending { .. })
    }

    pub const fn shown_at(&mut self, offset: usize) {
        self.state = SeekState::Shown { offset };
    }
}

impl App {
    pub(crate) fn seek_highlight(&mut self, direction: SeekDirection) {
        let Some(buffer) = self.state.active_buffer() else {
            return;
        };
        if buffer.buffer_type == BufferType::Shell {
            return;
        }
        let messages = &buffer.messages;
        let index_of = |id: u64| messages.iter().position(|message| message.id == id);
        let anchor = self
            .highlight_seek
            .as_ref()
            .filter(|seek| seek.anchors(&buffer.id, self.scroll_offset))
            .and_then(|seek| index_of(seek.message_id));
        let target = match direction {
            SeekDirection::Previous => {
                let end = anchor
                    .or_else(|| {
                        self.chat_rows
                            .as_ref()
                            .and_then(|rows| rows.top_message(&buffer.id))
                            .and_then(index_of)
                    })
                    .unwrap_or(messages.len());
                messages.range(..end).rposition(|message| message.highlight)
            }
            SeekDirection::Next => {
                let Some(start) = anchor.or_else(|| {
                    self.chat_rows
                        .as_ref()
                        .and_then(|rows| rows.bottom_message(&buffer.id))
                        .and_then(index_of)
                }) else {
                    return;
                };
                messages
                    .iter()
                    .skip(start + 1)
                    .position(|message| message.highlight)
                    .map(|offset| start + 1 + offset)
            }
        };
        match target {
            Some(index) => {
                let message_id = messages[index].id;
                // Like PageUp: keep the loaded history while reading it, so
                // `enforce_scrollback` does not drop the target or later anchors.
                self.pin_active_backlog();
                let Some(buffer_id) = self.state.active_buffer_id.clone() else {
                    return;
                };
                self.highlight_seek = Some(HighlightSeek {
                    buffer_id,
                    message_id,
                    state: SeekState::Pending {
                        from: self.scroll_offset,
                    },
                });
            }
            None if direction == SeekDirection::Next => {
                self.highlight_seek = None;
                self.scroll_offset = 0;
                self.collapse_backlog_if_at_bottom();
            }
            None => {}
        }
    }

    /// Explicit scroll actions decide the view, so they drop a seek that the
    /// renderer has not applied yet.
    pub(crate) fn cancel_pending_highlight_seek(&mut self) {
        if self.highlight_seek.as_ref().is_some_and(HighlightSeek::is_pending) {
            self.highlight_seek = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::buffer::{Buffer, Message, MessageType};

    fn message(id: u64, highlight: bool) -> Message {
        Message {
            redaction_ref: None,
            redaction_msgid: None,
            log_key: None,
            id,
            timestamp: chrono::Utc::now(),
            message_type: MessageType::Message,
            nick: Some("peer".into()),
            nick_mode: None,
            text: format!("line {id}"),
            highlight,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
            wire_origin: None,
            translation_suffix_at: None,
        }
    }

    fn render(app: &mut App) {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 8)).unwrap();
        terminal
            .draw(|frame| crate::ui::chat_view::render(frame, frame.area(), app))
            .unwrap();
    }

    fn app_with(highlights: &[u64]) -> App {
        app_with_text(highlights, |id| format!("line {id}"))
    }

    fn app_with_text(highlights: &[u64], text: impl Fn(u64) -> String) -> App {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let mut buffer = Buffer::for_test("net", BufferType::Channel, "#chan");
        for id in 1..=100 {
            let mut line = message(id, highlights.contains(&id));
            line.text = text(id);
            buffer.messages.push_back(line);
        }
        app.state.add_buffer(buffer);
        app.state.set_active_buffer("net/#chan");
        render(&mut app);
        app
    }

    fn target(app: &App) -> u64 {
        app.highlight_seek.as_ref().unwrap().message_id
    }

    fn visible(app: &App) -> (u64, u64) {
        let rows = app.chat_rows.as_ref().unwrap();
        (
            rows.top_message("net/#chan").unwrap(),
            rows.bottom_message("net/#chan").unwrap(),
        )
    }

    #[test]
    fn previous_and_next_walk_highlights_outside_the_view_and_return_live() {
        let mut app = app_with(&[10, 40, 99]);
        assert_eq!(visible(&app).1, 100);
        app.seek_highlight(SeekDirection::Previous);
        render(&mut app);
        let (top, bottom) = visible(&app);
        assert!((top..=bottom).contains(&40), "{top}..{bottom}");
        assert!(app.scroll_offset > 0);
        assert!(app.state.buffers["net/#chan"].pin_backlog);
        app.seek_highlight(SeekDirection::Previous);
        render(&mut app);
        let (top, bottom) = visible(&app);
        assert!((top..=bottom).contains(&10), "{top}..{bottom}");
        app.seek_highlight(SeekDirection::Previous);
        render(&mut app);
        assert_eq!(target(&app), 10);
        app.seek_highlight(SeekDirection::Next);
        render(&mut app);
        assert_eq!(target(&app), 40);
        app.seek_highlight(SeekDirection::Next);
        render(&mut app);
        assert_eq!(target(&app), 99);
        app.seek_highlight(SeekDirection::Next);
        render(&mut app);
        assert!(app.highlight_seek.is_none());
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(visible(&app).1, 100);
    }

    #[test]
    fn the_target_stays_visible_beyond_the_per_message_render_budget() {
        let mut app = app_with_text(&[10], |id| format!("{id} {}", "word ".repeat(300)));
        app.seek_highlight(SeekDirection::Previous);
        for _ in 0..3 {
            render(&mut app);
            let (top, bottom) = visible(&app);
            assert!((top..=bottom).contains(&10), "{top}..{bottom}");
        }
        assert!(app.scroll_offset > 100 * 16);
        let (top, _) = visible(&app);
        let offset = app.scroll_offset;
        app.scroll_offset += 3;
        render(&mut app);
        assert_eq!(app.scroll_offset, offset + 3);
        assert!(visible(&app).0 <= top);
        app.scroll_offset -= 6;
        render(&mut app);
        assert_eq!(app.scroll_offset, offset - 3);
        assert!(visible(&app).0 >= top);
    }

    #[test]
    fn manual_scrolling_replaces_the_anchor_with_the_visible_edge() {
        let mut app = app_with(&[10, 40, 70]);
        app.seek_highlight(SeekDirection::Previous);
        render(&mut app);
        assert_eq!(target(&app), 70);
        app.scroll_offset += 40;
        render(&mut app);
        let (top, _) = visible(&app);
        assert!(top < 40);
        app.seek_highlight(SeekDirection::Previous);
        render(&mut app);
        assert_eq!(target(&app), 10);
    }

    #[test]
    fn a_later_scroll_before_the_redraw_wins_over_a_pending_seek() {
        let mut app = app_with(&[10, 40, 70]);
        app.execute_binding(&crate::keybindings::Binding::new(
            "multi",
            "seek_previous_highlight;scroll_end",
        ));
        render(&mut app);
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(visible(&app).1, 100);
        assert!(app.highlight_seek.is_none());

        app.seek_highlight(SeekDirection::Previous);
        app.scroll_offset += 10;
        render(&mut app);
        assert_eq!(app.scroll_offset, 10);
        assert!(app.highlight_seek.is_none());
    }

    #[test]
    fn repeated_presses_before_a_redraw_continue_from_the_pending_target() {
        let mut app = app_with(&[10, 40, 70]);
        app.seek_highlight(SeekDirection::Previous);
        app.seek_highlight(SeekDirection::Previous);
        render(&mut app);
        let (top, bottom) = visible(&app);
        assert!((top..=bottom).contains(&40), "{top}..{bottom}");
    }

    #[test]
    fn without_highlights_previous_stays_and_next_at_the_bottom_is_a_no_op() {
        let mut app = app_with(&[]);
        app.seek_highlight(SeekDirection::Previous);
        assert!(app.highlight_seek.is_none());
        app.seek_highlight(SeekDirection::Next);
        render(&mut app);
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(visible(&app).1, 100);
    }
}

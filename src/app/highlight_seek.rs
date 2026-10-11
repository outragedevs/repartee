//! `seek_previous_highlight` / `seek_next_highlight` binding actions: jump the
//! chat view to the nearest loaded highlight above or below what is visible.

use super::App;
use crate::state::buffer::BufferType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekDirection {
    Previous,
    Next,
}

/// The highlight the last seek targeted. `scroll_offset` is `None` until the
/// chat renderer has scrolled it into view, then records the resulting offset;
/// the seek stays the anchor for the next one only while the view is still
/// there, so any manual scrolling falls back to the visible edges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSeek {
    pub buffer_id: String,
    pub message_id: u64,
    pub scroll_offset: Option<usize>,
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
            .filter(|seek| {
                seek.buffer_id == buffer.id
                    && seek
                        .scroll_offset
                        .is_none_or(|offset| offset == self.scroll_offset)
            })
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
                self.highlight_seek = Some(HighlightSeek {
                    buffer_id: buffer.id.clone(),
                    message_id: messages[index].id,
                    scroll_offset: None,
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
        let mut app = crate::app::input::submit_typing_tests::test_app();
        let mut buffer = Buffer::for_test("net", BufferType::Channel, "#chan");
        for id in 1..=100 {
            buffer.messages.push_back(message(id, highlights.contains(&id)));
        }
        app.state.add_buffer(buffer);
        app.state.set_active_buffer("net/#chan");
        render(&mut app);
        app
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
        app.seek_highlight(SeekDirection::Previous);
        render(&mut app);
        let (top, bottom) = visible(&app);
        assert!((top..=bottom).contains(&10), "{top}..{bottom}");
        app.seek_highlight(SeekDirection::Previous);
        render(&mut app);
        assert_eq!(app.highlight_seek.as_ref().unwrap().message_id, 10);
        app.seek_highlight(SeekDirection::Next);
        render(&mut app);
        assert_eq!(app.highlight_seek.as_ref().unwrap().message_id, 40);
        app.seek_highlight(SeekDirection::Next);
        render(&mut app);
        assert_eq!(app.highlight_seek.as_ref().unwrap().message_id, 99);
        app.seek_highlight(SeekDirection::Next);
        render(&mut app);
        assert!(app.highlight_seek.is_none());
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(visible(&app).1, 100);
    }

    #[test]
    fn manual_scrolling_replaces_the_anchor_with_the_visible_edge() {
        let mut app = app_with(&[10, 40, 70]);
        app.seek_highlight(SeekDirection::Previous);
        render(&mut app);
        assert_eq!(app.highlight_seek.as_ref().unwrap().message_id, 70);
        app.scroll_offset += 40;
        render(&mut app);
        let (top, _) = visible(&app);
        assert!(top < 40);
        app.seek_highlight(SeekDirection::Previous);
        render(&mut app);
        assert_eq!(app.highlight_seek.as_ref().unwrap().message_id, 10);
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

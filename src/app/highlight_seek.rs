//! `seek_previous_highlight` / `seek_next_highlight` binding actions: jump the
//! chat view to the nearest loaded highlight above or below what is visible.

use std::collections::VecDeque;

use super::App;
use crate::state::buffer::{BufferType, Message};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekDirection {
    Previous,
    Next,
}

/// The last seek. It stays in effect only while the view has not been
/// scrolled by anything else since: `offset` is the scroll offset the seek was
/// requested at, or where the renderer placed its target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSeek {
    buffer_id: String,
    offset: usize,
    state: SeekState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SeekState {
    /// The view was scrolled since the last frame, so its edges are unknown;
    /// the renderer resolves these seeks, in order, against the frame it draws.
    Deferred { queue: VecDeque<SeekDirection> },
    /// Waiting for the chat renderer, which knows the wrapped line counts, to
    /// scroll this message into view.
    Pending { message_id: u64 },
    /// The renderer placed this message in view at `offset`.
    Shown { message_id: u64 },
}

impl HighlightSeek {
    fn valid(&self, buffer_id: &str, scroll_offset: usize) -> bool {
        self.buffer_id == buffer_id && self.offset == scroll_offset
    }

    #[cfg(test)]
    const fn anchor_message(&self) -> Option<u64> {
        match self.state {
            SeekState::Pending { message_id } | SeekState::Shown { message_id } => Some(message_id),
            SeekState::Deferred { .. } => None,
        }
    }

    /// Message the renderer should scroll to, unless the view moved since the
    /// seek was requested.
    pub fn pending_target(&self, buffer_id: &str, scroll_offset: usize) -> Option<u64> {
        match self.state {
            SeekState::Pending { message_id } if self.valid(buffer_id, scroll_offset) => {
                Some(message_id)
            }
            _ => None,
        }
    }

    /// Not applied by the renderer yet.
    pub const fn is_pending(&self) -> bool {
        !matches!(self.state, SeekState::Shown { .. })
    }

    pub const fn is_deferred(&self) -> bool {
        matches!(self.state, SeekState::Deferred { .. })
    }

    /// The renderer clamped the scroll offset a deferred seek was requested at;
    /// that is still the same view, so the seek follows the clamp.
    pub const fn follow_clamp(&mut self, requested: usize, clamped: usize) {
        if self.is_deferred() && self.offset == requested {
            self.offset = clamped;
        }
    }

    pub fn shown_at(&mut self, offset: usize) {
        if let SeekState::Pending { message_id } = self.state {
            self.state = SeekState::Shown { message_id };
            self.offset = offset;
        }
    }
}

/// Index of the nearest highlight strictly before `before` (Previous) or
/// strictly after `after` (Next).
fn nearest_highlight(
    messages: &VecDeque<Message>,
    direction: SeekDirection,
    from: Option<usize>,
) -> Option<usize> {
    match direction {
        SeekDirection::Previous => messages
            .range(..from.unwrap_or(messages.len()))
            .rposition(|message| message.highlight),
        SeekDirection::Next => {
            let start = from? + 1;
            messages
                .iter()
                .skip(start)
                .position(|message| message.highlight)
                .map(|offset| start + offset)
        }
    }
}

/// Where a sequence of seeks currently stands.
#[derive(Clone, Copy)]
enum SeekPosition {
    /// The rendered view: message indices of its top and bottom rows.
    View { top: Option<usize>, bottom: Option<usize> },
    /// A highlight a previous seek landed on.
    At(usize),
    /// The live end of the conversation.
    Live,
}

impl App {
    pub(crate) fn seek_highlight(&mut self, direction: SeekDirection) {
        self.highlight_seek_requests = self.highlight_seek_requests.wrapping_add(1);
        let Some(buffer) = self.state.active_buffer() else {
            return;
        };
        if buffer.buffer_type == BufferType::Shell {
            return;
        }
        let buffer_id = buffer.id.clone();
        let index_of = |id: u64| buffer.messages.iter().position(|message| message.id == id);
        let seek = self
            .highlight_seek
            .as_ref()
            .filter(|seek| seek.valid(&buffer_id, self.scroll_offset));
        let start = match seek.map(|seek| &seek.state) {
            Some(SeekState::Pending { message_id } | SeekState::Shown { message_id }) => {
                index_of(*message_id).map(SeekPosition::At)
            }
            Some(SeekState::Deferred { queue }) => {
                let mut queue = queue.clone();
                queue.push_back(direction);
                self.defer_seeks(buffer_id, queue);
                return;
            }
            None => self
                .chat_rows
                .as_ref()
                .filter(|rows| rows.rendered_at(&buffer_id, self.scroll_offset))
                .map(|rows| SeekPosition::View {
                    top: rows.top_message(&buffer_id).and_then(index_of),
                    bottom: rows.bottom_message(&buffer_id).and_then(index_of),
                }),
        };
        match start {
            Some(start) => {
                self.run_seeks(start, VecDeque::from([direction]));
            }
            None => self.defer_seeks(buffer_id, VecDeque::from([direction])),
        }
    }

    fn defer_seeks(&mut self, buffer_id: String, queue: VecDeque<SeekDirection>) {
        self.highlight_seek = Some(HighlightSeek {
            buffer_id,
            offset: self.scroll_offset,
            state: SeekState::Deferred { queue },
        });
    }

    /// Resolve deferred seeks against the rows the chat view just rendered.
    /// Returns whether the view must be redrawn: a target is now pending, the
    /// seeks returned to the live end, or more of them wait for the live view.
    pub(crate) fn resolve_deferred_seek(&mut self) -> bool {
        let Some(HighlightSeek {
            buffer_id,
            offset,
            state: SeekState::Deferred { queue },
        }) = self.highlight_seek.clone()
        else {
            return false;
        };
        let fresh = self
            .chat_rows
            .as_ref()
            .filter(|rows| offset == self.scroll_offset && rows.rendered_at(&buffer_id, offset));
        let (Some(rows), Some(buffer)) = (fresh, self.state.buffers.get(&buffer_id)) else {
            self.highlight_seek = None;
            return false;
        };
        let index_of = |id: u64| buffer.messages.iter().position(|message| message.id == id);
        let start = SeekPosition::View {
            top: rows.top_message(&buffer_id).and_then(index_of),
            bottom: rows.bottom_message(&buffer_id).and_then(index_of),
        };
        self.run_seeks(start, queue);
        self.scroll_offset != offset || self.highlight_seek.as_ref().is_some_and(HighlightSeek::is_pending)
    }

    /// Apply `queue` in order, each seek from where the previous one landed,
    /// exactly as separate presses with a redraw in between would.
    fn run_seeks(&mut self, start: SeekPosition, mut queue: VecDeque<SeekDirection>) {
        let Some(buffer) = self.state.active_buffer() else {
            return;
        };
        let buffer_id = buffer.id.clone();
        let mut position = start;
        while let Some(direction) = queue.front().copied() {
            let from = match (position, direction) {
                // Seeks after returning live need the live view's edges.
                (SeekPosition::Live, _) => break,
                (SeekPosition::At(index), _) => Some(index),
                (SeekPosition::View { top, .. }, SeekDirection::Previous) => top,
                (SeekPosition::View { bottom: None, .. }, SeekDirection::Next) => {
                    queue.pop_front();
                    continue;
                }
                (SeekPosition::View { bottom, .. }, SeekDirection::Next) => bottom,
            };
            queue.pop_front();
            match nearest_highlight(&buffer.messages, direction, from) {
                Some(found) => position = SeekPosition::At(found),
                None if direction == SeekDirection::Next => position = SeekPosition::Live,
                None => {}
            }
        }
        match position {
            SeekPosition::At(index) => {
                let message_id = buffer.messages[index].id;
                // Like PageUp: keep the loaded history while reading it, so
                // `enforce_scrollback` does not drop the target or later anchors.
                self.pin_active_backlog();
                self.highlight_seek = Some(HighlightSeek {
                    buffer_id,
                    offset: self.scroll_offset,
                    state: SeekState::Pending { message_id },
                });
            }
            SeekPosition::Live => {
                self.highlight_seek = None;
                self.scroll_offset = 0;
                self.collapse_backlog_if_at_bottom();
                if !queue.is_empty() {
                    self.defer_seeks(buffer_id, queue);
                }
            }
            SeekPosition::View { .. } => {
                if self.highlight_seek.as_ref().is_some_and(HighlightSeek::is_deferred) {
                    self.highlight_seek = None;
                }
            }
        }
    }

    /// Later input and binding actions decide the view, so they drop a seek
    /// that the renderer has not applied yet.
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
        app.highlight_seek.as_ref().unwrap().anchor_message().unwrap()
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

        app.scroll_offset = 0;
        render(&mut app);
        app.execute_binding(&crate::keybindings::Binding::new(
            "multi",
            "seek_previous_highlight;insert_text x",
        ));
        render(&mut app);
        assert_eq!(app.scroll_offset, 0);
        assert!(app.highlight_seek.is_none());

        app.seek_highlight(SeekDirection::Previous);
        app.handle_event(crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('y'),
            crossterm::event::KeyModifiers::NONE,
        )));
        render(&mut app);
        assert_eq!(app.scroll_offset, 0);
        assert!(app.highlight_seek.is_none());
    }

    #[test]
    fn a_bound_key_seeks_even_with_its_own_release_in_the_same_batch() {
        let mut app = app_with(&[10, 40, 70]);
        app.config
            .keyboard
            .set("meta-K", crate::keybindings::Binding::new("seek_previous_highlight", ""))
            .unwrap();
        let press = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('K'),
            crossterm::event::KeyModifiers::ALT | crossterm::event::KeyModifiers::SHIFT,
        );
        let release = crossterm::event::KeyEvent {
            kind: crossterm::event::KeyEventKind::Release,
            ..press
        };
        app.handle_event(crossterm::event::Event::Key(press));
        app.handle_event(crossterm::event::Event::Key(release));
        render(&mut app);
        assert_eq!(target(&app), 70);
        let (top, bottom) = visible(&app);
        assert!((top..=bottom).contains(&70), "{top}..{bottom}");

        // Repeated presses in one event batch, before any redraw, keep stepping.
        for _ in 0..2 {
            app.handle_event(crossterm::event::Event::Key(press));
            app.handle_event(crossterm::event::Event::Key(release));
        }
        render(&mut app);
        assert_eq!(target(&app), 10);

        // The same with Escape arriving as its own event before each key.
        app.scroll_offset = 0;
        app.highlight_seek = None;
        render(&mut app);
        let escape = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        );
        let shifted_k = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('K'),
            crossterm::event::KeyModifiers::SHIFT,
        );
        for _ in 0..2 {
            app.handle_event(crossterm::event::Event::Key(escape));
            app.handle_event(crossterm::event::Event::Key(shifted_k));
        }
        render(&mut app);
        assert_eq!(target(&app), 40);
    }

    #[test]
    fn a_scroll_earlier_in_the_same_batch_is_the_reference_for_the_seek() {
        let mut app = app_with(&[10, 40, 70]);
        app.execute_binding(&crate::keybindings::Binding::new(
            "multi",
            "scroll_start;seek_previous_highlight",
        ));
        render(&mut app);
        assert_eq!(visible(&app).0, 1);
        assert!(app.highlight_seek.is_none());

        app.scroll_offset = 0;
        render(&mut app);
        app.execute_binding(&crate::keybindings::Binding::new(
            "multi",
            "scroll_start;seek_next_highlight;seek_next_highlight",
        ));
        render(&mut app);
        assert_eq!(target(&app), 40);
        let (top, bottom) = visible(&app);
        assert!((top..=bottom).contains(&40), "{top}..{bottom}");
    }

    #[test]
    fn queued_seeks_in_both_directions_apply_in_order() {
        let mut app = app_with(&[40]);
        app.execute_binding(&crate::keybindings::Binding::new(
            "multi",
            "scroll_start;seek_next_highlight;seek_previous_highlight",
        ));
        render(&mut app);
        assert_eq!(target(&app), 40);
        let (top, bottom) = visible(&app);
        assert!((top..=bottom).contains(&40), "{top}..{bottom}");

        let mut app = app_with(&[40, 95]);
        app.execute_binding(&crate::keybindings::Binding::new(
            "multi",
            "scroll_start;seek_next_highlight;seek_next_highlight;seek_next_highlight;seek_previous_highlight",
        ));
        render(&mut app);
        assert_eq!(target(&app), 40);
    }

    #[test]
    fn queued_seeks_past_the_newest_highlight_return_to_the_live_end() {
        let mut app = app_with(&[40]);
        app.execute_binding(&crate::keybindings::Binding::new(
            "multi",
            "scroll_start;seek_next_highlight;seek_next_highlight",
        ));
        render(&mut app);
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(visible(&app).1, 100);
        assert!(app.highlight_seek.is_none());

        let mut app = app_with(&[]);
        app.execute_binding(&crate::keybindings::Binding::new(
            "multi",
            "scroll_start;seek_next_highlight",
        ));
        render(&mut app);
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(visible(&app).1, 100);
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

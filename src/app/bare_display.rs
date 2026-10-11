//! Bare display, as in weechat (`meta-l`): the active buffer alone, printed as
//! plain text with mouse reporting off, so the terminal can select text and
//! recognise long URLs that wrap across rows. Scroll keys page through the
//! buffer; a change to the input or to the active buffer returns to the normal
//! layout.

use std::collections::HashMap;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::AppConfig;
use crate::state::buffer::{Buffer, BufferType};
use crate::theme::ThemeFile;
use crate::ui::bare_view::{self, BareMessage};

use super::App;
use super::highlight_seek::SeekDirection;

pub struct BareDisplay {
    buffer_id: String,
    /// Message on the top row while scrolled; `None` follows the end of the
    /// buffer.
    start: Option<u64>,
    /// Return to the normal layout at this time (`/bare <seconds>`).
    until: Option<Instant>,
    /// The input line when the bare display was first drawn; changing it
    /// leaves. Taken at the first frame rather than on entry, so the command
    /// line that entered (`/bare`) being cleared does not count.
    input: Option<String>,
    /// Output last written. An unchanged screen is not written again, which
    /// would clear the user's selection.
    shown: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BareScroll {
    LineUp,
    LineDown,
    PageUp,
    PageDown,
    Top,
    Bottom,
    Highlight(SeekDirection),
}

/// Shown messages as `(index, row)`, with the prepared text of every message
/// the layout measured.
struct BareView {
    placed: Vec<(usize, usize)>,
    messages: HashMap<usize, BareMessage>,
}

impl BareView {
    fn new(
        buffer: &Buffer,
        start: Option<usize>,
        (cols, rows): (usize, usize),
        theme: &ThemeFile,
        config: &AppConfig,
    ) -> Self {
        let mut messages = HashMap::new();
        let placed = bare_view::layout(buffer.messages.len(), start, rows, |index| {
            messages
                .entry(index)
                .or_insert_with(|| bare_message(buffer, index, theme, config))
                .rows(cols)
        });
        Self { placed, messages }
    }

    fn output(&self, (cols, rows): (usize, usize)) -> Vec<u8> {
        bare_view::render(
            self.placed
                .iter()
                .map(|(index, row)| (*row, &self.messages[index])),
            cols,
            rows,
        )
    }
}

fn bare_message(buffer: &Buffer, index: usize, theme: &ThemeFile, config: &AppConfig) -> BareMessage {
    BareMessage::new(&crate::ui::message_line::bare_text(
        &buffer.messages[index],
        theme,
        config,
    ))
}

/// Index of message `id` in `buffer`; `None` once it is gone, which makes the
/// view follow the end again.
fn message_index(buffer: &Buffer, id: Option<u64>) -> Option<usize> {
    let id = id?;
    buffer.messages.iter().position(|message| message.id == id)
}

impl App {
    /// Enter the bare display, or leave it when it is on. `delay` returns to
    /// the normal layout after that long.
    pub(crate) fn toggle_bare_display(&mut self, delay: Option<Duration>) -> Result<(), &'static str> {
        if self.bare_display.is_some() {
            self.exit_bare_display();
            return Ok(());
        }
        if self.terminal.is_none() {
            return Err("Bare display needs an attached terminal");
        }
        if self.settings_panel.is_some() || self.wizard.is_some() || self.emote_picker.is_open() {
            return Err("Close the open dialog before switching to bare display");
        }
        let Some(buffer) = self.state.active_buffer() else {
            return Err("No active window");
        };
        if buffer.buffer_type == BufferType::Shell {
            return Err("Bare display is not available in shell windows");
        }
        let until = match delay {
            Some(delay) => Some(Instant::now().checked_add(delay).ok_or("Delay is too long")?),
            None => None,
        };
        let buffer_id = buffer.id.clone();
        // A scrolled view keeps its top message; the live view keeps following.
        let start = if self.scroll_offset == 0 {
            None
        } else {
            self.chat_rows
                .as_ref()
                .and_then(|rows| rows.top_message(&buffer_id))
        };
        self.cancel_pending_highlight_seek();
        if !matches!(self.image_preview, crate::image_preview::PreviewStatus::Hidden) {
            self.dismiss_image_preview();
        }
        self.bare_display = Some(BareDisplay {
            buffer_id,
            start,
            until,
            input: None,
            shown: None,
        });
        Ok(())
    }

    /// Return to the normal layout.
    pub(crate) fn exit_bare_display(&mut self) {
        if self.bare_display.take().is_none() {
            return;
        }
        if let Some(terminal) = self.terminal.as_mut()
            && let Err(error) = crossterm::execute!(
                terminal.backend_mut(),
                crossterm::event::EnableMouseCapture
            )
        {
            tracing::debug!("re-enabling mouse capture after bare display failed: {error}");
        }
        self.needs_full_redraw = true;
    }

    /// Write the bare display again even if it has not changed.
    pub(crate) fn redraw_bare_display(&mut self) {
        if let Some(bare) = self.bare_display.as_mut() {
            bare.shown = None;
        }
    }

    pub(crate) fn bare_display_deadline(&self) -> Option<Instant> {
        self.bare_display.as_ref()?.until
    }

    pub(crate) fn tick_bare_display(&mut self) {
        if self
            .bare_display_deadline()
            .is_some_and(|until| until <= Instant::now())
        {
            self.exit_bare_display();
        }
    }

    const fn bare_size(&self) -> (usize, usize) {
        (self.cached_term_cols as usize, self.cached_term_rows as usize)
    }

    /// Scroll keys page through the bare display. Returns whether `key` was
    /// one; Up and Down scroll too, which also makes the mouse wheel work
    /// where the terminal turns it into arrow keys.
    pub(super) fn bare_display_key(&mut self, key: KeyEvent) -> bool {
        if self.bare_display.is_none() || !(key.modifiers - KeyModifiers::SHIFT).is_empty() {
            return false;
        }
        let scroll = match key.code {
            KeyCode::Up => BareScroll::LineUp,
            KeyCode::Down => BareScroll::LineDown,
            KeyCode::PageUp => BareScroll::PageUp,
            KeyCode::PageDown => BareScroll::PageDown,
            KeyCode::Home => BareScroll::Top,
            KeyCode::End => BareScroll::Bottom,
            _ => return false,
        };
        self.scroll_bare_display(scroll);
        true
    }

    pub(crate) fn scroll_bare_display(&mut self, scroll: BareScroll) {
        let (cols, rows) = self.bare_size();
        let Some(bare) = self.bare_display.as_ref() else {
            return;
        };
        let Some(buffer) = self.state.buffers.get(&bare.buffer_id) else {
            return;
        };
        let count = buffer.messages.len();
        if count == 0 || rows == 0 {
            return;
        }
        let view = BareView::new(
            buffer,
            message_index(buffer, bare.start),
            (cols, rows),
            &self.theme,
            &self.config,
        );
        let top = view.placed.first().map_or(0, |(index, _)| *index);
        let bottom = view.placed.last().map_or(0, |(index, _)| *index);
        let height = |index| bare_message(buffer, index, &self.theme, &self.config).rows(cols);
        let start = match scroll {
            BareScroll::LineUp => Some(top.saturating_sub(1)),
            BareScroll::LineDown => Some(top + 1),
            BareScroll::PageUp => {
                // The messages above that fill one screen, at least one.
                let mut start = top;
                let mut used = 0;
                while start > 0 {
                    used += height(start - 1);
                    if used > rows && start < top {
                        break;
                    }
                    start -= 1;
                }
                Some(start)
            }
            BareScroll::PageDown => Some(bottom + 1),
            BareScroll::Top => Some(0),
            BareScroll::Bottom => None,
            BareScroll::Highlight(SeekDirection::Previous) => {
                match buffer.messages.range(..top).rposition(|message| message.highlight) {
                    Some(index) => Some(index),
                    None => return,
                }
            }
            // Past the newest highlight the view returns to the live end, as
            // highlight seeking does in the normal layout.
            BareScroll::Highlight(SeekDirection::Next) => buffer
                .messages
                .iter()
                .skip(top + 1)
                .position(|message| message.highlight)
                .map(|offset| top + 1 + offset),
        };
        // A scrolled view that reaches the last message follows the end again.
        let start = start
            .filter(|&start| {
                let mut used = 0;
                start < count
                    && (start..count).any(|index| {
                        used += height(index);
                        used > rows
                    })
            })
            .map(|start| buffer.messages[start].id);
        if let Some(bare) = self.bare_display.as_mut() {
            bare.start = start;
        }
    }

    /// Draw the bare display in place of the normal layout. `None` when it
    /// has just ended and the normal layout should be drawn; otherwise whether
    /// the frame went out (`false`: earlier output is still on its way).
    pub(super) fn render_bare_frame(&mut self) -> Option<bool> {
        let bare = self.bare_display.as_mut()?;
        let input = bare.input.get_or_insert_with(|| self.input.value.clone());
        if *input != self.input.value
            || self.state.active_buffer_id.as_deref() != Some(bare.buffer_id.as_str())
            || !self.state.buffers.contains_key(&bare.buffer_id)
        {
            self.exit_bare_display();
            return None;
        }
        let size = self.bare_size();
        let output = {
            let bare = self.bare_display.as_ref()?;
            let buffer = self.state.buffers.get(&bare.buffer_id)?;
            BareView::new(
                buffer,
                message_index(buffer, bare.start),
                size,
                &self.theme,
                &self.config,
            )
            .output(size)
        };
        let first = self.bare_display.as_ref()?.shown.is_none();
        let at_end = self.bare_display.as_ref()?.start.is_none();
        if self.bare_display.as_ref()?.shown.as_ref() == Some(&output) {
            self.mark_terminal_read_at_end(at_end);
            return Some(true);
        }
        let socket = self.socket_output.clone();
        if socket.as_ref().is_some_and(|socket| !socket.begin_frame()) {
            return Some(false);
        }
        let mut terminal = self.terminal.take()?;
        if first {
            // Inline images and emotes would stay over the text.
            self.inline_previews.clear_graphics(
                terminal.backend_mut(),
                self.picker.protocol_type(),
                self.in_tmux,
            );
            self.emote_animator.clear();
        }
        let result = (|| -> io::Result<()> {
            crossterm::queue!(terminal.backend_mut(), crossterm::event::DisableMouseCapture)?;
            terminal.hide_cursor()?;
            terminal.backend_mut().write_all(&output)?;
            terminal.backend_mut().flush()?;
            if let Some(socket) = &socket {
                socket.finish_frame()?;
            }
            Ok(())
        })();
        self.terminal = Some(terminal);
        match result {
            Ok(()) => {
                if let Some(bare) = self.bare_display.as_mut() {
                    bare.shown = Some(output);
                }
                self.mark_terminal_read_at_end(at_end);
            }
            Err(error) => {
                if let Some(socket) = &socket {
                    socket.cancel_frame();
                }
                if socket.is_some() && error.kind() == io::ErrorKind::WouldBlock {
                    tracing::warn!("bare display exceeded the output limit: {error}");
                } else {
                    tracing::warn!("bare display write failed, triggering detach: {error}");
                    self.should_detach = true;
                }
            }
        }
        Some(true)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crossterm::event::Event;
    use tokio::sync::mpsc;

    use super::*;
    use crate::session::protocol::MainMessage;
    use crate::session::writer::{MAX_SOCKET_OUTPUT_QUEUE_BYTES, SocketWriter};
    use crate::state::buffer::{Message, MessageType};

    struct Harness {
        app: App,
        rx: mpsc::UnboundedReceiver<MainMessage>,
        queued: Arc<AtomicUsize>,
    }

    impl Harness {
        fn new(messages: u64) -> Self {
            let mut app = crate::app::input::submit_typing_tests::test_app();
            let (tx, rx) = mpsc::unbounded_channel();
            let queued = Arc::new(AtomicUsize::new(0));
            let writer = SocketWriter::new(tx, Arc::clone(&queued), MAX_SOCKET_OUTPUT_QUEUE_BYTES);
            app.socket_output = Some(writer.output());
            app.terminal = Some(crate::ui::setup_socket_terminal(Box::new(writer), 40, 6).unwrap());
            app.is_socket_attached = true;
            app.cached_term_cols = 40;
            app.cached_term_rows = 6;
            let mut buffer = Buffer::for_test("net", BufferType::Channel, "#chan");
            for id in 1..=messages {
                buffer.messages.push_back(message(id, &format!("line {id}")));
            }
            app.state.add_buffer(buffer);
            app.state.set_active_buffer("net/#chan");
            let mut harness = Self { app, rx, queued };
            harness.frame();
            harness
        }

        /// Deliver queued output, render once, and return what reached the
        /// terminal.
        fn frame(&mut self) -> String {
            let mut out = self.drain();
            self.app.render_terminal_frame();
            out.extend(self.drain());
            String::from_utf8_lossy(&out).into_owned()
        }

        fn drain(&mut self) -> Vec<u8> {
            let mut out = Vec::new();
            while let Ok(message) = self.rx.try_recv() {
                if let MainMessage::Output(bytes) = message {
                    self.queued.fetch_sub(bytes.len(), Ordering::AcqRel);
                    out.extend(bytes);
                }
            }
            out
        }

        fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
            self.app.handle_event(Event::Key(KeyEvent::new(code, modifiers)));
        }

        fn start(&self) -> Option<u64> {
            self.app.bare_display.as_ref().unwrap().start
        }
    }

    fn message(id: u64, text: &str) -> Message {
        Message {
            redaction_ref: None,
            redaction_msgid: None,
            log_key: None,
            id,
            timestamp: chrono::Utc::now(),
            message_type: MessageType::Message,
            nick: Some("peer".into()),
            nick_mode: Some("@".into()),
            text: text.into(),
            highlight: id == 5,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
            wire_origin: None,
            translation_suffix_at: None,
        }
    }

    const MOUSE_OFF: &str = "\x1b[?1000l";
    const MOUSE_ON: &str = "\x1b[?1000h";

    #[tokio::test]
    async fn prints_the_buffer_as_plain_text_with_the_mouse_released() {
        let mut harness = Harness::new(20);
        harness.key(KeyCode::Char('l'), KeyModifiers::ALT);
        assert!(harness.app.bare_display.is_some());
        let out = harness.frame();
        assert!(out.contains(MOUSE_OFF));
        assert!(out.contains("<@peer> line 20"));
        assert!(out.contains("<@peer> line 15"));
        assert!(!out.contains("line 14"));
        // Nothing changed: nothing is written, so a selection survives.
        assert_eq!(harness.frame(), "");

        harness.key(KeyCode::Char('l'), KeyModifiers::ALT);
        assert!(harness.app.bare_display.is_none());
        let out = harness.frame();
        assert!(out.contains(MOUSE_ON));
        assert!(!out.contains("<@peer>"));
    }

    #[tokio::test]
    async fn a_long_message_is_not_broken_by_the_client() {
        let mut harness = Harness::new(3);
        let url = format!("https://example.org/{}", "x".repeat(100));
        let id = harness.app.state.next_message_id();
        harness
            .app
            .state
            .buffers
            .get_mut("net/#chan")
            .unwrap()
            .messages
            .push_back(message(id, &url));
        harness.app.toggle_bare_display(None).unwrap();
        assert!(harness.frame().contains(&url));
    }

    #[tokio::test]
    async fn scroll_keys_page_through_and_return_to_the_live_end() {
        let mut harness = Harness::new(20);
        harness.app.toggle_bare_display(None).unwrap();
        harness.frame();
        harness.key(KeyCode::PageUp, KeyModifiers::NONE);
        assert_eq!(harness.start(), Some(9));
        assert!(harness.frame().contains("line 9"));
        harness.key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(harness.start(), Some(8));
        harness.key(KeyCode::Home, KeyModifiers::NONE);
        assert_eq!(harness.start(), Some(1));
        harness.key(KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(harness.start(), Some(7));
        harness.key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(harness.start(), Some(8));
        harness.key(KeyCode::End, KeyModifiers::NONE);
        assert_eq!(harness.start(), None);
        harness.key(KeyCode::Up, KeyModifiers::NONE);
        harness.key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(harness.start(), None);
        assert!(harness.app.input.value.is_empty());
        assert!(harness.app.bare_display.is_some());
    }

    #[tokio::test]
    async fn highlight_seeking_jumps_to_highlighted_messages() {
        let mut harness = Harness::new(20);
        harness.app.toggle_bare_display(None).unwrap();
        harness.app.execute_binding(&crate::keybindings::Binding::new("seek_previous_highlight", ""));
        assert_eq!(harness.start(), Some(5));
        harness.app.execute_binding(&crate::keybindings::Binding::new("seek_next_highlight", ""));
        assert_eq!(harness.start(), None);
    }

    #[tokio::test]
    async fn typing_leaves_and_keeps_the_typed_text() {
        let mut harness = Harness::new(20);
        harness.app.toggle_bare_display(None).unwrap();
        harness.frame();
        harness.key(KeyCode::Char('a'), KeyModifiers::NONE);
        let out = harness.frame();
        assert!(harness.app.bare_display.is_none());
        assert!(out.contains(MOUSE_ON));
        assert_eq!(harness.app.input.value, "a");
    }

    #[tokio::test]
    async fn the_bare_command_enters_and_its_cleared_input_does_not_leave() {
        let mut harness = Harness::new(20);
        for ch in "/bare".chars() {
            harness.key(KeyCode::Char(ch), KeyModifiers::NONE);
        }
        harness.key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(harness.frame().contains("<@peer> line 20"));
        assert!(harness.app.bare_display.is_some());
    }

    #[tokio::test]
    async fn switching_windows_elsewhere_leaves() {
        let mut harness = Harness::new(20);
        harness
            .app
            .state
            .add_buffer(Buffer::for_test("net", BufferType::Channel, "#other"));
        harness.app.toggle_bare_display(None).unwrap();
        harness.frame();
        harness.app.state.set_active_buffer("net/#other");
        harness.frame();
        assert!(harness.app.bare_display.is_none());
    }

    #[tokio::test]
    async fn new_messages_redraw_the_live_view_but_not_a_scrolled_one() {
        let mut harness = Harness::new(20);
        harness.app.toggle_bare_display(None).unwrap();
        harness.frame();
        let add = |harness: &mut Harness, text: &str| {
            let id = harness.app.state.next_message_id();
            harness
                .app
                .state
                .buffers
                .get_mut("net/#chan")
                .unwrap()
                .messages
                .push_back(message(id, text));
        };
        add(&mut harness, "fresh");
        assert!(harness.frame().contains("<@peer> fresh"));
        harness.key(KeyCode::PageUp, KeyModifiers::NONE);
        harness.frame();
        add(&mut harness, "later");
        assert_eq!(harness.frame(), "");
    }

    #[tokio::test]
    async fn a_delay_returns_to_the_normal_layout() {
        let mut harness = Harness::new(20);
        harness.app.toggle_bare_display(Some(Duration::ZERO)).unwrap();
        assert!(harness.app.bare_display_deadline().is_some());
        harness.app.tick_bare_display();
        assert!(harness.app.bare_display.is_none());
    }

    #[tokio::test]
    async fn a_resize_writes_the_unchanged_screen_again() {
        let mut harness = Harness::new(20);
        harness.app.toggle_bare_display(None).unwrap();
        harness.frame();
        assert_eq!(harness.frame(), "");
        harness.app.handle_event(Event::Resize(40, 6));
        harness.drain();
        assert!(harness.frame().contains("<@peer> line 20"));
    }

    #[tokio::test]
    async fn an_unrepresentable_delay_is_refused() {
        let mut harness = Harness::new(1);
        assert!(harness.app.toggle_bare_display(Some(Duration::MAX)).is_err());
        assert!(harness.app.bare_display.is_none());
    }

    #[tokio::test]
    async fn shell_windows_and_detached_sessions_are_refused() {
        let mut harness = Harness::new(1);
        harness
            .app
            .state
            .add_buffer(Buffer::for_test("net", BufferType::Shell, "sh"));
        harness.app.state.set_active_buffer("net/sh");
        assert!(harness.app.toggle_bare_display(None).is_err());
        harness.app.state.set_active_buffer("net/#chan");
        harness.app.terminal = None;
        assert!(harness.app.toggle_bare_display(None).is_err());
    }
}

use std::collections::VecDeque;
use std::io::{self, Write};

use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use unicode_width::UnicodeWidthStr;

pub struct GlyphBackend<W: Write> {
    inner: CrosstermBackend<W>,
    /// The shim's screen when this backend draws for a socket-attached view.
    /// The daemon has no terminal of its own, so size and cursor queries are
    /// answered from here instead of asking a tty that does not exist.
    remote: Option<RemoteScreen>,
}

#[derive(Clone, Copy)]
struct RemoteScreen {
    size: Size,
    cursor: Position,
}

impl<W: Write> GlyphBackend<W> {
    pub const fn new(writer: W) -> Self {
        Self {
            inner: CrosstermBackend::new(writer),
            remote: None,
        }
    }

    /// A backend drawing for a terminal on the other end of a socket.
    pub const fn remote(writer: W, size: Size) -> Self {
        Self {
            inner: CrosstermBackend::new(writer),
            remote: Some(RemoteScreen { size, cursor: Position::ORIGIN }),
        }
    }

    /// Record the shim's new screen size after it resized.
    pub const fn set_remote_size(&mut self, size: Size) {
        if let Some(remote) = &mut self.remote {
            remote.size = size;
        }
    }
}

impl<W: Write> Backend for GlyphBackend<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let mut content = content.peekable();
        let mut pending = VecDeque::new();
        self.inner.draw(std::iter::from_fn(|| {
            if let Some(update) = pending.pop_front() {
                return Some(update);
            }
            let current @ (x, y, cell) = content.next()?;
            let symbol = cell.symbol();
            if !symbol.contains('\u{fe0f}') || symbol.contains('\x1b') {
                return Some(current);
            }
            let width = u16::try_from(symbol.width()).unwrap_or(1);
            let end = x.saturating_add(width);
            while content
                .peek()
                .is_some_and(|(next_x, next_y, _)| *next_y == y && *next_x > x && *next_x < end)
            {
                pending.push_back(content.next().unwrap());
            }
            pending.push_back(current);
            pending.pop_front()
        }))
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.inner.append_lines(n)
    }
    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> io::Result<Position> {
        match self.remote {
            Some(remote) => Ok(remote.cursor),
            None => self.inner.get_cursor_position(),
        }
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let position = position.into();
        if let Some(remote) = &mut self.remote {
            remote.cursor = position;
        }
        self.inner.set_cursor_position(position)
    }
    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }
    fn size(&self) -> io::Result<Size> {
        self.remote.map_or_else(|| self.inner.size(), |remote| Ok(remote.size))
    }
    fn window_size(&mut self) -> io::Result<WindowSize> {
        match self.remote {
            Some(remote) => Ok(WindowSize { columns_rows: remote.size, pixels: Size::default() }),
            None => self.inner.window_size(),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }
}

impl<W: Write> Write for GlyphBackend<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.inner.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Write::flush(&mut self.inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ratatui::{Terminal, TerminalOptions, Viewport, layout::Rect, widgets::Paragraph};

    use super::*;

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn scrolling_past_vs16_emoji_clears_the_distant_character() {
        for glyph in ["❤️", "☀️", "✈️", "😀", "漢", "👨‍👩‍👧‍👦", "👍🏽", "🇵🇱"]
        {
            let output = Capture(Arc::new(Mutex::new(Vec::new())));
            let mut terminal = Terminal::with_options(
                GlyphBackend::new(output.clone()),
                TerminalOptions {
                    viewport: Viewport::Fixed(Rect::new(0, 0, 40, 4)),
                },
            )
            .unwrap();
            let mut screen = vt100::Parser::new(4, 40, 0);
            let wide_line = format!("{glyph}                  Z");
            for text in ["abcdefghijklmnopqrstuvwx", wide_line.as_str(), "x"] {
                terminal
                    .draw(|frame| frame.render_widget(Paragraph::new(text), frame.area()))
                    .unwrap();
                let bytes = std::mem::take(&mut *output.0.lock().unwrap());
                let emitted = String::from_utf8(bytes).unwrap();
                let normalized = emitted.replace(glyph, "😀");
                screen.process(normalized.as_bytes());
            }
            assert_eq!(screen.screen().contents().trim_end(), "x", "{glyph}");
        }
    }

    #[test]
    fn trailing_cell_is_cleared_before_the_wide_glyph_is_printed() {
        let old = ratatui::buffer::Buffer::with_lines(["abc"]);
        let new = ratatui::buffer::Buffer::with_lines(["❤️Z"]);
        let output = Capture(Arc::new(Mutex::new(Vec::new())));
        let mut backend = GlyphBackend::new(output.clone());
        backend.draw(old.diff(&new).into_iter()).unwrap();
        let emitted = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        assert!(
            emitted.starts_with("\x1b[1;2H \x1b[1;1H❤️\x1b[1;3HZ"),
            "{emitted:?}"
        );
    }

    #[test]
    fn removing_a_wide_glyph_still_clears_both_columns() {
        let old = ratatui::buffer::Buffer::with_lines(["❤️Z"]);
        let new = ratatui::buffer::Buffer::with_lines(["x  "]);
        let output = Capture(Arc::new(Mutex::new(Vec::new())));
        let mut backend = GlyphBackend::new(output.clone());
        backend.draw(old.diff(&new).into_iter()).unwrap();
        let bytes = output.0.lock().unwrap().clone();
        let mut screen = vt100::Parser::new(1, 3, 0);
        screen.process("😀Z".as_bytes());
        screen.process(&bytes);
        assert_eq!(screen.screen().contents().trim_end(), "x");
    }

    /// Since ratatui-core 0.1.1 a cell's width comes from its symbol. An
    /// image protocol packs a whole escape sequence into one cell, so unless
    /// that cell carries a forced width the diff skips as many cells as the
    /// escape is "wide": the text beside and below the image never reaches
    /// the terminal and inline images render blank. This is why ratatui-core
    /// was pinned to 0.1.0 while ratatui-image 10 was in use.
    #[test]
    #[expect(deprecated, reason = "fixed font metrics make the protocol output deterministic")]
    fn image_cells_do_not_swallow_the_rest_of_the_frame() {
        use ratatui_image::{StatefulImage, picker::{Picker, ProtocolType}};

        for (protocol, marker) in [
            (ProtocolType::Kitty, "\x1b_G"),
            (ProtocolType::Iterm2, "\x1b]1337;"),
            (ProtocolType::Sixel, "\x1bP"),
        ] {
            let output = Capture(Arc::new(Mutex::new(Vec::new())));
            let mut terminal = Terminal::with_options(
                GlyphBackend::new(output.clone()),
                TerminalOptions {
                    viewport: Viewport::Fixed(Rect::new(0, 0, 40, 6)),
                },
            )
            .unwrap();
            let mut picker = Picker::from_fontsize((8, 16).into());
            picker.set_protocol_type(protocol);
            let mut image = picker.new_resize_protocol(image::DynamicImage::new_rgb8(32, 32));
            terminal
                .draw(|frame| {
                    frame.render_stateful_widget(StatefulImage::default(), Rect::new(0, 0, 4, 2), &mut image);
                    frame.render_widget(Paragraph::new("beside"), Rect::new(10, 0, 20, 1));
                    frame.render_widget(Paragraph::new("below the image"), Rect::new(0, 4, 40, 1));
                })
                .unwrap();
            let bytes = output.0.lock().unwrap().clone();
            assert!(
                String::from_utf8_lossy(&bytes).contains(marker),
                "{protocol:?}: image escape missing"
            );
            let mut screen = vt100::Parser::new(6, 40, 0);
            screen.process(&bytes);
            let text_at = |row: u16, col: u16, len: u16| -> String {
                (col..col + len)
                    .filter_map(|c| screen.screen().cell(row, c))
                    .map(|cell| if cell.has_contents() { cell.contents().to_string() } else { " ".into() })
                    .collect()
            };
            assert_eq!(text_at(0, 10, 6), "beside", "{protocol:?}");
            assert_eq!(text_at(4, 0, 15), "below the image", "{protocol:?}");
        }
    }

    /// The daemon behind a socket-attached view has no terminal of its own.
    /// Since ratatui-core 0.1.2, `Terminal::clear` and `Terminal::resize`
    /// ask the backend for the cursor position and screen size even with a
    /// fixed viewport; answered by the daemon's (missing) tty, they failed and
    /// every full redraw detached the session.
    #[test]
    fn socket_terminal_clears_and_resizes_from_the_shim_size() {
        let output = Capture(Arc::new(Mutex::new(Vec::new())));
        let mut terminal = crate::ui::setup_socket_terminal(Box::new(output), 50, 10).unwrap();
        assert_eq!(terminal.backend().size().unwrap(), Size::new(50, 10));
        terminal.backend_mut().set_cursor_position(Position::new(3, 4)).unwrap();
        assert_eq!(terminal.backend_mut().get_cursor_position().unwrap(), Position::new(3, 4));
        terminal.clear().unwrap();

        crate::ui::resize_terminal(&mut terminal, 60, 12).unwrap();
        assert_eq!(terminal.backend().size().unwrap(), Size::new(60, 12));
        assert_eq!(terminal.size().unwrap(), Size::new(60, 12));
        terminal.clear().unwrap();
        terminal.draw(|frame| frame.render_widget(Paragraph::new("redrawn"), frame.area())).unwrap();
    }
}

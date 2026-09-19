use std::collections::VecDeque;
use std::io::{self, Write};

use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use unicode_width::UnicodeWidthStr;

pub struct GlyphBackend<W: Write> {
    inner: CrosstermBackend<W>,
}

impl<W: Write> GlyphBackend<W> {
    pub const fn new(writer: W) -> Self {
        Self {
            inner: CrosstermBackend::new(writer),
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
        self.inner.get_cursor_position()
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }
    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }
    fn size(&self) -> io::Result<Size> {
        self.inner.size()
    }
    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
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
}

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme::hex_to_color;

pub struct InputState {
    pub value: String,
    pub cursor_pos: usize,
}

impl InputState {
    pub fn new() -> Self {
        InputState {
            value: String::new(),
            cursor_pos: 0,
        }
    }

    pub fn insert_char(&mut self, c: char) {
        self.value.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
    }

    pub fn backspace(&mut self) {
        if self.cursor_pos > 0 {
            // Find previous char boundary
            let prev = self.value[..self.cursor_pos]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.value.drain(prev..self.cursor_pos);
            self.cursor_pos = prev;
        }
    }

    pub fn delete(&mut self) {
        if self.cursor_pos < self.value.len() {
            let next = self.value[self.cursor_pos..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor_pos + i)
                .unwrap_or(self.value.len());
            self.value.drain(self.cursor_pos..next);
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos = self.value[..self.cursor_pos]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor_pos < self.value.len() {
            self.cursor_pos = self.value[self.cursor_pos..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor_pos + i)
                .unwrap_or(self.value.len());
        }
    }

    pub fn home(&mut self) {
        self.cursor_pos = 0;
    }

    pub fn end(&mut self) {
        self.cursor_pos = self.value.len();
    }

    pub fn clear(&mut self) -> String {
        self.cursor_pos = 0;
        std::mem::take(&mut self.value)
    }
}

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let colors = &app.theme.colors;
    let fg_muted = hex_to_color(&colors.fg_muted).unwrap_or(Color::DarkGray);
    let fg = hex_to_color(&colors.fg).unwrap_or(Color::White);

    let server_label = app
        .state
        .connections
        .values()
        .next()
        .map(|c| c.label.as_str())
        .unwrap_or("");
    let channel_name = app
        .state
        .active_buffer()
        .map(|b| b.name.as_str())
        .unwrap_or("");
    let nick = app
        .state
        .connections
        .values()
        .next()
        .map(|c| c.nick.as_str())
        .unwrap_or("");

    let prompt = app
        .config
        .statusbar
        .prompt
        .replace("$server", server_label)
        .replace("$channel", channel_name)
        .replace("$nick", nick);

    let (before_cursor, after_cursor) = app.input.value.split_at(app.input.cursor_pos);
    let cursor_char = after_cursor.chars().next().unwrap_or(' ');
    let after_cursor_rest = if after_cursor.len() > cursor_char.len_utf8() {
        &after_cursor[cursor_char.len_utf8()..]
    } else {
        ""
    };

    let cursor_color = hex_to_color(&colors.cursor).unwrap_or(Color::White);

    let line = Line::from(vec![
        Span::styled(prompt, Style::default().fg(fg_muted)),
        Span::styled(before_cursor.to_string(), Style::default().fg(fg)),
        Span::styled(
            cursor_char.to_string(),
            Style::default().fg(Color::Black).bg(cursor_color),
        ),
        Span::styled(after_cursor_rest.to_string(), Style::default().fg(fg)),
    ]);

    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_backspace() {
        let mut input = InputState::new();
        input.insert_char('h');
        input.insert_char('i');
        assert_eq!(input.value, "hi");
        assert_eq!(input.cursor_pos, 2);
        input.backspace();
        assert_eq!(input.value, "h");
        assert_eq!(input.cursor_pos, 1);
    }

    #[test]
    fn move_cursor() {
        let mut input = InputState::new();
        input.insert_char('a');
        input.insert_char('b');
        input.insert_char('c');
        input.move_left();
        assert_eq!(input.cursor_pos, 2);
        input.move_left();
        assert_eq!(input.cursor_pos, 1);
        input.insert_char('X');
        assert_eq!(input.value, "aXbc");
    }

    #[test]
    fn home_and_end() {
        let mut input = InputState::new();
        input.insert_char('a');
        input.insert_char('b');
        input.home();
        assert_eq!(input.cursor_pos, 0);
        input.end();
        assert_eq!(input.cursor_pos, 2);
    }

    #[test]
    fn delete_at_cursor() {
        let mut input = InputState::new();
        input.insert_char('a');
        input.insert_char('b');
        input.insert_char('c');
        input.home();
        input.delete();
        assert_eq!(input.value, "bc");
    }

    #[test]
    fn clear_returns_value() {
        let mut input = InputState::new();
        input.insert_char('t');
        input.insert_char('e');
        input.insert_char('s');
        input.insert_char('t');
        let val = input.clear();
        assert_eq!(val, "test");
        assert_eq!(input.value, "");
        assert_eq!(input.cursor_pos, 0);
    }
}

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme::hex_to_color;

const MAX_HISTORY: usize = 100;

pub struct TabCompletionState {
    pub prefix: String,
    pub matches: Vec<String>,
    pub index: usize,
    pub text_before: String,
    pub is_start_of_line: bool,
    pub is_command: bool,
}

pub struct InputState {
    pub value: String,
    pub cursor_pos: usize,
    pub tab_state: Option<TabCompletionState>,
    pub history: Vec<String>,
    pub history_index: Option<usize>,
    pub saved_input: Option<String>,
}

impl InputState {
    pub fn new() -> Self {
        InputState {
            value: String::new(),
            cursor_pos: 0,
            tab_state: None,
            history: Vec::new(),
            history_index: None,
            saved_input: None,
        }
    }

    pub fn insert_char(&mut self, c: char) {
        self.value.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
        self.tab_state = None;
    }

    pub fn backspace(&mut self) {
        if self.cursor_pos > 0 {
            let prev = self.value[..self.cursor_pos]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.value.drain(prev..self.cursor_pos);
            self.cursor_pos = prev;
        }
        self.tab_state = None;
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
        self.tab_state = None;
    }

    pub fn move_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos = self.value[..self.cursor_pos]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
        self.tab_state = None;
    }

    pub fn move_right(&mut self) {
        if self.cursor_pos < self.value.len() {
            self.cursor_pos = self.value[self.cursor_pos..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor_pos + i)
                .unwrap_or(self.value.len());
        }
        self.tab_state = None;
    }

    pub fn home(&mut self) {
        self.cursor_pos = 0;
        self.tab_state = None;
    }

    pub fn end(&mut self) {
        self.cursor_pos = self.value.len();
        self.tab_state = None;
    }

    pub fn clear(&mut self) -> String {
        self.cursor_pos = 0;
        self.tab_state = None;
        std::mem::take(&mut self.value)
    }

    pub fn submit(&mut self) -> String {
        let val = self.clear();
        if !val.is_empty() {
            self.history.push(val.clone());
            if self.history.len() > MAX_HISTORY {
                self.history.remove(0);
            }
        }
        self.history_index = None;
        self.saved_input = None;
        val
    }

    pub fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        match self.history_index {
            None => {
                self.saved_input = Some(self.value.clone());
                let idx = self.history.len() - 1;
                self.history_index = Some(idx);
                self.value.clone_from(&self.history[idx]);
                self.cursor_pos = self.value.len();
            }
            Some(idx) if idx > 0 => {
                let new_idx = idx - 1;
                self.history_index = Some(new_idx);
                self.value.clone_from(&self.history[new_idx]);
                self.cursor_pos = self.value.len();
            }
            _ => {}
        }
        self.tab_state = None;
    }

    pub fn history_down(&mut self) {
        match self.history_index {
            Some(idx) if idx + 1 < self.history.len() => {
                let new_idx = idx + 1;
                self.history_index = Some(new_idx);
                self.value.clone_from(&self.history[new_idx]);
                self.cursor_pos = self.value.len();
            }
            Some(_) => {
                self.history_index = None;
                if let Some(saved) = self.saved_input.take() {
                    self.value = saved;
                } else {
                    self.value.clear();
                }
                self.cursor_pos = self.value.len();
            }
            None => {}
        }
        self.tab_state = None;
    }

    pub fn tab_complete(&mut self, nicks: &[String], commands: &[&str]) {
        if let Some(ref mut tab) = self.tab_state {
            if tab.matches.is_empty() {
                return;
            }
            tab.index = (tab.index + 1) % tab.matches.len();
            let completion = &tab.matches[tab.index];
            let suffix = if tab.is_command {
                " ".to_string()
            } else if tab.is_start_of_line {
                ": ".to_string()
            } else {
                " ".to_string()
            };
            self.value = format!("{}{}{}", tab.text_before, completion, suffix);
            self.cursor_pos = self.value.len();
        } else {
            let text = self.value[..self.cursor_pos].to_string();
            let (text_before, word) = match text.rfind(' ') {
                Some(pos) => (text[..=pos].to_string(), text[pos + 1..].to_string()),
                None => (String::new(), text),
            };
            if word.is_empty() {
                return;
            }
            let is_start_of_line = text_before.is_empty();
            let is_command = is_start_of_line && word.starts_with('/');

            let prefix = word;
            let mut matches: Vec<String> = if is_command {
                let cmd_prefix = &prefix[1..]; // strip leading /
                commands
                    .iter()
                    .filter(|c| c.to_lowercase().starts_with(&cmd_prefix.to_lowercase()))
                    .map(|c| format!("/{c}"))
                    .collect()
            } else {
                nicks
                    .iter()
                    .filter(|n| n.to_lowercase().starts_with(&prefix.to_lowercase()))
                    .cloned()
                    .collect()
            };
            matches.sort_by_key(|a| a.to_lowercase());

            if matches.is_empty() {
                return;
            }

            let completion = &matches[0];
            let suffix = if is_command { " " } else if is_start_of_line { ": " } else { " " };
            self.value = format!("{}{}{}", text_before, completion, suffix);
            self.cursor_pos = self.value.len();

            self.tab_state = Some(TabCompletionState {
                prefix,
                matches,
                index: 0,
                text_before,
                is_start_of_line,
                is_command,
            });
        }
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

    #[test]
    fn history_push_and_navigate() {
        let mut input = InputState::new();
        input.value = "first".to_string();
        input.cursor_pos = 5;
        input.submit();

        input.value = "second".to_string();
        input.cursor_pos = 6;
        input.submit();

        assert_eq!(input.history, vec!["first", "second"]);

        // Navigate up
        input.value = "current".to_string();
        input.cursor_pos = 7;
        input.history_up();
        assert_eq!(input.value, "second");
        assert_eq!(input.history_index, Some(1));

        input.history_up();
        assert_eq!(input.value, "first");
        assert_eq!(input.history_index, Some(0));

        // At top, stays
        input.history_up();
        assert_eq!(input.value, "first");
        assert_eq!(input.history_index, Some(0));
    }

    #[test]
    fn history_saved_input_restoration() {
        let mut input = InputState::new();
        input.value = "cmd1".to_string();
        input.cursor_pos = 4;
        input.submit();

        input.value = "typing...".to_string();
        input.cursor_pos = 9;

        input.history_up();
        assert_eq!(input.value, "cmd1");

        input.history_down();
        assert_eq!(input.value, "typing...");
        assert!(input.history_index.is_none());
        assert!(input.saved_input.is_none());
    }

    #[test]
    fn history_empty_input_not_pushed() {
        let mut input = InputState::new();
        input.submit();
        assert!(input.history.is_empty());
    }

    #[test]
    fn nick_completion_at_start_of_line() {
        let mut input = InputState::new();
        input.value = "fer".to_string();
        input.cursor_pos = 3;

        let nicks = vec!["ferris".to_string(), "helper".to_string()];
        input.tab_complete(&nicks, &[]);

        assert_eq!(input.value, "ferris: ");
        assert_eq!(input.cursor_pos, 8);
    }

    #[test]
    fn nick_completion_mid_line() {
        let mut input = InputState::new();
        input.value = "hey fer".to_string();
        input.cursor_pos = 7;

        let nicks = vec!["ferris".to_string(), "helper".to_string()];
        input.tab_complete(&nicks, &[]);

        assert_eq!(input.value, "hey ferris ");
        assert_eq!(input.cursor_pos, 11);
    }

    #[test]
    fn nick_completion_cycling() {
        let mut input = InputState::new();
        input.value = "h".to_string();
        input.cursor_pos = 1;

        let nicks = vec![
            "helper".to_string(),
            "hank".to_string(),
            "hiro".to_string(),
        ];
        input.tab_complete(&nicks, &[]);
        assert_eq!(input.value, "hank: "); // sorted: hank, helper, hiro

        input.tab_complete(&nicks, &[]);
        assert_eq!(input.value, "helper: ");

        input.tab_complete(&nicks, &[]);
        assert_eq!(input.value, "hiro: ");

        // Wraps around
        input.tab_complete(&nicks, &[]);
        assert_eq!(input.value, "hank: ");
    }

    #[test]
    fn command_completion() {
        let mut input = InputState::new();
        input.value = "/jo".to_string();
        input.cursor_pos = 3;

        let commands = &["join", "part", "msg", "quit"];
        input.tab_complete(&[], commands);

        assert_eq!(input.value, "/join ");
    }

    #[test]
    fn tab_state_reset_on_other_key() {
        let mut input = InputState::new();
        input.value = "fer".to_string();
        input.cursor_pos = 3;

        let nicks = vec!["ferris".to_string()];
        input.tab_complete(&nicks, &[]);
        assert!(input.tab_state.is_some());

        input.insert_char('x');
        assert!(input.tab_state.is_none());
    }
}

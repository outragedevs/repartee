use std::collections::VecDeque;

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme::hex_to_color;

const MAX_HISTORY: usize = 100;

#[allow(dead_code)]
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
    pub history: VecDeque<String>,
    pub history_index: Option<usize>,
    pub saved_input: Option<String>,
}

impl InputState {
    pub const fn new() -> Self {
        Self {
            value: String::new(),
            cursor_pos: 0,
            tab_state: None,
            history: VecDeque::new(),
            history_index: None,
            saved_input: None,
        }
    }

    pub fn insert_char(&mut self, c: char) {
        // Reject control characters — newlines, tabs, etc. must not enter
        // the input buffer. Multiline paste is handled by Event::Paste or
        // the '\n' key handler in handle_key().
        if c.is_control() {
            return;
        }
        self.value.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
        self.tab_state = None;
    }

    pub fn backspace(&mut self) {
        if self.cursor_pos > 0 {
            let prev = self.value[..self.cursor_pos]
                .char_indices()
                .last()
                .map_or(0, |(i, _)| i);
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
                .map_or(self.value.len(), |(i, _)| self.cursor_pos + i);
            self.value.drain(self.cursor_pos..next);
        }
        self.tab_state = None;
    }

    pub fn move_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos = self.value[..self.cursor_pos]
                .char_indices()
                .last()
                .map_or(0, |(i, _)| i);
        }
        self.tab_state = None;
    }

    pub fn move_right(&mut self) {
        if self.cursor_pos < self.value.len() {
            self.cursor_pos = self.value[self.cursor_pos..]
                .char_indices()
                .nth(1)
                .map_or(self.value.len(), |(i, _)| self.cursor_pos + i);
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

    /// Clear from cursor to start of line (Ctrl+U).
    pub fn clear_to_start(&mut self) {
        if self.cursor_pos > 0 {
            self.value.drain(..self.cursor_pos);
            self.cursor_pos = 0;
        }
        self.tab_state = None;
    }

    /// Clear from cursor to end of line (Ctrl+K).
    pub fn clear_to_end(&mut self) {
        if self.cursor_pos < self.value.len() {
            self.value.truncate(self.cursor_pos);
        }
        self.tab_state = None;
    }

    /// Delete the word before cursor (Ctrl+W).
    pub fn delete_word_back(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let before = &self.value[..self.cursor_pos];
        // Skip trailing whitespace, then skip non-whitespace
        let trimmed_end = before.trim_end().len();
        let word_start = before[..trimmed_end]
            .rfind(char::is_whitespace)
            .map_or(0, |i| i + 1);
        self.value.drain(word_start..self.cursor_pos);
        self.cursor_pos = word_start;
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
            self.history.push_back(val.clone());
            if self.history.len() > MAX_HISTORY {
                self.history.pop_front();
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

    pub fn tab_complete(
        &mut self,
        nicks: &[String],
        commands: &[&str],
        setting_paths: &[String],
    ) {
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
            self.value = format!("{}{completion}{suffix}", tab.text_before);
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

            // Detect subcommand context: /help <partial> or /set <partial>
            let subcommand_ctx = detect_subcommand_context(&text_before);

            let prefix = word;
            let mut matches: Vec<String> = match subcommand_ctx {
                Some(SubcommandContext::Help) => {
                    // Complete with command names (without /)
                    commands
                        .iter()
                        .filter(|c| c.to_lowercase().starts_with(&prefix.to_lowercase()))
                        .map(ToString::to_string)
                        .collect()
                }
                Some(SubcommandContext::Set) => {
                    // Complete with setting paths
                    setting_paths
                        .iter()
                        .filter(|p| p.to_lowercase().starts_with(&prefix.to_lowercase()))
                        .cloned()
                        .collect()
                }
                Some(SubcommandContext::Subcommand(ref subcmds)) => {
                    // Complete with doc-driven subcommand names
                    subcmds
                        .iter()
                        .filter(|s| s.to_lowercase().starts_with(&prefix.to_lowercase()))
                        .cloned()
                        .collect()
                }
                None if is_command => {
                    let cmd_prefix = &prefix[1..]; // strip leading /
                    commands
                        .iter()
                        .filter(|c| c.to_lowercase().starts_with(&cmd_prefix.to_lowercase()))
                        .map(|c| format!("/{c}"))
                        .collect()
                }
                None => {
                    nicks
                        .iter()
                        .filter(|n| n.to_lowercase().starts_with(&prefix.to_lowercase()))
                        .cloned()
                        .collect()
                }
            };
            matches.sort_by_key(|a| a.to_lowercase());

            if matches.is_empty() {
                return;
            }

            let completion = &matches[0];
            let suffix = if is_command { " " } else if is_start_of_line { ": " } else { " " };
            self.value = format!("{text_before}{completion}{suffix}");
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

enum SubcommandContext {
    Help,
    Set,
    Subcommand(Vec<String>),
}

/// Detect if the user is typing a subcommand for a command.
/// `text_before` is the text before the word being completed (including trailing space).
///
/// Special cases: `/help` completes command names, `/set` completes setting paths.
/// For any other command with `## Subcommands` in its docs, complete subcommand names.
fn detect_subcommand_context(text_before: &str) -> Option<SubcommandContext> {
    let trimmed = text_before.trim();
    let lower = trimmed.to_lowercase();
    // "/help <partial>" or "/? <partial>"
    if lower == "/help" || lower == "/?" {
        return Some(SubcommandContext::Help);
    }
    // "/set <partial>" (first arg is path)
    if lower == "/set" {
        return Some(SubcommandContext::Set);
    }
    // Check if this is a command with subcommands in docs.
    // Only match "/command" (single command, no further args yet)
    if let Some(cmd) = lower.strip_prefix('/')
        && !cmd.contains(' ')
    {
        let names = crate::commands::docs::get_subcommand_names(cmd);
        if !names.is_empty() {
            return Some(SubcommandContext::Subcommand(
                names.into_iter().map(String::from).collect(),
            ));
        }
    }
    None
}

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let colors = &app.theme.colors;
    let fg_muted = hex_to_color(&colors.fg_muted).unwrap_or(Color::DarkGray);
    let fg = hex_to_color(&colors.fg).unwrap_or(Color::Reset);

    let active_buf = app.state.active_buffer();
    let conn = active_buf.and_then(|b| app.state.connections.get(&b.connection_id));
    let server_label = conn.map_or("", |c| c.label.as_str());
    let channel_name = active_buf.map_or("", |b| b.name.as_str());
    let nick = conn.map_or("", |c| c.nick.as_str());

    let prompt = app
        .config
        .statusbar
        .prompt
        .replace("$server", server_label)
        .replace("$channel", channel_name)
        .replace("$nick", nick);

    let prompt_width = prompt.chars().count();
    let available_width = (area.width as usize).saturating_sub(prompt_width);

    // Calculate cursor position in chars (not bytes)
    let cursor_char_pos = app.input.value[..app.input.cursor_pos].chars().count();

    // Scroll offset: keep cursor visible within available_width
    let scroll_offset = if available_width == 0 {
        0
    } else if cursor_char_pos >= available_width {
        cursor_char_pos - available_width + 1
    } else {
        0
    };

    // Build byte-index lookup for char positions (Vec<usize> — no char data copied)
    let byte_indices: Vec<usize> = app.input.value.char_indices().map(|(i, _)| i).collect();
    let total_chars = byte_indices.len();
    let string_len = app.input.value.len();
    let visible_end = (scroll_offset + available_width).min(total_chars);

    let byte_at = |char_idx: usize| -> usize {
        if char_idx >= total_chars { string_len } else { byte_indices[char_idx] }
    };

    let before_cursor = &app.input.value[byte_at(scroll_offset)..byte_at(cursor_char_pos)];
    let cursor_char = app.input.value[byte_at(cursor_char_pos)..].chars().next().unwrap_or(' ');
    let after_cursor = if cursor_char_pos + 1 < visible_end {
        &app.input.value[byte_at(cursor_char_pos + 1)..byte_at(visible_end)]
    } else {
        ""
    };

    let cursor_color = hex_to_color(&colors.cursor).unwrap_or(Color::Reset);

    let line = Line::from(vec![
        Span::styled(prompt, Style::default().fg(fg_muted)),
        Span::styled(before_cursor, Style::default().fg(fg)),
        Span::styled(
            cursor_char.to_string(),
            Style::default().fg(Color::Black).bg(cursor_color),
        ),
        Span::styled(after_cursor, Style::default().fg(fg)),
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

        assert_eq!(input.history, VecDeque::from(["first".to_string(), "second".to_string()]));

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
        input.tab_complete(&nicks, &[], &[]);

        assert_eq!(input.value, "ferris: ");
        assert_eq!(input.cursor_pos, 8);
    }

    #[test]
    fn nick_completion_mid_line() {
        let mut input = InputState::new();
        input.value = "hey fer".to_string();
        input.cursor_pos = 7;

        let nicks = vec!["ferris".to_string(), "helper".to_string()];
        input.tab_complete(&nicks, &[], &[]);

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
        input.tab_complete(&nicks, &[], &[]);
        assert_eq!(input.value, "hank: "); // sorted: hank, helper, hiro

        input.tab_complete(&nicks, &[], &[]);
        assert_eq!(input.value, "helper: ");

        input.tab_complete(&nicks, &[], &[]);
        assert_eq!(input.value, "hiro: ");

        // Wraps around
        input.tab_complete(&nicks, &[], &[]);
        assert_eq!(input.value, "hank: ");
    }

    #[test]
    fn command_completion() {
        let mut input = InputState::new();
        input.value = "/jo".to_string();
        input.cursor_pos = 3;

        let commands = &["join", "part", "msg", "quit"];
        input.tab_complete(&[], commands, &[]);

        assert_eq!(input.value, "/join ");
    }

    #[test]
    fn help_subcommand_completion() {
        let mut input = InputState::new();
        input.value = "/help cl".to_string();
        input.cursor_pos = 8;

        let commands = &["connect", "close", "clear", "quit"];
        input.tab_complete(&[], commands, &[]);
        assert_eq!(input.value, "/help clear ");
    }

    #[test]
    fn help_subcommand_cycling() {
        let mut input = InputState::new();
        input.value = "/help c".to_string();
        input.cursor_pos = 7;

        let commands = &["connect", "close", "clear"];
        input.tab_complete(&[], commands, &[]);
        assert_eq!(input.value, "/help clear ");

        input.tab_complete(&[], commands, &[]);
        assert_eq!(input.value, "/help close ");

        input.tab_complete(&[], commands, &[]);
        assert_eq!(input.value, "/help connect ");
    }

    #[test]
    fn set_path_completion() {
        let mut input = InputState::new();
        input.value = "/set general.ni".to_string();
        input.cursor_pos = 15;

        let settings = vec!["general.nick".to_string(), "general.username".to_string()];
        input.tab_complete(&[], &[], &settings);
        assert_eq!(input.value, "/set general.nick ");
    }

    #[test]
    fn set_path_completion_section() {
        let mut input = InputState::new();
        input.value = "/set dis".to_string();
        input.cursor_pos = 8;

        let settings = vec![
            "display.nick_column_width".to_string(),
            "display.show_timestamps".to_string(),
        ];
        input.tab_complete(&[], &[], &settings);
        assert_eq!(input.value, "/set display.nick_column_width ");
    }

    #[test]
    fn clear_to_start() {
        let mut input = InputState::new();
        input.value = "hello world".to_string();
        input.cursor_pos = 5;
        input.clear_to_start();
        assert_eq!(input.value, " world");
        assert_eq!(input.cursor_pos, 0);
    }

    #[test]
    fn clear_to_start_at_beginning() {
        let mut input = InputState::new();
        input.value = "hello".to_string();
        input.cursor_pos = 0;
        input.clear_to_start();
        assert_eq!(input.value, "hello");
        assert_eq!(input.cursor_pos, 0);
    }

    #[test]
    fn clear_to_end() {
        let mut input = InputState::new();
        input.value = "hello world".to_string();
        input.cursor_pos = 5;
        input.clear_to_end();
        assert_eq!(input.value, "hello");
        assert_eq!(input.cursor_pos, 5);
    }

    #[test]
    fn clear_to_end_at_end() {
        let mut input = InputState::new();
        input.value = "hello".to_string();
        input.cursor_pos = 5;
        input.clear_to_end();
        assert_eq!(input.value, "hello");
        assert_eq!(input.cursor_pos, 5);
    }

    #[test]
    fn delete_word_back_single_word() {
        let mut input = InputState::new();
        input.value = "hello".to_string();
        input.cursor_pos = 5;
        input.delete_word_back();
        assert_eq!(input.value, "");
        assert_eq!(input.cursor_pos, 0);
    }

    #[test]
    fn delete_word_back_multiple_words() {
        let mut input = InputState::new();
        input.value = "hello world".to_string();
        input.cursor_pos = 11;
        input.delete_word_back();
        assert_eq!(input.value, "hello ");
        assert_eq!(input.cursor_pos, 6);
    }

    #[test]
    fn delete_word_back_with_trailing_spaces() {
        let mut input = InputState::new();
        input.value = "hello   ".to_string();
        input.cursor_pos = 8;
        input.delete_word_back();
        assert_eq!(input.value, "");
        assert_eq!(input.cursor_pos, 0);
    }

    #[test]
    fn delete_word_back_at_start() {
        let mut input = InputState::new();
        input.value = "hello".to_string();
        input.cursor_pos = 0;
        input.delete_word_back();
        assert_eq!(input.value, "hello");
        assert_eq!(input.cursor_pos, 0);
    }

    #[test]
    fn delete_word_back_mid_line() {
        let mut input = InputState::new();
        input.value = "one two three".to_string();
        input.cursor_pos = 7; // after "one two"
        input.delete_word_back();
        assert_eq!(input.value, "one  three");
        assert_eq!(input.cursor_pos, 4);
    }

    #[test]
    fn tab_state_reset_on_other_key() {
        let mut input = InputState::new();
        input.value = "fer".to_string();
        input.cursor_pos = 3;

        let nicks = vec!["ferris".to_string()];
        input.tab_complete(&nicks, &[], &[]);
        assert!(input.tab_state.is_some());

        input.insert_char('x');
        assert!(input.tab_state.is_none());
    }
}

use std::collections::VecDeque;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::keybindings::{Binding, Dispatch, KeyboardConfig, Keymap, Sequence};

use super::App;

pub struct NativeBindings {
    config: Option<KeyboardConfig>,
    keymap: Keymap,
    sequence: Sequence,
    events: VecDeque<(KeyEvent, Vec<String>)>,
    clock: Instant,
    context: Option<String>,
    shell: bool,
}

impl Default for NativeBindings {
    fn default() -> Self {
        Self {
            config: None,
            keymap: KeyboardConfig::default()
                .compile()
                .expect("valid default bindings"),
            sequence: Sequence::default(),
            events: VecDeque::new(),
            clock: Instant::now(),
            context: None,
            shell: false,
        }
    }
}

fn key_tokens(key: KeyEvent) -> Vec<String> {
    let token = match key.code {
        KeyCode::Char(ch) if key.modifiers.contains(KeyModifiers::CONTROL) => {
            format!("^{}", ch.to_ascii_uppercase())
        }
        KeyCode::Char(ch) => ch.to_string(),
        KeyCode::Esc => "^[".into(),
        KeyCode::Enter => "^M".into(),
        KeyCode::Tab => "^I".into(),
        KeyCode::BackTab => "stab".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::PageUp => "prior".into(),
        KeyCode::PageDown => "next".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::F(number) => format!("f{number}"),
        _ => return Vec::new(),
    };
    let mut tokens = crate::keybindings::terminal_tokens(&token);
    if key.modifiers.contains(KeyModifiers::ALT) {
        tokens.insert(0, "^[".into());
    }
    tokens
}

fn token_key(token: &str) -> Option<KeyEvent> {
    let mut modifiers = KeyModifiers::NONE;
    let code = match token {
        "^[" => KeyCode::Esc,
        "^M" => KeyCode::Enter,
        "^I" => KeyCode::Tab,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "prior" => KeyCode::PageUp,
        "next" => KeyCode::PageDown,
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "stab" => KeyCode::BackTab,
        _ if token.starts_with('^') && token.len() == 2 => {
            modifiers = KeyModifiers::CONTROL;
            KeyCode::Char(token.chars().nth(1)?.to_ascii_lowercase())
        }
        _ if token.chars().count() == 1 => KeyCode::Char(token.chars().next()?),
        _ => return None,
    };
    Some(KeyEvent::new(code, modifiers))
}

impl App {
    pub(super) fn cancel_bindings(&mut self) {
        self.bindings.sequence.clear();
        let events = std::mem::take(&mut self.bindings.events);
        for (event, _) in events {
            if self.bindings.shell {
                if let Some(buffer_id) = self.bindings.context.clone() {
                    self.forward_key_to_shell_buffer(&buffer_id, event);
                }
            } else if let KeyCode::Char(ch) = event.code
                && (event.modifiers - KeyModifiers::SHIFT).is_empty()
            {
                self.input.insert_char(ch);
            }
        }
    }

    fn sync_bindings(&mut self) {
        if self.bindings.config.as_ref() != Some(&self.config.keyboard)
            || self.bindings.shell != self.shell_input_active
        {
            self.cancel_bindings();
            match self.config.keyboard.compile() {
                Ok(keymap) => {
                    self.bindings.keymap = if self.shell_input_active {
                        keymap.navigation_only()
                    } else {
                        keymap
                    }
                }
                Err(error) => crate::commands::helpers::add_local_event(
                    self,
                    &crate::commands::helpers::escape_format(&format!(
                        "Invalid keyboard configuration: {error}"
                    )),
                ),
            }
            self.bindings.shell = self.shell_input_active;
            self.bindings.config = Some(self.config.keyboard.clone());
        }
        if self.bindings.context != self.state.active_buffer_id {
            self.cancel_bindings();
            self.bindings
                .context
                .clone_from(&self.state.active_buffer_id);
        }
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META)
        {
            self.cancel_bindings();
            self.handle_key_unbound(key);
            return;
        }
        self.sync_bindings();
        if self.shell_input_active && key.code == KeyCode::Esc {
            self.cancel_bindings();
            self.handle_key_unbound(key);
            return;
        }
        if self.settings_panel.is_some()
            || self.wizard.is_some()
            || self.emote_picker.is_open()
            || (key.code == KeyCode::Esc
                && (self.input.spell_state.is_some()
                    || !matches!(
                        self.image_preview,
                        crate::image_preview::PreviewStatus::Hidden
                    )))
        {
            self.cancel_bindings();
            self.handle_key_unbound(key);
            return;
        }
        let tokens = key_tokens(key);
        if tokens.is_empty() {
            self.handle_key_unbound(key);
            return;
        }
        let now = u64::try_from(self.bindings.clock.elapsed().as_millis()).unwrap_or(u64::MAX);
        let expired = self.bindings.sequence.expire(&self.bindings.keymap, now);
        self.dispatch_bindings(expired);
        self.bindings.events.push_back((key, tokens.clone()));
        let actions = self
            .bindings
            .sequence
            .feed_event(&self.bindings.keymap, tokens, now);
        self.dispatch_bindings(actions);
    }

    pub(super) fn binding_deadline(&mut self) -> Option<Instant> {
        self.sync_bindings();
        if self.settings_panel.is_some() || self.wizard.is_some() || self.emote_picker.is_open() {
            self.cancel_bindings();
        }
        self.bindings
            .sequence
            .deadline(&self.bindings.keymap)
            .and_then(|milliseconds| {
                self.bindings
                    .clock
                    .checked_add(std::time::Duration::from_millis(milliseconds))
            })
    }

    pub(super) fn tick_bindings(&mut self) {
        let input_before = self.input.value.clone();
        let buffer_before = self.state.active_buffer_id.clone();
        self.sync_bindings();
        if self.settings_panel.is_some() || self.wizard.is_some() || self.emote_picker.is_open() {
            return;
        }
        let now = u64::try_from(self.bindings.clock.elapsed().as_millis()).unwrap_or(u64::MAX);
        let actions = self.bindings.sequence.expire(&self.bindings.keymap, now);
        self.dispatch_bindings(actions);
        if self.input.value != input_before || self.state.active_buffer_id != buffer_before {
            self.on_input_changed();
        }
    }

    fn dispatch_bindings(&mut self, actions: Vec<Dispatch>) {
        let mut actions = VecDeque::from(actions);
        while let Some(action) = actions.pop_front() {
            match action {
                Dispatch::Replay(token) => {
                    if let Some((key, tokens)) = self.bindings.events.front()
                        && tokens.first() == Some(&token)
                        && tokens
                            .iter()
                            .skip(1)
                            .zip(actions.iter())
                            .all(|(token, action)| *action == Dispatch::Replay(token.clone()))
                        && actions.len() >= tokens.len().saturating_sub(1)
                    {
                        let key = *key;
                        for _ in 1..tokens.len() {
                            actions.pop_front();
                        }
                        self.bindings.events.pop_front();
                        self.handle_key_unbound(key);
                    } else if let Some(key) = token_key(&token) {
                        self.handle_key_unbound(key);
                    }
                }
                Dispatch::Action(binding) => {
                    self.bindings.events.clear();
                    self.execute_binding(&binding);
                }
            }
        }
    }

    fn execute_binding(&mut self, binding: &Binding) {
        let direct_key = match binding.action.as_str() {
            "backward_character" => Some(KeyCode::Left),
            "forward_character" => Some(KeyCode::Right),
            "beginning_of_line" => Some(KeyCode::Home),
            "end_of_line" => Some(KeyCode::End),
            "backward_history" => Some(KeyCode::Up),
            "forward_history" => Some(KeyCode::Down),
            "backspace" => Some(KeyCode::Backspace),
            "delete_character" => Some(KeyCode::Delete),
            "send_line" => Some(KeyCode::Enter),
            "word_completion" => Some(KeyCode::Tab),
            "scroll_backward" => Some(KeyCode::PageUp),
            "scroll_forward" => Some(KeyCode::PageDown),
            _ => None,
        };
        if let Some(code) = direct_key {
            self.handle_key_unbound(KeyEvent::new(code, KeyModifiers::NONE));
            return;
        }
        match binding.action.as_str() {
            "change_window" => {
                if let Ok(number) = binding.data.parse() {
                    self.switch_to_buffer_num(number);
                }
            }
            "active_window" => self.switch_to_activity_buffer(),
            "previous_window" | "next_window" => {
                if binding.action == "previous_window" {
                    self.state.prev_buffer();
                } else {
                    self.state.next_buffer();
                }
                self.scroll_offset = 0;
                self.reset_sidepanel_scrolls();
                self.update_shell_input_state();
            }
            "command" => {
                self.submit_from_tui(&format!("/{}", binding.data.trim_start_matches('/')));
            }
            "multi" => {
                for item in binding.data.split(';') {
                    let item = item.trim_start();
                    let (action, data) = item.split_once(' ').unwrap_or((item, ""));
                    self.execute_binding(&Binding::new(action, data));
                }
            }
            "insert_text" => {
                for ch in binding.data.chars() {
                    self.input.insert_char(ch);
                }
            }
            "erase_line" => {
                self.input.clear();
            }
            "erase_to_beg_of_line" => self.input.clear_to_start(),
            "erase_to_end_of_line" => self.input.clear_to_end(),
            "delete_previous_word" => self.input.delete_word_back(),
            "scroll_start" => self.scroll_offset = usize::MAX / 2,
            "scroll_end" => {
                self.scroll_offset = 0;
                self.collapse_backlog_if_at_bottom();
            }
            "refresh_screen" => self.needs_full_redraw = true,
            "backward_word" => {
                while self.input.cursor_pos > 0
                    && self.input.value[..self.input.cursor_pos]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_whitespace)
                {
                    self.input.move_left();
                }
                while self.input.cursor_pos > 0
                    && self.input.value[..self.input.cursor_pos]
                        .chars()
                        .next_back()
                        .is_some_and(|ch| !ch.is_whitespace())
                {
                    self.input.move_left();
                }
            }
            "forward_word" => {
                while self.input.value[self.input.cursor_pos..]
                    .chars()
                    .next()
                    .is_some_and(|ch| !ch.is_whitespace())
                {
                    self.input.move_right();
                }
                while self.input.value[self.input.cursor_pos..]
                    .chars()
                    .next()
                    .is_some_and(char::is_whitespace)
                {
                    self.input.move_right();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::buffer::{Buffer, BufferType};

    #[test]
    fn pending_special_keys_keep_their_events_and_shell_escape_never_waits() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        app.config
            .keyboard
            .set("left-x", Binding::new("nothing", ""))
            .unwrap();
        app.input.value = "ab".into();
        app.input.cursor_pos = 2;
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.input.cursor_pos, 2);
        app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(app.input.value, "azb");
        app.config.keyboard.key_timeout = 100;
        app.config
            .keyboard
            .set("g", Binding::new("nothing", ""))
            .unwrap();
        app.config
            .keyboard
            .set("g-g", Binding::new("nothing", ""))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        let remaining = app
            .binding_deadline()
            .unwrap()
            .saturating_duration_since(Instant::now());
        assert!(remaining <= std::time::Duration::from_millis(100));
        assert!(app.bindings.sequence.is_pending());
        app.shell_input_active = true;
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.bindings.sequence.is_pending());
        assert!(app.bindings.events.is_empty());
        app.config
            .keyboard
            .set("g-g", Binding::new("change_window", "1"))
            .unwrap();
        let composer = app.input.value.clone();
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        assert!(app.bindings.sequence.is_pending());
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.input.value, composer);
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        app.cancel_bindings();
        assert_eq!(app.input.value, composer);
    }

    #[test]
    fn bind_sequences_use_window_numbers_and_preserve_paste_and_alt_enter() {
        let mut app = crate::app::input::submit_typing_tests::test_app();
        for number in 1..=15 {
            app.state.add_buffer(Buffer::for_test(
                "net",
                BufferType::Channel,
                &format!("#room{number}"),
            ));
        }
        let ids = app.state.numbered_buffer_ids();
        app.state.set_active_buffer(&ids[0]);
        for (key, action, data) in [
            ("meta-q", "key", "win1"),
            ("meta-w", "key", "win2"),
            ("win1-win2", "change_window", "12"),
        ] {
            app.config
                .keyboard
                .set(key, Binding::new(action, data))
                .unwrap();
        }
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT));
        assert_eq!(app.state.active_buffer_id.as_ref(), Some(&ids[0]));
        app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT));
        assert_eq!(app.state.active_buffer_id.as_ref(), Some(&ids[11]));
        app.config.keyboard.remove("meta-1");
        app.handle_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT));
        assert_eq!(app.state.active_buffer_id.as_ref(), Some(&ids[11]));
        app.config
            .keyboard
            .set("g-g", Binding::new("change_window", "1"))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        app.handle_paste("❤tekst");
        assert_eq!(app.input.value, "g❤tekst");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(app.input.value, "g❤tekst\n");
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.input.value, "g❤tekst\ng");
        assert_eq!(app.input.cursor_pos, "g❤tekst\n".len());
        app.input.backspace();
        app.input.end();
        app.input.backspace();
        app.input.insert_newline();
        app.handle_key(KeyEvent::new_with_kind(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        assert_eq!(app.input.value, "g❤tekst\n");
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        app.teardown_shim();
        assert!(!app.bindings.sequence.is_pending());
        assert_eq!(app.input.value, "g❤tekst\ng");
    }
}

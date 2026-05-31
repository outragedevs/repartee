use std::collections::HashMap;
use std::time::Instant;

use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Position;
use tokio::sync::mpsc::error::TrySendError;

use crate::state::buffer::{
    ActivityLevel, Buffer, BufferType, Message, MessageType, make_buffer_id,
};
use crate::ui::layout::UiRegions;

use super::{App, MAX_ALIAS_DEPTH, MAX_PASTE_LINES, expand_alias_template};

impl App {
    pub(crate) fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Paste(text) => self.handle_paste(&text),
            Event::Resize(cols, rows) => {
                self.cached_term_cols = cols;
                self.cached_term_rows = rows;
                self.resize_all_shells();
            }
            _ => {}
        }
    }

    /// Maximum time (ms) between ESC and follow-up key to treat as ESC+key combo.
    const ESC_TIMEOUT_MS: u128 = 500;

    /// Check if a recent ESC press should combine with the current key.
    fn consume_esc_prefix(&mut self) -> bool {
        self.last_esc_time
            .take()
            .is_some_and(|t| t.elapsed().as_millis() < Self::ESC_TIMEOUT_MS)
    }

    /// Switch to buffer N (0-9) — shared logic for Alt+N and ESC+N.
    pub(crate) fn switch_to_buffer_num(&mut self, n: usize) {
        if n == 0 {
            // 0 goes to default Status buffer
            let default_buf_id = make_buffer_id(Self::DEFAULT_CONN_ID, "Status");
            if self.state.buffers.contains_key(&default_buf_id) {
                self.state.set_active_buffer(&default_buf_id);
                self.scroll_offset = 0;
                self.reset_sidepanel_scrolls();
            }
        } else {
            // 1..9 map to real buffers (excluding _default)
            let real_ids: Vec<_> = self
                .state
                .sorted_buffer_ids()
                .into_iter()
                .filter(|id| {
                    self.state
                        .buffers
                        .get(id.as_str())
                        .is_none_or(|b| b.connection_id != Self::DEFAULT_CONN_ID)
                })
                .collect();
            let idx = n - 1; // 1 = index 0
            if idx < real_ids.len() {
                self.state.set_active_buffer(&real_ids[idx]);
                self.scroll_offset = 0;
                self.reset_sidepanel_scrolls();
            }
        }
        self.update_shell_input_state();
    }

    /// Reset sidepanel scroll offsets (e.g. on buffer switch).
    #[allow(clippy::missing_const_for_fn)] // const &mut self not stable
    pub(crate) fn reset_sidepanel_scrolls(&mut self) {
        self.buffer_list_scroll = 0;
        self.nick_list_scroll = 0;
    }

    #[allow(clippy::too_many_lines)]
    fn handle_key(&mut self, key: event::KeyEvent) {
        // Shell input mode: forward most keys to the active shell PTY.
        if self.shell_input_active {
            // Ctrl+] exits shell input mode (telnet convention).
            if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char(']') {
                self.shell_input_active = false;
                return;
            }
            // Alt+digit / Alt+arrow switches buffers even in shell mode.
            if key.modifiers.contains(KeyModifiers::ALT) {
                if let KeyCode::Char(c) = key.code
                    && c.is_ascii_digit()
                {
                    let n = c.to_digit(10).unwrap_or(0) as usize;
                    self.switch_to_buffer_num(n);
                    return;
                }
                match key.code {
                    KeyCode::Left => {
                        self.state.prev_buffer();
                        self.scroll_offset = 0;
                        self.reset_sidepanel_scrolls();
                        self.update_shell_input_state();
                        return;
                    }
                    KeyCode::Right => {
                        self.state.next_buffer();
                        self.scroll_offset = 0;
                        self.reset_sidepanel_scrolls();
                        self.update_shell_input_state();
                        return;
                    }
                    _ => {}
                }
            }
            // Forward everything else to the shell PTY.
            self.forward_key_to_shell(key);
            return;
        }

        // Emote picker overlay swallows all keys while open.
        if self.emote_picker.is_open() {
            self.handle_emote_picker_key(key);
            return;
        }

        // Check for ESC+key combos (ESC pressed recently, now a follow-up key)
        let esc_active = if key.code == KeyCode::Esc {
            // Don't consume ESC prefix on another ESC press
            self.last_esc_time.take();
            false
        } else {
            self.consume_esc_prefix()
        };

        // ESC+digit → buffer switch (like Alt+digit)
        // ESC+Left/Right → prev/next buffer (like Alt+Left/Right)
        if esc_active {
            match key.code {
                KeyCode::Char(c) if c.is_ascii_digit() && key.modifiers.is_empty() => {
                    let n = c.to_digit(10).unwrap_or(0) as usize;
                    self.switch_to_buffer_num(n);
                    return;
                }
                KeyCode::Left if key.modifiers.is_empty() => {
                    self.state.prev_buffer();
                    self.scroll_offset = 0;
                    self.reset_sidepanel_scrolls();
                    return;
                }
                KeyCode::Right if key.modifiers.is_empty() => {
                    self.state.next_buffer();
                    self.scroll_offset = 0;
                    self.reset_sidepanel_scrolls();
                    return;
                }
                _ => {
                    // ESC expired or unrecognized follow-up — fall through to normal handling
                }
            }
        }

        match (key.modifiers, key.code) {
            // ESC — dismiss spell suggestions, image preview, or record for ESC+key combo
            (_, KeyCode::Esc) => {
                if self.input.spell_state.is_some() {
                    self.input.dismiss_spell();
                } else if matches!(
                    self.image_preview,
                    crate::image_preview::PreviewStatus::Hidden
                ) {
                    self.last_esc_time = Some(Instant::now());
                } else {
                    self.dismiss_image_preview();
                }
            }
            (KeyModifiers::CONTROL, KeyCode::Char('q' | 'c')) => self.should_quit = true,
            (KeyModifiers::CONTROL, KeyCode::Char('g')) => self.open_emote_picker(),
            (KeyModifiers::CONTROL, KeyCode::Char('l')) => {
                // Force redraw (happens automatically on next iteration)
            }
            // Ctrl+U — clear line from cursor to start
            (KeyModifiers::CONTROL, KeyCode::Char('u')) => self.input.clear_to_start(),
            // Ctrl+K — clear line from cursor to end
            (KeyModifiers::CONTROL, KeyCode::Char('k')) => self.input.clear_to_end(),
            // Ctrl+W — delete word before cursor
            (KeyModifiers::CONTROL, KeyCode::Char('w')) => self.input.delete_word_back(),
            // Ctrl+A — move cursor to start (same as Home)
            (KeyModifiers::CONTROL, KeyCode::Char('a')) | (_, KeyCode::Home) => self.input.home(),
            // Ctrl+B — move cursor left (same as Left)
            (KeyModifiers::CONTROL, KeyCode::Char('b')) => self.input.move_left(),
            // Ctrl+E — move cursor to end (same as End)
            (KeyModifiers::CONTROL, KeyCode::Char('e')) | (_, KeyCode::End) => {
                self.input.end();
                self.scroll_offset = 0;
            }
            (KeyModifiers::ALT, KeyCode::Char(c)) if c.is_ascii_digit() => {
                let n = c.to_digit(10).unwrap_or(0) as usize;
                self.switch_to_buffer_num(n);
            }
            (mods, KeyCode::Left) if mods.contains(KeyModifiers::ALT) => {
                self.state.prev_buffer();
                self.scroll_offset = 0;
                self.reset_sidepanel_scrolls();
            }
            (mods, KeyCode::Right) if mods.contains(KeyModifiers::ALT) => {
                self.state.next_buffer();
                self.scroll_offset = 0;
                self.reset_sidepanel_scrolls();
            }
            // Enter key, or newline chars arriving individually when bracketed
            // paste isn't supported — submit the current input line.
            (_, KeyCode::Enter | KeyCode::Char('\n' | '\r')) => {
                // Accept any active spell correction before submitting.
                self.input.spell_state = None;
                let text = self.input.submit();
                if !text.is_empty() {
                    self.handle_submit(&text);
                }
            }
            (_, KeyCode::Backspace) => {
                self.input.dismiss_spell();
                self.input.backspace();
            }
            (_, KeyCode::Delete) => self.input.delete(),
            (mods, KeyCode::Left) if !mods.contains(KeyModifiers::ALT) => self.input.move_left(),
            (mods, KeyCode::Right) if !mods.contains(KeyModifiers::ALT) => self.input.move_right(),
            (_, KeyCode::Up) => self.input.history_up(),
            (_, KeyCode::Down) => self.input.history_down(),
            (_, KeyCode::PageUp) => {
                self.scroll_offset = self.scroll_offset.saturating_add(10);
                if self.log_browser_mode {
                    self.maybe_paginate_log_buffer();
                }
            }
            (_, KeyCode::PageDown) => {
                self.scroll_offset = self.scroll_offset.saturating_sub(10);
            }
            (_, KeyCode::Tab) => {
                let is_highlight = self
                    .input
                    .spell_state
                    .as_ref()
                    .is_some_and(|s| s.highlight_only);
                if is_highlight {
                    // Highlight mode: Tab dismisses suggestions and performs normal tab completion.
                    self.input.spell_state = None;
                    self.handle_tab();
                } else if self.input.spell_state.is_some() {
                    // Replace mode: Tab cycles spell suggestions.
                    self.input.cycle_spell_suggestion();
                } else {
                    self.handle_tab();
                }
            }
            // Log-browser hotkey: bare `q` / `Q` with no modifiers (other than
            // Shift) quits the browser when the input line is empty. Mirrors
            // weechat's `q` shortcut in `/buffer history`-style read-only views.
            // The empty-buffer guard means typing `quit` or any text that
            // starts with `q` still works as a normal slash command sequence.
            (mods, KeyCode::Char('q' | 'Q'))
                if self.log_browser_mode
                    && self.input.value.is_empty()
                    && (mods.is_empty() || mods == KeyModifiers::SHIFT) =>
            {
                self.should_quit = true;
            }
            (mods, KeyCode::Char(c)) if mods.is_empty() || mods == KeyModifiers::SHIFT => {
                let is_highlight = self
                    .input
                    .spell_state
                    .as_ref()
                    .is_some_and(|s| s.highlight_only);
                if is_highlight {
                    // Highlight mode: any keystroke dismisses suggestions, input proceeds normally.
                    self.input.spell_state = None;
                    self.input.insert_char(c);
                    if c == ' ' || (c.is_ascii_punctuation() && c != '/') {
                        self.check_spelling_after_separator();
                    }
                } else if self.input.spell_state.is_some() {
                    // Replace mode: handle accept keys specially.
                    if c == ' ' {
                        // Space: accept current suggestion.
                        let needs_space = self.input.spell_state.as_ref().is_some_and(|s| {
                            self.input.value[s.word_end..]
                                .chars()
                                .next()
                                .is_none_or(|ch| ch != ' ')
                        });
                        self.input.spell_state = None;
                        if needs_space {
                            self.input.insert_char(' ');
                        }
                    } else if matches!(c, '.' | ',' | '!' | '?' | ';' | ':') {
                        // Punctuation: accept and replace trailing separator with it.
                        self.input.accept_spell_with_punctuation(c);
                    } else {
                        // Any other char: accept current suggestion and continue typing.
                        self.input.spell_state = None;
                        self.input.insert_char(c);
                    }
                } else {
                    self.input.insert_char(c);
                    // After typing a word separator, check spelling of the completed word.
                    if c == ' ' || (c.is_ascii_punctuation() && c != '/') {
                        self.check_spelling_after_separator();
                    }
                }
            }
            _ => {}
        }
    }

    pub(crate) fn handle_paste(&mut self, text: &str) {
        // In shell input mode, forward paste directly to the PTY.
        if self.shell_input_active
            && let Some(buf) = self.state.active_buffer()
        {
            let buf_id = buf.id.clone();
            if let Some(shell_id) = self
                .shell_mgr
                .session_id_for_buffer(&buf_id)
                .map(ToString::to_string)
            {
                // Check if shell enabled bracketed paste mode.
                let bracketed = self
                    .shell_mgr
                    .screen(&shell_id)
                    .is_some_and(vt100::Screen::bracketed_paste);
                if bracketed {
                    self.shell_mgr.write(&shell_id, b"\x1b[200~");
                }
                self.shell_mgr.write(&shell_id, text.as_bytes());
                if bracketed {
                    self.shell_mgr.write(&shell_id, b"\x1b[201~");
                }
                return;
            }
        }

        let lines: Vec<&str> = text.split('\n').collect();
        let non_empty: Vec<&str> = lines
            .iter()
            .map(|l| l.trim_end_matches('\r'))
            .filter(|l| !l.is_empty())
            .collect();

        if non_empty.len() <= 1 {
            // Single line (or empty): insert into input buffer at cursor.
            let single = non_empty.first().copied().unwrap_or("");
            for ch in single.chars() {
                self.input.insert_char(ch);
            }
            return;
        }

        // Multiline paste: prepend any existing input to the first line,
        // send it immediately, queue the rest with 500ms spacing.
        // Matches kokoirc and erssi behavior.
        self.paste_queue.clear();

        let current_input = self.input.submit();
        let first = if current_input.is_empty() {
            non_empty[0].to_string()
        } else {
            format!("{current_input}{}", non_empty[0])
        };

        // Send first line immediately
        self.handle_submit(&first);

        // Queue remaining lines
        for line in &non_empty[1..] {
            self.paste_queue.push_back((*line).to_string());
        }

        // Cap paste queue to avoid unbounded memory growth from huge pastes.
        if self.paste_queue.len() > MAX_PASTE_LINES {
            let dropped = self.paste_queue.len() - MAX_PASTE_LINES;
            self.paste_queue.truncate(MAX_PASTE_LINES);
            tracing::warn!("paste truncated to {MAX_PASTE_LINES} lines ({dropped} dropped)");
        }
    }

    /// Send one queued paste line. Called every 500ms by the paste timer.
    pub(crate) fn drain_paste_queue(&mut self) {
        if let Some(line) = self.paste_queue.pop_front() {
            self.handle_submit(&line);
        }
    }

    fn handle_mouse(&mut self, mouse: event::MouseEvent) {
        let Some(regions) = self.ui_regions else {
            return;
        };
        let pos = Position::new(mouse.column, mouse.row);

        // Emote picker: a left click inserts the clicked emote (or closes on a
        // click outside any cell). It takes priority over all other mouse use.
        if let crate::ui::emote_picker::EmotePickerState::Open { cell_rects, .. } =
            &self.emote_picker
        {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                let hit = cell_rects
                    .iter()
                    .find(|(_, r)| r.contains(pos))
                    .map(|(i, _)| *i);
                if let Some(idx) = hit {
                    self.insert_emote_by_index(idx);
                }
                self.emote_picker = crate::ui::emote_picker::EmotePickerState::Hidden;
            }
            return;
        }

        // Forward mouse events to the shell PTY when in shell input mode
        // and the mouse is within the chat (shell render) area.
        if self.shell_input_active && regions.chat_area.is_some_and(|r| r.contains(pos)) {
            self.forward_mouse_to_shell(mouse, &regions);
            return;
        }

        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if regions.chat_area.is_some_and(|r| r.contains(pos)) {
                    self.scroll_offset = self.scroll_offset.saturating_add(3);
                    if self.log_browser_mode {
                        self.maybe_paginate_log_buffer();
                    }
                } else if regions.buffer_list_area.is_some_and(|r| r.contains(pos)) {
                    self.buffer_list_scroll = self.buffer_list_scroll.saturating_sub(1);
                } else if regions.nick_list_area.is_some_and(|r| r.contains(pos)) {
                    self.nick_list_scroll = self.nick_list_scroll.saturating_sub(1);
                }
            }
            MouseEventKind::ScrollDown => {
                if regions.chat_area.is_some_and(|r| r.contains(pos)) {
                    self.scroll_offset = self.scroll_offset.saturating_sub(3);
                } else if let Some(r) = regions.buffer_list_area
                    && r.contains(pos)
                {
                    let visible_h = r.height as usize;
                    let max = self.buffer_list_total.saturating_sub(visible_h);
                    if self.buffer_list_scroll < max {
                        self.buffer_list_scroll += 1;
                    }
                } else if let Some(r) = regions.nick_list_area
                    && r.contains(pos)
                {
                    let visible_h = r.height as usize;
                    let max = self.nick_list_total.saturating_sub(visible_h);
                    if self.nick_list_scroll < max {
                        self.nick_list_scroll += 1;
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // Dismiss image preview on any click (same as ESC).
                if !matches!(
                    self.image_preview,
                    crate::image_preview::PreviewStatus::Hidden
                ) {
                    self.dismiss_image_preview();
                    return;
                }
                if let Some(buf_area) = regions.buffer_list_area
                    && buf_area.contains(pos)
                {
                    let y_offset = mouse.row.saturating_sub(buf_area.y) as usize;
                    self.handle_buffer_list_click(y_offset);
                } else if let Some(nick_area) = regions.nick_list_area
                    && nick_area.contains(pos)
                {
                    let y_offset = mouse.row.saturating_sub(nick_area.y) as usize;
                    self.handle_nick_list_click(y_offset);
                } else if let Some(chat_area) = regions.chat_area
                    && chat_area.contains(pos)
                {
                    let y_offset = mouse.row.saturating_sub(chat_area.y) as usize;
                    self.handle_chat_click(y_offset);
                }
            }
            _ => {}
        }
    }

    fn handle_buffer_list_click(&mut self, y_offset: usize) {
        // Clamp scroll the same way the renderer does — prevents click offset
        // when buffer_list_scroll exceeds max_scroll (e.g. after reattach or
        // channels parted while scrolled).
        let visible_h = self
            .ui_regions
            .and_then(|r| r.buffer_list_area)
            .map_or(0, |r| r.height as usize);
        let max_scroll = self.buffer_list_total.saturating_sub(visible_h);
        let clamped_scroll = self.buffer_list_scroll.min(max_scroll);
        self.buffer_list_scroll = clamped_scroll;
        let logical_row = y_offset + clamped_scroll;
        let sorted_ids = self.state.sorted_buffer_ids();
        // Every non-default buffer occupies one row — matches the renderer.
        let mut row = 0usize;
        for id in &sorted_ids {
            let Some(buf) = self.state.buffers.get(id.as_str()) else {
                continue;
            };
            if buf.connection_id == Self::DEFAULT_CONN_ID {
                continue;
            }
            if row == logical_row {
                self.state.set_active_buffer(id);
                self.scroll_offset = 0;
                self.nick_list_scroll = 0;
                self.update_shell_input_state();
                return;
            }
            row += 1;
        }
    }

    fn handle_nick_list_click(&mut self, y_offset: usize) {
        use crate::state::sorting;

        // Clamp scroll the same way the renderer does.
        let visible_h = self
            .ui_regions
            .and_then(|r| r.nick_list_area)
            .map_or(0, |r| r.height as usize);
        let max_scroll = self.nick_list_total.saturating_sub(visible_h);
        let clamped_scroll = self.nick_list_scroll.min(max_scroll);
        self.nick_list_scroll = clamped_scroll;
        let logical_row = y_offset + clamped_scroll;

        // Row 0 is the "N users" header line — skip it
        if logical_row == 0 {
            return;
        }
        let nick_index = logical_row - 1;

        // Get the sorted nick list from the active buffer
        let (conn_id, nick_name) = {
            let Some(buf) = self.state.active_buffer() else {
                return;
            };
            if buf.buffer_type != BufferType::Channel {
                return;
            }
            let nick_refs: Vec<_> = buf.users.values().collect();
            let sorted = sorting::sort_nicks(&nick_refs, sorting::DEFAULT_PREFIX_ORDER);
            let Some(entry) = sorted.get(nick_index) else {
                return;
            };
            (buf.connection_id.clone(), entry.nick.clone())
        };

        // Create a query buffer for that nick if it doesn't exist, then switch to it
        let query_buf_id = make_buffer_id(&conn_id, &nick_name);
        if !self.state.buffers.contains_key(&query_buf_id) {
            self.state.add_buffer(Buffer {
                id: query_buf_id.clone(),
                connection_id: conn_id,
                buffer_type: BufferType::Query,
                name: nick_name,
                messages: std::collections::VecDeque::new(),
                activity: ActivityLevel::None,
                unread_count: 0,
                last_read: chrono::Utc::now(),
                topic: None,
                topic_set_by: None,
                users: HashMap::new(),
                modes: None,
                mode_params: None,
                list_modes: HashMap::new(),
                last_speakers: Vec::new(),
                peer_handle: None,
                log_total_lines: None,
                log_oldest_ts: None,
                log_newest_ts: None,
                history_exhausted: false,
                log_initial_loaded: false,
            });
        }
        self.state.set_active_buffer(&query_buf_id);
        self.scroll_offset = 0;
        self.nick_list_scroll = 0;
    }

    fn handle_chat_click(&mut self, y_offset: usize) {
        if !self.config.image_preview.enabled {
            return;
        }

        let Some(buf) = self.state.active_buffer() else {
            return;
        };

        // Map the clicked row to the corresponding message, same logic as chat_view render.
        let total = buf.messages.len();
        let chat_height = self
            .ui_regions
            .and_then(|r| r.chat_area)
            .map_or(0, |a| a.height as usize);
        let max_scroll = total.saturating_sub(chat_height);
        let scroll = self.scroll_offset.min(max_scroll);
        let skip = total.saturating_sub(chat_height + scroll);
        let msg_index = skip + y_offset;

        let Some(msg) = buf.messages.get(msg_index) else {
            return;
        };

        // Extract URLs from message text and preview the first classifiable one.
        let urls = crate::image_preview::detect::extract_urls(&msg.text);
        if let Some(classification) = urls.first() {
            self.show_image_preview(&classification.url);
        }
    }

    /// Initialize the spell checker from config.
    pub(crate) fn init_spellchecker(&mut self) {
        let dict_dir = crate::spellcheck::SpellChecker::resolve_dict_dir(
            &self.config.spellcheck.dictionary_dir,
        );
        let checker = crate::spellcheck::SpellChecker::load(
            &self.config.spellcheck.languages,
            &dict_dir,
            self.config.spellcheck.computing,
        );
        if checker.is_active() {
            tracing::info!(
                dicts = checker.dict_count(),
                computing = checker.has_computing(),
                "spell checker initialized"
            );
            self.spellchecker = Some(checker);
        } else {
            tracing::info!("spell checker: no dictionaries loaded");
            self.spellchecker = None;
        }
    }

    /// Reload the spell checker (called from `/set spellcheck.*`).
    pub fn reload_spellchecker(&mut self) {
        if self.config.spellcheck.enabled {
            self.init_spellchecker();
        } else {
            self.spellchecker = None;
        }
    }

    /// Check the last completed word for spelling and set up correction state.
    fn check_spelling_after_separator(&mut self) {
        // Skip if spell checking is disabled or no checker loaded.
        let Some(ref checker) = self.spellchecker else {
            return;
        };
        // Skip commands.
        if self.input.is_command() {
            return;
        }
        // Extract the last completed word (may include trailing punctuation).
        let Some((raw_start, _raw_end, raw_word)) = self.input.last_completed_word() else {
            return;
        };

        // Strip leading/trailing punctuation (WeeChat-style).
        // "do?" → "do", "hello!" → "hello", "'test'" → "test"
        let (stripped, strip_offset, strip_end) =
            crate::spellcheck::strip_word_punctuation(&raw_word);
        if stripped.is_empty() {
            return;
        }

        // Actual byte positions in the input buffer for the stripped word.
        let word_start = raw_start + strip_offset;
        let word_end = raw_start + strip_end;

        // Collect nicks from the active buffer to skip.
        let nicks: std::collections::HashSet<String> = self
            .state
            .active_buffer()
            .map_or_else(std::collections::HashSet::new, |buf| {
                buf.users.values().map(|e| e.nick.clone()).collect()
            });

        // Check the stripped word.
        if checker.check(stripped, &nicks) {
            return;
        }
        // Misspelled — get suggestions ranked by dictionary priority.
        let suggestions = checker.suggest(stripped);
        if suggestions.is_empty() {
            return;
        }
        let highlight_only = self.config.spellcheck.mode == "highlight";
        self.input.spell_state = Some(crate::ui::input::SpellCorrection {
            word_start,
            word_end,
            original: stripped.to_string(),
            suggestions,
            index: 0,
            highlight_only,
        });

        if !highlight_only {
            // Replace mode: immediately apply the first suggestion so it's visible
            // in the input and ready to accept with Space. Tab cycles to the next one.
            self.input.apply_spell_suggestion(0);
        }
    }

    /// Open the emote picker overlay (Ctrl+G or `/emote` with no args).
    pub(crate) fn open_emote_picker(&mut self) {
        self.emote_picker = crate::ui::emote_picker::EmotePickerState::Open {
            filter: String::new(),
            selected: 0,
            cell_rects: Vec::new(),
        };
    }

    /// Handle a key while the emote picker is open.
    fn handle_emote_picker_key(&mut self, key: event::KeyEvent) {
        use crate::ui::emote_picker::EmotePickerState;
        let mut close = false;
        let mut insert_idx: Option<u32> = None;
        if let EmotePickerState::Open {
            filter, selected, ..
        } = &mut self.emote_picker
        {
            let filtered = EmotePickerState::filtered_indices(filter);
            let len = filtered.len();
            match (key.modifiers, key.code) {
                (_, KeyCode::Esc) => close = true,
                (_, KeyCode::Enter) => {
                    insert_idx = filtered.get(*selected).copied();
                    close = true;
                }
                (_, KeyCode::Left | KeyCode::Up) => *selected = selected.saturating_sub(1),
                (_, KeyCode::Right | KeyCode::Down) => {
                    *selected = (*selected + 1).min(len.saturating_sub(1));
                }
                (_, KeyCode::Home) => *selected = 0,
                (_, KeyCode::End) => *selected = len.saturating_sub(1),
                (_, KeyCode::Backspace) => {
                    filter.pop();
                    *selected = 0;
                }
                (m, KeyCode::Char(c))
                    if !m.contains(KeyModifiers::CONTROL)
                        && !m.contains(KeyModifiers::ALT)
                        && !c.is_control() =>
                {
                    filter.push(c);
                    *selected = 0;
                }
                _ => {}
            }
        }
        if let Some(idx) = insert_idx {
            self.insert_emote_by_index(idx);
        }
        if close {
            self.emote_picker = crate::ui::emote_picker::EmotePickerState::Hidden;
        }
    }

    /// Insert `:name:` for the registry index at the input cursor.
    pub(crate) fn insert_emote_by_index(&mut self, index: u32) {
        if let Some(name) = crate::emotes::names().get(index as usize) {
            let token = format!(":{name}:");
            let at = self.input.cursor_pos;
            self.input.value.insert_str(at, &token);
            self.input.cursor_pos = at + token.len();
            self.input.tab_state = None;
        }
    }

    fn handle_tab(&mut self) {
        let (nicks, last_speakers): (Vec<String>, Vec<String>) =
            self.state.active_buffer().map_or_else(
                || (Vec::new(), Vec::new()),
                |buf| {
                    let nicks: Vec<String> = buf.users.values().map(|e| e.nick.clone()).collect();
                    // Filter last_speakers to only nicks still on the channel.
                    // Speakers who PARTed/QUITed stay in last_speakers for
                    // history but should not appear in tab completion.
                    let speakers: Vec<String> = buf
                        .last_speakers
                        .iter()
                        .filter(|s| {
                            let lower = s.to_lowercase();
                            buf.users.contains_key(&lower)
                        })
                        .cloned()
                        .collect();
                    (nicks, speakers)
                },
            );
        let builtin_commands = crate::commands::registry::get_command_names();
        // Include user-defined alias names in tab completion
        let alias_names: Vec<String> = self.config.aliases.keys().cloned().collect();
        let mut all_commands: Vec<&str> = builtin_commands.to_vec();
        all_commands.extend(alias_names.iter().map(String::as_str));
        all_commands.sort_unstable();
        all_commands.dedup();
        let setting_paths = crate::commands::settings::get_setting_paths(&self.config);
        self.input
            .tab_complete(&nicks, &last_speakers, &all_commands, &setting_paths);
    }

    pub(crate) fn handle_submit(&mut self, text: &str) {
        if let Some(parsed) = crate::commands::parser::parse_command(text) {
            self.execute_command_with_depth(&parsed, 0);
        } else if self.log_browser_mode {
            // Spec: log mode rejects plain text — there is no IRC
            // connection to send to, and routing through
            // `handle_plain_message` would produce the unrelated
            // "Cannot send messages to this buffer" line.
            crate::commands::helpers::add_local_event(self, "log mode: only slash commands. /help");
        } else {
            self.handle_plain_message(text);
        }
        self.scroll_offset = 0;
        // Submitted text may change state (command or sent message).
        self.script_snapshot_dirty = true;
    }

    pub(crate) fn execute_command(&mut self, parsed: &crate::commands::parser::ParsedCommand) {
        self.execute_command_with_depth(parsed, 0);
        self.script_snapshot_dirty = true;
    }

    fn execute_command_with_depth(
        &mut self,
        parsed: &crate::commands::parser::ParsedCommand,
        depth: u8,
    ) {
        if depth > MAX_ALIAS_DEPTH {
            crate::commands::helpers::add_local_event(
                self,
                &format!("Alias recursion limit reached (max {MAX_ALIAS_DEPTH})"),
            );
            return;
        }

        // Log-browser mode short-circuits the registry: the only valid
        // verbs are `/search`, `/quit`, `/help` (plus their aliases). Any
        // other command echoes a hint and returns. Scripts are unloaded
        // in log mode so we skip the script emit too.
        if self.log_browser_mode {
            match parsed.name.as_str() {
                "search" => crate::commands::handlers_logs::cmd_log_search(self, &parsed.args),
                "quit" | "exit" => {
                    crate::commands::handlers_logs::cmd_log_quit(self, &parsed.args);
                }
                "help" => crate::commands::handlers_logs::cmd_log_help(self, &parsed.args),
                other => crate::commands::helpers::add_local_event(
                    self,
                    &format!("log mode: only /search, /quit, /help (got /{other})"),
                ),
            }
            return;
        }

        // Emit to scripts — they can suppress commands
        {
            use crate::scripting::api::events;
            let mut params = HashMap::new();
            params.insert("command".to_string(), parsed.name.clone());
            params.insert("args".to_string(), parsed.args.join(" "));
            if let Some(conn_id) = self.active_conn_id() {
                params.insert("connection_id".to_string(), conn_id.to_owned());
            }
            if self.emit_script_event(events::COMMAND_INPUT, params) {
                return;
            }
        }
        let commands = crate::commands::registry::get_commands();
        // Find by name or alias (built-in commands first)
        let found = commands.iter().find(|(name, def)| {
            *name == parsed.name || def.aliases.contains(&parsed.name.as_str())
        });
        if let Some((_, def)) = found {
            (def.handler)(self, &parsed.args);
        } else if let Some(template) = self.config.aliases.get(&parsed.name).cloned() {
            // Gather context for variable expansion
            let (channel, nick, server) = self.alias_context();
            let expanded = expand_alias_template(&template, &parsed.args, &channel, &nick, &server);

            // Split by ; for command chaining
            for part in expanded.split(';').map(str::trim).filter(|s| !s.is_empty()) {
                if let Some(reparsed) = crate::commands::parser::parse_command(part) {
                    self.execute_command_with_depth(&reparsed, depth + 1);
                } else {
                    self.handle_plain_message(part);
                }
            }
        } else if self.script_manager.as_ref().is_some_and(|m| {
            let conn_id = self.state.active_buffer().map(|b| b.connection_id.as_str());
            m.handle_command(&parsed.name, &parsed.args, conn_id)
                .is_some()
        }) {
            // Script handled the command
        } else {
            crate::commands::helpers::add_local_event(
                self,
                &format!("Unknown command: /{}. Type /help for a list.", parsed.name),
            );
        }
    }

    /// Gather context variables for alias expansion (`$C`, `$N`, `$S`, `$T`).
    fn alias_context(&self) -> (String, String, String) {
        let buf = self.state.active_buffer();
        let channel = buf.map_or_else(String::new, |b| b.name.clone());
        let conn_id = buf.map_or("", |b| b.connection_id.as_str());
        let conn = self.state.connections.get(conn_id);
        let nick = conn.map_or_else(String::new, |c| c.nick.clone());
        let server = conn.map_or_else(String::new, |c| c.label.clone());
        (channel, nick, server)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "flat dispatch for DCC/channel/query message routing"
    )]
    fn handle_plain_message(&mut self, text: &str) {
        let Some(active_id) = self.state.active_buffer_id.clone() else {
            return;
        };

        let (conn_id, nick, buffer_name, buf_type) = {
            let Some(buf) = self.state.active_buffer() else {
                return;
            };
            // Only send to channels and queries, not server/status buffers
            if !matches!(
                buf.buffer_type,
                BufferType::Channel | BufferType::Query | BufferType::DccChat
            ) {
                crate::commands::helpers::add_local_event(
                    self,
                    "Cannot send messages to this buffer",
                );
                return;
            }
            let conn = self.state.connections.get(&buf.connection_id);
            let nick = conn.map(|c| c.nick.clone()).unwrap_or_default();
            (
                buf.connection_id.clone(),
                nick,
                buf.name.clone(),
                buf.buffer_type.clone(),
            )
        };

        // DCC CHAT routing: send via DCC channel, not IRC.
        if buf_type == BufferType::DccChat {
            let dcc_nick = buffer_name.strip_prefix('=').unwrap_or(&buffer_name);
            if let Some(record) = self.dcc.find_connected(dcc_nick) {
                let record_id = record.id.clone();
                if let Err(e) = self.dcc.send_chat_line(&record_id, text) {
                    crate::commands::helpers::add_local_event(
                        self,
                        &format!("DCC send error: {e}"),
                    );
                    return;
                }
                // Display locally
                let our_nick = self
                    .state
                    .connections
                    .values()
                    .next()
                    .map(|c| c.nick.clone())
                    .unwrap_or_default();
                let msg_id = self.state.next_message_id();
                self.state.add_message(
                    &active_id,
                    Message {
                        id: msg_id,
                        timestamp: chrono::Utc::now(),
                        message_type: MessageType::Message,
                        nick: Some(our_nick),
                        nick_mode: None,
                        text: text.to_string(),
                        highlight: false,
                        event_key: None,
                        event_params: None,
                        log_msg_id: None,
                        log_ref_id: None,
                        tags: None,
                    },
                );
            } else {
                crate::commands::helpers::add_local_event(
                    self,
                    "No active DCC CHAT session for this buffer",
                );
            }
            return;
        }

        // Outgoing shrink is the FIRST step for any message that
        // qualifies — by the time we reach E2E encrypt / IRC send /
        // local echo, the text is already shortened (or the original
        // on timeout). Worker handles the shrink async and posts
        // back to the main loop where `apply_shrink_deliver` runs
        // the rest of the pipeline with the substituted text.
        //
        // Capture EVERY state-dependent value at dispatch time so
        // the deferred deliver is immune to /nick, /close, +o/+v,
        // or anything else that mutates state during the shrink
        // wait — see PendingOutgoing's doc-comment for the full
        // list. The text-length cap matches the documented v1 scope
        // (multi-chunk messages fall through to the synchronous
        // path); per-chunk substitution accounting is out of scope.
        let pre_extracted_urls =
            crate::shrink::find_long_urls(text, self.state.shrink_min_url_length as usize);
        if self.config.shrink.enabled
            && self.config.shrink.outgoing_enabled
            && self.shrink_client.is_some()
            && text.len() <= crate::irc::MESSAGE_MAX_BYTES
            && !pre_extracted_urls.is_empty()
        {
            let captured_nick = self
                .state
                .connections
                .get(&conn_id)
                .map_or_else(|| nick.clone(), |c| c.nick.clone());
            let captured_own_mode = self.state.nick_prefix(&active_id, &captured_nick);
            let captured_peer_handle = if buf_type == BufferType::Query {
                self.state
                    .buffers
                    .get(&active_id)
                    .and_then(|b| b.peer_handle.clone())
            } else {
                None
            };
            let pending = crate::app::shrink::PendingOutgoing {
                conn_id: conn_id.clone(),
                buffer_id: active_id.clone(),
                buffer_name: buffer_name.clone(),
                buffer_type: buf_type.clone(),
                original_text: text.to_string(),
                urls: pre_extracted_urls,
                nick: captured_nick,
                own_mode: captured_own_mode,
                peer_handle: captured_peer_handle,
            };
            // `try_send` rather than blocking — the queue is sized
            // for bursts. Distinguish Full (transient backpressure,
            // recoverable) from Closed (permanent worker death) so
            // the operator gets a one-shot diagnostic in the
            // permanent-breakage case instead of a stream of look-
            // alike warnings. Either way we fall through to the
            // synchronous send path below with the original text.
            match self.shrink_outgoing_tx.try_send(pending) {
                Ok(()) => return,
                Err(TrySendError::Full(_)) => {
                    tracing::warn!("shrink: outgoing queue full, sending unshrunk");
                }
                Err(TrySendError::Closed(_)) => {
                    tracing::error!(
                        "shrink: outgoing worker dead, sending unshrunk \
                         (restart required to restore shrink)"
                    );
                    crate::commands::helpers::add_local_event(
                        self,
                        &format!(
                            "{err}shrink: outgoing worker has died — \
                             restart to restore. URLs will not be \
                             shortened until then.{rst}",
                            err = crate::commands::types::C_ERR,
                            rst = crate::commands::types::C_RST,
                        ),
                    );
                }
            }
        }

        // E2E: if enabled for this channel, encrypt the full text into a
        // list of RPE2E01 wire-format lines (one per chunk). Each wire line
        // already fits inside the IRC byte budget, so we skip
        // `split_irc_message`. The plaintext is still displayed locally as
        // the original (unencrypted) text — the user reads what they typed.
        //
        // For PMs the context key is `@<peer_handle>` (spec §6), so we
        // pass the buffer type through — the helper derives the
        // pseudochannel from the Query buffer's cached `peer_handle`.
        // When E2E is configured for a Query buffer but the peer's
        // `ident@host` has not been captured yet (the user has never
        // received a message from them), the helper returns `None` and
        // we surface a themed refusal rather than leaking cleartext
        // under a weak key.
        let Some((wire_lines, plain_echo)) =
            self.e2e_encrypt_or_passthrough(&active_id, &buffer_name, &buf_type, text, None)
        else {
            crate::commands::helpers::add_local_event(
                self,
                &format!(
                    "{err}[E2E] cannot encrypt PM without peer handle \
                     — wait for a message from them first{rst}",
                    err = crate::commands::types::C_ERR,
                    rst = crate::commands::types::C_RST,
                ),
            );
            return;
        };

        // When echo-message is enabled, the server will echo our message back
        // with authoritative server-time — skip local display and wait for echo.
        //
        // E2E is the exception: the server echo is the ciphertext wire,
        // and `try_decrypt_e2e` cannot decrypt our own outgoing key
        // (there is no incoming session keyed on our own handle). We
        // therefore do the local echo immediately for E2E messages
        // regardless of the cap state, and `handle_privmsg` swallows the
        // matching server echo in `is_own && starts_with "+RPE2E01"`.
        let echo_message_enabled = self
            .state
            .connections
            .get(&conn_id)
            .is_some_and(|c| c.enabled_caps.contains("echo-message"));
        let is_e2e_encrypted = wire_lines
            .first()
            .is_some_and(|w| w.starts_with("+RPE2E01"));

        let own_mode = self.state.nick_prefix(&active_id, &nick);

        for wire in wire_lines {
            // Try to send via IRC if connected
            if let Some(handle) = self.irc_handles.get(&conn_id)
                && handle.sender.send_privmsg(&buffer_name, &wire).is_err()
            {
                crate::commands::helpers::add_local_event(self, "Failed to send message");
                return;
            }
        }

        // If lazy-rotate produced REKEY sends we must ship them as NOTICEs
        // to each remaining trusted peer on this channel (spec §5.3).
        // `e2e_encrypt_or_passthrough` already pushed entries into
        // `state.pending_e2e_sends`; drain them now on the same tick so
        // the fresh session key reaches the peers before their next
        // decrypt attempt.
        if !self.state.pending_e2e_sends.is_empty() {
            self.drain_pending_e2e_sends();
        }

        if !echo_message_enabled || is_e2e_encrypted {
            // Local echo: show the user what they typed (plaintext), not the
            // wire format. For non-E2E messages we split at word boundaries
            // so very long lines wrap in the local buffer the same way they
            // used to.
            let local_chunks = if plain_echo.len() <= crate::irc::MESSAGE_MAX_BYTES {
                vec![plain_echo]
            } else {
                crate::irc::split_irc_message(&plain_echo, crate::irc::MESSAGE_MAX_BYTES)
            };
            for chunk in local_chunks {
                let id = self.state.next_message_id();
                self.state.add_message(
                    &active_id,
                    Message {
                        id,
                        timestamp: chrono::Utc::now(),
                        message_type: MessageType::Message,
                        nick: Some(nick.clone()),
                        nick_mode: own_mode.map(|c| c.to_string()),
                        text: chunk,
                        highlight: false,
                        event_key: None,
                        event_params: None,
                        log_msg_id: None,
                        log_ref_id: None,
                        tags: None,
                    },
                );
            }
        }
    }

    /// Return `Some((wire_lines, local_plain))` where `wire_lines` is what
    /// goes out on IRC (encrypted when e2e is enabled on the conversation,
    /// otherwise the plain text split at IRC byte boundaries) and
    /// `local_plain` is what is echoed into the local buffer for the user.
    ///
    /// Returns `None` to signal a hard refusal: the conversation is a PM
    /// with an E2E config enabled under the `@<peer_handle>` pseudochannel
    /// (spec §6), but the peer's `ident@host` has not been captured yet
    /// (the user has never received a message from this peer in this
    /// buffer). The caller must surface a themed error and drop the
    /// message. This prevents silently encrypting under a nick-keyed row
    /// that would collide with other peers sharing the same nick.
    ///
    /// For real IRC channels (`#&!+`) the context is the channel name,
    /// unchanged. For Query buffers the context is `@<peer_handle>`
    /// derived from the buffer's cached handle.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn e2e_encrypt_or_passthrough(
        &mut self,
        buffer_id: &str,
        buffer_name: &str,
        buffer_type: &BufferType,
        text: &str,
        // Pre-resolved Query peer_handle. Inline callers pass None
        // (live buffer is guaranteed present). The deferred shrink
        // path captures the handle at dispatch time and passes
        // Some(handle) so a `/close` during the shrink wait can't
        // make this fall through to plain_passthrough() and leak
        // ciphertext-intended plaintext on the wire.
        captured_peer_handle: Option<&str>,
    ) -> Option<(Vec<String>, String)> {
        let plain_passthrough = || {
            Some((
                crate::irc::split_irc_message(text, crate::irc::MESSAGE_MAX_BYTES),
                text.to_string(),
            ))
        };

        if text.starts_with(['.', '!']) {
            return plain_passthrough();
        }

        let Some(mgr) = self.state.e2e_manager.clone() else {
            return plain_passthrough();
        };

        // Derive the keyring context from the conversation. Channels pass
        // through unchanged; PMs require a server-stamped peer handle we
        // cached on the Query buffer at the first incoming PRIVMSG.
        //
        // CRITICAL: look the buffer up by `buffer_id` (the caller's
        // captured target), NOT `state.active_buffer()`. The deferred
        // shrink path runs this helper from a main-loop arm long after
        // the user typed the message; the active buffer may have
        // moved on. Resolving peer_handle from active_buffer would
        // encrypt under the WRONG peer's session key (or fall back to
        // plain) and produce a confidentiality regression.
        let context: String = match buffer_type {
            BufferType::Channel => buffer_name.to_string(),
            BufferType::Query => {
                // Prefer a caller-captured peer_handle (deferred
                // shrink path) over a live state lookup — the buffer
                // may have been closed during the shrink wait.
                let handle_from_state = self
                    .state
                    .buffers
                    .get(buffer_id)
                    .and_then(|b| b.peer_handle.as_deref());
                let peer_handle = captured_peer_handle.or(handle_from_state);
                let Some(peer_handle) = peer_handle else {
                    // No peer handle yet. Check whether E2E was
                    // (mis)configured under the bare-nick key from an
                    // earlier version — if a legacy enabled row exists,
                    // refuse rather than falling back to it. Otherwise
                    // there is no E2E state at all, so plain passthrough
                    // is safe.
                    let legacy_enabled = mgr
                        .keyring()
                        .get_channel_config(buffer_name)
                        .ok()
                        .flatten()
                        .is_some_and(|c| c.enabled);
                    if legacy_enabled {
                        return None;
                    }
                    return plain_passthrough();
                };
                crate::e2e::context_key(buffer_name, peer_handle)
            }
            // Server/Status/DccChat/Shell/Mentions/Special: E2E does not
            // apply. handle_plain_message already gates messaging on
            // Channel|Query|DccChat, so we only reach this arm if a new
            // sendable type is added in the future — passthrough is the
            // safe default.
            _ => return plain_passthrough(),
        };

        let enabled = mgr
            .keyring()
            .get_channel_config(&context)
            .ok()
            .flatten()
            .is_some_and(|c| c.enabled);
        if !enabled {
            return plain_passthrough();
        }
        let result = mgr.encrypt_outgoing(&context, text);

        // Drain any REKEY CTCPs produced by a lazy rotate that happened
        // inside `encrypt_outgoing`. These must go out as NOTICEs to the
        // remaining trusted peers on this channel. We resolve peer handle
        // → nick via the active buffer's user list; if the nick can't be
        // resolved (peer left the channel between handshake and rotation)
        // the distribution entry is dropped with a warning — the peer
        // will re-handshake on next ciphertext if they come back.
        let rekey_sends = mgr.take_pending_rekey_sends();
        if !rekey_sends.is_empty() {
            // Resolve the connection from the caller-passed
            // buffer_id, NOT the active buffer, so REKEY NOTICEs
            // from a deferred-shrink encrypt land on the correct
            // connection. Fall back to splitting buffer_id on '/'
            // — `make_buffer_id` joins as `conn_id/channel`, so the
            // first segment is recoverable even when the buffer was
            // closed during the shrink wait window.
            let conn_id_opt = self
                .state
                .buffers
                .get(buffer_id)
                .map(|b| b.connection_id.clone())
                .or_else(|| {
                    buffer_id
                        .split_once('/')
                        .map(|(conn_id, _)| conn_id.to_string())
                });
            if let Some(conn_id) = conn_id_opt {
                // Use the caller-passed `buffer_id` directly. For Channel
                // buffers `buffer_id` already keys to the right buffer; for
                // Query (PM) E2E the `context` we'd reconstruct from is
                // `@<peer_handle>`, which does NOT match how Query buffers
                // are stored (keyed by nick), so reconstructing via
                // `make_buffer_id(&conn_id, &context)` would always miss
                // and silently drop REKEY NOTICEs for every E2E PM.
                for rk in rekey_sends {
                    let nick = self.state.buffers.get(buffer_id).and_then(|b| {
                        b.users.values().find_map(|u| {
                            let ident = u.ident.as_deref().unwrap_or("");
                            let host = u.host.as_deref().unwrap_or("");
                            let handle = format!("{ident}@{host}");
                            if handle == rk.target_handle {
                                Some(u.nick.clone())
                            } else {
                                None
                            }
                        })
                    });
                    let Some(nick) = nick else {
                        tracing::warn!(
                            target_handle = %rk.target_handle,
                            channel = %context,
                            "rekey drop: no nick resolved for handle on current channel"
                        );
                        continue;
                    };
                    self.state
                        .pending_e2e_sends
                        .push(crate::state::PendingE2eSend {
                            connection_id: conn_id.clone(),
                            target: nick,
                            notice_text: rk.notice_text,
                        });
                }
            } else {
                tracing::warn!("rekey drop: no active buffer to resolve connection");
            }
        }

        match result {
            Ok(wires) => Some((wires, text.to_string())),
            Err(e) => {
                tracing::warn!("e2e encrypt failed on {context}: {e}; sending cleartext");
                plain_passthrough()
            }
        }
    }

    /// Get the IRC sender for the active buffer's connection, if connected.
    pub fn active_irc_sender(&self) -> Option<&::irc::client::Sender> {
        let buf = self.state.active_buffer()?;
        let handle = self.irc_handles.get(&buf.connection_id)?;
        Some(&handle.sender)
    }

    /// Get the connection ID of the active buffer.
    pub fn active_conn_id(&self) -> Option<&str> {
        self.state
            .active_buffer()
            .map(|buf| buf.connection_id.as_str())
    }

    /// Update `shell_input_active` based on the current active buffer type.
    /// Called after buffer switches to auto-enable/disable shell input mode.
    pub fn update_shell_input_state(&mut self) {
        self.shell_input_active = self
            .state
            .active_buffer()
            .is_some_and(|b| b.buffer_type == BufferType::Shell);
    }

    /// Serialize a crossterm `KeyEvent` to terminal bytes and write to the active shell PTY.
    pub(crate) fn forward_key_to_shell(&mut self, key: event::KeyEvent) {
        let Some(buf) = self.state.active_buffer() else {
            return;
        };
        let buf_id = buf.id.clone();
        let Some(shell_id) = self
            .shell_mgr
            .session_id_for_buffer(&buf_id)
            .map(ToString::to_string)
        else {
            return;
        };

        // Check if the shell has enabled application cursor mode (DECSET ?1).
        let app_cursor = self
            .shell_mgr
            .screen(&shell_id)
            .is_some_and(vt100::Screen::application_cursor);

        let bytes = key_event_to_bytes(&key, app_cursor);
        if !bytes.is_empty() {
            self.shell_mgr.write(&shell_id, &bytes);
        }
    }

    /// Forward a mouse event to the active shell PTY using SGR (mode 1006) encoding.
    /// Coordinates are translated to be relative to the shell render area.
    pub(crate) fn forward_mouse_to_shell(&mut self, mouse: event::MouseEvent, regions: &UiRegions) {
        let Some(chat_area) = regions.chat_area else {
            return;
        };
        let Some(buf) = self.state.active_buffer() else {
            return;
        };
        let buf_id = buf.id.clone();
        let Some(shell_id) = self
            .shell_mgr
            .session_id_for_buffer(&buf_id)
            .map(ToString::to_string)
        else {
            return;
        };

        // Check if the shell has enabled mouse tracking.
        let Some(screen) = self.shell_mgr.screen(&shell_id) else {
            return;
        };
        if matches!(screen.mouse_protocol_mode(), vt100::MouseProtocolMode::None) {
            return;
        }

        // Translate to shell-relative coordinates (1-based for SGR).
        let sx = mouse.column.saturating_sub(chat_area.x) + 1;
        let sy = mouse.row.saturating_sub(chat_area.y) + 1;

        // SGR encoding: CSI < button ; x ; y M/m
        let (button, suffix) = match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => (0u8, b'M'),
            MouseEventKind::Down(MouseButton::Right) => (2, b'M'),
            MouseEventKind::Down(MouseButton::Middle) => (1, b'M'),
            MouseEventKind::Up(MouseButton::Left) => (0, b'm'),
            MouseEventKind::Up(MouseButton::Right) => (2, b'm'),
            MouseEventKind::Up(MouseButton::Middle) => (1, b'm'),
            MouseEventKind::ScrollUp => (64, b'M'),
            MouseEventKind::ScrollDown => (65, b'M'),
            MouseEventKind::Drag(MouseButton::Left) => (32, b'M'),
            MouseEventKind::Drag(MouseButton::Right) => (34, b'M'),
            MouseEventKind::Drag(MouseButton::Middle) => (33, b'M'),
            MouseEventKind::Moved => (35, b'M'),
            _ => return,
        };

        let seq = format!("\x1b[<{button};{sx};{sy}{}", suffix as char);
        self.shell_mgr.write(&shell_id, seq.as_bytes());
    }

    /// Handle a dictionary download event.
    pub(crate) fn handle_dict_event(&mut self, ev: crate::spellcheck::DictEvent) {
        use crate::commands::types::{C_CMD, C_DIM, C_ERR, C_OK, C_RST, divider};
        use crate::spellcheck::DictEvent;
        let ev_fn = crate::commands::helpers::add_local_event;
        match ev {
            DictEvent::ListResult { entries } => {
                ev_fn(self, &divider("Available Dictionaries"));
                for entry in &entries {
                    let status = if entry.installed {
                        format!(" {C_OK}[installed]{C_RST}")
                    } else {
                        String::new()
                    };
                    ev_fn(
                        self,
                        &format!("  {C_CMD}{:<8}{C_RST} {}{status}", entry.code, entry.name),
                    );
                }
                ev_fn(
                    self,
                    &format!("  {C_DIM}Use /spellcheck get <lang> to download{C_RST}"),
                );
            }
            DictEvent::Downloaded { lang } => {
                ev_fn(
                    self,
                    &format!("{C_OK}Dictionary {lang} downloaded successfully{C_RST}"),
                );
                self.reload_spellchecker();
                let loaded = self
                    .spellchecker
                    .as_ref()
                    .map_or(0, crate::spellcheck::SpellChecker::dict_count);
                ev_fn(
                    self,
                    &format!("{C_OK}Spell checker reloaded ({loaded} dictionaries){C_RST}"),
                );
            }
            DictEvent::Error { message } => {
                ev_fn(self, &format!("{C_ERR}{message}{C_RST}"));
            }
        }
    }
}

/// Serialize a crossterm `KeyEvent` to terminal escape bytes for PTY input.
///
/// `app_cursor` indicates whether the shell has enabled application cursor mode
/// (DECSET ?1). When true, arrow keys use SS3 prefix (`\x1b O`) instead of
/// CSI prefix (`\x1b [`), which programs like vim/less expect.
fn key_event_to_bytes(key: &event::KeyEvent, app_cursor: bool) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    // SS3 prefix for application cursor mode, CSI for normal mode.
    let arrow_prefix: &[u8] = if app_cursor { b"\x1bO" } else { b"\x1b[" };

    match key.code {
        KeyCode::Char(c) if ctrl => {
            // Ctrl+letter → control character (0x01..0x1A).
            let byte = (c.to_ascii_lowercase() as u8)
                .wrapping_sub(b'a')
                .wrapping_add(1);
            if alt { vec![0x1b, byte] } else { vec![byte] }
        }
        KeyCode::Char(c) => {
            // Alt+char → ESC prefix (standard terminal encoding for meta key).
            let mut result = Vec::with_capacity(if alt { 5 } else { 4 });
            if alt {
                result.push(0x1b);
            }
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            result.extend_from_slice(s.as_bytes());
            result
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => vec![0x1b, b'[', b'Z'],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => [arrow_prefix, b"A"].concat(),
        KeyCode::Down => [arrow_prefix, b"B"].concat(),
        KeyCode::Right => [arrow_prefix, b"C"].concat(),
        KeyCode::Left => [arrow_prefix, b"D"].concat(),
        KeyCode::Home => [arrow_prefix, b"H"].concat(),
        KeyCode::End => [arrow_prefix, b"F"].concat(),
        KeyCode::PageUp => vec![0x1b, b'[', b'5', b'~'],
        KeyCode::PageDown => vec![0x1b, b'[', b'6', b'~'],
        KeyCode::Insert => vec![0x1b, b'[', b'2', b'~'],
        KeyCode::Delete => vec![0x1b, b'[', b'3', b'~'],
        KeyCode::F(1) => vec![0x1b, b'O', b'P'],
        KeyCode::F(2) => vec![0x1b, b'O', b'Q'],
        KeyCode::F(3) => vec![0x1b, b'O', b'R'],
        KeyCode::F(4) => vec![0x1b, b'O', b'S'],
        KeyCode::F(n @ 5..=12) => {
            // F5-F12 use CSI nn ~ encoding.
            let code = match n {
                5 => b"15",
                6 => b"17",
                7 => b"18",
                8 => b"19",
                9 => b"20",
                10 => b"21",
                11 => b"23",
                12 => b"24",
                _ => return vec![],
            };
            let mut seq = vec![0x1b, b'['];
            seq.extend_from_slice(code.as_slice());
            seq.push(b'~');
            seq
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event;

    // ── expand_alias_template tests ──

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|a| (*a).to_string()).collect()
    }

    #[test]
    fn alias_positional_args() {
        let result = expand_alias_template("/join $0", &args(&["#test"]), "", "", "");
        assert_eq!(result, "/join #test");
    }

    #[test]
    fn alias_all_args() {
        let result =
            expand_alias_template("/msg NickServ $*", &args(&["identify", "pass"]), "", "", "");
        assert_eq!(result, "/msg NickServ identify pass");
    }

    #[test]
    fn alias_auto_append_star() {
        let result =
            expand_alias_template("/msg NickServ", &args(&["identify", "pass"]), "", "", "");
        assert_eq!(result, "/msg NickServ identify pass");
    }

    #[test]
    fn alias_context_variables() {
        let result = expand_alias_template("/topic $C", &[], "#rust", "ferris", "libera");
        assert_eq!(result, "/topic #rust");
    }

    #[test]
    fn alias_context_braced_syntax() {
        let result = expand_alias_template("/msg ${N} hello from ${S}", &[], "#ch", "me", "srv");
        assert_eq!(result, "/msg me hello from srv");
    }

    #[test]
    fn alias_range_args() {
        let result = expand_alias_template(
            "/msg $0 $1-",
            &args(&["nick", "hello", "world"]),
            "",
            "",
            "",
        );
        assert_eq!(result, "/msg nick hello world");
    }

    #[test]
    fn alias_missing_positional_replaced_empty() {
        let result = expand_alias_template("/msg $0 $1", &args(&["nick"]), "", "", "");
        assert_eq!(result, "/msg nick");
    }

    #[test]
    fn alias_chaining_template() {
        let result =
            expand_alias_template("/join $0; /msg $0 hello", &args(&["#test"]), "", "", "");
        assert_eq!(result, "/join #test; /msg #test hello");
    }

    #[test]
    fn alias_empty_args_star() {
        let result = expand_alias_template("/who $C", &[], "#general", "me", "srv");
        assert_eq!(result, "/who #general");
    }

    #[test]
    fn alias_dollar_t_same_as_c() {
        let result = expand_alias_template("/topic $T", &[], "#rust", "", "");
        assert_eq!(result, "/topic #rust");
    }

    #[test]
    fn alias_range_args_out_of_bounds_returns_empty() {
        let result = expand_alias_template("/msg $0 $3-", &args(&["nick", "hello"]), "", "", "");
        assert_eq!(result, "/msg nick");
    }

    #[test]
    fn alias_range_args_at_boundary() {
        let result = expand_alias_template("/msg $0 $2-", &args(&["nick", "hello"]), "", "", "");
        assert_eq!(result, "/msg nick");
    }

    // ── key_event_to_bytes tests ──────────────────────────────────────────

    fn make_key(code: KeyCode, mods: KeyModifiers) -> event::KeyEvent {
        event::KeyEvent::new(code, mods)
    }

    #[test]
    fn key_to_bytes_char() {
        assert_eq!(
            key_event_to_bytes(&make_key(KeyCode::Char('a'), KeyModifiers::NONE), false),
            b"a"
        );
    }

    #[test]
    fn key_to_bytes_enter() {
        assert_eq!(
            key_event_to_bytes(&make_key(KeyCode::Enter, KeyModifiers::NONE), false),
            b"\r"
        );
    }

    #[test]
    fn key_to_bytes_backspace() {
        assert_eq!(
            key_event_to_bytes(&make_key(KeyCode::Backspace, KeyModifiers::NONE), false),
            vec![0x7f]
        );
    }

    #[test]
    fn key_to_bytes_ctrl_c() {
        assert_eq!(
            key_event_to_bytes(&make_key(KeyCode::Char('c'), KeyModifiers::CONTROL), false),
            vec![0x03]
        );
    }

    #[test]
    fn key_to_bytes_ctrl_d() {
        assert_eq!(
            key_event_to_bytes(&make_key(KeyCode::Char('d'), KeyModifiers::CONTROL), false),
            vec![0x04]
        );
    }

    #[test]
    fn key_to_bytes_arrow_up() {
        assert_eq!(
            key_event_to_bytes(&make_key(KeyCode::Up, KeyModifiers::NONE), false),
            vec![0x1b, b'[', b'A']
        );
    }

    #[test]
    fn key_to_bytes_arrow_down() {
        assert_eq!(
            key_event_to_bytes(&make_key(KeyCode::Down, KeyModifiers::NONE), false),
            vec![0x1b, b'[', b'B']
        );
    }

    #[test]
    fn key_to_bytes_tab() {
        assert_eq!(
            key_event_to_bytes(&make_key(KeyCode::Tab, KeyModifiers::NONE), false),
            b"\t"
        );
    }

    #[test]
    fn key_to_bytes_esc() {
        assert_eq!(
            key_event_to_bytes(&make_key(KeyCode::Esc, KeyModifiers::NONE), false),
            vec![0x1b]
        );
    }

    #[test]
    fn key_to_bytes_f1() {
        assert_eq!(
            key_event_to_bytes(&make_key(KeyCode::F(1), KeyModifiers::NONE), false),
            vec![0x1b, b'O', b'P']
        );
    }

    #[test]
    fn key_to_bytes_f5() {
        assert_eq!(
            key_event_to_bytes(&make_key(KeyCode::F(5), KeyModifiers::NONE), false),
            vec![0x1b, b'[', b'1', b'5', b'~']
        );
    }

    #[test]
    fn key_to_bytes_page_up() {
        assert_eq!(
            key_event_to_bytes(&make_key(KeyCode::PageUp, KeyModifiers::NONE), false),
            vec![0x1b, b'[', b'5', b'~']
        );
    }

    #[test]
    fn key_to_bytes_home() {
        assert_eq!(
            key_event_to_bytes(&make_key(KeyCode::Home, KeyModifiers::NONE), false),
            vec![0x1b, b'[', b'H']
        );
    }

    #[test]
    fn key_to_bytes_arrow_up_app_cursor_mode() {
        assert_eq!(
            key_event_to_bytes(&make_key(KeyCode::Up, KeyModifiers::NONE), true),
            vec![0x1b, b'O', b'A']
        );
    }

    #[test]
    fn key_to_bytes_home_app_cursor_mode() {
        assert_eq!(
            key_event_to_bytes(&make_key(KeyCode::Home, KeyModifiers::NONE), true),
            vec![0x1b, b'O', b'H']
        );
    }

    #[test]
    fn key_to_bytes_alt_char() {
        assert_eq!(
            key_event_to_bytes(&make_key(KeyCode::Char('x'), KeyModifiers::ALT), false),
            vec![0x1b, b'x']
        );
    }

    #[test]
    fn key_to_bytes_alt_ctrl_c() {
        assert_eq!(
            key_event_to_bytes(
                &make_key(
                    KeyCode::Char('c'),
                    KeyModifiers::ALT | KeyModifiers::CONTROL
                ),
                false
            ),
            vec![0x1b, 0x03]
        );
    }
}

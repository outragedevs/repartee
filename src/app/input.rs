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

/// Derive the terminal's cell pixel size (font width/height) from a window-size
/// report. Returns `None` when any dimension is zero — many terminals leave the
/// pixel fields unset, in which case we must keep the previously detected size
/// rather than divide by or compute a bogus `0`.
const fn font_size_from_window_px(
    columns: u16,
    rows: u16,
    width_px: u16,
    height_px: u16,
) -> Option<(u16, u16)> {
    if columns == 0 || rows == 0 || width_px == 0 || height_px == 0 {
        return None;
    }
    Some((width_px / columns, height_px / rows))
}

impl App {
    pub(crate) fn handle_event(&mut self, event: Event) {
        // Snapshot once, around every arm, rather than per-arm: Mouse never
        // snapshotted before (yet `handle_mouse` inserts emote text via
        // `insert_emote_by_index`), and no arm watched `active_buffer_id` — an
        // Alt+digit/arrow buffer switch left the Tui typing source attached to
        // the old buffer, which then got a spurious `paused`. `on_input_changed`
        // reads the current buffer + value and drives `on_activity`, whose
        // source-move logic releases the old target immediately, so a switch
        // with no value change still needs to run it.
        let input_before = self.input.value.clone();
        let buffer_before = self.state.active_buffer_id.clone();
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Paste(text) => self.handle_paste(&text),
            Event::Resize(cols, rows) => {
                self.cached_term_cols = cols;
                self.cached_term_rows = rows;
                self.refresh_emote_font_size();
                self.resize_all_shells();
            }
            _ => {}
        }
        if self.input.value != input_before || self.state.active_buffer_id != buffer_before {
            self.on_input_changed();
        }
    }

    /// Re-derive the terminal cell pixel size after a resize and, if it changed,
    /// rebuild the image picker so inline emotes scale to the new font size. The
    /// emote frame cache is keyed by `(emote, frame, bg)` — not by cell size — so
    /// it must be cleared, otherwise stale-sized bitmaps would keep rendering.
    ///
    /// Pixel size is read from `window_size()` (a `TIOCGWINSZ` ioctl on Unix), not
    /// by re-querying via stdio: the crossterm event stream owns stdin during the
    /// session, so a stdio query would race it. Terminals that report a font-size
    /// change as a resize event update the ioctl's pixel fields, so this catches
    /// live zoom in/out. Terminals that leave the pixel fields at 0 are left
    /// untouched (the startup-detected size stands).
    pub(crate) fn refresh_emote_font_size(&mut self) {
        let Ok(ws) = crossterm::terminal::window_size() else {
            return;
        };
        let Some(new_size) = font_size_from_window_px(ws.columns, ws.rows, ws.width, ws.height)
        else {
            return;
        };
        if new_size == self.picker.font_size() {
            return;
        }
        tracing::debug!(
            old = ?self.picker.font_size(),
            new = ?new_size,
            "refreshing picker font_size after resize"
        );
        #[expect(deprecated, reason = "only API to set font dimensions on a Picker")]
        let mut new_picker = ratatui_image::picker::Picker::from_fontsize(new_size);
        new_picker.set_protocol_type(self.picker.protocol_type());
        self.picker = new_picker;
        self.emote_animator.clear();
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
            let real_ids = self.state.numbered_buffer_ids();
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

        // Wizard overlay (top-most modal) swallows all keys while open.
        if self.wizard.is_some() {
            self.handle_wizard_key(key);
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
                self.collapse_backlog_if_at_bottom();
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
            // Alt+Enter inserts a literal newline for multi-line compose (sent
            // as one draft/multiline batch on submit). Must precede the plain
            // Enter arm, which uses a wildcard for modifiers. Works on terminals
            // that report Alt+Enter as an ALT-modified key; where they don't, it
            // degrades to a normal Enter (submit). We deliberately do NOT push
            // the Kitty keyboard-enhancement protocol to force it, since that
            // would change reporting for every key (ESC/Alt chords) and risk
            // regressing existing input handling. Paste is the universal
            // multi-line entry path.
            // Never insert a newline into a slash command: it would be routed
            // through parse_command/handlers (not the multiline-safe send path),
            // where an embedded `\n` is truncated on the wire by the IRC codec
            // while local echo shows the full text. Commands are single-line, so
            // when the input is a command this arm's guard fails and Alt+Enter
            // falls through to the plain Enter arm below (submit).
            (mods, KeyCode::Enter)
                if mods.contains(KeyModifiers::ALT)
                    && !self.input.value.trim_start().starts_with('/') =>
            {
                self.input.spell_state = None;
                self.input.insert_newline();
            }
            // Enter key, or newline chars arriving individually when bracketed
            // paste isn't supported — submit the current input line.
            (_, KeyCode::Enter | KeyCode::Char('\n' | '\r')) => {
                // Accept any active spell correction before submitting.
                self.input.spell_state = None;
                let text = self.input.submit();
                if !text.is_empty() {
                    self.submit_from_tui(&text);
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
                } else {
                    self.pin_active_backlog();
                    self.maybe_load_older_chat_backlog();
                }
            }
            (_, KeyCode::PageDown) => {
                self.scroll_offset = self.scroll_offset.saturating_sub(10);
                self.collapse_backlog_if_at_bottom();
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
        // Wizard overlay (top-most modal) captures paste: insert it into the
        // focused text field. Fields are single-line, so take the first pasted
        // line and drop control chars. Swallowed even when a non-text field is
        // focused, so paste never leaks to the input line behind the modal.
        if let Some(w) = self.wizard.as_mut() {
            if w.is_text_focused() {
                let line = text.split('\n').next().unwrap_or("").trim_end_matches('\r');
                for ch in line.chars().filter(|c| !c.is_control()) {
                    w.insert_char(ch);
                }
            }
            return;
        }

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

        // draft/multiline: when the active connection supports the cap AND the
        // paste is pure plaintext (no command lines) AND it's within the paste
        // cap, coalesce the whole paste into ONE submit so it goes out as a
        // single multiline batch. Interior blank lines are preserved; only
        // trailing blank lines are trimmed. Oversized pastes (> MAX_PASTE_LINES)
        // fall through to the legacy queued path, which truncates and throttles
        // (500ms/line) — preserving the memory/flood guard. The legacy path is
        // also required when multiline is unsupported, so no raw `\n` hits wire.
        let active_conn = self.state.active_buffer().map(|b| b.connection_id.clone());
        let any_command = lines.iter().any(|l| l.trim_start().starts_with('/'));
        // Peek (don't consume) the already-typed input: if it's a command
        // prefix (e.g. `/msg nick `), coalescing would prepend it and produce a
        // command string with embedded `\n` that `handle_submit` routes to a
        // command handler (which drops everything after the first line). Fall to
        // the legacy queued path in that case.
        let input_is_command = self.input.value.trim_start().starts_with('/');
        // Count real lines via `text.lines()` (excludes the trailing empty
        // element that `split('\n')` yields for text ending in a newline), so a
        // paste of exactly MAX_PASTE_LINES + a final newline isn't pushed to the
        // legacy path by an off-by-one.
        if !any_command
            && !input_is_command
            && text.lines().count() <= MAX_PASTE_LINES
            && active_conn
                .as_deref()
                .is_some_and(|c| self.multiline_supported(c))
        {
            let mut raw: Vec<&str> = lines.iter().map(|l| l.trim_end_matches('\r')).collect();
            while raw.last().is_some_and(|l| l.is_empty()) {
                raw.pop();
            }
            let current_input = self.input.submit();
            let joined = if current_input.is_empty() {
                raw.join("\n")
            } else {
                format!("{current_input}{}", raw.join("\n"))
            };
            if !joined.is_empty() {
                self.submit_from_tui(&joined);
            }
            return;
        }

        // Multiline paste (legacy): prepend any existing input to the first line,
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
        self.submit_from_tui(&first);

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
            self.submit_from_tui(&line);
        }
    }

    fn handle_mouse(&mut self, mouse: event::MouseEvent) {
        let Some(regions) = self.ui_regions else {
            return;
        };
        let pos = Position::new(mouse.column, mouse.row);

        // Wizard overlay (top-most modal) captures all mouse use while open.
        if self.wizard.is_some() {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.handle_wizard_click(pos);
            }
            return;
        }

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
                    } else {
                        self.pin_active_backlog();
                        self.maybe_load_older_chat_backlog();
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
                    self.collapse_backlog_if_at_bottom();
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
        let (clamped_scroll, _) = crate::ui::chat_view::resolve_scroll(
            self.buffer_list_total,
            visible_h,
            self.buffer_list_scroll,
        );
        self.buffer_list_scroll = clamped_scroll;
        let logical_row = y_offset + clamped_scroll;
        // Every numbered buffer occupies exactly one row — matches the renderer.
        let numbered = self.state.numbered_buffer_ids();
        if let Some(id) = numbered.get(logical_row) {
            let id = id.clone();
            self.state.set_active_buffer(&id);
            self.scroll_offset = 0;
            self.nick_list_scroll = 0;
            self.update_shell_input_state();
        }
    }

    fn handle_nick_list_click(&mut self, y_offset: usize) {
        use crate::state::sorting;

        // Clamp scroll the same way the renderer does.
        let visible_h = self
            .ui_regions
            .and_then(|r| r.nick_list_area)
            .map_or(0, |r| r.height as usize);
        let (clamped_scroll, _) = crate::ui::chat_view::resolve_scroll(
            self.nick_list_total,
            visible_h,
            self.nick_list_scroll,
        );
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
                pin_backlog: false,
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

        // Map the clicked row to the corresponding message, same logic as
        // chat_view render (approximated in message units — wrapped messages
        // shift the mapping; do NOT write the clamped value back here).
        let total = buf.messages.len();
        let chat_height = self
            .ui_regions
            .and_then(|r| r.chat_area)
            .map_or(0, |a| a.height as usize);
        let (_scroll, skip) = crate::ui::chat_view::resolve_scroll(
            total,
            chat_height,
            self.scroll_offset,
        );
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
        if !self.emotes_input_enabled() {
            return;
        }
        self.emote_picker = crate::ui::emote_picker::EmotePickerState::Open {
            filter: String::new(),
            selected: 0,
            cell_rects: Vec::new(),
            cols: 1,
        };
    }

    /// Handle a key while the emote picker is open.
    fn handle_emote_picker_key(&mut self, key: event::KeyEvent) {
        use crate::ui::emote_picker::EmotePickerState;
        let mut close = false;
        let mut insert_idx: Option<u32> = None;
        if let EmotePickerState::Open {
            filter,
            selected,
            cols,
            ..
        } = &mut self.emote_picker
        {
            let filtered = EmotePickerState::filtered_indices(filter);
            let len = filtered.len();
            let cols = (*cols).max(1);
            match (key.modifiers, key.code) {
                // Quit chord still works while the picker is open.
                (KeyModifiers::CONTROL, KeyCode::Char('q' | 'c')) => {
                    self.should_quit = true;
                    close = true;
                }
                (_, KeyCode::Esc) => close = true,
                (_, KeyCode::Enter) => {
                    insert_idx = filtered.get(*selected).copied();
                    close = true;
                }
                // Left/Right move within a row; Up/Down move by a grid row.
                (_, KeyCode::Left) => *selected = selected.saturating_sub(1),
                (_, KeyCode::Right) => *selected = (*selected + 1).min(len.saturating_sub(1)),
                (_, KeyCode::Up) => *selected = selected.saturating_sub(cols),
                (_, KeyCode::Down) => *selected = (*selected + cols).min(len.saturating_sub(1)),
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

    /// Open the add/edit-server wizard (`/wizard server [id]`). With `id`, opens
    /// in edit mode pre-filled from that server; without, opens a blank add form.
    pub(crate) fn open_server_wizard(&mut self, id: Option<&str>) {
        use crate::ui::wizard::{WizardMode, server::build_wizard};
        let wizard = match id {
            Some(id) => {
                let Some(server) = self.config.servers.get(id) else {
                    crate::commands::helpers::add_local_event(
                        self,
                        &format!("No server with id '{id}'"),
                    );
                    return;
                };
                build_wizard(WizardMode::Edit { id: id.to_string() }, Some(server))
            }
            None => build_wizard(WizardMode::Add, None),
        };
        self.wizard = Some(wizard);
    }

    /// Handle a key while the wizard overlay is open.
    fn handle_wizard_key(&mut self, key: event::KeyEvent) {
        use crate::ui::wizard::Focus;
        match (key.modifiers, key.code) {
            // Quit chord still works while the wizard is open.
            (KeyModifiers::CONTROL, KeyCode::Char('q' | 'c')) => {
                self.should_quit = true;
                self.wizard = None;
            }
            (_, KeyCode::Esc) => self.wizard = None,
            (_, KeyCode::Enter) => match self.wizard.as_ref().map(|w| w.focus) {
                Some(Focus::Save) => self.wizard_save(),
                Some(Focus::Cancel) => self.wizard = None,
                Some(Focus::Field(_)) => {
                    if let Some(w) = self.wizard.as_mut() {
                        w.focus_next();
                    }
                }
                None => {}
            },
            // Remaining keys only mutate the wizard's own state.
            _ => {
                if let Some(w) = self.wizard.as_mut() {
                    match (key.modifiers, key.code) {
                        (_, KeyCode::Tab | KeyCode::Down) => w.focus_next(),
                        (_, KeyCode::BackTab | KeyCode::Up) => w.focus_prev(),
                        (_, KeyCode::Left) => {
                            if w.is_select_focused() {
                                w.cycle_focused(false);
                            } else {
                                w.prev_page();
                            }
                        }
                        (_, KeyCode::Right) => {
                            if w.is_select_focused() {
                                w.cycle_focused(true);
                            } else {
                                w.next_page();
                            }
                        }
                        (_, KeyCode::Char(' ')) if w.is_toggle_focused() => w.toggle_focused(),
                        (_, KeyCode::Backspace) => w.backspace(),
                        (m, KeyCode::Char(c))
                            if !m.contains(KeyModifiers::CONTROL)
                                && !m.contains(KeyModifiers::ALT)
                                && w.is_text_focused() =>
                        {
                            w.insert_char(c);
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Validate + persist the wizard's server, or surface the error inline.
    fn wizard_save(&mut self) {
        let Some(w) = self.wizard.as_ref() else {
            return;
        };
        match crate::ui::wizard::server::build(w, &self.config.servers) {
            Ok(built) => {
                let cfg_path = crate::constants::config_path();
                let env_path = crate::constants::env_path();
                let id = built.id.clone();
                let result = crate::commands::handlers_admin::apply_server_config(
                    &mut self.config,
                    &cfg_path,
                    &env_path,
                    &built.id,
                    built.config,
                    built.password,
                    built.sasl_pass,
                );
                self.cached_config_toml = None;
                // config.servers changed in-memory; refresh the keyring's
                // legacy-adoption isolation count — see the helper.
                self.refresh_e2e_configured_networks();
                match result {
                    Ok(()) => {
                        self.wizard = None;
                        crate::commands::helpers::add_local_event(
                            self,
                            &format!("Server '{id}' saved"),
                        );
                    }
                    Err(e) => {
                        if let Some(w) = self.wizard.as_mut() {
                            w.error = Some(format!("Save failed: {e}"));
                        }
                    }
                }
            }
            Err(msg) => {
                if let Some(w) = self.wizard.as_mut() {
                    w.error = Some(msg);
                }
            }
        }
    }

    /// Handle a left click while the wizard overlay is open.
    fn handle_wizard_click(&mut self, pos: Position) {
        use crate::ui::wizard::Focus;
        enum Hit {
            Save,
            Cancel,
            Tab(usize),
            Field(usize),
            None,
        }
        // Resolve the click against recorded rects (immutable read first).
        let hit = self.wizard.as_ref().map_or(Hit::None, |w| {
            if w.save_rect.is_some_and(|r| r.contains(pos)) {
                Hit::Save
            } else if w.cancel_rect.is_some_and(|r| r.contains(pos)) {
                Hit::Cancel
            } else if let Some(&(p, _)) = w.tab_rects.iter().find(|(_, r)| r.contains(pos)) {
                Hit::Tab(p)
            } else if let Some(&(i, _)) = w.field_rects.iter().find(|(_, r)| r.contains(pos)) {
                Hit::Field(i)
            } else {
                Hit::None
            }
        });

        match hit {
            Hit::Save => self.wizard_save(),
            Hit::Cancel => self.wizard = None,
            Hit::Tab(p) => {
                if let Some(w) = self.wizard.as_mut() {
                    w.set_page(p);
                }
            }
            Hit::Field(i) => {
                if let Some(w) = self.wizard.as_mut() {
                    w.focus = Focus::Field(i);
                    w.sync_cursor_to_focus();
                    if w.is_toggle_focused() {
                        w.toggle_focused();
                    } else if w.is_select_focused() {
                        w.cycle_focused(true);
                    }
                }
            }
            Hit::None => {}
        }
    }

    /// Insert `:name:` for the registry index at the input cursor, in the
    /// configured emote language (`emotes.lang`).
    pub(crate) fn insert_emote_by_index(&mut self, index: u32) {
        let lang = self.config.emotes.lang.to_registry();
        let name = crate::emotes::display_name(index, lang);
        if name == "?" {
            return;
        }
        let token = format!(":{name}:");
        let at = self.input.cursor_pos;
        self.input.value.insert_str(at, &token);
        self.input.cursor_pos = at + token.len();
        self.input.tab_state = None;
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
        let emotes_on = self.emotes_input_enabled();
        self.input.tab_complete(
            &nicks,
            &last_speakers,
            &all_commands,
            &setting_paths,
            emotes_on,
        );
    }

    /// Returns whether a real message reached the wire **for the buffer that was
    /// active when the submit started** — what the `+typing` machine needs (see
    /// [`App::submit_from_tui`]). A command reports `false` here even when it
    /// speaks (a `/me`, a `/msg`): command handlers are `fn(&mut App, &[String])`
    /// and cannot return an outcome, so the ones that put a message on the wire
    /// call `App::note_message_sent` from `send_gated_message` — which knows the
    /// target buffer, and so also gets `/msg <other>` right (it charges the
    /// message to the OTHER buffer, not the one we typed the command in).
    pub(crate) fn handle_submit(&mut self, text: &str) -> bool {
        let sent_message = if let Some(parsed) = crate::commands::parser::parse_command(text) {
            self.execute_command_with_depth(&parsed, 0);
            false
        } else if self.log_browser_mode {
            // Spec: log mode rejects plain text — there is no IRC
            // connection to send to, and routing through
            // `handle_plain_message` would produce the unrelated
            // "Cannot send messages to this buffer" line.
            crate::commands::helpers::add_local_event(self, "log mode: only slash commands. /help");
            false
        } else {
            self.handle_plain_message(text)
        };
        self.scroll_offset = 0;
        // Submitted text may change state (command or sent message).
        self.script_snapshot_dirty = true;
        sent_message
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

    /// Whether the active connection can send `draft/multiline` batches: the
    /// limits are negotiated AND the required cap trio (`batch`, `message-tags`,
    /// and `draft/multiline` itself) is recorded as enabled. Gating on
    /// `enabled_caps` rather than just `conn.multiline.is_some()` closes the
    /// advertise-but-not-acknowledged window for a runtime `CAP NEW`.
    fn multiline_supported(&self, conn_id: &str) -> bool {
        self.state.connections.get(conn_id).is_some_and(|c| {
            c.multiline.is_some()
                && c.enabled_caps.contains("draft/multiline")
                && c.enabled_caps.contains("batch")
                && c.enabled_caps.contains("message-tags")
        })
    }

    /// Returns whether the message reached the wire. Every path that ends
    /// without a frame leaving the process — a refusal from the outbound E2E
    /// gate, a dead connection, a failed send, a buffer we cannot speak into —
    /// reports `false`, because the `+typing` machine keys BOTH the owed `done`
    /// and the §3.1 suppression window on it (see [`App::submit_from_tui`]).
    /// A partial multi-frame send counts as sent: those frames DID reach the
    /// peers, and they retract our typing exactly like a whole one.
    #[expect(
        clippy::too_many_lines,
        reason = "flat dispatch for DCC/channel/query message routing"
    )]
    fn handle_plain_message(&mut self, text: &str) -> bool {
        let Some(active_id) = self.state.active_buffer_id.clone() else {
            return false;
        };

        let (conn_id, nick, buffer_name, buf_type) = {
            let Some(buf) = self.state.active_buffer() else {
                return false;
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
                return false;
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
            let mut sent_any = false;
            let dcc_nick = buffer_name.strip_prefix('=').unwrap_or(&buffer_name);
            if let Some(record) = self.dcc.find_connected(dcc_nick) {
                let record_id = record.id.clone();
                // DCC CHAT is a line-based protocol (`send_chat_line` appends LF),
                // so a multi-line message (from paste coalescing or Alt+Enter)
                // must be sent and echoed PER logical line — never as one
                // embedded-`\n` blob, which would desync the peer's line count
                // from the single local echo.
                let our_nick = self
                    .state
                    .connections
                    .values()
                    .next()
                    .map(|c| c.nick.clone())
                    .unwrap_or_default();
                for line in text.split('\n') {
                    if let Err(e) = self.dcc.send_chat_line(&record_id, line) {
                        crate::commands::helpers::add_local_event(
                            self,
                            &format!("DCC send error: {e}"),
                        );
                        return sent_any;
                    }
                    sent_any = true;
                    let msg_id = self.state.next_message_id();
                    self.state.add_message(
                        &active_id,
                        Message {
                            id: msg_id,
                            timestamp: chrono::Utc::now(),
                            message_type: MessageType::Message,
                            nick: Some(our_nick.clone()),
                            nick_mode: None,
                            text: line.to_string(),
                            highlight: false,
                            event_key: None,
                            event_params: None,
                            log_msg_id: None,
                            log_ref_id: None,
                            tags: None,
                            wire_origin: None,
                        },
                    );
                }
            } else {
                crate::commands::helpers::add_local_event(
                    self,
                    "No active DCC CHAT session for this buffer",
                );
            }
            return sent_any;
        }

        // Same precheck as send_gated_message: the E2E gate below may
        // rotate the outgoing session and queue REKEYs, which the drain
        // DROPS without an IRC handle — and the local echo would render a
        // message that never left. Refuse up front.
        if !self.irc_handles.contains_key(&conn_id) {
            crate::commands::helpers::add_local_event(
                self,
                "Failed to send message: connection unavailable",
            );
            return false;
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
        // Never hand an E2E conversation's content to the external shortener:
        // the URL is part of the end-to-end-protected message, and the shrink
        // worker POSTs it in cleartext to a third-party API before the E2E gate
        // ever runs. Use the FAIL-CLOSED predicate, not the advisory
        // `e2e_enabled_for_target`: the advisory returns false for unresolved /
        // legacy DM state and keyring read errors, which the send gate still
        // REFUSES as E2E-enabled — shrinking on those would leak the URL to the
        // shortener before the refusal. Skip outgoing shrink whenever E2E
        // cannot be ruled out and fall through to the synchronous path, where
        // the original URL is encrypted on the wire like the rest of the message.
        let e2e_possible = self.state.e2e_possible_for_target(&conn_id, &buffer_name);

        // Outgoing translation runs BEFORE shrink and, like it, is skipped
        // entirely whenever E2E cannot be ruled out — for exactly the same
        // reason, and on the same fail-closed predicate: the translation
        // worker sends the cleartext to a third-party provider before the
        // E2E gate ever runs.
        //
        // Nothing reaches the wire here. The worker posts the outcome back
        // and `send_outgoing_translated` runs the rest of the pipeline
        // seconds later, or refuses and hands the text back to the user.
        // The whole outgoing policy lives in one place; see
        // `outgoing_translate_policy`. Once translation is REQUIRED, every
        // way of not completing it refuses — falling through would put text
        // on a channel in a language the user did not choose.
        //
        // This is the opposite policy to shrink below, which falls through by
        // design: an unshortened URL is still the message the user wrote.
        match self.outgoing_translate_policy(&active_id, text, e2e_possible) {
            crate::app::translate::OutgoingTranslatePolicy::NotApplicable => {}
            crate::app::translate::OutgoingTranslatePolicy::Refuse(reason) => {
                return self.refuse_untranslatable_send(text, reason);
            }
            crate::app::translate::OutgoingTranslatePolicy::Translate => {
                let Some(pending) = self.build_outgoing_translate(
                    &crate::app::translate::OutgoingRequest {
                        conn_id: &conn_id,
                        buffer_id: &active_id,
                        buffer_name: &buffer_name,
                        buffer_type: &buf_type,
                        nick: &nick,
                        text,
                        is_action: false,
                        // Typed into the buffer: the existing echo rule.
                        echo: crate::app::translate::OutgoingEchoPlan::BufferInput,
                    },
                ) else {
                    // The policy said translate, so this is a race (the
                    // buffer closed, or E2E was enabled between the two
                    // checks). Refuse — it is never permission to send.
                    return self.refuse_untranslatable_send(
                        text,
                        "this conversation can no longer be translated",
                    );
                };
                self.state.reserve_echo_slot(&active_id, pending.echo_id);
                let reserved_id = pending.echo_id;
                match self.translate_outgoing_tx.try_send(pending) {
                    // Nothing is on the wire yet, so we must not claim a
                    // send. The deferred path reports the outcome via
                    // `note_message_sent`.
                    Ok(()) => return false,
                    Err(TrySendError::Full(_)) => {
                        tracing::warn!("translate: outgoing queue full, refusing to send");
                        self.state.release_echo_slot(&active_id, reserved_id);
                        return self
                            .refuse_untranslatable_send(text, "the translation queue is full");
                    }
                    Err(TrySendError::Closed(_)) => {
                        tracing::error!("translate: outgoing worker dead, refusing to send");
                        self.state.release_echo_slot(&active_id, reserved_id);
                        return self.refuse_untranslatable_send(
                            text,
                            "the translation worker has died — restart to restore it",
                        );
                    }
                }
            }
        }

        if !e2e_possible
            && self.config.shrink.enabled
            && self.config.shrink.outgoing_enabled
            && self.shrink_client.is_some()
            && text.len() <= crate::irc::MESSAGE_MAX_BYTES
            && !crate::irc::multiline::needs_multiline(text)
            && !pre_extracted_urls.is_empty()
        {
            let captured_nick = self
                .state
                .connections
                .get(&conn_id)
                .map_or_else(|| nick.clone(), |c| c.nick.clone());
            let captured_own_mode = self.state.nick_prefix(&active_id, &captured_nick);
            // Resolve the FULL E2E peer handle now, while the buffer still
            // exists: its live peer_handle OR the network-scoped cached handle
            // `/e2e on` keyed its config under. Capturing only `b.peer_handle`
            // (None until the peer speaks) would let a `/close` during the
            // shrink wait strand `e2e_encrypt_or_passthrough` — it could no
            // longer recover the network to resolve the cache, falling through
            // to plaintext for an E2E-enabled DM.
            let captured_peer_handle = if buf_type == BufferType::Query {
                // A keyring read error here is logged and treated as "unresolved"
                // for the capture; the authoritative refuse-vs-plaintext decision
                // is re-made in `e2e_encrypt_or_passthrough` at send time.
                self.state.resolve_query_peer_handle(&active_id, &buffer_name)
                    .unwrap_or_else(|e| {
                        tracing::warn!(
                            "e2e: failed to resolve DM peer handle for {buffer_name}: {e}"
                        );
                        None
                    })
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
                // Nothing is on the wire YET — the worker posts the substituted
                // text back and `send_outgoing_substituted` sends it seconds
                // later, reporting the outcome itself via `note_message_sent`.
                // Claiming a send here would discard the `done` we still owe if
                // that deferred send never lands.
                Ok(()) => return false,
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
        // When E2E is configured for a Query buffer but we can't safely
        // encrypt (peer `ident@host` not known yet, keyring read failed, or
        // encryption failed), the helper returns `Err(reason)` and we surface
        // the reason-specific refusal rather than leaking cleartext.
        let (wire_lines, plain_echo) =
            match self.state.e2e_encrypt_or_passthrough(&active_id, &buffer_name, &buf_type, text, None) {
                Ok(v) => v,
                Err(reason) => {
                    crate::commands::helpers::add_local_event(self, &reason.user_message());
                    return false;
                }
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

        // --- Case B: draft/multiline outbound ---
        // Non-E2E plaintext on a connection that fully supports the cap trio,
        // where the message needs multiline (has `\n` or exceeds the per-PRIVMSG
        // cap). Frame BATCH(+ref) / @batch-tagged PRIVMSGs / BATCH(-ref). E2E and
        // the single-line hot path fall through unchanged below.
        if !is_e2e_encrypted
            && self.multiline_supported(&conn_id)
            && crate::irc::multiline::needs_multiline(text)
            && let Some(limits) = self.state.connections.get(&conn_id).and_then(|c| c.multiline)
            && let Some(batches) = crate::irc::multiline::partition(text, &limits)
        {
            let mut send_failed = false;
            let mut sent_any = false;
            'batches: for batch in &batches {
                // Allocate the ref under the mutable conn borrow FIRST, then take
                // the immutable handle borrow to send (avoids a borrow conflict).
                let Some(batch_ref) = self
                    .state
                    .connections
                    .get_mut(&conn_id)
                    .map(crate::state::connection::Connection::next_batch_ref)
                else {
                    send_failed = true;
                    break 'batches;
                };
                let frames =
                    crate::irc::multiline::multiline_frames(&buffer_name, &batch_ref, batch);
                if let Some(handle) = self.irc_handles.get(&conn_id) {
                    for frame in frames {
                        if handle.sender().send(frame).is_err() {
                            send_failed = true;
                            break 'batches;
                        }
                        sent_any = true;
                    }
                } else {
                    send_failed = true;
                    break 'batches;
                }
            }
            if send_failed {
                crate::commands::helpers::add_local_event(self, "Failed to send message");
                // Frames from an earlier batch may already be at the peers —
                // those retract our typing whether or not the rest made it.
                return sent_any;
            }
            // Local echo (echo-message off): ONE message PER BATCH so local
            // history/logging matches the wire. A multi-batch send (text longer
            // than the server's max-lines) produces several IRC messages that
            // other clients — and the echo-message path (inbound reassembly per
            // batch) — both see as separate messages; a single combined echo
            // would disagree with both.
            if !echo_message_enabled {
                for batch in &batches {
                    let id = self.state.next_message_id();
                    self.state.add_message(
                        &active_id,
                        Message {
                            id,
                            timestamp: chrono::Utc::now(),
                            message_type: MessageType::Message,
                            nick: Some(nick.clone()),
                            nick_mode: own_mode.map(|c| c.to_string()),
                            text: crate::irc::multiline::reassemble(batch),
                            highlight: false,
                            event_key: None,
                            event_params: None,
                            log_msg_id: None,
                            log_ref_id: None,
                            tags: None,
                            wire_origin: None,
                        },
                    );
                }
            }
            return sent_any;
        }

        // --- Case C: non-E2E plaintext with embedded `\n` but no usable
        // multiline cap (or an unrepresentable batch). Send each logical line as
        // a separate PRIVMSG so no raw `\n` reaches the wire (`IrcCodec::sanitize`
        // truncates at the first `\n`). Blank lines are SKIPPED — a standalone
        // empty PRIVMSG is rejected by most servers and would locally echo a
        // blank line; interior blanks are only representable inside a multiline
        // batch (Case B). This matches the legacy paste path's empty-line filter.
        if !is_e2e_encrypted && text.contains('\n') {
            let mut sent_any = false;
            for line in text.split('\n') {
                let line = line.trim_end_matches('\r');
                if line.is_empty() {
                    continue;
                }
                let chunks = if line.len() <= crate::irc::MESSAGE_MAX_BYTES {
                    vec![line.to_string()]
                } else {
                    crate::irc::split_irc_message(line, crate::irc::MESSAGE_MAX_BYTES)
                };
                for chunk in chunks {
                    if let Some(handle) = self.irc_handles.get(&conn_id) {
                        if handle.sender().send_privmsg(&buffer_name, &chunk).is_err() {
                            crate::commands::helpers::add_local_event(
                                self,
                                "Failed to send message",
                            );
                            return sent_any;
                        }
                        sent_any = true;
                    }
                    if !echo_message_enabled {
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
                                wire_origin: None,
                            },
                        );
                    }
                }
            }
            return sent_any;
        }

        // --- Else: E2E, or single-line plaintext (the common hot path). ---
        let mut sent_any = false;
        for wire in wire_lines {
            // Try to send via IRC if connected
            if let Some(handle) = self.irc_handles.get(&conn_id) {
                if handle.sender().send_privmsg(&buffer_name, &wire).is_err() {
                    crate::commands::helpers::add_local_event(self, "Failed to send message");
                    return sent_any;
                }
                sent_any = true;
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
            if is_e2e_encrypted {
                // E2E: echo the full plaintext as ONE message so embedded
                // newlines render as a single local message (matching the one
                // logical message the user typed). The peer still receives the
                // E2E chunker's per-chunk wire (no E2E reassembly — out of scope).
                let id = self.state.next_message_id();
                self.state.add_message(
                    &active_id,
                    Message {
                        id,
                        timestamp: chrono::Utc::now(),
                        message_type: MessageType::Message,
                        nick: Some(nick),
                        nick_mode: own_mode.map(|c| c.to_string()),
                        text: plain_echo,
                        highlight: false,
                        event_key: None,
                        event_params: None,
                        log_msg_id: None,
                        log_ref_id: None,
                        tags: None,
                        wire_origin: None,
                    },
                );
            } else {
                // Non-E2E single line: existing byte-split echo so very long
                // lines wrap in the local buffer the same way they used to.
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
                            wire_origin: None,
                        },
                    );
                }
            }
        }
        sent_any
    }

    /// Get the IRC sender for the active buffer's connection, if connected.
    ///
    /// The returned [`crate::irc::IrcSender`] charges the connection's flood
    /// budget on every send — there is no way to reach the socket around it.
    pub fn active_irc_sender(&self) -> Option<&crate::irc::IrcSender> {
        let buf = self.state.active_buffer()?;
        let handle = self.irc_handles.get(&buf.connection_id)?;
        Some(handle.sender())
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

    // ── font_size_from_window_px tests ──

    #[test]
    fn font_size_divides_pixels_by_cells() {
        // 80×24 cells over a 640×384px window => an 8×16px cell.
        assert_eq!(font_size_from_window_px(80, 24, 640, 384), Some((8, 16)));
    }

    #[test]
    fn font_size_tracks_a_larger_font() {
        // Same grid, a zoomed-in font reports a bigger pixel window => bigger cell.
        assert_eq!(font_size_from_window_px(80, 24, 800, 480), Some((10, 20)));
    }

    #[test]
    fn font_size_none_when_pixels_unreported() {
        // Terminals that leave the pixel fields at 0 must yield None, not a 0 cell.
        assert_eq!(font_size_from_window_px(80, 24, 0, 0), None);
        assert_eq!(font_size_from_window_px(80, 24, 640, 0), None);
        assert_eq!(font_size_from_window_px(80, 24, 0, 384), None);
    }

    #[test]
    fn font_size_none_when_grid_is_zero() {
        assert_eq!(font_size_from_window_px(0, 24, 640, 384), None);
        assert_eq!(font_size_from_window_px(80, 0, 640, 384), None);
    }

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

/// The submit → `+typing` hook, driven through the REAL submit path.
///
/// The bug these exist for cannot be caught below this level: the state
/// machine (`app::typing`) is already proven to keep an owed `done` when told
/// `sent_message = false` — what was wrong was the *value* handed to it. It
/// was `should_type(text)`, a predicate over the text the user typed, so a
/// message that was composed and then REFUSED (E2E gate, dead connection,
/// failed send) still reported "sent". Only a test that runs the actual
/// `handle_submit` can tell the two apart.
///
/// `App::new` touches disk (config dir, theme, storage, terminal query), so
/// the fixture builds the struct directly — the same reason `send_typing_frame`
/// exists as a free function over borrowed state.
#[cfg(test)]
pub mod submit_typing_tests {
    #![allow(clippy::unwrap_used, reason = "test code")]

    use super::{App, BufferType};
    use crate::app::typing::TypingSource;
    use crate::irc::handle::{FLOOD_PENALTY_THRESHOLD_MS, IrcHandle, IrcSender};
    use crate::irc::typing::TypingState;
    use crate::state::buffer::Buffer;
    use crate::state::connection::{Connection, ConnectionStatus};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;
    use tokio::sync::mpsc;

    /// An `App` on a registered connection `net`, with `#rust` (channel) and
    /// `bob` (query) open and a capturing sender behind the IRC handle.
    struct Wired {
        app: App,
        sender: IrcSender,
    }

    impl Wired {
        fn new() -> Self {
            let mut app = test_app();
            app.state.add_connection(make_connection());
            app.state.add_buffer(make_buffer("#rust", BufferType::Channel));
            app.state.add_buffer(make_buffer("bob", BufferType::Query));
            let sender = IrcSender::capturing(u64::from(FLOOD_PENALTY_THRESHOLD_MS));
            app.irc_handles.insert(
                "net".to_string(),
                IrcHandle::new("net".to_string(), sender.clone(), None, None),
            );
            Self { app, sender }
        }

        /// A submit from the terminal, exactly as the Enter key does it.
        fn submit(&mut self, text: &str) {
            self.app.submit_from_tui(text);
        }

        /// Exactly what left the process, as wire lines.
        fn wire(&self) -> Vec<String> {
            self.sender
                .captured()
                .iter()
                .map(ToString::to_string)
                .collect()
        }

        /// The state we last put on the wire for `buffer_id` — `Some` means a
        /// retraction is still OWED to the peers.
        fn owed(&self, buffer_id: &str) -> Option<TypingState> {
            self.app.typing.sent_state(buffer_id)
        }

        /// Whether the §3.1 "we just said something" window is open — it mutes
        /// typing in this buffer for 3s, so a phantom one silences a real retype.
        fn suppressed(&self, buffer_id: &str) -> bool {
            self.app.typing.last_message_at(buffer_id).is_some()
        }

        /// Enable E2E on the DM under a nick-keyed row while the peer's
        /// `ident@host` stays unknown — the ordinary state of an `/e2e on` query
        /// before the peer has spoken this session. The gate refuses
        /// (`E2eRefusal::NoPeerHandle`) rather than leak plaintext.
        fn enable_e2e_on_bob(&self) {
            self.app
                .state
                .e2e_manager
                .as_ref()
                .unwrap()
                .keyring()
                .set_channel_config(&crate::e2e::keyring::ChannelConfig {
                    channel: "bob".to_string(),
                    enabled: true,
                    mode: crate::e2e::keyring::ChannelMode::Normal,
                })
                .unwrap();
        }
    }

    #[test]
    fn an_e2e_refused_submit_still_owes_the_typing_done() {
        // THE REGRESSION. `/query bob`, `/e2e on`, bob has not spoken yet.
        // Alice types (an `active` goes out), pauses (a `paused` goes out), then
        // hits Enter. The E2E gate REFUSES — nothing reaches the wire — so the
        // `paused` is still the last thing bob heard: the `done` is owed. The
        // old hook read the TEXT (`should_type`), called it a sent message, and
        // threw the owed `done` away — bob showed "alice is typing…" for the
        // full 30s paused TTL, and alice's retype was muted for 3s on top.
        let mut w = Wired::new();
        w.enable_e2e_on_bob();
        w.app.state.set_active_buffer("net/bob");
        w.app
            .typing
            .confirm_sent("net/bob", TypingState::Paused, Instant::now());

        w.submit("are you there?");

        assert!(
            w.wire().is_empty(),
            "the E2E gate refused: nothing may reach the wire"
        );
        assert_eq!(
            w.owed("net/bob"),
            Some(TypingState::Paused),
            "a refused send retracts nothing, so the done is still owed"
        );
        assert!(
            !w.suppressed("net/bob"),
            "a message that was never sent must not open the 3s suppression window"
        );
    }

    #[test]
    fn a_send_on_a_dead_connection_still_owes_the_typing_done() {
        // Same class, no E2E: the handle is gone, `handle_plain_message` prints
        // "connection unavailable" and returns without sending.
        let mut w = Wired::new();
        w.app.irc_handles.clear();
        w.app.state.set_active_buffer("net/#rust");
        w.app
            .typing
            .confirm_sent("net/#rust", TypingState::Active, Instant::now());

        w.submit("hello");

        assert!(w.wire().is_empty());
        assert_eq!(w.owed("net/#rust"), Some(TypingState::Active));
        assert!(!w.suppressed("net/#rust"));
    }

    #[test]
    fn a_message_that_reaches_the_wire_retires_the_target_and_suppresses_typing() {
        // The other half: a real PRIVMSG clears typing at the receivers all by
        // itself, so no `done` is owed — and §3.1 mutes typing here for 3s.
        let mut w = Wired::new();
        w.app.state.set_active_buffer("net/#rust");
        w.app
            .typing
            .confirm_sent("net/#rust", TypingState::Active, Instant::now());

        w.submit("hello");

        assert_eq!(w.wire(), vec!["PRIVMSG #rust hello\r\n"]);
        assert_eq!(
            w.owed("net/#rust"),
            None,
            "the PRIVMSG is the retraction — nothing is owed"
        );
        assert!(w.suppressed("net/#rust"), "§3.1 window must be open");
    }

    #[test]
    fn a_local_command_charges_nothing_even_when_it_writes_to_the_wire() {
        // `/whois` puts a WHOIS on the wire but says nothing to the CHANNEL, so
        // it neither retracts our typing there nor announces us: the `done` is
        // still owed and no suppression window opens. (The gate is not "did any
        // byte leave" — it is "did a message to THIS target leave".)
        let mut w = Wired::new();
        w.app.state.set_active_buffer("net/#rust");
        w.app
            .typing
            .confirm_sent("net/#rust", TypingState::Active, Instant::now());

        w.submit("/whois bob");

        assert!(
            w.wire().iter().any(|l| l.starts_with("WHOIS")),
            "the command itself must still run: {:?}",
            w.wire()
        );
        assert_eq!(w.owed("net/#rust"), Some(TypingState::Active));
        assert!(!w.suppressed("net/#rust"));
    }

    #[test]
    fn an_action_counts_as_a_message_only_when_it_reaches_the_wire() {
        // `/me` is the one command that speaks INTO the active buffer, so a
        // successful one retires the target like any message — and a refused one
        // (E2E, same gate as above) must not.
        let mut w = Wired::new();
        w.app.state.set_active_buffer("net/#rust");
        let now = Instant::now();
        w.app.typing.confirm_sent("net/#rust", TypingState::Active, now);

        w.submit("/me waves");

        assert_eq!(w.wire(), vec!["PRIVMSG #rust :\u{1}ACTION waves\u{1}\r\n"]);
        assert_eq!(w.owed("net/#rust"), None);
        assert!(w.suppressed("net/#rust"));

        // The refused twin.
        w.enable_e2e_on_bob();
        w.app.state.set_active_buffer("net/bob");
        w.app.typing.confirm_sent("net/bob", TypingState::Paused, now);

        w.submit("/me waves");

        assert_eq!(w.wire().len(), 1, "the refused ACTION must not reach bob");
        assert_eq!(w.owed("net/bob"), Some(TypingState::Paused));
        assert!(!w.suppressed("net/bob"));
    }

    #[test]
    fn the_typing_update_lands_on_the_buffer_we_typed_in_not_the_one_we_end_up_in() {
        // A submit can MOVE the active buffer: `/query bob` opens and switches.
        // The typing state we owe belongs to the buffer the text was typed in.
        let mut w = Wired::new();
        w.app.state.set_active_buffer("net/#rust");
        w.app
            .typing
            .confirm_sent("net/#rust", TypingState::Active, Instant::now());

        w.submit("/query bob");

        assert_eq!(w.app.state.active_buffer_id.as_deref(), Some("net/bob"));
        assert_eq!(
            w.owed("net/#rust"),
            Some(TypingState::Active),
            "the done owed to #rust must not follow us into the query"
        );
        assert!(!w.suppressed("net/#rust"));
        assert!(!w.suppressed("net/bob"));
    }

    // ── A buffer switch that no keystroke drove ──
    //
    // `handle_event` is not the only thing that moves `active_buffer_id`, and the
    // TUI has ONE input box with no per-buffer drafts. Every one of these paths
    // leaves the terminal's draft pointing at a NEW buffer, so the Tui typing
    // source has to follow it or the 1s tick keeps announcing us to the old one.

    /// Type `hello wor` into the currently active buffer, exactly as the keyboard
    /// would, and confirm the source is attached where we think it is.
    fn start_typing_in(w: &mut Wired, buffer_id: &str) {
        w.app.state.set_active_buffer(buffer_id);
        w.app.input.value = "hello wor".to_string();
        w.app.on_input_changed();
        assert_eq!(
            w.app.typing.source_buffer(&TypingSource::Tui),
            Some(buffer_id)
        );
        assert_eq!(w.app.typing.sent_state(buffer_id), Some(TypingState::Active));
    }

    /// What the abandoned buffer is told once its 3s throttle window opens.
    ///
    /// This is where the damage actually shows. The `done` owed to the old buffer
    /// is throttled at the instant of the switch (the spec's 3s window binds
    /// `done` too), so nothing reaches the wire yet — what matters is what the
    /// NEXT tick decides. With the source stranded it re-proposes `active`/
    /// `paused` and keeps the old buffer's peers showing "typing…" for the full
    /// TTL; with the source moved it proposes exactly one `done`.
    fn what_the_next_tick_says_to(w: &mut Wired, buffer_id: &str) -> Vec<TypingState> {
        w.app
            .typing
            .on_tick(Instant::now() + std::time::Duration::from_secs(4))
            .into_iter()
            .filter(|(id, _)| id == buffer_id)
            .map(|(_, state)| state)
            .collect()
    }

    #[test]
    fn a_web_buffer_switch_moves_the_tui_typing_source() {
        // THE REGRESSION. You type `hello wor` in #rust in the terminal, then tap
        // the `bob` tab on your phone. `WebCommand::SwitchBuffer` flips the GLOBAL
        // active buffer, so the terminal now shows `bob` with your draft still in
        // the box — but nothing told the typing machine, so its Tui source stayed
        // on #rust and the tick kept refreshing #rust for the full 30s TTL while
        // `bob` was told nothing at all.
        let mut w = Wired::new();
        start_typing_in(&mut w, "net/#rust");

        w.app.handle_web_command(
            crate::web::protocol::WebCommand::SwitchBuffer {
                buffer_id: "net/bob".to_string(),
            },
            "tab-1",
        );
        // What the main loop does after every dispatch, whatever the dispatch was.
        w.app.sync_tui_typing_source();

        assert_eq!(
            w.app.typing.source_buffer(&TypingSource::Tui),
            Some("net/bob"),
            "the terminal's draft is composed against bob now — the source must follow"
        );
        assert_eq!(
            what_the_next_tick_says_to(&mut w, "net/#rust"),
            vec![TypingState::Done],
            "#rust must be retracted, not refreshed"
        );
    }

    #[test]
    fn every_other_non_keyboard_switch_moves_the_source_too() {
        // `set_active_buffer` is the whole of what `/window` from a Lua script
        // (`app/scripting.rs`), an accepted DCC chat (`app/dcc.rs`) and an IRC
        // event opening a query (`irc/events.rs`) do — and the last of those has
        // only `&mut AppState`, so it could not call into the typing machine even
        // if it wanted to. The loop-level sync is what covers all three.
        let mut w = Wired::new();
        start_typing_in(&mut w, "net/#rust");

        w.app.state.set_active_buffer("net/bob");
        w.app.sync_tui_typing_source();

        assert_eq!(
            w.app.typing.source_buffer(&TypingSource::Tui),
            Some("net/bob")
        );
        assert_eq!(
            what_the_next_tick_says_to(&mut w, "net/#rust"),
            vec![TypingState::Done]
        );
    }

    #[test]
    fn reload_runs_the_same_post_config_typing_sync_that_set_does() {
        // THE REGRESSION. `/reload` replaces `app.config` wholesale — every bit
        // as much a config change as `/set` — but it only re-synced `typing.show`.
        //
        // Hand-edit `[typing] send_channels = false` and `/reload` while a `sent`
        // is outstanding on #rust: `typing_send_target` now returns `None` for
        // every channel, so `send_typing` refuses, `confirm_sent` never runs, and
        // `prune` retains any target with `sent.is_some()`. The machine proposes
        // `Done`, the guard re-refuses it, forever — and the `done` owed to
        // #rust's peers is never sent. Verbatim the defect that
        // `forget_switched_off_buffers` exists to prevent.
        let mut w = Wired::new();
        start_typing_in(&mut w, "net/#rust");

        let mut edited = crate::config::AppConfig::default();
        edited.typing.send_channels = false;
        crate::commands::handlers_admin::apply_reloaded_config(&mut w.app, edited);

        assert_eq!(
            w.app.typing.sent_state("net/#rust"),
            None,
            "the switched-off channel must be forgotten, not left holding `sent`"
        );
        for secs in [4, 8, 600] {
            assert!(
                what_the_next_tick_says_to(&mut w, "net/#rust").is_empty(),
                "a switched-off channel must never be re-proposed (t+{secs}s)"
            );
        }
    }

    #[test]
    fn a_transient_swap_and_restore_leaves_the_source_alone() {
        // `app/shrink.rs` and `commands/handlers_e2e.rs` borrow `active_buffer_id`
        // for one synchronous call and put it straight back. The loop-level sync
        // compares rather than hooks the setter, so it sees no change at all — a
        // setter hook would have fired twice and reset the pause clock.
        let mut w = Wired::new();
        start_typing_in(&mut w, "net/#rust");
        let before = w.wire().len();

        let prior = w.app.state.active_buffer_id.clone();
        w.app.state.active_buffer_id = Some("net/bob".to_string());
        w.app.state.active_buffer_id = prior;
        w.app.sync_tui_typing_source();

        assert_eq!(
            w.app.typing.source_buffer(&TypingSource::Tui),
            Some("net/#rust")
        );
        assert_eq!(w.wire().len(), before, "nothing new reached the wire");
    }

    #[test]
    fn closing_the_active_buffer_releases_the_tui_source() {
        // The log browser drops `active_buffer_id` to `None` (`app/log_browser.rs`).
        // The terminal is composing into nothing, so the source must be dropped —
        // not left attached to a buffer that keeps getting refreshed forever.
        let mut w = Wired::new();
        start_typing_in(&mut w, "net/#rust");

        w.app.state.active_buffer_id = None;
        w.app.sync_tui_typing_source();

        assert_eq!(w.app.typing.source_buffer(&TypingSource::Tui), None);
        assert_eq!(
            what_the_next_tick_says_to(&mut w, "net/#rust"),
            vec![TypingState::Done],
            "#rust must be retracted, not refreshed forever"
        );
    }

    #[test]
    fn a_web_submit_reports_the_real_outcome_too() {
        // `WebCommand::SendMessage` runs the same submit path; a browser session
        // is its own typing source, and a refused send must leave ITS owed `done`
        // outstanding exactly like the terminal's.
        let mut w = Wired::new();
        w.enable_e2e_on_bob();
        let now = Instant::now();
        w.app.typing.confirm_sent("net/bob", TypingState::Paused, now);
        w.app.on_web_typing("s1", "net/bob", true);

        w.app.handle_web_command(
            crate::web::protocol::WebCommand::SendMessage {
                buffer_id: "net/bob".to_string(),
                text: "are you there?".to_string(),
            },
            "s1",
        );

        assert!(w.wire().is_empty(), "the E2E gate refused");
        assert_eq!(w.owed("net/bob"), Some(TypingState::Paused));
        assert!(!w.suppressed("net/bob"));

        // ...and a web message that DOES reach the wire behaves like the TUI's.
        w.app.handle_web_command(
            crate::web::protocol::WebCommand::SendMessage {
                buffer_id: "net/#rust".to_string(),
                text: "hello".to_string(),
            },
            "s1",
        );
        assert_eq!(w.wire(), vec!["PRIVMSG #rust hello\r\n"]);
        assert_eq!(w.owed("net/#rust"), None);
        assert!(w.suppressed("net/#rust"));
        // The web source itself was released by the submit.
        assert!(
            w.app
                .typing
                .source_is_active(&TypingSource::Web("s1".to_string()))
                == Some(false),
            "the submitting source must be marked idle"
        );
    }

    // ── fixtures ──

    fn make_buffer(name: &str, buffer_type: BufferType) -> Buffer {
        Buffer {
            id: format!("net/{name}"),
            connection_id: "net".to_string(),
            buffer_type,
            name: name.to_string(),
            messages: std::collections::VecDeque::new(),
            activity: crate::state::buffer::ActivityLevel::None,
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
            pin_backlog: false,
        }
    }

    /// Also used by the translate tests, which need a connection whose
    /// `enabled_caps` they can set.
    pub fn make_connection() -> Connection {
        Connection {
            id: "net".to_string(),
            label: "NetServer".to_string(),
            status: ConnectionStatus::Connected,
            own_handle: None,
            nick: "me".to_string(),
            user_modes: String::new(),
            isupport: HashMap::new(),
            isupport_parsed: crate::irc::isupport::Isupport::new(),
            error: None,
            lag: None,
            lag_pending: false,
            reconnect_attempts: 0,
            reconnect_delay_secs: 30,
            next_reconnect: None,
            should_reconnect: true,
            joined_channels: Vec::new(),
            origin_config: crate::config::ServerConfig {
                label: "NetServer".to_string(),
                address: "irc.test.net".to_string(),
                port: 6697,
                tls: true,
                tls_verify: true,
                autoconnect: false,
                channels: vec![],
                nick: None,
                username: None,
                realname: None,
                password: None,
                sasl_user: None,
                sasl_pass: None,
                bind_ip: None,
                encoding: None,
                auto_reconnect: Some(true),
                reconnect_delay: None,
                reconnect_max_retries: None,
                autosendcmd: None,
                sasl_mechanism: None,
                client_cert_path: None,
                sasl_key_path: None,
            },
            local_ip: None,
            enabled_caps: std::collections::HashSet::from(["message-tags".to_string()]),
            chathistory: crate::irc::chathistory::HistoryState::new(),
            who_token_counter: 0,
            silent_who_channels: std::collections::HashSet::new(),
            silent_banlist_channels: std::collections::HashSet::new(),
            multiline: None,
            batch_ref_counter: 0,
        }
    }

    /// `App` with every service inert: no storage, no scripts, no shrink client,
    /// no terminal. Only the pieces the submit path reads are real — the state,
    /// the config, the IRC handles and the typing machine.
    #[expect(
        clippy::too_many_lines,
        reason = "one line per App field — a struct literal cannot be shortened"
    )]
    pub fn test_app() -> App {
        let mut state = crate::state::AppState::new();
        let db = crate::storage::db::open_database(false).unwrap();
        let keyring = crate::e2e::keyring::Keyring::new(Arc::new(Mutex::new(db)));
        state.e2e_manager = Some(Arc::new(
            crate::e2e::manager::E2eManager::load_or_init(keyring).unwrap(),
        ));

        let config = crate::config::AppConfig::default();
        let (irc_tx, irc_rx) = mpsc::channel(16);
        let (preview_tx, preview_rx) = mpsc::channel(16);
        let (dict_tx, dict_rx) = mpsc::channel(16);
        let (web_cmd_tx, web_cmd_rx) = mpsc::channel(16);
        let (script_action_tx, script_action_rx) = mpsc::channel(16);
        let (dcc, dcc_rx) = crate::dcc::DccManager::new();
        let (shell_mgr, shell_rx) = crate::shell::ShellManager::new();
        let script_state = Arc::new(std::sync::RwLock::new(state.script_snapshot()));
        // Hand-rolled instead of `ShrinkRuntime::build`, which spawns its
        // drain/worker tasks and so needs a tokio reactor. `shrink_client:
        // None` is what makes the submit path skip the deferred shrink
        // entirely, so nothing here is ever read.
        let (shrink_outgoing_tx, _shrink_outgoing_rx) = mpsc::channel(16);
        let (shrink_deliver_tx, shrink_deliver_rx) = mpsc::channel(16);
        // Same reasoning as shrink above: hand-rolled so no tokio reactor is
        // needed. `state.translate_active` stays false, so the submit path
        // never dispatches and these are never read.
        let (translate_outgoing_tx, _translate_outgoing_rx) = mpsc::channel(16);
        let (translate_deliver_tx, translate_deliver_rx) = mpsc::channel(16);

        App {
            state,
            config,
            theme: crate::theme::loader::default_theme(),
            input: crate::ui::input::InputState::new(),
            should_quit: false,
            script_snapshot_dirty: false,
            splash_visible: 0,
            splash_done: true,
            scroll_offset: 0,
            chat_scroll_at_top: false,
            ui_regions: None,
            irc_handles: HashMap::new(),
            forwarder_handles: HashMap::new(),
            irc_tx,
            irc_rx,
            last_esc_time: None,
            buffer_list_scroll: 0,
            buffer_list_total: 0,
            nick_list_scroll: 0,
            nick_list_total: 0,
            lag_pings: HashMap::new(),
            batch_trackers: HashMap::new(),
            storage: None,
            last_event_purge: Instant::now(),
            last_mention_purge: Instant::now(),
            quit_message: None,
            image_preview: crate::image_preview::PreviewStatus::default(),
            image_clear_rect: None,
            preview_rx,
            preview_tx,
            http_client: reqwest::Client::new(),
            picker: ratatui_image::picker::Picker::halfblocks(),
            in_tmux: false,
            emote_placements: Vec::new(),
            emote_animator: crate::app::emote_anim::EmoteAnimator::default(),
            emote_anim_start: Instant::now(),
            emote_picker: crate::ui::emote_picker::EmotePickerState::default(),
            wizard: None,
            needs_full_redraw: false,
            outer_terminal: "xterm".to_string(),
            color_support: crate::nick_color::ColorSupport::TrueColor,
            image_proto_source: "test".to_string(),
            shim_term_env: None,
            channel_query_queues: HashMap::new(),
            channel_query_in_flight: HashMap::new(),
            channel_query_sent_at: HashMap::new(),
            paste_queue: std::collections::VecDeque::new(),
            script_manager: None,
            script_api: None,
            script_state,
            script_action_rx,
            script_commands: HashMap::new(),
            script_config: HashMap::new(),
            active_timers: HashMap::new(),
            script_action_tx,
            wrap_indent: 0,
            cached_config_toml: None,
            terminal: None,
            detached: false,
            should_detach: false,
            log_browser_mode: false,
            log_db: None,
            socket_listener: None,
            socket_output_tx: None,
            shim_event_rx: None,
            is_socket_attached: false,
            term_reader_stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            term_rx: None,
            shim_output_handle: None,
            shim_input_handle: None,
            cached_term_cols: 80,
            cached_term_rows: 24,
            dcc,
            dcc_rx,
            shell_mgr,
            shell_rx,
            shell_input_active: false,
            last_shell_web_broadcast: Instant::now(),
            shell_broadcast_pending: None,
            spellchecker: None,
            dict_rx,
            dict_tx,
            web_broadcaster: Arc::new(crate::web::broadcast::WebBroadcaster::new(16)),
            web_cmd_rx,
            web_cmd_tx,
            web_server_handle: None,
            web_sessions: None,
            web_rate_limiter: None,
            web_state_snapshot: None,
            web_active_buffers: HashMap::new(),
            web_restart_pending: false,
            last_day: chrono::Local::now().date_naive(),
            shrink_client: None,
            shrink_cache: Arc::new(parking_lot::Mutex::new(crate::shrink::ShrinkCache::new(4))),
            shrink_outgoing_tx,
            shrink_deliver_tx,
            shrink_deliver_rx,
            translate_outgoing_tx,
            translate_deliver_rx,
            translate_deliver_tx,
            translate_backend: None,
            translate_in_flight: None,
            translate_in_flight_applied: 0,
            translate_in_flight_debt: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            submit_origin: crate::app::translate::SubmitOrigin::Tui,
            conn_generations: std::collections::HashMap::new(),
            translate_timeout_ms: None,
            // NEVER the real config path: a handler that saves would clobber
            // the developer's own configuration during `cargo test`.
            config_path: std::env::temp_dir().join(format!(
                "{}-test-config-do-not-use.toml",
                crate::constants::APP_NAME
            )),
            cli_bind_override: None,
            typing: crate::app::typing::TypingSender::default(),
        }
    }
}

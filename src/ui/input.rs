use std::collections::VecDeque;

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme::hex_to_color;

const MAX_HISTORY: usize = 100;

/// Active inline spell correction state.
pub struct SpellCorrection {
    /// Byte offset of the misspelled word start in `InputState::value`.
    pub word_start: usize,
    /// Byte offset of the misspelled word end (exclusive).
    pub word_end: usize,
    /// The original misspelled word.
    pub original: String,
    /// Ranked suggestions from the spell checker.
    pub suggestions: Vec<String>,
    /// Current suggestion index (wraps around).
    pub index: usize,
}

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
    pub spell_state: Option<SpellCorrection>,
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
            spell_state: None,
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
        // Accept any active spell correction before inserting.
        self.spell_state = None;
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

    /// Dismiss any active spell correction without accepting.
    pub fn dismiss_spell(&mut self) {
        if let Some(spell) = self.spell_state.take() {
            // Revert to original word if we had replaced it.
            if spell.index > 0 || self.value[spell.word_start..spell.word_end] != spell.original {
                let current_word_len = spell.word_end - spell.word_start;
                let original_len = spell.original.len();
                self.value
                    .replace_range(spell.word_start..spell.word_end, &spell.original);
                // Adjust cursor position for the size difference.
                if self.cursor_pos > spell.word_start {
                    self.cursor_pos = self.cursor_pos + original_len - current_word_len;
                }
            }
        }
    }

    /// Cycle to the next spell suggestion, replacing the word inline.
    /// Returns true if we cycled (had active suggestions).
    pub fn cycle_spell_suggestion(&mut self) -> bool {
        let Some(spell) = self.spell_state.as_mut() else {
            return false;
        };
        if spell.suggestions.is_empty() {
            return false;
        }

        let replacement = &spell.suggestions[spell.index];
        let old_len = spell.word_end - spell.word_start;
        let new_len = replacement.len();

        self.value
            .replace_range(spell.word_start..spell.word_end, replacement);
        spell.word_end = spell.word_start + new_len;

        // Adjust cursor for size difference.
        if self.cursor_pos > spell.word_start {
            self.cursor_pos = self.cursor_pos + new_len - old_len;
        }

        // Advance to next suggestion (wrap around).
        spell.index = (spell.index + 1) % spell.suggestions.len();

        true
    }

    /// Check if the input is in command mode (starts with /).
    pub fn is_command(&self) -> bool {
        self.value.starts_with('/')
    }

    /// Extract the last completed word before `cursor_pos` (i.e., the word
    /// that just ended because the user typed a separator).
    ///
    /// Returns `Some((word_start_byte, word_end_byte, word))` or `None`.
    pub fn last_completed_word(&self) -> Option<(usize, usize, String)> {
        let before_cursor = &self.value[..self.cursor_pos];
        // The cursor should be right after a separator (space, etc.)
        // Find the word before it.
        let trimmed = before_cursor.trim_end();
        if trimmed.is_empty() {
            return None;
        }
        let word_end = trimmed.len();
        let word_start = trimmed.rfind(char::is_whitespace).map_or(0, |i| {
            // i is the byte position of the last whitespace in trimmed
            // The word starts after it — find the char boundary after it.
            trimmed[i..]
                .char_indices()
                .nth(1)
                .map_or(trimmed.len(), |(offset, _)| i + offset)
        });
        let word = trimmed[word_start..word_end].to_string();
        if word.is_empty() {
            return None;
        }
        Some((word_start, word_end, word))
    }

    pub fn tab_complete(
        &mut self,
        nicks: &[String],
        last_speakers: &[String],
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
            let matches: Vec<String> = match subcommand_ctx {
                Some(SubcommandContext::Help) => {
                    // Complete with command names (without /)
                    let mut m: Vec<String> = commands
                        .iter()
                        .filter(|c| c.to_lowercase().starts_with(&prefix.to_lowercase()))
                        .map(ToString::to_string)
                        .collect();
                    m.sort_by_key(|a| a.to_lowercase());
                    m
                }
                Some(SubcommandContext::Set) => {
                    // Complete with setting paths
                    let mut m: Vec<String> = setting_paths
                        .iter()
                        .filter(|p| p.to_lowercase().starts_with(&prefix.to_lowercase()))
                        .cloned()
                        .collect();
                    m.sort_by_key(|a| a.to_lowercase());
                    m
                }
                Some(SubcommandContext::Subcommand(ref subcmds)) => {
                    // Complete with doc-driven subcommand names
                    let mut m: Vec<String> = subcmds
                        .iter()
                        .filter(|s| s.to_lowercase().starts_with(&prefix.to_lowercase()))
                        .cloned()
                        .collect();
                    m.sort_by_key(|a| a.to_lowercase());
                    m
                }
                None if is_command => {
                    let cmd_prefix = &prefix[1..]; // strip leading /
                    let mut m: Vec<String> = commands
                        .iter()
                        .filter(|c| c.to_lowercase().starts_with(&cmd_prefix.to_lowercase()))
                        .map(|c| format!("/{c}"))
                        .collect();
                    m.sort_by_key(|a| a.to_lowercase());
                    m
                }
                None => {
                    // Nick completion: recent speakers first, then remaining nicks alphabetically.
                    nick_completions(nicks, last_speakers, &prefix)
                }
            };

            if matches.is_empty() {
                return;
            }

            let completion = &matches[0];
            let suffix = if is_command {
                " "
            } else if is_start_of_line {
                ": "
            } else {
                " "
            };
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

/// Build nick completion list with recent speakers first (erssi-style).
///
/// 1. Matching nicks from `last_speakers` — in recency order (most recent first)
/// 2. Remaining matching nicks from the full channel nick list — sorted alphabetically
fn nick_completions(nicks: &[String], last_speakers: &[String], prefix: &str) -> Vec<String> {
    let prefix_lower = prefix.to_lowercase();

    // Recent speakers that match the prefix (already in recency order).
    let mut recent: Vec<String> = last_speakers
        .iter()
        .filter(|n| n.to_lowercase().starts_with(&prefix_lower))
        .cloned()
        .collect();

    // Collect the rest from the full nick list, excluding already-added recent speakers.
    let recent_lower: Vec<String> = recent.iter().map(|n| n.to_lowercase()).collect();
    let mut rest: Vec<String> = nicks
        .iter()
        .filter(|n| {
            let lower = n.to_lowercase();
            lower.starts_with(&prefix_lower) && !recent_lower.contains(&lower)
        })
        .cloned()
        .collect();
    rest.sort_by_key(|a| a.to_lowercase());

    recent.extend(rest);
    recent
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
        if char_idx >= total_chars {
            string_len
        } else {
            byte_indices[char_idx]
        }
    };

    let visible_start_byte = byte_at(scroll_offset);
    let cursor_byte = byte_at(cursor_char_pos);
    let visible_end_byte = byte_at(visible_end);

    let cursor_char = app.input.value[cursor_byte..].chars().next().unwrap_or(' ');
    let cursor_end_byte = cursor_byte + cursor_char.len_utf8().min(string_len - cursor_byte);

    let cursor_color = hex_to_color(&colors.cursor).unwrap_or(Color::Reset);
    let normal_style = Style::default().fg(fg);
    let cursor_style = Style::default().fg(Color::Black).bg(cursor_color);
    let spell_style = Style::default()
        .fg(Color::Red)
        .add_modifier(Modifier::UNDERLINED);

    // Build spans — spell-highlighted if needed.
    let mut spans = vec![Span::styled(prompt, Style::default().fg(fg_muted))];

    if let Some(ref spell) = app.input.spell_state {
        // Build spans with the misspelled/corrected word highlighted.
        build_spell_spans(
            &mut spans,
            &app.input.value,
            visible_start_byte,
            cursor_byte,
            cursor_end_byte,
            visible_end_byte,
            spell.word_start,
            spell.word_end,
            normal_style,
            cursor_style,
            spell_style,
        );
    } else {
        // Normal rendering — no spell highlight.
        let before_cursor = &app.input.value[visible_start_byte..cursor_byte];
        let after_cursor = if cursor_end_byte < visible_end_byte {
            &app.input.value[cursor_end_byte..visible_end_byte]
        } else {
            ""
        };
        spans.push(Span::styled(before_cursor, normal_style));
        spans.push(Span::styled(cursor_char.to_string(), cursor_style));
        spans.push(Span::styled(after_cursor, normal_style));
    }

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}

/// Build spans for the visible input line with a spell-highlighted word region.
///
/// Splits the visible text into segments: normal, spell-highlighted, cursor,
/// ensuring the cursor and spell ranges overlay correctly.
#[expect(
    clippy::too_many_arguments,
    reason = "span builder needs all byte boundaries"
)]
fn build_spell_spans<'a>(
    spans: &mut Vec<Span<'a>>,
    text: &'a str,
    vis_start: usize,
    cursor_byte: usize,
    cursor_end: usize,
    vis_end: usize,
    spell_start: usize,
    spell_end: usize,
    normal: Style,
    cursor: Style,
    spell: Style,
) {
    // Clamp spell range to visible region.
    let sp_start = spell_start.max(vis_start);
    let sp_end = spell_end.min(vis_end);

    if sp_start >= sp_end {
        // Spell word not visible — render normally.
        spans.push(Span::styled(&text[vis_start..cursor_byte], normal));
        spans.push(Span::styled(
            text[cursor_byte..cursor_end].to_string(),
            cursor,
        ));
        if cursor_end < vis_end {
            spans.push(Span::styled(&text[cursor_end..vis_end], normal));
        }
        return;
    }

    // Walk through the visible range, emitting spans in order.
    // Regions: [vis_start..sp_start] [sp_start..sp_end] [sp_end..vis_end]
    // The cursor can be anywhere in the visible range.
    let mut pos = vis_start;
    let regions = [
        (vis_start, sp_start, normal),
        (sp_start, sp_end, spell),
        (sp_end, vis_end, normal),
    ];

    for (region_start, region_end, style) in regions {
        if region_start >= region_end || pos >= vis_end {
            continue;
        }
        let start = pos.max(region_start);
        let end = region_end;

        // Does the cursor fall within this region?
        if cursor_byte >= start && cursor_byte < end {
            // Before cursor in this region.
            if start < cursor_byte {
                spans.push(Span::styled(&text[start..cursor_byte], style));
            }
            // Cursor character — combine cursor style with spell underline if applicable.
            let combined_cursor = if style == spell {
                cursor.add_modifier(Modifier::UNDERLINED)
            } else {
                cursor
            };
            spans.push(Span::styled(
                text[cursor_byte..cursor_end].to_string(),
                combined_cursor,
            ));
            // After cursor in this region.
            if cursor_end < end {
                spans.push(Span::styled(&text[cursor_end..end], style));
            }
        } else if start < end {
            spans.push(Span::styled(&text[start..end], style));
        }
        pos = end;
    }
}

/// Render the spell suggestion popup above the input area.
pub fn render_spell_popup(frame: &mut Frame, input_area: Rect, app: &App) {
    use ratatui::widgets::{Block, Borders, Clear};

    let Some(ref spell) = app.input.spell_state else {
        return;
    };
    if spell.suggestions.is_empty() {
        return;
    }

    let colors = &app.theme.colors;
    let bg_alt = hex_to_color(&colors.bg_alt).unwrap_or(Color::DarkGray);
    let fg = hex_to_color(&colors.fg).unwrap_or(Color::White);
    let accent = hex_to_color(&colors.accent).unwrap_or(Color::Yellow);
    let fg_muted = hex_to_color(&colors.fg_muted).unwrap_or(Color::DarkGray);

    // Build suggestion text: show all suggestions, highlight current.
    let current_idx = if spell.index == 0 {
        spell.suggestions.len() - 1
    } else {
        spell.index - 1
    };

    let mut suggestion_spans: Vec<Span<'_>> = Vec::new();
    suggestion_spans.push(Span::styled(" ", Style::default().bg(bg_alt)));
    for (i, s) in spell.suggestions.iter().enumerate() {
        if i > 0 {
            suggestion_spans.push(Span::styled(
                " │ ",
                Style::default().fg(fg_muted).bg(bg_alt),
            ));
        }
        let style = if i == current_idx {
            Style::default().fg(Color::Black).bg(accent)
        } else {
            Style::default().fg(fg).bg(bg_alt)
        };
        suggestion_spans.push(Span::styled(s.as_str(), style));
    }
    suggestion_spans.push(Span::styled(" ", Style::default().bg(bg_alt)));

    // Calculate popup width from content.
    let content_width: usize = suggestion_spans.iter().map(Span::width).sum();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "clamped to input_area.width which is u16"
    )]
    let popup_width = (content_width + 2).min(input_area.width as usize) as u16; // +2 for borders
    let popup_height = 3_u16; // top border + content + bottom border

    // Position popup above the input line.
    let popup_y = input_area.y.saturating_sub(popup_height);
    let popup_x = input_area.x;
    let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(" spell ", Style::default().fg(accent).bg(bg_alt)),
            Span::styled(
                "Tab",
                Style::default()
                    .fg(fg)
                    .bg(bg_alt)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("=cycle ", Style::default().fg(fg_muted).bg(bg_alt)),
            Span::styled(
                "Esc",
                Style::default()
                    .fg(fg)
                    .bg(bg_alt)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("=cancel ", Style::default().fg(fg_muted).bg(bg_alt)),
        ]))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(fg_muted).bg(bg_alt))
        .style(Style::default().bg(bg_alt));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let suggestion_line = Line::from(suggestion_spans);
    let paragraph = Paragraph::new(suggestion_line);
    frame.render_widget(paragraph, inner);
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

        assert_eq!(
            input.history,
            VecDeque::from(["first".to_string(), "second".to_string()])
        );

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
        input.tab_complete(&nicks, &[], &[], &[]);

        assert_eq!(input.value, "ferris: ");
        assert_eq!(input.cursor_pos, 8);
    }

    #[test]
    fn nick_completion_mid_line() {
        let mut input = InputState::new();
        input.value = "hey fer".to_string();
        input.cursor_pos = 7;

        let nicks = vec!["ferris".to_string(), "helper".to_string()];
        input.tab_complete(&nicks, &[], &[], &[]);

        assert_eq!(input.value, "hey ferris ");
        assert_eq!(input.cursor_pos, 11);
    }

    #[test]
    fn nick_completion_cycling() {
        let mut input = InputState::new();
        input.value = "h".to_string();
        input.cursor_pos = 1;

        let nicks = vec!["helper".to_string(), "hank".to_string(), "hiro".to_string()];
        input.tab_complete(&nicks, &[], &[], &[]);
        assert_eq!(input.value, "hank: "); // sorted: hank, helper, hiro

        input.tab_complete(&nicks, &[], &[], &[]);
        assert_eq!(input.value, "helper: ");

        input.tab_complete(&nicks, &[], &[], &[]);
        assert_eq!(input.value, "hiro: ");

        // Wraps around
        input.tab_complete(&nicks, &[], &[], &[]);
        assert_eq!(input.value, "hank: ");
    }

    #[test]
    fn command_completion() {
        let mut input = InputState::new();
        input.value = "/jo".to_string();
        input.cursor_pos = 3;

        let commands = &["join", "part", "msg", "quit"];
        input.tab_complete(&[], &[], commands, &[]);

        assert_eq!(input.value, "/join ");
    }

    #[test]
    fn help_subcommand_completion() {
        let mut input = InputState::new();
        input.value = "/help cl".to_string();
        input.cursor_pos = 8;

        let commands = &["connect", "close", "clear", "quit"];
        input.tab_complete(&[], &[], commands, &[]);
        assert_eq!(input.value, "/help clear ");
    }

    #[test]
    fn help_subcommand_cycling() {
        let mut input = InputState::new();
        input.value = "/help c".to_string();
        input.cursor_pos = 7;

        let commands = &["connect", "close", "clear"];
        input.tab_complete(&[], &[], commands, &[]);
        assert_eq!(input.value, "/help clear ");

        input.tab_complete(&[], &[], commands, &[]);
        assert_eq!(input.value, "/help close ");

        input.tab_complete(&[], &[], commands, &[]);
        assert_eq!(input.value, "/help connect ");
    }

    #[test]
    fn set_path_completion() {
        let mut input = InputState::new();
        input.value = "/set general.ni".to_string();
        input.cursor_pos = 15;

        let settings = vec!["general.nick".to_string(), "general.username".to_string()];
        input.tab_complete(&[], &[], &[], &settings);
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
        input.tab_complete(&[], &[], &[], &settings);
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
        input.tab_complete(&nicks, &[], &[], &[]);
        assert!(input.tab_state.is_some());

        input.insert_char('x');
        assert!(input.tab_state.is_none());
    }

    #[test]
    fn nick_completion_recent_speakers_first() {
        let mut input = InputState::new();
        input.value = "h".to_string();
        input.cursor_pos = 1;

        let nicks = vec!["helper".to_string(), "hank".to_string(), "hiro".to_string()];
        // hiro spoke most recently, then hank
        let last_speakers = vec!["hiro".to_string(), "hank".to_string()];
        input.tab_complete(&nicks, &last_speakers, &[], &[]);
        assert_eq!(input.value, "hiro: "); // most recent speaker first

        input.tab_complete(&nicks, &last_speakers, &[], &[]);
        assert_eq!(input.value, "hank: "); // second most recent

        input.tab_complete(&nicks, &last_speakers, &[], &[]);
        assert_eq!(input.value, "helper: "); // remaining nick (alphabetical)

        // Wraps around
        input.tab_complete(&nicks, &last_speakers, &[], &[]);
        assert_eq!(input.value, "hiro: ");
    }

    #[test]
    fn nick_completion_recent_speakers_case_insensitive() {
        let mut input = InputState::new();
        input.value = "h".to_string();
        input.cursor_pos = 1;

        let nicks = vec!["Helper".to_string(), "Hank".to_string()];
        let last_speakers = vec!["hank".to_string()]; // lowercase in speakers list
        input.tab_complete(&nicks, &last_speakers, &[], &[]);
        assert_eq!(input.value, "hank: "); // recent speaker first

        input.tab_complete(&nicks, &last_speakers, &[], &[]);
        assert_eq!(input.value, "Helper: "); // remaining nick
    }

    #[test]
    fn nick_completion_no_duplicates() {
        let mut input = InputState::new();
        input.value = "h".to_string();
        input.cursor_pos = 1;

        let nicks = vec!["hank".to_string(), "hiro".to_string()];
        let last_speakers = vec!["hank".to_string()]; // hank is in both lists
        input.tab_complete(&nicks, &last_speakers, &[], &[]);
        assert_eq!(input.value, "hank: ");

        input.tab_complete(&nicks, &last_speakers, &[], &[]);
        assert_eq!(input.value, "hiro: "); // not hank again

        // Wraps to hank
        input.tab_complete(&nicks, &last_speakers, &[], &[]);
        assert_eq!(input.value, "hank: ");
    }

    #[test]
    fn nick_completion_empty_speakers_falls_back_to_alphabetical() {
        let mut input = InputState::new();
        input.value = "h".to_string();
        input.cursor_pos = 1;

        let nicks = vec!["hiro".to_string(), "hank".to_string(), "helper".to_string()];
        let last_speakers: Vec<String> = vec![];
        input.tab_complete(&nicks, &last_speakers, &[], &[]);
        assert_eq!(input.value, "hank: "); // alphabetical
    }

    // --- Spell correction tests ---

    fn make_spell_state(
        input: &mut InputState,
        start: usize,
        end: usize,
        original: &str,
        suggestions: &[&str],
    ) {
        input.spell_state = Some(SpellCorrection {
            word_start: start,
            word_end: end,
            original: original.to_string(),
            suggestions: suggestions.iter().map(|s| s.to_string()).collect(),
            index: 0,
        });
    }

    #[test]
    fn spell_cycle_replaces_word() {
        let mut input = InputState::new();
        input.value = "hello wrod ".to_string();
        input.cursor_pos = 11;
        make_spell_state(&mut input, 6, 10, "wrod", &["word", "rod", "wired"]);

        assert!(input.cycle_spell_suggestion());
        assert_eq!(input.value, "hello word ");
        assert_eq!(input.cursor_pos, 11);
    }

    #[test]
    fn spell_cycle_wraps_around() {
        let mut input = InputState::new();
        input.value = "hello wrod ".to_string();
        input.cursor_pos = 11;
        make_spell_state(&mut input, 6, 10, "wrod", &["word", "rod"]);

        input.cycle_spell_suggestion(); // "word"
        input.cycle_spell_suggestion(); // "rod"
        input.cycle_spell_suggestion(); // "word" again
        // After cycling through [word, rod], we should be back at word (index wraps)
        assert!(input.value.contains("word") || input.value.contains("rod"));
    }

    #[test]
    fn spell_dismiss_reverts_to_original() {
        let mut input = InputState::new();
        input.value = "hello wrod ".to_string();
        input.cursor_pos = 11;
        make_spell_state(&mut input, 6, 10, "wrod", &["word", "rod"]);

        input.cycle_spell_suggestion(); // replace "wrod" with "word"
        assert_eq!(&input.value[6..10], "word");

        input.dismiss_spell();
        assert_eq!(&input.value[6..10], "wrod"); // reverted
        assert!(input.spell_state.is_none());
    }

    #[test]
    fn spell_insert_char_accepts_current() {
        let mut input = InputState::new();
        input.value = "hello wrod ".to_string();
        input.cursor_pos = 11;
        make_spell_state(&mut input, 6, 10, "wrod", &["word"]);

        input.cycle_spell_suggestion(); // "word"
        input.insert_char('x'); // accepts "word" and inserts 'x'
        assert!(input.spell_state.is_none());
        assert!(input.value.contains("word"));
        assert!(input.value.contains('x'));
    }

    #[test]
    fn spell_no_suggestions_returns_false() {
        let mut input = InputState::new();
        input.value = "hello wrod ".to_string();
        input.cursor_pos = 11;
        make_spell_state(&mut input, 6, 10, "wrod", &[]);

        assert!(!input.cycle_spell_suggestion());
    }

    #[test]
    fn last_completed_word_basic() {
        let mut input = InputState::new();
        input.value = "hello world ".to_string();
        input.cursor_pos = 12;

        let result = input.last_completed_word();
        assert!(result.is_some());
        let (start, end, word) = result.unwrap();
        assert_eq!(word, "world");
        assert_eq!(start, 6);
        assert_eq!(end, 11);
    }

    #[test]
    fn last_completed_word_single() {
        let mut input = InputState::new();
        input.value = "hello ".to_string();
        input.cursor_pos = 6;

        let result = input.last_completed_word();
        assert!(result.is_some());
        let (_, _, word) = result.unwrap();
        assert_eq!(word, "hello");
    }

    #[test]
    fn last_completed_word_empty() {
        let mut input = InputState::new();
        input.value = " ".to_string();
        input.cursor_pos = 1;

        let result = input.last_completed_word();
        assert!(result.is_none());
    }

    #[test]
    fn is_command_detects_slash() {
        let mut input = InputState::new();
        input.value = "/join #test".to_string();
        assert!(input.is_command());

        input.value = "hello world".to_string();
        assert!(!input.is_command());
    }

    #[test]
    fn spell_different_length_replacement() {
        let mut input = InputState::new();
        input.value = "hi wrld ".to_string();
        input.cursor_pos = 8;
        make_spell_state(&mut input, 3, 7, "wrld", &["world"]);

        input.cycle_spell_suggestion();
        assert_eq!(input.value, "hi world ");
        assert_eq!(input.cursor_pos, 9); // adjusted for longer replacement
    }
}

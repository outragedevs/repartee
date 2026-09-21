use crate::settings_model::{SECTIONS, SettingChange, SettingField, SettingKind};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use std::collections::HashSet;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Search,
    Field(usize),
    Save,
    Cancel,
    Defaults,
    Network,
}

#[derive(Clone, Copy)]
pub enum Action {
    None,
    Save,
    Cancel,
    Network,
}

pub struct SettingsPanel {
    pub fields: Vec<SettingField>,
    originals: Vec<String>,
    touched: HashSet<usize>,
    section: usize,
    query: String,
    focus: Focus,
    cursor: usize,
    offset: usize,
    pub error: Option<String>,
    hits: Vec<(Rect, Focus)>,
    sections: Vec<(Rect, usize)>,
}

impl SettingsPanel {
    pub fn new(config: &crate::config::AppConfig) -> Self {
        let fields = crate::config::settings::catalog::fields(config);
        let originals = fields.iter().map(|f| f.value.clone()).collect();
        Self {
            fields,
            originals,
            touched: HashSet::new(),
            section: 0,
            query: String::new(),
            focus: Focus::Search,
            cursor: 0,
            offset: 0,
            error: None,
            hits: Vec::new(),
            sections: Vec::new(),
        }
    }

    pub fn changes(&self) -> Vec<SettingChange> {
        self.fields
            .iter()
            .enumerate()
            .filter(|(i, f)| {
                f.value != self.originals[*i]
                    || (f.kind == SettingKind::Secret && self.touched.contains(i))
            })
            .map(|(i, f)| SettingChange {
                path: f.path.clone(),
                original: self.originals[i].clone(),
                value: f.value.clone(),
            })
            .collect()
    }

    fn visible(&self) -> Vec<usize> {
        let query = self.query.to_lowercase();
        self.fields
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                if query.is_empty() {
                    f.section == self.section
                } else {
                    format!("{} {} {}", f.label, f.path, f.description)
                        .to_lowercase()
                        .contains(&query)
                }
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn set_focus(&mut self, focus: Focus) {
        self.focus = focus;
        self.cursor = match focus {
            Focus::Search => self.query.chars().count(),
            Focus::Field(i) => self.fields[i].value.chars().count(),
            _ => 0,
        };
    }

    fn move_focus(&mut self, forward: bool) {
        let mut ring = vec![Focus::Search];
        ring.extend(self.visible().into_iter().map(Focus::Field));
        ring.extend([Focus::Save, Focus::Cancel, Focus::Defaults, Focus::Network]);
        let at = ring.iter().position(|f| *f == self.focus).unwrap_or(0);
        self.set_focus(ring[(at + if forward { 1 } else { ring.len() - 1 }) % ring.len()]);
    }

    pub fn insert(&mut self, text: &str) {
        let value = match self.focus {
            Focus::Search => &mut self.query,
            Focus::Field(i)
                if !matches!(
                    self.fields[i].kind,
                    SettingKind::Toggle | SettingKind::Select(_)
                ) =>
            {
                self.touched.insert(i);
                &mut self.fields[i].value
            }
            _ => return,
        };
        let clean: String = text.chars().filter(|c| !c.is_control()).collect();
        let at = value
            .char_indices()
            .nth(self.cursor)
            .map_or(value.len(), |(i, _)| i);
        value.insert_str(at, &clean);
        self.cursor += clean.chars().count();
        self.offset = 0;
    }

    fn erase(&mut self, backwards: bool) {
        let value = match self.focus {
            Focus::Search => &mut self.query,
            Focus::Field(i)
                if !matches!(
                    self.fields[i].kind,
                    SettingKind::Toggle | SettingKind::Select(_)
                ) =>
            {
                self.touched.insert(i);
                &mut self.fields[i].value
            }
            _ => return,
        };
        if backwards && self.cursor == 0 {
            return;
        }
        let at = self.cursor - usize::from(backwards);
        if let Some((byte, ch)) = value.char_indices().nth(at) {
            value.replace_range(byte..byte + ch.len_utf8(), "");
            self.cursor = at;
        }
        self.offset = 0;
    }

    fn activate(&mut self) -> Action {
        match self.focus {
            Focus::Save => Action::Save,
            Focus::Cancel => Action::Cancel,
            Focus::Network => Action::Network,
            Focus::Defaults => {
                for (i, field) in self
                    .fields
                    .iter_mut()
                    .enumerate()
                    .filter(|(_, f)| f.section == self.section)
                {
                    if let Some(value) = &field.default_value {
                        field.value.clone_from(value);
                        self.touched.insert(i);
                    }
                }
                Action::None
            }
            Focus::Field(i) => {
                let f = &mut self.fields[i];
                match &f.kind {
                    SettingKind::Toggle => {
                        f.value = (f.value != "true").to_string();
                        self.touched.insert(i);
                    }
                    SettingKind::Select(options) if !options.is_empty() => {
                        let at = options.iter().position(|o| o == &f.value).unwrap_or(0);
                        f.value.clone_from(&options[(at + 1) % options.len()]);
                        self.touched.insert(i);
                    }
                    _ => {}
                }
                Action::None
            }
            Focus::Search => {
                self.move_focus(true);
                Action::None
            }
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> Action {
        if key.kind == crossterm::event::KeyEventKind::Release {
            return Action::None;
        }
        match (key.modifiers, key.code) {
            (_, KeyCode::Esc) => return Action::Cancel,
            (m, KeyCode::Char('s')) if m.contains(KeyModifiers::CONTROL) => return Action::Save,
            (m, KeyCode::Char('f')) if m.contains(KeyModifiers::CONTROL) => {
                self.set_focus(Focus::Search);
            }
            (m, KeyCode::Left | KeyCode::Right) if m.contains(KeyModifiers::ALT) => {
                self.section = (self.section
                    + if key.code == KeyCode::Right {
                        1
                    } else {
                        SECTIONS.len() - 1
                    })
                    % SECTIONS.len();
                self.query.clear();
                self.offset = 0;
                self.set_focus(Focus::Search);
            }
            (_, KeyCode::Tab | KeyCode::Down) => self.move_focus(true),
            (_, KeyCode::BackTab | KeyCode::Up) => self.move_focus(false),
            (_, KeyCode::Enter) => return self.activate(),
            (_, KeyCode::Char(' ')) if matches!(self.focus, Focus::Field(i) if matches!(self.fields[i].kind, SettingKind::Toggle | SettingKind::Select(_))) =>
            {
                return self.activate();
            }
            (_, KeyCode::Backspace) => self.erase(true),
            (_, KeyCode::Delete) => self.erase(false),
            (_, KeyCode::Left) => self.cursor = self.cursor.saturating_sub(1),
            (_, KeyCode::Right) => {
                let length = match self.focus {
                    Focus::Search => self.query.chars().count(),
                    Focus::Field(i) => self.fields[i].value.chars().count(),
                    _ => 0,
                };
                self.cursor = (self.cursor + 1).min(length);
            }
            (_, KeyCode::Home) => self.cursor = 0,
            (_, KeyCode::End) => self.set_focus(self.focus),
            (m, KeyCode::Char(ch)) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.insert(&ch.to_string());
            }
            _ => {}
        }
        Action::None
    }

    pub fn mouse(&mut self, mouse: MouseEvent) -> Action {
        match mouse.kind {
            MouseEventKind::ScrollDown => self.move_focus(true),
            MouseEventKind::ScrollUp => self.move_focus(false),
            MouseEventKind::Down(MouseButton::Left) => {
                let point = Position::new(mouse.column, mouse.row);
                if let Some((_, section)) = self.sections.iter().find(|(r, _)| r.contains(point)) {
                    self.section = *section;
                    self.query.clear();
                    self.offset = 0;
                    self.set_focus(Focus::Search);
                } else if let Some((_, focus)) = self.hits.iter().find(|(r, _)| r.contains(point)) {
                    self.set_focus(*focus);
                    if self.focus != Focus::Search {
                        return self.activate();
                    }
                }
            }
            _ => {}
        }
        Action::None
    }
}

#[expect(clippy::too_many_lines)]
pub fn render(frame: &mut Frame, area: Rect, app: &mut crate::app::App) {
    let Some(panel) = &mut app.settings_panel else {
        return;
    };
    let bg = crate::theme::hex_to_color(&app.theme.colors.bg).unwrap_or(Color::Black);
    let fg = crate::theme::hex_to_color(&app.theme.colors.fg).unwrap_or(Color::White);
    let base = Style::default().fg(fg).bg(bg);
    let selected = base.add_modifier(Modifier::REVERSED);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Settings ")
        .style(base);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    panel.hits.clear();
    panel.sections.clear();
    if inner.width < 20 || inner.height < 8 {
        return;
    }
    let nav_width = 23.min(inner.width / 3);
    let body = Rect::new(
        inner.x + nav_width + 1,
        inner.y,
        inner.width.saturating_sub(nav_width + 1),
        inner.height,
    );
    for (i, name) in SECTIONS.iter().enumerate() {
        let y = inner.y + u16::try_from(i).unwrap_or(0);
        if y >= inner.bottom().saturating_sub(2) {
            break;
        }
        let rect = Rect::new(inner.x, y, nav_width, 1);
        frame.render_widget(
            Paragraph::new(*name).style(if i == panel.section { selected } else { base }),
            rect,
        );
        panel.sections.push((rect, i));
    }
    let search = Rect::new(body.x, body.y, body.width, 1);
    frame.render_widget(
        Paragraph::new(format!("Search: {}", panel.query)).style(if panel.focus == Focus::Search {
            selected
        } else {
            base
        }),
        search,
    );
    panel.hits.push((search, Focus::Search));
    let visible = panel.visible();
    let rows = usize::from(body.height.saturating_sub(9) / 2).max(1);
    if let Focus::Field(i) = panel.focus
        && let Some(at) = visible.iter().position(|v| *v == i)
    {
        if at < panel.offset {
            panel.offset = at;
        }
        if at >= panel.offset + rows {
            panel.offset = at + 1 - rows;
        }
    }
    for (row, &i) in visible.iter().skip(panel.offset).take(rows).enumerate() {
        let field = &panel.fields[i];
        let y = body.y + 2 + u16::try_from(row * 2).unwrap_or(0);
        let focused = panel.focus == Focus::Field(i);
        let mut value = match &field.kind {
            SettingKind::Toggle => {
                if field.value == "true" {
                    "[x]".into()
                } else {
                    "[ ]".into()
                }
            }
            SettingKind::Secret if field.value.is_empty() => {
                if field.configured {
                    "(configured; unchanged)".into()
                } else {
                    "(not configured)".into()
                }
            }
            SettingKind::Secret => "•".repeat(field.value.chars().count()),
            _ => field.value.clone(),
        };
        if focused
            && !matches!(
                field.kind,
                SettingKind::Toggle | SettingKind::Select(_) | SettingKind::Secret
            )
        {
            let at = value
                .char_indices()
                .nth(panel.cursor)
                .map_or(value.len(), |(i, _)| i);
            value.insert(at, '|');
            let start = panel
                .cursor
                .saturating_sub(usize::from(body.width.saturating_sub(3)));
            value = value.chars().skip(start).collect();
        }
        frame.render_widget(
            Paragraph::new(field.label.clone()).style(base),
            Rect::new(body.x, y, body.width, 1),
        );
        let rect = Rect::new(body.x, y + 1, body.width, 1);
        frame.render_widget(
            Paragraph::new(value).style(if focused { selected } else { base }),
            rect,
        );
        panel
            .hits
            .push((Rect::new(body.x, y, body.width, 2), Focus::Field(i)));
    }
    let help = if let Some(error) = &panel.error {
        error.clone()
    } else if let Focus::Field(i) = panel.focus {
        format!("{} {}", panel.fields[i].description, panel.fields[i].effect)
    } else if panel.section == 5 {
        "Chat shortcuts: Alt+digit switches buffers; Alt+A selects activity; Tab completes; PageUp/PageDown scroll. Edit command aliases above.".into()
    } else {
        "Ctrl+F search · Alt+Left/Right section · Tab move · Ctrl+S save · Esc cancel".into()
    };
    frame.render_widget(
        Paragraph::new(help).wrap(Wrap { trim: true }).style(base),
        Rect::new(body.x, body.bottom().saturating_sub(6), body.width, 3),
    );
    let labels = [
        (" Save ", Focus::Save),
        (" Cancel ", Focus::Cancel),
        (" Defaults ", Focus::Defaults),
        (" Add network ", Focus::Network),
    ];
    let mut x = inner.x;
    let mut y = inner.bottom().saturating_sub(2);
    for (label, focus) in labels {
        let width = u16::try_from(label.len()).unwrap_or(0);
        if x + width > inner.right() {
            x = inner.x;
            y += 1;
        }
        let rect = Rect::new(x, y, width.min(inner.right().saturating_sub(x)), 1);
        frame.render_widget(
            Paragraph::new(label).style(if panel.focus == focus {
                selected
            } else {
                base.add_modifier(Modifier::BOLD)
            }),
            rect,
        );
        panel.hits.push((rect, focus));
        x += width + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focusing_secret_does_not_clear_it_and_cancel_keeps_source_unchanged() {
        let mut config = crate::config::AppConfig::default();
        config.web.password = "fixture-secret".into();
        let mut panel = SettingsPanel::new(&config);
        let secret = panel
            .fields
            .iter()
            .position(|f| f.path == "web.password")
            .unwrap();
        panel.set_focus(Focus::Field(secret));
        panel.activate();
        assert!(panel.changes().is_empty());
        let nick = panel
            .fields
            .iter()
            .position(|f| f.path == "general.nick")
            .unwrap();
        panel.set_focus(Focus::Field(nick));
        panel.insert("suffix");
        assert_eq!(panel.changes().len(), 1);
        assert!(matches!(
            panel.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Action::Cancel
        ));
        assert_eq!(config.web.password, "fixture-secret");
        assert!(!config.general.nick.ends_with("suffix"));
    }
}

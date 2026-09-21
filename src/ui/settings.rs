mod collection;

use crate::settings_model::{SettingChange, SettingField, SettingKind, SettingsScope};
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
    Channels,
    Help,
}

#[derive(Clone, Copy)]
pub enum Action {
    None,
    Save,
    Cancel,
    Network,
    Channels(SettingsScope),
}

pub struct SettingsPanel {
    pub scope: SettingsScope,
    pub fields: Vec<SettingField>,
    editor: Option<collection::Editor>,
    help_open: bool,
    help_field: Option<usize>,
    originals: Vec<String>,
    touched: HashSet<usize>,
    section: usize,
    network: Option<String>,
    network_hits: Vec<(Rect, Option<String>)>,
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
        Self::from_fields(fields, SettingsScope::General)
    }

    pub fn from_fields(fields: Vec<SettingField>, scope: SettingsScope) -> Self {
        let network = if scope == SettingsScope::General { None } else { fields.first().and_then(|f| f.network()).map(str::to_string) };
        let originals = fields.iter().map(|f| f.value.clone()).collect();
        Self {
            scope,
            fields,
            editor: None,
            help_open: false,
            help_field: None,
            originals,
            touched: HashSet::new(),
            section: 0,
            network,
            network_hits: Vec::new(),
            query: String::new(),
            focus: Focus::Search,
            cursor: 0,
            offset: 0,
            error: None,
            hits: Vec::new(),
            sections: Vec::new(),
        }
    }

    fn networks(&self) -> Vec<(Option<String>, String)> {
        let mut networks = if self.scope == SettingsScope::General { vec![(None, "General".into())] } else { Vec::new() };
        for field in &self.fields {
            if let Some(id) = field.network()
                && !networks
                    .iter()
                    .any(|(existing, _)| existing.as_deref() == Some(id))
            {
                networks.push((Some(id.to_string()), field.group().to_string()));
            }
        }
        networks
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
                        && (self.section != 0 || f.network() == self.network.as_deref())
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
        if let Focus::Field(index) = focus {
            self.help_field = Some(index);
        }
        self.cursor = match focus {
            Focus::Search => self.query.chars().count(),
            Focus::Field(i) => self.fields[i].value.chars().count(),
            _ => 0,
        };
    }

    fn move_focus(&mut self, forward: bool) {
        let mut ring = vec![Focus::Search];
        ring.extend(self.visible().into_iter().map(Focus::Field));
        ring.extend([Focus::Save, Focus::Cancel]);
        if self.scope == SettingsScope::General {
            ring.push(Focus::Defaults);
            ring.push(if matches!(self.section, 7 | 8) { Focus::Channels } else { Focus::Network });
        }
        ring.push(Focus::Help);
        let at = ring.iter().position(|f| *f == self.focus).unwrap_or(0);
        self.set_focus(ring[(at + if forward { 1 } else { ring.len() - 1 }) % ring.len()]);
    }

    pub fn insert(&mut self, text: &str) {
        if let Some(editor) = &mut self.editor {
            editor.insert(text);
            return;
        }
        let value = match self.focus {
            Focus::Search => &mut self.query,
            Focus::Field(i)
                if !self.fields[i].is_collection()
                    && !matches!(
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
                if !self.fields[i].is_collection()
                    && !matches!(
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
            Focus::Help => {
                self.help_open = true;
                Action::None
            }
            Focus::Save => Action::Save,
            Focus::Cancel => Action::Cancel,
            Focus::Network => Action::Network,
            Focus::Channels => Action::Channels(if self.section == 7 { SettingsScope::EncryptionChannels } else { SettingsScope::TranslationChannels }),
            Focus::Defaults => {
                if !self.query.is_empty() {
                    self.error = Some(
                        "Choose a category without an active search before restoring defaults."
                            .into(),
                    );
                    return Action::None;
                }
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
                if self.fields[i].is_collection() {
                    let field = &self.fields[i];
                    match crate::settings_model::collection::Collection::open(
                        &field.path,
                        &field.value,
                    ) {
                        Ok(value) => {
                            self.editor =
                                Some(collection::Editor::new(i, field.label.clone(), value));
                        }
                        Err(error) => self.error = Some(error),
                    }
                    return Action::None;
                }
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

    fn editor_outcome(&mut self, outcome: collection::Outcome) {
        match outcome {
            collection::Outcome::Stay => {}
            collection::Outcome::Cancel => self.editor = None,
            collection::Outcome::Apply(value) => {
                if let Some(editor) = self.editor.take() {
                    self.fields[editor.field].value = value;
                    self.touched.insert(editor.field);
                }
            }
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> Action {
        if key.kind == crossterm::event::KeyEventKind::Release {
            return Action::None;
        }

        if let Some(editor) = &mut self.editor {
            let outcome = editor.key(key);
            self.editor_outcome(outcome);
            return Action::None;
        }
        if self.help_open {
            if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::F(1)) {
                self.help_open = false;
            }
            return Action::None;
        }
        if key.code == KeyCode::F(1) {
            self.help_open = true;
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
                        self.scope.sections().len() - 1
                    })
                    % self.scope.sections().len();
                self.query.clear();
                self.offset = 0;
                self.set_focus(Focus::Search);
            }
            (m, KeyCode::Left | KeyCode::Right)
                if m.contains(KeyModifiers::CONTROL) && self.section == 0 =>
            {
                let networks = self.networks();
                let current = networks
                    .iter()
                    .position(|(id, _)| *id == self.network)
                    .unwrap_or(0);
                let step = if key.code == KeyCode::Right {
                    1
                } else {
                    networks.len() - 1
                };
                self.network
                    .clone_from(&networks[(current + step) % networks.len()].0);
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
        if let Some(editor) = &mut self.editor {
            let outcome = editor.mouse(mouse);
            self.editor_outcome(outcome);
            return Action::None;
        }
        if self.help_open {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                self.help_open = false;
            }
            return Action::None;
        }
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
                } else if let Some((_, network)) =
                    self.network_hits.iter().find(|(r, _)| r.contains(point))
                {
                    self.network = network.clone();
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

#[allow(clippy::too_many_lines)]
pub fn render(frame: &mut Frame, area: Rect, app: &mut crate::app::App) {
    let palette = super::wizard::form_palette(&app.theme);
    let Some(panel) = &mut app.settings_panel else {
        return;
    };
    let super::wizard::FormPalette {
        bg,
        fg,
        muted,
        border,
        accent,
        field_bg,
    } = palette;
    let base = Style::default().fg(fg).bg(bg);
    let selected = Style::default()
        .fg(bg)
        .bg(accent)
        .add_modifier(Modifier::BOLD);
    let popup = crate::ui::centered_rect(
        area,
        area.width.saturating_sub(4).clamp(22, 108),
        area.height.saturating_sub(2).clamp(14, 34),
    );
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" {} ", panel.scope.title()),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(border))
        .style(base);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    panel.hits.clear();
    panel.sections.clear();
    panel.network_hits.clear();
    if inner.width < 62 || inner.height < 18 {
        frame.render_widget(
            Paragraph::new("Settings needs at least 68 × 22. Resize the terminal; Esc cancels.")
                .wrap(Wrap { trim: true })
                .style(base),
            inner,
        );
        return;
    }
    let padded = inner.inner(Margin::new(1, 0));
    let search = Rect::new(padded.x, padded.y, padded.width, 1);
    frame.render_widget(
        Paragraph::new(format!(
            " Search  {}{}",
            panel.query,
            if panel.focus == Focus::Search {
                "▏"
            } else {
                ""
            }
        ))
        .style(if panel.focus == Focus::Search {
            Style::default().fg(fg).bg(field_bg)
        } else {
            Style::default().fg(muted).bg(field_bg)
        }),
        search,
    );
    panel.hits.push((search, Focus::Search));
    let mut footer = footer_controls(Rect::new(
        padded.x,
        padded.y,
        padded.width,
        padded.height.saturating_sub(1),
    ));
    if panel.scope != SettingsScope::General {
        footer.retain(|(_, focus, _)| !matches!(focus, Focus::Defaults | Focus::Network));
    } else if matches!(panel.section, 7 | 8) {
        for (label, focus, _) in &mut footer {
            if *focus == Focus::Network { *label = " Channels "; *focus = Focus::Channels; }
        }
    }
    let footer_y = footer[0].2.y;
    let help_y = footer_y.saturating_sub(4);
    let nav = Rect::new(
        padded.x,
        padded.y + 2,
        if panel.scope == SettingsScope::General { 23 } else { 0 },
        help_y.saturating_sub(padded.y + 2),
    );
    if panel.scope == SettingsScope::General {
    frame.render_widget(
        Block::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(border)),
        Rect::new(nav.x, nav.y, nav.width + 1, nav.height),
    );
    for (index, name) in panel.scope.sections().iter().enumerate() {
        let rect = Rect::new(
            nav.x,
            nav.y + u16::try_from(index).unwrap_or(0),
            nav.width,
            1,
        );
        frame.render_widget(
            Paragraph::new(format!(" {name}")).style(if index == panel.section {
                selected
            } else {
                Style::default().fg(muted).bg(bg)
            }),
            rect,
        );
        panel.sections.push((rect, index));
    }
    }
    let body_x = if panel.scope == SettingsScope::General { nav.right() + 2 } else { padded.x };
    let body = Rect::new(
        body_x,
        nav.y,
        padded.right().saturating_sub(body_x),
        nav.height,
    );
    let heading = if panel.query.is_empty() {
        panel.scope.sections()[panel.section].to_string()
    } else {
        "Search results".into()
    };
    frame.render_widget(
        Paragraph::new(heading).style(Style::default().fg(accent).add_modifier(Modifier::BOLD)),
        Rect::new(body.x, body.y, body.width, 1),
    );
    let visible = panel.visible();
    let mut display_rows = Vec::new();
    let mut previous_group = "";
    for &index in &visible {
        let group = panel.fields[index].group();
        if group != previous_group {
            display_rows.push((index, true));
            previous_group = group;
        }
        display_rows.push((index, false));
    }
    let rows = usize::from(body.height.saturating_sub(2)).max(1);
    if let Focus::Field(index) = panel.focus
        && let Some(at) = display_rows
            .iter()
            .position(|candidate| *candidate == (index, false))
    {
        if at < panel.offset {
            panel.offset = at.saturating_sub(1);
        }
        if at >= panel.offset + rows {
            panel.offset = at + 1 - rows;
        }
    }
    panel.offset = panel.offset.min(display_rows.len().saturating_sub(rows));
    let label_width = (body.width * 3 / 5).min(32);
    for (row, &(index, heading)) in display_rows
        .iter()
        .skip(panel.offset)
        .take(rows)
        .enumerate()
    {
        if heading {
            frame.render_widget(
                Paragraph::new(panel.fields[index].group())
                    .style(Style::default().fg(accent).add_modifier(Modifier::BOLD)),
                Rect::new(
                    body.x,
                    body.y + 2 + u16::try_from(row).unwrap_or(0),
                    body.width,
                    1,
                ),
            );
            continue;
        }
        let field = &panel.fields[index];
        let focused = panel.focus == Focus::Field(index);
        let changed = field.value != panel.originals[index]
            || panel.touched.contains(&index) && field.kind == SettingKind::Secret;
        let y = body.y + 2 + u16::try_from(row).unwrap_or(0);
        let rect = Rect::new(
            body.x + label_width + 1,
            y,
            body.width.saturating_sub(label_width + 1),
            1,
        );
        let mut value = if field.is_collection() {
            let count =
                crate::settings_model::collection::Collection::open(&field.path, &field.value)
                    .map_or(0, |c| c.rows.len());
            format!("Edit list ({count})")
        } else {
            match &field.kind {
                SettingKind::Toggle => {
                    if field.value == "true" {
                        "[x] On".into()
                    } else {
                        "[ ] Off".into()
                    }
                }
                SettingKind::Select(_) => format!(
                    "‹ {} ›",
                    if field.value.is_empty() {
                        "Inherit"
                    } else {
                        &field.value
                    }
                ),
                SettingKind::Secret if field.value.is_empty() => {
                    if field.configured {
                        "(unchanged)".into()
                    } else {
                        "(not set)".into()
                    }
                }
                SettingKind::Secret => "•".repeat(field.value.chars().count()),
                _ => field.value.clone(),
            }
        };
        if focused
            && !field.is_collection()
            && !matches!(field.kind, SettingKind::Toggle | SettingKind::Select(_))
        {
            let at = value
                .char_indices()
                .nth(panel.cursor)
                .map_or(value.len(), |(i, _)| i);
            value.insert(at, '▏');
            value = value
                .chars()
                .skip(
                    panel
                        .cursor
                        .saturating_sub(usize::from(rect.width.saturating_sub(2))),
                )
                .collect();
        }
        frame.render_widget(
            Paragraph::new(format!(
                "{}{}",
                if changed { "*" } else { " " },
                field.short_label()
            ))
            .style(
                Style::default()
                    .fg(if focused || changed { accent } else { fg })
                    .bg(bg),
            ),
            Rect::new(body.x, y, label_width, 1),
        );
        frame.render_widget(
            Paragraph::new(value).style(if focused {
                selected
            } else {
                Style::default().fg(fg).bg(field_bg)
            }),
            rect,
        );
        panel
            .hits
            .push((Rect::new(body.x, y, body.width, 1), Focus::Field(index)));
    }
    if panel.section == 0 && panel.query.is_empty() {
        let networks = panel.networks();
        let selected_at = networks
            .iter()
            .position(|(id, _)| *id == panel.network)
            .unwrap_or(0);
        let mut start = 0;
        while networks[start..=selected_at]
            .iter()
            .map(|(_, name)| name.chars().count() + 3)
            .sum::<usize>()
            > usize::from(body.width.saturating_sub(6))
            && start < selected_at
        {
            start += 1;
        }
        for (label, x, target) in [
            (
                " ‹ ",
                body.x,
                (selected_at + networks.len() - 1) % networks.len(),
            ),
            (" › ", body.right() - 3, (selected_at + 1) % networks.len()),
        ] {
            let rect = Rect::new(x, body.y + 1, 3, 1);
            frame.render_widget(
                Paragraph::new(label).style(Style::default().fg(fg).bg(field_bg)),
                rect,
            );
            panel.network_hits.push((rect, networks[target].0.clone()));
        }
        let mut x = body.x + 3;
        for (id, name) in networks.into_iter().skip(start) {
            let width = u16::try_from(name.chars().count() + 2)
                .unwrap_or(body.width)
                .min(body.width.saturating_sub(6));
            if x + width > body.right() - 3 {
                break;
            }
            let rect = Rect::new(x, body.y + 1, width, 1);
            frame.render_widget(
                Paragraph::new(format!(" {name} ")).style(if id == panel.network {
                    selected
                } else {
                    Style::default().fg(fg).bg(field_bg)
                }),
                rect,
            );
            panel.network_hits.push((rect, id));
            x += width + 1;
        }
    } else {
        frame.render_widget(
            Paragraph::new(format!("{} fields", visible.len()))
                .alignment(Alignment::Right)
                .style(Style::default().fg(muted)),
            Rect::new(body.x, body.y + 1, body.width, 1),
        );
    }
    frame.render_widget(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(border)),
        Rect::new(padded.x, help_y, padded.width, 1),
    );
    let help = if let Some(error) = &panel.error {
        error.clone()
    } else if let Focus::Field(index) = panel.focus {
        let f = &panel.fields[index];
        format!("{} · {} {}", f.label, f.description, f.effect)
    } else {
        "Changes are applied only when you save. * marks an edited setting.".into()
    };
    frame.render_widget(
        Paragraph::new(help.clone())
            .wrap(Wrap { trim: true })
            .style(
                Style::default()
                    .fg(if panel.error.is_some() {
                        Color::Red
                    } else {
                        muted
                    })
                    .bg(bg),
            ),
        Rect::new(padded.x, help_y + 1, padded.width, 2),
    );
    for (label, focus, rect) in footer {
        frame.render_widget(
            Paragraph::new(label).style(if panel.focus == focus {
                selected
            } else {
                Style::default().fg(fg).bg(field_bg)
            }),
            rect,
        );
        panel.hits.push((rect, focus));
    }
    frame.render_widget(
        Paragraph::new(if panel.section == 0 {
            "Ctrl+←→ network · Tab move · Ctrl+S save · Esc cancel"
        } else {
            "Tab move · Alt+←→ category · Ctrl+F search · Ctrl+S save · Esc cancel"
        })
        .style(Style::default().fg(muted).bg(bg)),
        Rect::new(padded.x, padded.bottom() - 1, padded.width, 1),
    );
    if panel.help_open {
        let popup = crate::ui::centered_rect(
            area,
            area.width.saturating_sub(6).min(84),
            12.min(area.height),
        );
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Setting help ")
            .style(base)
            .border_style(Style::default().fg(accent));
        let content = block.inner(popup).inner(Margin::new(1, 0));
        frame.render_widget(block, popup);
        let text = panel.help_field.map_or(help, |index| {
            let field = &panel.fields[index];
            format!(
                "{}\n{}\n\n{}\n{}",
                field.label, field.path, field.description, field.effect
            )
        });
        frame.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: true }),
            Rect::new(
                content.x,
                content.y,
                content.width,
                content.height.saturating_sub(2),
            ),
        );
        frame.render_widget(
            Paragraph::new(" Close · Enter / Esc ").style(selected),
            Rect::new(content.x, content.bottom() - 1, 21.min(content.width), 1),
        );
    }
    if let Some(editor) = &mut panel.editor {
        editor.render(frame, area, &palette);
    }
}

fn footer_controls(inner: Rect) -> Vec<(&'static str, Focus, Rect)> {
    let labels = [
        (" Save ", Focus::Save),
        (" Cancel ", Focus::Cancel),
        (" Defaults ", Focus::Defaults),
        (" Add network ", Focus::Network),
        (" Help ", Focus::Help),
    ];
    let mut controls = Vec::new();
    let mut x = inner.x;
    let mut row = 0;
    for (label, focus) in labels {
        let width = u16::try_from(label.len()).unwrap_or(0).min(inner.width);
        if x + width > inner.right() {
            x = inner.x;
            row += 1;
        }
        controls.push((label, focus, Rect::new(x, row, width, 1)));
        x += width + 1;
    }
    let start = inner.bottom().saturating_sub(row + 1);
    for (_, _, rect) in &mut controls {
        rect.y += start;
    }
    controls
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_terminal_footer_keeps_all_buttons_visible_and_clickable() {
        let inner = Rect::new(1, 1, 20, 12);
        let controls = footer_controls(inner);
        assert_eq!(controls.len(), 5);
        for (_, _, rect) in &controls {
            assert!(inner.contains(Position::new(rect.x, rect.y)));
            assert!(inner.contains(Position::new(rect.right() - 1, rect.bottom() - 1)));
        }
        assert!(
            controls
                .windows(2)
                .all(|pair| !pair[0].2.intersects(pair[1].2))
        );
    }

    #[test]
    fn focusing_secret_does_not_clear_it_and_cancel_keeps_source_unchanged() {
        let mut config = crate::config::AppConfig::default();
        config.web.password = "fixture-secret".into();
        config.general.nick = "custom-nick".into();
        let mut panel = SettingsPanel::new(&config);
        let secret = panel
            .fields
            .iter()
            .position(|f| f.path == "web.password")
            .unwrap();
        panel.set_focus(Focus::Field(secret));
        panel.activate();
        assert!(panel.changes().is_empty());
        panel.query = "logging".into();
        panel.set_focus(Focus::Defaults);
        panel.activate();
        assert!(panel.changes().is_empty());
        assert!(panel.error.is_some());
        panel.query.clear();
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

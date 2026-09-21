use crate::settings_model::collection::{CellKind, Collection};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Row(usize),
    Cell(usize),
    Add,
    Automatic,
    Apply,
    Cancel,
    Remove(usize),
    Up(usize),
    Down(usize),
}

pub(super) enum Outcome {
    Stay,
    Cancel,
    Apply(String),
}

struct Entry {
    index: Option<usize>,
    values: Vec<String>,
}

pub(super) struct Editor {
    pub field: usize,
    title: String,
    collection: Collection,
    entry: Option<Entry>,
    focus: Target,
    cursor: usize,
    offset: usize,
    error: Option<String>,
    hits: Vec<(Rect, Target)>,
}

impl Editor {
    pub const fn new(field: usize, title: String, collection: Collection) -> Self {
        Self {
            field,
            title,
            collection,
            entry: None,
            focus: Target::Add,
            cursor: 0,
            offset: 0,
            error: None,
            hits: Vec::new(),
        }
    }

    fn ring(&self) -> Vec<Target> {
        let mut ring = self.entry.as_ref().map_or_else(
            || {
                (0..self.collection.rows.len())
                    .map(Target::Row)
                    .chain([Target::Add])
                    .collect::<Vec<_>>()
            },
            |entry| {
                (0..entry.values.len())
                    .map(Target::Cell)
                    .collect::<Vec<_>>()
            },
        );
        if self.entry.is_none() && self.collection.optional {
            ring.push(Target::Automatic);
        }
        ring.extend([Target::Apply, Target::Cancel]);
        ring
    }

    fn set_focus(&mut self, focus: Target) {
        self.focus = focus;
        self.cursor = if let (Some(entry), Target::Cell(i)) = (&self.entry, focus) {
            entry.values[i].chars().count()
        } else {
            0
        };
    }

    fn move_focus(&mut self, forward: bool) {
        let ring = self.ring();
        let at = ring
            .iter()
            .position(|target| *target == self.focus)
            .unwrap_or(0);
        self.set_focus(ring[(at + if forward { 1 } else { ring.len() - 1 }) % ring.len()]);
    }

    fn column_kind(&self, index: usize) -> CellKind {
        if self.collection.key_label.is_some() && index == 0 {
            return CellKind::Text;
        }
        self.collection.columns[index - usize::from(self.collection.key_label.is_some())].kind
    }

    fn start(&mut self, index: Option<usize>) {
        let row = index
            .and_then(|i| self.collection.rows.get(i))
            .cloned()
            .unwrap_or_else(|| self.collection.new_row());
        let mut values = self.collection.cells(&row);
        if self.collection.key_label.is_some() {
            values.insert(0, row.key);
        }
        self.entry = Some(Entry { index, values });
        self.offset = 0;
        self.error = None;
        self.set_focus(Target::Cell(0));
    }

    pub fn insert(&mut self, text: &str) {
        let Target::Cell(index) = self.focus else {
            return;
        };
        let kind = self.column_kind(index);
        if matches!(kind, CellKind::Toggle | CellKind::Select(_)) {
            return;
        }
        let Some(entry) = &mut self.entry else {
            return;
        };
        let value = &mut entry.values[index];
        let clean: String = text
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' && kind == CellKind::List)
            .collect();
        let at = value
            .char_indices()
            .nth(self.cursor)
            .map_or(value.len(), |(i, _)| i);
        value.insert_str(at, &clean);
        self.cursor += clean.chars().count();
    }

    fn erase(&mut self, backwards: bool) {
        let Target::Cell(index) = self.focus else {
            return;
        };
        if matches!(self.column_kind(index), CellKind::Toggle | CellKind::Select(_)) || backwards && self.cursor == 0 {
            return;
        }
        if let Some(entry) = &mut self.entry {
            let value = &mut entry.values[index];
            let at = self.cursor - usize::from(backwards);
            if let Some((byte, ch)) = value.char_indices().nth(at) {
                value.replace_range(byte..byte + ch.len_utf8(), "");
                self.cursor = at;
            }
        }
    }

    fn activate(&mut self, target: Target) -> Outcome {
        match target {
            Target::Automatic => self.collection.inherited = !self.collection.inherited,
            Target::Add => self.start(None),
            Target::Row(index) => self.start(Some(index)),
            Target::Cell(index) => {
                let kind = self.column_kind(index);
                if let Some(entry) = &mut self.entry {
                    match kind {
                        CellKind::Toggle => entry.values[index] = (entry.values[index] != "true").to_string(),
                        CellKind::Select(options) => {
                            let at = options.iter().position(|option| *option == entry.values[index]).unwrap_or(0);
                            entry.values[index] = options[(at + 1) % options.len()].into();
                        }
                        _ => {}
                    }
                }
            }
            Target::Cancel => {
                if self.entry.take().is_none() {
                    return Outcome::Cancel;
                }
                self.offset = 0;
                self.error = None;
                self.set_focus(Target::Add);
            }
            Target::Apply => {
                if let Some(entry) = &self.entry {
                    let keyed = self.collection.key_label.is_some();
                    let key = if keyed {
                        entry.values[0].clone()
                    } else {
                        String::new()
                    };
                    match self.collection.update_row(
                        entry.index,
                        key,
                        &entry.values[usize::from(keyed)..],
                    ) {
                        Ok(()) => {
                            self.entry = None;
                            self.offset = 0;
                            self.error = None;
                            self.set_focus(Target::Apply);
                        }
                        Err(error) => self.error = Some(error),
                    }
                } else {
                    return Outcome::Apply(self.collection.serialize());
                }
            }
            Target::Remove(index) => {
                self.collection.rows.remove(index);
                self.set_focus(Target::Add);
            }
            Target::Up(index) if index > 0 => {
                self.collection.rows.swap(index, index - 1);
                self.set_focus(Target::Row(index - 1));
            }
            Target::Down(index) if index + 1 < self.collection.rows.len() => {
                self.collection.rows.swap(index, index + 1);
                self.set_focus(Target::Row(index + 1));
            }
            _ => {}
        }
        Outcome::Stay
    }

    pub fn key(&mut self, key: KeyEvent) -> Outcome {
        if key.kind == crossterm::event::KeyEventKind::Release {
            return Outcome::Stay;
        }
        match (key.modifiers, key.code) {
            (_, KeyCode::Esc) => return self.activate(Target::Cancel),
            (m, KeyCode::Char('s')) if m.contains(KeyModifiers::CONTROL) => {
                return self.activate(Target::Apply);
            }
            (m, KeyCode::Up | KeyCode::Down)
                if m.contains(KeyModifiers::CONTROL)
                    && self.entry.is_none()
                    && self.collection.ordered() =>
            {
                if let Target::Row(index) = self.focus {
                    return self.activate(if key.code == KeyCode::Up {
                        Target::Up(index)
                    } else {
                        Target::Down(index)
                    });
                }
            }
            (_, KeyCode::Tab | KeyCode::Down) => self.move_focus(true),
            (_, KeyCode::BackTab | KeyCode::Up) => self.move_focus(false),
            (m, KeyCode::Enter) if m.contains(KeyModifiers::ALT) => self.insert("\n"),
            (_, KeyCode::Enter) => return self.activate(self.focus),
            (_, KeyCode::Char(' ')) if matches!(self.focus, Target::Cell(i) if matches!(self.column_kind(i), CellKind::Toggle | CellKind::Select(_))) =>
            {
                return self.activate(self.focus);
            }
            (_, KeyCode::Delete) if self.entry.is_none() => {
                if let Target::Row(index) = self.focus {
                    return self.activate(Target::Remove(index));
                }
            }
            (_, KeyCode::Backspace) => self.erase(true),
            (_, KeyCode::Delete) => self.erase(false),
            (_, KeyCode::Left) => self.cursor = self.cursor.saturating_sub(1),
            (_, KeyCode::Right) => {
                if let (Some(entry), Target::Cell(i)) = (&self.entry, self.focus) {
                    self.cursor = (self.cursor + 1).min(entry.values[i].chars().count());
                }
            }
            (_, KeyCode::Home) => self.cursor = 0,
            (_, KeyCode::End) => self.set_focus(self.focus),
            (m, KeyCode::Char(ch)) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.insert(&ch.to_string());
            }
            _ => {}
        }
        Outcome::Stay
    }

    pub fn mouse(&mut self, mouse: MouseEvent) -> Outcome {
        match mouse.kind {
            MouseEventKind::ScrollDown => self.move_focus(true),
            MouseEventKind::ScrollUp => self.move_focus(false),
            MouseEventKind::Down(MouseButton::Left) => {
                let point = Position::new(mouse.column, mouse.row);
                if let Some((_, target)) = self.hits.iter().find(|(rect, _)| rect.contains(point)) {
                    let target = *target;
                    self.set_focus(target);
                    return self.activate(target);
                }
            }
            _ => {}
        }
        Outcome::Stay
    }

    #[allow(clippy::too_many_lines)]
    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        palette: &super::super::wizard::FormPalette,
    ) {
        let popup = super::super::centered_rect(
            area,
            area.width.saturating_sub(6).min(96),
            area.height.saturating_sub(2).min(30),
        );
        let base = Style::default().fg(palette.fg).bg(palette.bg);
        let selected = Style::default()
            .fg(palette.bg)
            .bg(palette.accent)
            .add_modifier(Modifier::BOLD);
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", self.title))
            .style(base)
            .border_style(Style::default().fg(palette.accent));
        let inner = block.inner(popup).inner(Margin::new(1, 0));
        frame.render_widget(block, popup);
        self.hits.clear();
        if inner.height < 8 || inner.width < 40 {
            frame.render_widget(Paragraph::new("Resize the terminal. Esc returns."), inner);
            return;
        }
        let content = Rect::new(
            inner.x,
            inner.y + 2,
            inner.width,
            inner.height.saturating_sub(7),
        );
        let count = self
            .entry
            .as_ref()
            .map_or(self.collection.rows.len(), |e| e.values.len());
        let focused = match self.focus {
            Target::Cell(i) | Target::Row(i) => Some(i),
            _ => None,
        };
        let rows = usize::from(content.height);
        if let Some(at) = focused {
            if at < self.offset {
                self.offset = at;
            }
            if at >= self.offset + rows {
                self.offset = at + 1 - rows;
            }
        }
        self.offset = self.offset.min(count.saturating_sub(rows));
        let heading = if self.entry.is_some() {
            "Edit entry"
        } else if self.collection.inherited {
            "Automatic selection (overrides the list)"
        } else {
            "Entries — select to edit"
        };
        frame.render_widget(
            Paragraph::new(heading).style(Style::default().fg(palette.accent)),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        for index in self.offset..count.min(self.offset + rows) {
            let y = content.y + u16::try_from(index - self.offset).unwrap_or(0);
            if let Some(entry) = &self.entry {
                let keyed = self.collection.key_label.is_some();
                let label = if keyed && index == 0 {
                    self.collection.key_label.unwrap_or("Name")
                } else {
                    self.collection.columns[index - usize::from(keyed)].label
                };
                let label_width = (content.width / 2).min(32);
                let rect = Rect::new(
                    content.x + label_width + 1,
                    y,
                    content.width - label_width - 1,
                    1,
                );
                let mut value = entry.values[index].replace('\n', "↵");
                if self.column_kind(index) == CellKind::Toggle {
                    value = if value == "true" { "[x] On" } else { "[ ] Off" }.into();
                } else if matches!(self.column_kind(index), CellKind::Select(_)) {
                    value = format!("‹ {value} ›");
                } else if self.focus == Target::Cell(index) {
                    let at = value
                        .char_indices()
                        .nth(self.cursor)
                        .map_or(value.len(), |(i, _)| i);
                    value.insert(at, '▏');
                    value = value
                        .chars()
                        .skip(
                            self.cursor
                                .saturating_sub(usize::from(rect.width.saturating_sub(2))),
                        )
                        .collect();
                }
                frame.render_widget(
                    Paragraph::new(label),
                    Rect::new(content.x, y, label_width, 1),
                );
                frame.render_widget(
                    Paragraph::new(value).style(if self.focus == Target::Cell(index) {
                        selected
                    } else {
                        base.bg(palette.field_bg)
                    }),
                    rect,
                );
                self.hits.push((
                    Rect::new(content.x, y, content.width, 1),
                    Target::Cell(index),
                ));
            } else {
                let controls_width = if self.collection.ordered() { 19 } else { 11 };
                let rect = Rect::new(
                    content.x,
                    y,
                    content.width.saturating_sub(controls_width),
                    1,
                );
                frame.render_widget(
                    Paragraph::new(self.collection.summary(&self.collection.rows[index])).style(
                        if self.focus == Target::Row(index) {
                            selected
                        } else {
                            base
                        },
                    ),
                    rect,
                );
                self.hits.push((rect, Target::Row(index)));
                let mut x = rect.right();
                let mut actions = vec![
                    (" Edit ", Target::Row(index)),
                    (" Del ", Target::Remove(index)),
                ];
                if self.collection.ordered() {
                    actions.extend([(" ↑ ", Target::Up(index)), (" ↓ ", Target::Down(index))]);
                }
                for (label, target) in actions {
                    let width = u16::try_from(label.chars().count()).unwrap_or(0);
                    let rect = Rect::new(x, y, width, 1);
                    frame.render_widget(
                        Paragraph::new(label).style(base.bg(palette.field_bg)),
                        rect,
                    );
                    self.hits.push((rect, target));
                    x += width;
                }
            }
        }
        let message = self.error.as_deref().unwrap_or_else(|| {
            if self.entry.is_some() {
                "List fields: Alt+Enter separates items (↵)."
            } else {
                "Ctrl+↑↓ reorders · Delete removes · Enter edits"
            }
        });
        frame.render_widget(
            Paragraph::new(message)
                .wrap(ratatui::widgets::Wrap { trim: true })
                .style(Style::default().fg(if self.error.is_some() {
                    Color::Red
                } else {
                    palette.muted
                })),
            Rect::new(inner.x, inner.bottom() - 4, inner.width, 2),
        );
        let mut actions = Vec::new();
        if self.entry.is_none() {
            actions.push((" Add entry ", Target::Add));
            if self.collection.optional {
                actions.push((
                    if self.collection.inherited {
                        " Auto: on "
                    } else {
                        " Auto: off "
                    },
                    Target::Automatic,
                ));
            }
        }
        actions.push((
            if self.entry.is_some() {
                " Apply entry "
            } else {
                " Apply list "
            },
            Target::Apply,
        ));
        actions.push((" Cancel ", Target::Cancel));
        let mut x = inner.x;
        for (label, target) in actions {
            let width = u16::try_from(label.len()).unwrap_or(0);
            let rect = Rect::new(x, inner.bottom() - 1, width, 1);
            frame.render_widget(
                Paragraph::new(label).style(if self.focus == target {
                    selected
                } else {
                    base.bg(palette.field_bg)
                }),
                rect,
            );
            self.hits.push((rect, target));
            x += width + 1;
        }
    }
}

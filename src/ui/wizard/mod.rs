//! Reusable popup-form engine. UI-mechanism only — it knows nothing about
//! servers. Consumers (see [`server`]) supply a field schema + serializers.
//!
//! A wizard is a fixed-size modal with one or more pages. Tab/Shift-Tab move
//! focus across the focusable fields on the current page and the Save/Cancel
//! buttons; the page is switched with Left/Right when no Select field is
//! focused (or via the tab row / mouse). Mouse clicks focus fields, toggle
//! checkboxes, switch pages, and press buttons via rects recorded at render
//! time.

pub mod server;

use ratatui::layout::Rect;

/// What a field edits.
#[derive(Debug, Clone)]
pub enum FieldKind {
    /// Single-line free text.
    Text,
    /// Single-line text rendered as bullets (passwords).
    Masked,
    /// Boolean checkbox.
    Toggle,
    /// Cycle through a fixed set of options.
    Select(Vec<&'static str>),
    /// Integer text (validated by the consumer's `build`).
    Number,
}

/// The current value of a field, parallel to [`WizardState::fields`].
#[derive(Debug, Clone)]
pub enum FieldValue {
    /// Text / Masked / Number.
    Text(String),
    /// Toggle.
    Bool(bool),
    /// Index into a `Select`'s options.
    Choice(usize),
}

/// One field in the schema.
#[derive(Debug, Clone)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    /// 0 = first page (Basics), 1 = second (Advanced), …
    pub page: usize,
    /// Empty Text/Masked fails validation when true (checked by the consumer).
    pub required: bool,
    /// Not focusable / not editable (e.g. server id in edit mode).
    pub readonly: bool,
}

/// Where focus currently sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Field(usize),
    Save,
    Cancel,
}

/// Add vs edit (edit carries the existing id, which is the map key).
#[derive(Debug, Clone)]
pub enum WizardMode {
    Add,
    Edit { id: String },
}

/// Live state of an open wizard overlay.
#[derive(Debug, Clone)]
pub struct WizardState {
    pub mode: WizardMode,
    pub title: String,
    /// Tab labels per page (index = page number). Supplied by the consumer so
    /// the engine stays schema-agnostic; missing entries fall back to "Page".
    pub page_names: Vec<&'static str>,
    pub fields: Vec<Field>,
    pub values: Vec<FieldValue>,
    /// Whether each field was edited (drives masked-credential "unchanged").
    pub touched: Vec<bool>,
    pub page: usize,
    pub num_pages: usize,
    pub focus: Focus,
    /// Cursor (char index) within the focused Text/Masked/Number field.
    pub cursor: usize,
    pub error: Option<String>,
    // Render-recorded mouse hit rects (rebuilt every frame).
    pub field_rects: Vec<(usize, Rect)>,
    pub tab_rects: Vec<(usize, Rect)>,
    pub save_rect: Option<Rect>,
    pub cancel_rect: Option<Rect>,
}

impl WizardState {
    /// Build a wizard from a schema + initial values.
    ///
    /// # Panics
    /// Panics if `fields` and `values` differ in length (a schema bug).
    #[must_use]
    pub fn new(
        mode: WizardMode,
        title: String,
        page_names: Vec<&'static str>,
        fields: Vec<Field>,
        values: Vec<FieldValue>,
    ) -> Self {
        assert_eq!(fields.len(), values.len(), "schema/values length mismatch");
        let num_pages = fields.iter().map(|f| f.page + 1).max().unwrap_or(1);
        let touched = vec![false; fields.len()];
        let mut s = Self {
            mode,
            title,
            page_names,
            fields,
            values,
            touched,
            page: 0,
            num_pages,
            focus: Focus::Save,
            cursor: 0,
            error: None,
            field_rects: Vec::new(),
            tab_rects: Vec::new(),
            save_rect: None,
            cancel_rect: None,
        };
        s.focus = s.first_field_focus(0).unwrap_or(Focus::Save);
        s.sync_cursor_to_focus();
        s
    }

    /// Focusable (non-readonly) field indices on `page`, in order.
    fn page_fields(&self, page: usize) -> Vec<usize> {
        self.fields
            .iter()
            .enumerate()
            .filter(|(_, f)| f.page == page && !f.readonly)
            .map(|(i, _)| i)
            .collect()
    }

    fn first_field_focus(&self, page: usize) -> Option<Focus> {
        self.page_fields(page).first().map(|&i| Focus::Field(i))
    }

    /// Focus traversal order on the current page: fields, then Save, Cancel.
    fn focus_ring(&self) -> Vec<Focus> {
        let mut ring: Vec<Focus> = self
            .page_fields(self.page)
            .into_iter()
            .map(Focus::Field)
            .collect();
        ring.push(Focus::Save);
        ring.push(Focus::Cancel);
        ring
    }

    pub fn focus_next(&mut self) {
        let ring = self.focus_ring();
        let pos = ring.iter().position(|f| *f == self.focus).unwrap_or(0);
        self.focus = ring[(pos + 1) % ring.len()];
        self.sync_cursor_to_focus();
    }

    pub fn focus_prev(&mut self) {
        let ring = self.focus_ring();
        let pos = ring.iter().position(|f| *f == self.focus).unwrap_or(0);
        self.focus = ring[(pos + ring.len() - 1) % ring.len()];
        self.sync_cursor_to_focus();
    }

    pub fn set_page(&mut self, page: usize) {
        if page < self.num_pages {
            self.page = page;
            self.focus = self.first_field_focus(page).unwrap_or(Focus::Save);
            self.sync_cursor_to_focus();
        }
    }

    pub fn prev_page(&mut self) {
        self.set_page(self.page.saturating_sub(1));
    }

    pub fn next_page(&mut self) {
        if self.page + 1 < self.num_pages {
            self.set_page(self.page + 1);
        }
    }

    /// Place the cursor at the end of the focused text-like field.
    pub fn sync_cursor_to_focus(&mut self) {
        self.cursor = match self.focus {
            Focus::Field(i) => match &self.values[i] {
                FieldValue::Text(s) => s.chars().count(),
                FieldValue::Bool(_) | FieldValue::Choice(_) => 0,
            },
            Focus::Save | Focus::Cancel => 0,
        };
    }

    /// `&mut String` of the focused Text/Masked/Number field, marking it touched.
    fn focused_text_mut(&mut self) -> Option<&mut String> {
        if let Focus::Field(i) = self.focus
            && !self.fields[i].readonly
            && matches!(
                self.fields[i].kind,
                FieldKind::Text | FieldKind::Masked | FieldKind::Number
            )
        {
            self.touched[i] = true;
            if let FieldValue::Text(s) = &mut self.values[i] {
                return Some(s);
            }
        }
        None
    }

    pub fn insert_char(&mut self, c: char) {
        let cursor = self.cursor;
        if let Some(s) = self.focused_text_mut() {
            let byte = s.char_indices().nth(cursor).map_or(s.len(), |(b, _)| b);
            s.insert(byte, c);
            self.cursor += 1;
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let cursor = self.cursor;
        if let Some(s) = self.focused_text_mut()
            && let Some((b, ch)) = s.char_indices().nth(cursor - 1)
        {
            s.replace_range(b..b + ch.len_utf8(), "");
            self.cursor -= 1;
        }
    }

    /// Toggle the focused checkbox (no-op on other field kinds).
    pub fn toggle_focused(&mut self) {
        if let Focus::Field(i) = self.focus
            && let (FieldKind::Toggle, FieldValue::Bool(b)) =
                (&self.fields[i].kind, &mut self.values[i])
        {
            *b = !*b;
            self.touched[i] = true;
        }
    }

    /// Cycle the focused Select forward/backward (no-op on other field kinds).
    pub fn cycle_focused(&mut self, forward: bool) {
        if let Focus::Field(i) = self.focus
            && let (FieldKind::Select(opts), FieldValue::Choice(c)) =
                (&self.fields[i].kind, &mut self.values[i])
        {
            let n = opts.len().max(1);
            *c = if forward {
                (*c + 1) % n
            } else {
                (*c + n - 1) % n
            };
            self.touched[i] = true;
        }
    }

    #[must_use]
    pub fn is_select_focused(&self) -> bool {
        matches!(self.focus, Focus::Field(i) if matches!(self.fields[i].kind, FieldKind::Select(_)))
    }

    #[must_use]
    pub fn is_toggle_focused(&self) -> bool {
        matches!(self.focus, Focus::Field(i) if matches!(self.fields[i].kind, FieldKind::Toggle))
    }

    #[must_use]
    pub fn is_text_focused(&self) -> bool {
        matches!(self.focus, Focus::Field(i)
            if !self.fields[i].readonly
                && matches!(self.fields[i].kind, FieldKind::Text | FieldKind::Masked | FieldKind::Number))
    }

    fn field_index(&self, key: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.key == key)
    }

    // Value accessors used by consumers' `build`.

    #[must_use]
    pub fn text(&self, key: &str) -> &str {
        self.field_index(key)
            .and_then(|i| match &self.values[i] {
                FieldValue::Text(s) => Some(s.as_str()),
                FieldValue::Bool(_) | FieldValue::Choice(_) => None,
            })
            .unwrap_or("")
    }

    #[must_use]
    pub fn boolean(&self, key: &str) -> bool {
        self.field_index(key)
            .and_then(|i| match &self.values[i] {
                FieldValue::Bool(b) => Some(*b),
                FieldValue::Text(_) | FieldValue::Choice(_) => None,
            })
            .unwrap_or(false)
    }

    #[must_use]
    pub fn choice_str(&self, key: &str) -> &'static str {
        self.field_index(key)
            .and_then(|i| match (&self.fields[i].kind, &self.values[i]) {
                (FieldKind::Select(opts), FieldValue::Choice(c)) => opts.get(*c).copied(),
                _ => None,
            })
            .unwrap_or("")
    }

    #[must_use]
    pub fn was_touched(&self, key: &str) -> bool {
        self.field_index(key).is_some_and(|i| self.touched[i])
    }

    /// Label of the first required text-like field left empty, if any. Lets a
    /// consumer's `build` perform required-field validation declaratively.
    #[must_use]
    pub fn first_missing_required(&self) -> Option<&'static str> {
        self.fields
            .iter()
            .zip(&self.values)
            .find_map(|(f, v)| match v {
                FieldValue::Text(s) if f.required && s.trim().is_empty() => Some(f.label),
                _ => None,
            })
    }
}

// === Rendering ===

use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::App;
use crate::theme::hex_to_color;

/// Label column width inside the modal.
const LABEL_W: u16 = 24;

/// Render the wizard overlay (no-op when none open). Records hit rects onto the
/// `WizardState` for mouse hit-testing.
#[allow(
    clippy::too_many_lines,
    reason = "cohesive single-pass modal render (tabs, fields, buttons)"
)]
pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.wizard.is_none() {
        return;
    }
    // Resolve theme colors up front (Color is Copy) so the borrow of app.theme
    // ends before we take a mutable borrow of app.wizard.
    let colors = &app.theme.colors;
    let bg = hex_to_color(&colors.bg_alt).unwrap_or(Color::Black);
    let border = hex_to_color(&colors.border).unwrap_or(Color::DarkGray);
    let accent = hex_to_color(&colors.accent).unwrap_or(Color::Cyan);
    let fg = hex_to_color(&colors.fg).unwrap_or(Color::White);
    let muted = hex_to_color(&colors.fg_muted).unwrap_or(Color::Gray);

    let Some(w) = app.wizard.as_mut() else {
        return;
    };

    let popup = crate::ui::centered_rect(area, 66, 24);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(Span::styled(
            format!(" {} ", w.title),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(bg));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    w.field_rects.clear();
    w.tab_rects.clear();
    w.save_rect = None;
    w.cancel_rect = None;
    if inner.width == 0 || inner.height < 5 {
        return;
    }

    // Tab row.
    let mut x = inner.x + 1;
    for p in 0..w.num_pages {
        let label = w.page_names.get(p).copied().unwrap_or("Page");
        let txt = format!(" {label} ");
        let tw = u16::try_from(txt.chars().count()).unwrap_or(0);
        if x + tw > inner.x + inner.width {
            break;
        }
        let style = if p == w.page {
            Style::default()
                .fg(bg)
                .bg(accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(muted).bg(bg)
        };
        let rect = Rect::new(x, inner.y, tw, 1);
        frame.render_widget(Paragraph::new(Span::styled(txt, style)), rect);
        w.tab_rects.push((p, rect));
        x += tw + 1;
    }

    // Fields for the current page.
    let fields_top = inner.y + 2;
    let fields_bottom = inner.y + inner.height.saturating_sub(3); // leave error+buttons+hint
    let field_w = inner.width.saturating_sub(LABEL_W + 3);
    let mut y = fields_top;
    for (i, f) in w.fields.iter().enumerate() {
        if f.page != w.page {
            continue;
        }
        if y >= fields_bottom {
            break;
        }
        let focused = w.focus == Focus::Field(i);
        let label_style = Style::default().fg(if focused { accent } else { muted });
        let label = if f.readonly {
            format!("{} (locked)", f.label)
        } else {
            f.label.to_string()
        };
        frame.render_widget(
            Paragraph::new(Span::styled(label, label_style)),
            Rect::new(inner.x + 1, y, LABEL_W, 1),
        );
        let fx = inner.x + 1 + LABEL_W;
        let field_rect = Rect::new(fx, y, field_w, 1);
        let editing = focused && w.is_text_focused();
        let val_style = if focused {
            Style::default().fg(bg).bg(accent)
        } else if f.readonly {
            Style::default().fg(muted)
        } else {
            Style::default().fg(fg)
        };
        let rendered = render_value(f, &w.values[i], editing, w.cursor);
        frame.render_widget(
            Paragraph::new(Span::styled(rendered, val_style)),
            field_rect,
        );
        // Readonly fields are display-only: no click target, so they can never
        // be focused (the focus ring already excludes them).
        if !f.readonly {
            w.field_rects.push((i, field_rect));
        }
        y += 1;
    }

    // Error line (if any).
    if let Some(err) = &w.error {
        frame.render_widget(
            Paragraph::new(Span::styled(err.clone(), Style::default().fg(Color::Red))),
            Rect::new(
                inner.x + 1,
                inner.y + inner.height.saturating_sub(3),
                inner.width.saturating_sub(2),
                1,
            ),
        );
    }

    // Button row — hit-rects derive from the rendered label widths so they can
    // never drift from the drawn glyphs.
    let by = inner.y + inner.height.saturating_sub(2);
    let save_label = " Save ";
    let cancel_label = " Cancel ";
    let save_w = u16::try_from(save_label.len()).unwrap_or(6);
    let cancel_w = u16::try_from(cancel_label.len()).unwrap_or(8);
    let save_rect = Rect::new(inner.x + 2, by, save_w, 1);
    let cancel_rect = Rect::new(save_rect.x + save_w + 2, by, cancel_w, 1);
    frame.render_widget(
        Paragraph::new(Span::styled(
            save_label,
            button_style(w.focus == Focus::Save, accent, bg, muted),
        )),
        save_rect,
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            cancel_label,
            button_style(w.focus == Focus::Cancel, accent, bg, muted),
        )),
        cancel_rect,
    );
    w.save_rect = Some(save_rect);
    w.cancel_rect = Some(cancel_rect);

    // Hint line.
    let hint = "Tab move · Space toggle · ←→ page/select · Enter save · Esc cancel";
    frame.render_widget(
        Paragraph::new(Span::styled(hint, Style::default().fg(muted))),
        Rect::new(
            inner.x + 1,
            inner.y + inner.height.saturating_sub(1),
            inner.width.saturating_sub(2),
            1,
        ),
    );
}

fn button_style(focused: bool, accent: Color, bg: Color, muted: Color) -> Style {
    if focused {
        Style::default()
            .fg(bg)
            .bg(accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(muted)
    }
}

/// Render a field's value for display. `editing` shows a cursor caret in
/// text-like fields; masked fields show bullets (or `(unchanged)` when empty
/// and not being edited).
fn render_value(f: &Field, v: &FieldValue, editing: bool, cursor: usize) -> String {
    match (&f.kind, v) {
        (FieldKind::Toggle, FieldValue::Bool(b)) => {
            if *b { "[x]".into() } else { "[ ]".into() }
        }
        (FieldKind::Select(opts), FieldValue::Choice(c)) => {
            format!("‹ {} ›", opts.get(*c).copied().unwrap_or(""))
        }
        (FieldKind::Masked, FieldValue::Text(s)) => {
            if s.is_empty() {
                if editing { "_".into() } else { "(unchanged)".into() }
            } else {
                let mut out = "•".repeat(s.chars().count());
                if editing {
                    out.push('_');
                }
                out
            }
        }
        (_, FieldValue::Text(s)) => {
            if editing {
                let byte = s.char_indices().nth(cursor).map_or(s.len(), |(b, _)| b);
                let mut out = s.clone();
                out.insert(byte, '|');
                out
            } else {
                s.clone()
            }
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo() -> WizardState {
        let fields = vec![
            Field {
                key: "name",
                label: "Name",
                kind: FieldKind::Text,
                page: 0,
                required: true,
                readonly: false,
            },
            Field {
                key: "tls",
                label: "TLS",
                kind: FieldKind::Toggle,
                page: 0,
                required: false,
                readonly: false,
            },
            Field {
                key: "mech",
                label: "Mech",
                kind: FieldKind::Select(vec!["Auto", "PLAIN"]),
                page: 1,
                required: false,
                readonly: false,
            },
            Field {
                key: "id",
                label: "Id",
                kind: FieldKind::Text,
                page: 1,
                required: false,
                readonly: true,
            },
        ];
        let values = vec![
            FieldValue::Text(String::new()),
            FieldValue::Bool(false),
            FieldValue::Choice(0),
            FieldValue::Text("fixed".into()),
        ];
        WizardState::new(
            WizardMode::Add,
            "Add".into(),
            vec!["Basics", "Advanced"],
            fields,
            values,
        )
    }

    #[test]
    fn focus_ring_wraps_through_buttons() {
        let mut w = demo();
        assert_eq!(w.focus, Focus::Field(0)); // first focusable field
        w.focus_next();
        assert_eq!(w.focus, Focus::Field(1)); // tls
        w.focus_next();
        assert_eq!(w.focus, Focus::Save);
        w.focus_next();
        assert_eq!(w.focus, Focus::Cancel);
        w.focus_next();
        assert_eq!(w.focus, Focus::Field(0)); // wrap
        w.focus_prev();
        assert_eq!(w.focus, Focus::Cancel);
    }

    #[test]
    fn readonly_field_is_not_focusable() {
        let mut w = demo();
        w.set_page(1);
        // page 1 focusable fields: only "mech" (id is readonly)
        assert_eq!(w.focus, Focus::Field(2));
        w.focus_next();
        assert_eq!(w.focus, Focus::Save);
    }

    #[test]
    fn typing_edits_focused_text_and_marks_touched() {
        let mut w = demo();
        w.insert_char('h');
        w.insert_char('i');
        assert_eq!(w.text("name"), "hi");
        assert!(w.was_touched("name"));
        w.backspace();
        assert_eq!(w.text("name"), "h");
    }

    #[test]
    fn toggle_and_select_mutate_and_wrap() {
        let mut w = demo();
        w.focus_next(); // tls
        w.toggle_focused();
        assert!(w.boolean("tls"));
        w.set_page(1); // mech
        w.cycle_focused(true);
        assert_eq!(w.choice_str("mech"), "PLAIN");
        w.cycle_focused(true);
        assert_eq!(w.choice_str("mech"), "Auto"); // wraps
        w.cycle_focused(false);
        assert_eq!(w.choice_str("mech"), "PLAIN"); // backward wraps
    }

    #[test]
    fn readonly_field_is_not_editable_even_if_focused() {
        let mut w = demo();
        // Force focus onto the readonly "id" field (index 3) — defensive guard.
        w.focus = Focus::Field(3);
        assert!(!w.is_text_focused());
        w.insert_char('x');
        w.backspace();
        assert_eq!(w.text("id"), "fixed"); // unchanged
    }

    #[test]
    fn focus_predicates() {
        let mut w = demo();
        assert!(w.is_text_focused());
        w.focus_next();
        assert!(w.is_toggle_focused());
        w.set_page(1);
        assert!(w.is_select_focused());
    }
}

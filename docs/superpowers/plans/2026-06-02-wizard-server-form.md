# Wizard Form Toolkit + `/wizard server` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a reusable popup form toolkit and a `/wizard server` add/edit form working in both the TUI and web UI, persisting through the same shared helper as manual `/server add`.

**Architecture:** A UI-mechanism-only field engine (`src/ui/wizard/mod.rs`) holds typed fields + values + focus/page/validation. The server wizard (`src/ui/wizard/server.rs`) supplies a schema and `ServerConfig` ↔ values serializers. Both the TUI overlay and a structured web `SaveServer` command funnel into one `apply_server_config` persistence helper that writes `config.toml` plus `.env` credentials.

**Tech Stack:** Rust 2024, ratatui 0.30 `StatefulWidget`/immediate-mode (no form crate), crossterm mouse events, Leptos 0.7 (web modal), axum WSS (`WebCommand`), serde.

**Review protocol (per the user):** after each Task, run `make test` + `make clippy`, then `/code-review`, fix **all** findings before the next Task, and commit. A full-branch `/code-review` runs at the end.

---

## File Structure

**Create**
- `src/ui/wizard/mod.rs` — toolkit: `FieldKind`, `FieldValue`, `Field`, `Focus`, `WizardState`, navigation, text editing, render, mouse hit-testing.
- `src/ui/wizard/server.rs` — `server_schema`, `prefill_from`, `build`, `CredUpdate`, `BuiltServer`.
- `web-ui/src/components/wizard.rs` — Leptos modal component.

**Modify**
- `src/config/env.rs` — add `remove_env_value`.
- `src/config/mod.rs` — re-export nothing new; used by helper.
- `src/commands/handlers_admin.rs` — add `apply_server_config`, refactor `cmd_server add` to use it.
- `src/commands/registry.rs` — register `/wizard`.
- `src/commands/handlers_ui.rs` — add `cmd_wizard`.
- `src/app/mod.rs` — `wizard: Option<WizardState>` field + ctor + `open_server_wizard`.
- `src/app/input.rs` — route key + mouse to the wizard when open.
- `src/ui/mod.rs` — `pub mod wizard;`.
- `src/ui/layout.rs` — render the wizard overlay top-most.
- `src/web/protocol.rs` + `web-ui/src/protocol.rs` — `WebCommand::SaveServer`.
- `src/app/web.rs` — handle `SaveServer`.
- `web-ui/src/components/mod.rs`, `web-ui/src/state.rs`, `web-ui/src/components/layout.rs` — wire modal + state signal + "+ Add network" button.
- `README.md` — changelog entry.

---

## Task 1: Shared server persistence helper + `.env` credential writes

**Files:**
- Modify: `src/config/env.rs`
- Modify: `src/commands/handlers_admin.rs`
- Test: inline `#[cfg(test)]` in both files.

- [ ] **Step 1: Failing test — `remove_env_value` deletes a key line**

In `src/config/env.rs` tests module:

```rust
#[test]
fn remove_env_value_deletes_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".env");
    std::fs::write(&path, "FOO=a\nLIBERA_PASSWORD=secret\nBAR=b\n").unwrap();

    remove_env_value(&path, "LIBERA_PASSWORD").unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("FOO=a"));
    assert!(content.contains("BAR=b"));
    assert!(!content.contains("LIBERA_PASSWORD"));
}

#[test]
fn remove_env_value_missing_key_is_ok() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".env");
    std::fs::write(&path, "FOO=a\n").unwrap();
    remove_env_value(&path, "NOPE").unwrap();
    assert!(std::fs::read_to_string(&path).unwrap().contains("FOO=a"));
}
```

- [ ] **Step 2: Run, expect failure** — `make test` (or `cargo test -p repartee remove_env_value`). Expected: FAIL (function not found).

- [ ] **Step 3: Implement `remove_env_value`**

In `src/config/env.rs`, after `set_env_value`:

```rust
/// Remove a key from the `.env` file. No-op if the file or key is absent.
pub fn remove_env_value(path: &Path, key: &str) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let prefix = format!("{key}=");
    let kept: Vec<String> = std::fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim_start().starts_with(&prefix))
        .map(String::from)
        .collect();
    crate::fs_secure::write_file(path, kept.join("\n") + "\n", 0o600)?;
    Ok(())
}
```

- [ ] **Step 4: Run, expect pass** — `make test`. Expected: PASS.

- [ ] **Step 5: Failing test — `apply_server_config` persists config + routes creds to `.env`**

In `src/commands/handlers_admin.rs` tests module (add `use` for tempfile as needed):

```rust
#[test]
fn apply_server_config_writes_config_and_env_creds() {
    use crate::config::ServerConfig;
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("config.toml");
    let env_path = dir.path().join(".env");

    let mut servers = std::collections::HashMap::new();
    let base = ServerConfig {
        label: "Libera".into(), address: "irc.libera.chat".into(), port: 6697,
        tls: true, tls_verify: true, autoconnect: true, channels: vec!["#rust".into()],
        nick: Some("kofany".into()), username: None, realname: None,
        password: None, sasl_user: Some("kofany".into()), sasl_pass: None,
        bind_ip: None, encoding: None, auto_reconnect: None, reconnect_delay: None,
        reconnect_max_retries: None, autosendcmd: None, sasl_mechanism: None,
        client_cert_path: None,
    };

    apply_server_config(
        &mut servers, &cfg_path, &env_path, "libera", base,
        CredUpdate::Set("serverpass".into()), CredUpdate::Set("saslpass".into()),
    ).unwrap();

    // config.toml has the server but NOT the secrets.
    let toml = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(toml.contains("irc.libera.chat"));
    assert!(!toml.contains("serverpass"));
    assert!(!toml.contains("saslpass"));
    // .env has the secrets under the uppercased id.
    let env = std::fs::read_to_string(&env_path).unwrap();
    assert!(env.contains("LIBERA_PASSWORD=serverpass"));
    assert!(env.contains("LIBERA_SASL_PASS=saslpass"));
    // In-memory config carries the resolved creds.
    let s = servers.get("libera").unwrap();
    assert_eq!(s.password.as_deref(), Some("serverpass"));
    assert_eq!(s.sasl_pass.as_deref(), Some("saslpass"));
}

#[test]
fn apply_server_config_keep_and_remove_creds() {
    use crate::config::ServerConfig;
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("config.toml");
    let env_path = dir.path().join(".env");
    std::fs::write(&env_path, "LIBERA_PASSWORD=old\nLIBERA_SASL_PASS=oldsasl\n").unwrap();

    let mut servers = std::collections::HashMap::new();
    let mut base = ServerConfig {
        label: "Libera".into(), address: "irc.libera.chat".into(), port: 6697,
        tls: true, tls_verify: true, autoconnect: false, channels: vec![],
        nick: None, username: None, realname: None,
        password: Some("old".into()), sasl_user: None, sasl_pass: Some("oldsasl".into()),
        bind_ip: None, encoding: None, auto_reconnect: None, reconnect_delay: None,
        reconnect_max_retries: None, autosendcmd: None, sasl_mechanism: None,
        client_cert_path: None,
    };
    base.password = Some("old".into());

    // Keep password, remove sasl_pass.
    apply_server_config(
        &mut servers, &cfg_path, &env_path, "libera", base,
        CredUpdate::Keep, CredUpdate::Remove,
    ).unwrap();

    let env = std::fs::read_to_string(&env_path).unwrap();
    assert!(env.contains("LIBERA_PASSWORD=old"));   // kept
    assert!(!env.contains("LIBERA_SASL_PASS"));      // removed
    let s = servers.get("libera").unwrap();
    assert_eq!(s.password.as_deref(), Some("old"));
    assert!(s.sasl_pass.is_none());
}
```

- [ ] **Step 6: Run, expect failure** — `make test`. Expected: FAIL (`apply_server_config`, `CredUpdate` not found).

- [ ] **Step 7: Implement `CredUpdate` + `apply_server_config`**

In `src/commands/handlers_admin.rs` (top-level, `pub(crate)`):

```rust
/// How a credential should be persisted to `.env`.
#[derive(Debug, Clone)]
pub(crate) enum CredUpdate {
    /// Leave the existing `.env` value untouched (used in edit mode when the
    /// masked field was not modified).
    Keep,
    /// Write this value to `.env`.
    Set(String),
    /// Delete the key from `.env`.
    Remove,
}

/// Insert/overwrite a server in `servers`, persist `config.toml`, and route the
/// server password + SASL password to `.env` (never `config.toml`). Mutates the
/// in-memory `ServerConfig` so it carries the resolved credentials too.
///
/// `id` must already be lowercased. Shared by manual `/server add`, the TUI
/// wizard, and the web `SaveServer` command.
pub(crate) fn apply_server_config(
    servers: &mut std::collections::HashMap<String, crate::config::ServerConfig>,
    config_path: &std::path::Path,
    env_path: &std::path::Path,
    id: &str,
    mut config: crate::config::ServerConfig,
    password: CredUpdate,
    sasl_pass: CredUpdate,
) -> color_eyre::eyre::Result<()> {
    let upper = id.to_uppercase();

    // Resolve password.
    match password {
        CredUpdate::Set(v) => {
            crate::config::env::set_env_value(env_path, &format!("{upper}_PASSWORD"), &v)?;
            config.password = Some(v);
        }
        CredUpdate::Remove => {
            crate::config::env::remove_env_value(env_path, &format!("{upper}_PASSWORD"))?;
            config.password = None;
        }
        CredUpdate::Keep => { /* config.password already carries the in-memory value */ }
    }

    // Resolve SASL password.
    match sasl_pass {
        CredUpdate::Set(v) => {
            crate::config::env::set_env_value(env_path, &format!("{upper}_SASL_PASS"), &v)?;
            config.sasl_pass = Some(v);
        }
        CredUpdate::Remove => {
            crate::config::env::remove_env_value(env_path, &format!("{upper}_SASL_PASS"))?;
            config.sasl_pass = None;
        }
        CredUpdate::Keep => {}
    }

    servers.insert(id.to_string(), config);
    crate::config::save_config(config_path, &SaveCfg { servers })?;
    Ok(())
}
```

> NOTE for executor: `save_config` takes the whole `&AppConfig`, not just servers. The wizard/manual handlers already hold `app.config`. So `apply_server_config` should take `&mut crate::config::AppConfig` instead of a bare `servers` map, mutate `config.servers`, then `save_config(config_path, config)`. Adjust the signature to `apply_server_config(config: &mut AppConfig, config_path, env_path, id, server, password, sasl_pass)` and in tests build a minimal `AppConfig::default()`-style value. (The `SaveCfg` placeholder above is illustrative only — use `&mut AppConfig`.) Update the two tests to pass `&mut AppConfig` and read `config.servers`.

- [ ] **Step 8: Run, expect pass** — `make test`. Expected: PASS.

- [ ] **Step 9: Refactor `cmd_server add` to use the helper**

In `src/commands/handlers_admin.rs` `cmd_server`, replace the `"add"` arm body (after `parse_server_add_config`) so it routes creds through `.env` instead of dropping them on save:

```rust
"add" => {
    if args.len() < 3 {
        add_local_event(app, SERVER_ADD_USAGE);
        return;
    }
    let id = args[1].to_lowercase();
    let server_config = match parse_server_add_config(&args[2..]) {
        Ok(config) => config,
        Err(e) => {
            add_local_event(app, &format!("{C_ERR}{e}{C_RST}"));
            add_local_event(app, SERVER_ADD_USAGE);
            return;
        }
    };
    let password = server_config.password.clone()
        .map_or(CredUpdate::Remove, CredUpdate::Set);
    let sasl_pass = server_config.sasl_pass.clone()
        .map_or(CredUpdate::Remove, CredUpdate::Set);
    let env_path = crate::constants::env_path();
    let cfg_path = crate::constants::config_path();
    if let Err(e) = apply_server_config(
        &mut app.config, &cfg_path, &env_path, &id, server_config, password, sasl_pass,
    ) {
        add_local_event(app, &format!("{C_ERR}Failed to save server: {e}{C_RST}"));
        return;
    }
    app.cached_config_toml = None;
    add_local_event(app, &format!("{C_OK}Server '{id}' added{C_RST}"));
}
```

> Executor: confirm the env-path accessor name (`crate::constants::env_path()` or similar) by grepping `constants`; reuse whatever the codebase already uses to locate `.env`.

- [ ] **Step 10: Run full suite + clippy** — `make test` then `make clippy`. Expected: 0 failures; no NEW clippy warnings (pre-existing baseline only).

- [ ] **Step 11: Review + commit**

```bash
git add -A && git commit -m "feat(config): apply_server_config helper + .env credential persistence"
```
Then run `/code-review`, fix all findings, amend/extend commit.

---

## Task 2: Wizard toolkit core (field engine)

**Files:**
- Create: `src/ui/wizard/mod.rs`
- Modify: `src/ui/mod.rs` (`pub mod wizard;`)
- Test: inline `#[cfg(test)]` in `mod.rs`.

- [ ] **Step 1: Add module declaration**

In `src/ui/mod.rs` add near the other `pub mod` lines:

```rust
pub mod wizard;
```

- [ ] **Step 2: Write the core types + logic (no rendering yet)**

Create `src/ui/wizard/mod.rs`:

```rust
//! Reusable popup-form engine. UI-mechanism only — it knows nothing about
//! servers. Consumers (see `server.rs`) supply a field schema + serializers.
//!
//! A wizard is a fixed-size modal with one or more pages. Tab/Shift-Tab move
//! focus across the focusable fields on the current page and the Save/Cancel
//! buttons; the page is switched with Left/Right when no text field is focused
//! (or always via the tab row / mouse). Mouse clicks focus fields, toggle
//! checkboxes, switch pages, and press buttons via rects recorded at render time.

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

/// The current value of a field, parallel to `WizardState.fields`.
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
    /// Empty Text/Masked fails validation when true (checked by consumer).
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

/// Which concrete wizard this is (drives `build`/title).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardKind {
    Server,
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
    pub kind: WizardKind,
    pub mode: WizardMode,
    pub title: String,
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
    #[must_use]
    pub fn new(
        kind: WizardKind,
        mode: WizardMode,
        title: String,
        fields: Vec<Field>,
        values: Vec<FieldValue>,
    ) -> Self {
        let num_pages = fields.iter().map(|f| f.page + 1).max().unwrap_or(1);
        let touched = vec![false; fields.len()];
        let mut s = Self {
            kind, mode, title, fields, values, touched,
            page: 0, num_pages, focus: Focus::Save, cursor: 0, error: None,
            field_rects: Vec::new(), tab_rects: Vec::new(),
            save_rect: None, cancel_rect: None,
        };
        s.focus = s.first_field_focus(0).unwrap_or(Focus::Save);
        s.sync_cursor_to_focus();
        s
    }

    /// Focusable (non-readonly) field indices on `page`, in order.
    fn page_fields(&self, page: usize) -> Vec<usize> {
        self.fields.iter().enumerate()
            .filter(|(_, f)| f.page == page && !f.readonly)
            .map(|(i, _)| i)
            .collect()
    }

    fn first_field_focus(&self, page: usize) -> Option<Focus> {
        self.page_fields(page).first().map(|&i| Focus::Field(i))
    }

    /// The focus traversal order on the current page: fields then Save, Cancel.
    fn focus_ring(&self) -> Vec<Focus> {
        let mut ring: Vec<Focus> = self.page_fields(self.page).into_iter().map(Focus::Field).collect();
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

    pub fn next_page(&mut self) {
        let p = (self.page + 1) % self.num_pages;
        self.set_page(p);
    }

    /// Place the cursor at the end of the focused text-like field.
    fn sync_cursor_to_focus(&mut self) {
        self.cursor = match self.focus {
            Focus::Field(i) => match &self.values[i] {
                FieldValue::Text(s) => s.chars().count(),
                _ => 0,
            },
            _ => 0,
        };
    }

    fn focused_text_mut(&mut self) -> Option<&mut String> {
        if let Focus::Field(i) = self.focus {
            if matches!(self.fields[i].kind, FieldKind::Text | FieldKind::Masked | FieldKind::Number) {
                self.touched[i] = true;
                if let FieldValue::Text(s) = &mut self.values[i] {
                    return Some(s);
                }
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
        if self.cursor == 0 { return; }
        let cursor = self.cursor;
        if let Some(s) = self.focused_text_mut() {
            if let Some((b, ch)) = s.char_indices().nth(cursor - 1) {
                s.replace_range(b..b + ch.len_utf8(), "");
                self.cursor -= 1;
            }
        }
    }

    pub fn cursor_left(&mut self) { self.cursor = self.cursor.saturating_sub(1); }
    pub fn cursor_right(&mut self) {
        if let Focus::Field(i) = self.focus {
            if let FieldValue::Text(s) = &self.values[i] {
                self.cursor = (self.cursor + 1).min(s.chars().count());
            }
        }
    }

    /// Space on a Toggle, or Left/Right cycle on a Select.
    pub fn activate_focused(&mut self) {
        if let Focus::Field(i) = self.focus {
            self.touched[i] = true;
            match (&self.fields[i].kind, &mut self.values[i]) {
                (FieldKind::Toggle, FieldValue::Bool(b)) => *b = !*b,
                _ => {}
            }
        }
    }

    pub fn cycle_focused(&mut self, forward: bool) {
        if let Focus::Field(i) = self.focus {
            if let (FieldKind::Select(opts), FieldValue::Choice(c)) =
                (&self.fields[i].kind, &mut self.values[i])
            {
                let n = opts.len().max(1);
                *c = if forward { (*c + 1) % n } else { (*c + n - 1) % n };
                self.touched[i] = true;
            }
        }
    }

    // Value accessors used by consumers' `build`.
    #[must_use]
    pub fn text(&self, key: &str) -> &str {
        self.field_index(key)
            .and_then(|i| match &self.values[i] { FieldValue::Text(s) => Some(s.as_str()), _ => None })
            .unwrap_or("")
    }
    #[must_use]
    pub fn boolean(&self, key: &str) -> bool {
        self.field_index(key)
            .and_then(|i| match &self.values[i] { FieldValue::Bool(b) => Some(*b), _ => None })
            .unwrap_or(false)
    }
    #[must_use]
    pub fn choice_str(&self, key: &str) -> &'static str {
        self.field_index(key).and_then(|i| match (&self.fields[i].kind, &self.values[i]) {
            (FieldKind::Select(opts), FieldValue::Choice(c)) => opts.get(*c).copied(),
            _ => None,
        }).unwrap_or("")
    }
    #[must_use]
    pub fn was_touched(&self, key: &str) -> bool {
        self.field_index(key).is_some_and(|i| self.touched[i])
    }
    fn field_index(&self, key: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.key == key)
    }
}
```

- [ ] **Step 3: Add navigation/edit tests**

Append to `src/ui/wizard/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn demo() -> WizardState {
        let fields = vec![
            Field { key: "name", label: "Name", kind: FieldKind::Text, page: 0, required: true, readonly: false },
            Field { key: "tls", label: "TLS", kind: FieldKind::Toggle, page: 0, required: false, readonly: false },
            Field { key: "mech", label: "Mech", kind: FieldKind::Select(vec!["Auto","PLAIN"]), page: 1, required: false, readonly: false },
            Field { key: "id", label: "Id", kind: FieldKind::Text, page: 1, required: false, readonly: true },
        ];
        let values = vec![
            FieldValue::Text(String::new()),
            FieldValue::Bool(false),
            FieldValue::Choice(0),
            FieldValue::Text("fixed".into()),
        ];
        WizardState::new(WizardKind::Server, WizardMode::Add, "Add".into(), fields, values)
    }

    #[test]
    fn focus_ring_wraps_through_buttons() {
        let mut w = demo();
        assert_eq!(w.focus, Focus::Field(0));      // first focusable field
        w.focus_next();                             // tls
        assert_eq!(w.focus, Focus::Field(1));
        w.focus_next(); assert_eq!(w.focus, Focus::Save);
        w.focus_next(); assert_eq!(w.focus, Focus::Cancel);
        w.focus_next(); assert_eq!(w.focus, Focus::Field(0)); // wrap
        w.focus_prev(); assert_eq!(w.focus, Focus::Cancel);
    }

    #[test]
    fn readonly_field_is_not_focusable() {
        let mut w = demo();
        w.set_page(1);
        // page 1 focusable fields: only "mech" (id is readonly)
        assert_eq!(w.focus, Focus::Field(2));
        w.focus_next(); assert_eq!(w.focus, Focus::Save);
    }

    #[test]
    fn typing_edits_focused_text_and_marks_touched() {
        let mut w = demo();
        w.insert_char('h'); w.insert_char('i');
        assert_eq!(w.text("name"), "hi");
        assert!(w.was_touched("name"));
        w.backspace();
        assert_eq!(w.text("name"), "h");
    }

    #[test]
    fn toggle_and_select_mutate() {
        let mut w = demo();
        w.focus_next(); // tls
        w.activate_focused();
        assert!(w.boolean("tls"));
        w.set_page(1); // mech
        w.cycle_focused(true);
        assert_eq!(w.choice_str("mech"), "PLAIN");
        w.cycle_focused(true);
        assert_eq!(w.choice_str("mech"), "Auto"); // wraps
    }
}
```

- [ ] **Step 4: Run, expect pass** — `make test`. Expected: PASS.

- [ ] **Step 5: clippy** — `make clippy`. Fix any new warnings (derive where suggested, `#[must_use]`, etc.).

- [ ] **Step 6: Review + commit**

```bash
git add -A && git commit -m "feat(ui): reusable wizard form engine (fields, focus, edit, validation hooks)"
```
Run `/code-review`, fix all findings before Task 3.

---

## Task 3: Server wizard schema + serializers

**Files:**
- Create: `src/ui/wizard/server.rs`
- Test: inline `#[cfg(test)]`.

- [ ] **Step 1: Write the schema + build + prefill + slug**

Create `src/ui/wizard/server.rs`:

```rust
//! Server wizard: the field schema for adding/editing an IRC server and the
//! `ServerConfig` <-> field-values serializers. The reusable engine in
//! `super` knows none of this.

use std::collections::HashMap;

use super::{Field, FieldKind, FieldValue, WizardMode, WizardState, WizardKind};
use crate::commands::handlers_admin::CredUpdate;
use crate::config::ServerConfig;

const SASL_MECHS: &[&str] = &["Auto", "PLAIN", "EXTERNAL"];

/// Slugify a network name into a server id: lowercase, non-`[a-z0-9_]` runs
/// collapse to a single `_`, leading/trailing `_` trimmed.
#[must_use]
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_us = false;
    for ch in name.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
    }
    out.trim_matches('_').to_string()
}

/// Make `base` unique against existing ids by appending `_2`, `_3`, …
#[must_use]
pub fn unique_id(base: &str, servers: &HashMap<String, ServerConfig>) -> String {
    let base = if base.is_empty() { "server".to_string() } else { base.to_string() };
    if !servers.contains_key(&base) { return base; }
    (2..).map(|n| format!("{base}_{n}")).find(|c| !servers.contains_key(c)).unwrap_or(base)
}

/// The field schema. Page 0 = Basics, page 1 = Advanced.
fn schema(edit: bool) -> Vec<Field> {
    vec![
        Field { key: "network", label: "Network Name", kind: FieldKind::Text, page: 0, required: true, readonly: false },
        Field { key: "address", label: "Server address / IP", kind: FieldKind::Text, page: 0, required: true, readonly: false },
        Field { key: "port", label: "Port", kind: FieldKind::Number, page: 0, required: false, readonly: false },
        Field { key: "tls", label: "Use TLS/SSL", kind: FieldKind::Toggle, page: 0, required: false, readonly: false },
        Field { key: "tls_verify", label: "Verify TLS certificate", kind: FieldKind::Toggle, page: 0, required: false, readonly: false },
        Field { key: "bind_ip", label: "Bind IP", kind: FieldKind::Text, page: 0, required: false, readonly: false },
        // Advanced
        Field { key: "id", label: "Server id", kind: FieldKind::Text, page: 1, required: false, readonly: edit },
        Field { key: "nick", label: "Nick", kind: FieldKind::Text, page: 1, required: false, readonly: false },
        Field { key: "username", label: "Username", kind: FieldKind::Text, page: 1, required: false, readonly: false },
        Field { key: "realname", label: "Realname", kind: FieldKind::Text, page: 1, required: false, readonly: false },
        Field { key: "channels", label: "Channels (comma-separated)", kind: FieldKind::Text, page: 1, required: false, readonly: false },
        Field { key: "password", label: "Server password", kind: FieldKind::Masked, page: 1, required: false, readonly: false },
        Field { key: "sasl_user", label: "SASL user", kind: FieldKind::Text, page: 1, required: false, readonly: false },
        Field { key: "sasl_pass", label: "SASL pass", kind: FieldKind::Masked, page: 1, required: false, readonly: false },
        Field { key: "sasl_mechanism", label: "SASL mechanism", kind: FieldKind::Select(SASL_MECHS.to_vec()), page: 1, required: false, readonly: false },
        Field { key: "encoding", label: "Encoding", kind: FieldKind::Text, page: 1, required: false, readonly: false },
        Field { key: "autoconnect", label: "Autoconnect", kind: FieldKind::Toggle, page: 1, required: false, readonly: false },
        Field { key: "auto_reconnect", label: "Auto-reconnect", kind: FieldKind::Toggle, page: 1, required: false, readonly: false },
        Field { key: "reconnect_delay", label: "Reconnect delay (s)", kind: FieldKind::Number, page: 1, required: false, readonly: false },
        Field { key: "reconnect_max_retries", label: "Reconnect max retries", kind: FieldKind::Number, page: 1, required: false, readonly: false },
        Field { key: "autosendcmd", label: "Autosendcmd", kind: FieldKind::Text, page: 1, required: false, readonly: false },
        Field { key: "client_cert_path", label: "Client cert path", kind: FieldKind::Text, page: 1, required: false, readonly: false },
    ]
}

fn mech_index(m: Option<&str>) -> usize {
    match m { Some("PLAIN") => 1, Some("EXTERNAL") => 2, _ => 0 }
}

fn opt(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() { None } else { Some(t.to_string()) }
}

/// Default values for an "add" wizard.
fn add_values(fields: &[Field]) -> Vec<FieldValue> {
    fields.iter().map(|f| match (f.key, &f.kind) {
        ("port", _) => FieldValue::Text("6667".into()),
        ("tls_verify", _) => FieldValue::Bool(true),
        ("autoconnect", _) => FieldValue::Bool(true),
        ("auto_reconnect", _) => FieldValue::Bool(true),
        (_, FieldKind::Toggle) => FieldValue::Bool(false),
        (_, FieldKind::Select(_)) => FieldValue::Choice(0),
        _ => FieldValue::Text(String::new()),
    }).collect()
}

/// Values pre-filled from an existing server (edit mode). Masked credential
/// fields are left EMPTY + untouched so they mean "unchanged".
fn edit_values(fields: &[Field], id: &str, s: &ServerConfig) -> Vec<FieldValue> {
    fields.iter().map(|f| match (f.key, &f.kind) {
        ("network", _) => FieldValue::Text(s.label.clone()),
        ("address", _) => FieldValue::Text(s.address.clone()),
        ("port", _) => FieldValue::Text(s.port.to_string()),
        ("tls", _) => FieldValue::Bool(s.tls),
        ("tls_verify", _) => FieldValue::Bool(s.tls_verify),
        ("bind_ip", _) => FieldValue::Text(s.bind_ip.clone().unwrap_or_default()),
        ("id", _) => FieldValue::Text(id.to_string()),
        ("nick", _) => FieldValue::Text(s.nick.clone().unwrap_or_default()),
        ("username", _) => FieldValue::Text(s.username.clone().unwrap_or_default()),
        ("realname", _) => FieldValue::Text(s.realname.clone().unwrap_or_default()),
        ("channels", _) => FieldValue::Text(s.channels.join(", ")),
        ("sasl_user", _) => FieldValue::Text(s.sasl_user.clone().unwrap_or_default()),
        ("sasl_mechanism", _) => FieldValue::Choice(mech_index(s.sasl_mechanism.as_deref())),
        ("encoding", _) => FieldValue::Text(s.encoding.clone().unwrap_or_default()),
        ("autoconnect", _) => FieldValue::Bool(s.autoconnect),
        ("auto_reconnect", _) => FieldValue::Bool(s.auto_reconnect.unwrap_or(true)),
        ("reconnect_delay", _) => FieldValue::Text(s.reconnect_delay.map(|v| v.to_string()).unwrap_or_default()),
        ("reconnect_max_retries", _) => FieldValue::Text(s.reconnect_max_retries.map(|v| v.to_string()).unwrap_or_default()),
        ("autosendcmd", _) => FieldValue::Text(s.autosendcmd.clone().unwrap_or_default()),
        ("client_cert_path", _) => FieldValue::Text(s.client_cert_path.clone().unwrap_or_default()),
        // password / sasl_pass: empty + untouched = "unchanged"
        (_, FieldKind::Masked) => FieldValue::Text(String::new()),
        (_, FieldKind::Toggle) => FieldValue::Bool(false),
        (_, FieldKind::Select(_)) => FieldValue::Choice(0),
        _ => FieldValue::Text(String::new()),
    }).collect()
}

/// Construct the server wizard for add or edit.
#[must_use]
pub fn build_wizard(mode: WizardMode, existing: Option<&ServerConfig>) -> WizardState {
    let edit = matches!(mode, WizardMode::Edit { .. });
    let fields = schema(edit);
    let (title, values) = match (&mode, existing) {
        (WizardMode::Edit { id }, Some(s)) => (format!("Edit Server — {id}"), edit_values(&fields, id, s)),
        _ => ("Add Server".to_string(), add_values(&fields)),
    };
    WizardState::new(WizardKind::Server, mode, title, fields, values)
}

/// Result of a successful build: ready to hand to `apply_server_config`.
pub struct BuiltServer {
    pub id: String,
    pub config: ServerConfig,
    pub password: CredUpdate,
    pub sasl_pass: CredUpdate,
}

/// Validate + serialize the wizard into a `ServerConfig` and credential updates.
/// `servers` is used for uniqueness (add) and for the in-memory "kept" creds (edit).
///
/// # Errors
/// Returns a human-readable message when a required field is empty or a number
/// is out of range / the id collides.
pub fn build(w: &WizardState, servers: &HashMap<String, ServerConfig>) -> Result<BuiltServer, String> {
    let network = w.text("network").trim().to_string();
    if network.is_empty() { return Err("Network Name is required".into()); }
    let address = w.text("address").trim().to_string();
    if address.is_empty() { return Err("Server address is required".into()); }

    let tls = w.boolean("tls");
    let mut port: u16 = {
        let raw = w.text("port").trim();
        if raw.is_empty() { if tls { 6697 } else { 6667 } }
        else { raw.parse().map_err(|_| "Port must be a number 1–65535".to_string())? }
    };
    if tls && port == 6667 { port = 6697; }

    let parse_u64 = |key: &str, what: &str| -> Result<Option<u64>, String> {
        let raw = w.text(key).trim();
        if raw.is_empty() { Ok(None) } else { raw.parse::<u64>().map(Some).map_err(|_| format!("{what} must be a number")) }
    };
    let parse_u32 = |key: &str, what: &str| -> Result<Option<u32>, String> {
        let raw = w.text(key).trim();
        if raw.is_empty() { Ok(None) } else { raw.parse::<u32>().map(Some).map_err(|_| format!("{what} must be a number")) }
    };

    // id resolution
    let (mode_edit, id) = match &w.mode {
        WizardMode::Edit { id } => (true, id.clone()),
        WizardMode::Add => {
            let typed = w.text("id").trim();
            let base = if typed.is_empty() { slugify(&network) } else { slugify(typed) };
            (false, unique_id(&base, servers))
        }
    };

    let mech = match w.choice_str("sasl_mechanism") {
        "PLAIN" => Some("PLAIN".to_string()),
        "EXTERNAL" => Some("EXTERNAL".to_string()),
        _ => None,
    };

    let mut config = ServerConfig {
        label: network,
        address,
        port,
        tls,
        tls_verify: w.boolean("tls_verify"),
        autoconnect: w.boolean("autoconnect"),
        channels: w.text("channels").split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect(),
        nick: opt(w.text("nick")),
        username: opt(w.text("username")),
        realname: opt(w.text("realname")),
        password: None,
        sasl_user: opt(w.text("sasl_user")),
        sasl_pass: None,
        bind_ip: opt(w.text("bind_ip")),
        encoding: opt(w.text("encoding")),
        auto_reconnect: Some(w.boolean("auto_reconnect")),
        reconnect_delay: parse_u64("reconnect_delay", "Reconnect delay")?,
        reconnect_max_retries: parse_u32("reconnect_max_retries", "Reconnect max retries")?,
        autosendcmd: opt(w.text("autosendcmd")),
        sasl_mechanism: mech,
        client_cert_path: opt(w.text("client_cert_path")),
    };

    // Resolve masked credentials → CredUpdate + in-memory config value.
    let resolve_cred = |key: &str, existing: Option<&str>| -> CredUpdate {
        if !w.was_touched(key) {
            return CredUpdate::Keep; // unchanged
        }
        let v = w.text(key);
        if v.is_empty() { CredUpdate::Remove } else { CredUpdate::Set(v.to_string()) }
    };

    let existing = if mode_edit { servers.get(&id) } else { None };
    let password = resolve_cred("password", existing.and_then(|s| s.password.as_deref()));
    let sasl_pass = resolve_cred("sasl_pass", existing.and_then(|s| s.sasl_pass.as_deref()));
    // Keep in-memory creds consistent for the "Keep" case.
    if let CredUpdate::Keep = password { config.password = existing.and_then(|s| s.password.clone()); }
    if let CredUpdate::Set(ref v) = password { config.password = Some(v.clone()); }
    if let CredUpdate::Keep = sasl_pass { config.sasl_pass = existing.and_then(|s| s.sasl_pass.clone()); }
    if let CredUpdate::Set(ref v) = sasl_pass { config.sasl_pass = Some(v.clone()); }

    Ok(BuiltServer { id, config, password, sasl_pass })
}
```

> Executor note: `resolve_cred`'s `existing` arg is unused after refactor — drop the param to satisfy clippy, or keep and use. Prefer dropping it; read existing creds directly where needed.

- [ ] **Step 2: Tests — slug, uniqueness, round-trip, validation, creds**

Append to `server.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn empty_servers() -> HashMap<String, ServerConfig> { HashMap::new() }

    #[test]
    fn slugify_basics() {
        assert_eq!(slugify("Libera.Chat"), "libera_chat");
        assert_eq!(slugify("  My Net!! "), "my_net");
        assert_eq!(slugify("OFTC"), "oftc");
    }

    #[test]
    fn unique_id_suffixes() {
        let mut servers = empty_servers();
        servers.insert("libera".into(), dummy());
        assert_eq!(unique_id("libera", &servers), "libera_2");
        assert_eq!(unique_id("fresh", &servers), "fresh");
    }

    fn dummy() -> ServerConfig {
        ServerConfig {
            label: "x".into(), address: "x".into(), port: 6667, tls: false, tls_verify: true,
            autoconnect: false, channels: vec![], nick: None, username: None, realname: None,
            password: None, sasl_user: None, sasl_pass: None, bind_ip: None, encoding: None,
            auto_reconnect: None, reconnect_delay: None, reconnect_max_retries: None,
            autosendcmd: None, sasl_mechanism: None, client_cert_path: None,
        }
    }

    #[test]
    fn add_build_requires_network_and_address() {
        let w = build_wizard(WizardMode::Add, None);
        let err = build(&w, &empty_servers()).unwrap_err();
        assert!(err.contains("Network Name"));
    }

    #[test]
    fn add_build_happy_path_tls_bumps_port() {
        let mut w = build_wizard(WizardMode::Add, None);
        // fill network + address + tls on
        type_into(&mut w, "network", "Libera.Chat");
        type_into(&mut w, "address", "irc.libera.chat");
        set_bool(&mut w, "tls", true);
        let built = build(&w, &empty_servers()).unwrap();
        assert_eq!(built.id, "libera_chat");
        assert_eq!(built.config.port, 6697); // default 6667 + tls -> 6697
        assert!(built.config.tls);
        assert_eq!(built.config.label, "Libera.Chat");
    }

    #[test]
    fn add_build_password_sets_credupdate() {
        let mut w = build_wizard(WizardMode::Add, None);
        type_into(&mut w, "network", "Net");
        type_into(&mut w, "address", "host");
        type_into(&mut w, "password", "hunter2");
        let built = build(&w, &empty_servers()).unwrap();
        assert!(matches!(built.password, CredUpdate::Set(ref v) if v == "hunter2"));
        // password not stored in serialized config form, but is in-memory:
        assert_eq!(built.config.password.as_deref(), Some("hunter2"));
    }

    #[test]
    fn edit_untouched_password_is_kept() {
        let mut servers = empty_servers();
        let mut s = dummy();
        s.label = "Net".into(); s.address = "host".into();
        s.password = Some("orig".into());
        servers.insert("net".into(), s.clone());
        let w = build_wizard(WizardMode::Edit { id: "net".into() }, Some(&s));
        let built = build(&w, &servers).unwrap();
        assert!(matches!(built.password, CredUpdate::Keep));
        assert_eq!(built.config.password.as_deref(), Some("orig"));
        assert_eq!(built.id, "net");
    }

    // helpers: set a field's value directly by key for tests
    fn type_into(w: &mut WizardState, key: &str, text: &str) {
        let i = w.fields.iter().position(|f| f.key == key).unwrap();
        w.values[i] = FieldValue::Text(text.into());
        w.touched[i] = true;
    }
    fn set_bool(w: &mut WizardState, key: &str, b: bool) {
        let i = w.fields.iter().position(|f| f.key == key).unwrap();
        w.values[i] = FieldValue::Bool(b);
        w.touched[i] = true;
    }
}
```

- [ ] **Step 3: Run, expect pass** — `make test`. Expected: PASS.
- [ ] **Step 4: clippy** — `make clippy`; fix new warnings.
- [ ] **Step 5: Review + commit**

```bash
git add -A && git commit -m "feat(ui): server wizard schema + ServerConfig serializers + validation"
```
Run `/code-review`, fix all findings before Task 4.

---

## Task 4: TUI overlay — render, input/mouse routing, `/wizard` command

**Files:**
- Modify: `src/app/mod.rs` (field + ctor + `open_server_wizard`)
- Modify: `src/ui/wizard/mod.rs` (add `render` fn)
- Modify: `src/ui/layout.rs` (call render top-most)
- Modify: `src/app/input.rs` (key + mouse routing)
- Modify: `src/commands/registry.rs` (+`/wizard`)
- Modify: `src/commands/handlers_ui.rs` (`cmd_wizard`)

- [ ] **Step 1: App overlay field**

In `src/app/mod.rs`, add to the `App` struct near `emote_picker`:

```rust
/// Open add/edit-server (or future) wizard overlay, if any.
pub wizard: Option<crate::ui::wizard::WizardState>,
```

In the constructor (near `emote_picker: …default()`):

```rust
wizard: None,
```

Add helper methods on `App`:

```rust
pub(crate) fn open_server_wizard(&mut self, id: Option<&str>) {
    use crate::ui::wizard::{WizardMode, server::build_wizard};
    let w = match id {
        Some(id) => match self.config.servers.get(id) {
            Some(s) => build_wizard(WizardMode::Edit { id: id.to_string() }, Some(s)),
            None => {
                crate::commands::handlers_admin::add_local_event(
                    self, &format!("No server with id '{id}'"));
                return;
            }
        },
        None => build_wizard(WizardMode::Add, None),
    };
    self.wizard = Some(w);
}

#[must_use]
pub fn wizard_open(&self) -> bool { self.wizard.is_some() }
```

> Executor: `add_local_event` is currently private to `handlers_admin`; either make it `pub(crate)` or emit the event through whatever local-event helper `App` already exposes. Grep for the existing pattern.

- [ ] **Step 2: Render fn for the wizard**

Add to `src/ui/wizard/mod.rs` (uses theme colors like `emote_picker`):

```rust
use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::App;
use crate::theme::hex_to_color;

fn centered_rect(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width.saturating_sub(w)) / 2,
              area.y + (area.height.saturating_sub(h)) / 2, w, h)
}

/// Render the wizard overlay (no-op when none open). Records hit rects for mouse.
pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let Some(w) = app.wizard.as_mut() else { return; };
    let colors = &app.theme.colors;
    let bg = hex_to_color(&colors.bg_alt).unwrap_or(ratatui::style::Color::Black);
    let border = hex_to_color(&colors.fg_muted).unwrap_or(ratatui::style::Color::DarkGray);
    let accent = hex_to_color(&colors.accent).unwrap_or(ratatui::style::Color::Cyan);
    let err_col = hex_to_color(&colors.error).unwrap_or(ratatui::style::Color::Red);

    let popup = centered_rect(area, 60, 20);
    frame.render_widget(Clear, popup);

    // Tab row in the title: "Basics · Advanced"
    let block = Block::default()
        .title(Span::styled(format!(" {} ", w.title), Style::default().fg(accent)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(bg));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    w.field_rects.clear();
    w.tab_rects.clear();
    w.save_rect = None;
    w.cancel_rect = None;
    if inner.width == 0 || inner.height == 0 { return; }

    // Tab row (page selector) on the first inner line.
    let page_names = ["Basics", "Advanced"];
    let mut x = inner.x + 1;
    for p in 0..w.num_pages {
        let label = page_names.get(p).copied().unwrap_or("Page");
        let style = if p == w.page {
            Style::default().fg(bg).bg(accent).add_modifier(Modifier::BOLD)
        } else { Style::default().fg(border).bg(bg) };
        let txt = format!(" {label} ");
        let rect = Rect::new(x, inner.y, txt.len() as u16, 1);
        frame.render_widget(Paragraph::new(Span::styled(txt.clone(), style)), rect);
        w.tab_rects.push((p, rect));
        x += txt.len() as u16 + 1;
    }

    // Fields for the current page.
    let mut y = inner.y + 2;
    let label_w = 22u16;
    for (i, f) in w.fields.iter().enumerate() {
        if f.page != w.page { continue; }
        if y >= inner.y + inner.height.saturating_sub(3) { break; }
        let focused = w.focus == Focus::Field(i);
        let label_style = Style::default().fg(if focused { accent } else { border });
        frame.render_widget(
            Paragraph::new(Span::styled(f.label, label_style)),
            Rect::new(inner.x + 1, y, label_w, 1),
        );
        let fx = inner.x + 1 + label_w;
        let fw = inner.width.saturating_sub(label_w + 3);
        let field_rect = Rect::new(fx, y, fw, 1);
        let val_style = if focused {
            Style::default().fg(bg).bg(accent)
        } else { Style::default().fg(hex_to_color(&colors.fg).unwrap_or(ratatui::style::Color::White)) };
        let rendered = render_value(f, &w.values[i], focused && matches!(f.kind, FieldKind::Text|FieldKind::Masked|FieldKind::Number), w.cursor);
        frame.render_widget(Paragraph::new(Span::styled(rendered, val_style)), field_rect);
        w.field_rects.push((i, field_rect));
        y += 1;
    }

    // Error line (if any) just above the buttons.
    if let Some(err) = &w.error {
        let ey = inner.y + inner.height.saturating_sub(3);
        frame.render_widget(
            Paragraph::new(Span::styled(err.clone(), Style::default().fg(err_col))),
            Rect::new(inner.x + 1, ey, inner.width.saturating_sub(2), 1),
        );
    }

    // Button row + hint.
    let by = inner.y + inner.height.saturating_sub(2);
    let save_style = button_style(w.focus == Focus::Save, accent, bg, border);
    let cancel_style = button_style(w.focus == Focus::Cancel, accent, bg, border);
    let save_rect = Rect::new(inner.x + 2, by, 8, 1);
    let cancel_rect = Rect::new(inner.x + 12, by, 10, 1);
    frame.render_widget(Paragraph::new(Span::styled(" Save ", save_style)), save_rect);
    frame.render_widget(Paragraph::new(Span::styled(" Cancel ", cancel_style)), cancel_rect);
    w.save_rect = Some(save_rect);
    w.cancel_rect = Some(cancel_rect);

    let hint = "Tab move · Space toggle · ◂▸ page · Enter activate · Esc cancel";
    frame.render_widget(
        Paragraph::new(Span::styled(hint, Style::default().fg(border))),
        Rect::new(inner.x + 1, inner.y + inner.height.saturating_sub(1), inner.width.saturating_sub(2), 1),
    );
    let _ = Line::default(); // silence unused import if Line not needed
}

fn button_style(focused: bool, accent: ratatui::style::Color, bg: ratatui::style::Color, border: ratatui::style::Color) -> Style {
    if focused { Style::default().fg(bg).bg(accent).add_modifier(Modifier::BOLD) }
    else { Style::default().fg(border) }
}

fn render_value(f: &Field, v: &FieldValue, show_cursor: bool, cursor: usize) -> String {
    match (&f.kind, v) {
        (FieldKind::Toggle, FieldValue::Bool(b)) => if *b { "[x]".into() } else { "[ ]".into() },
        (FieldKind::Select(opts), FieldValue::Choice(c)) => format!("‹ {} ›", opts.get(*c).copied().unwrap_or("")),
        (FieldKind::Masked, FieldValue::Text(s)) => {
            if s.is_empty() && !show_cursor { "(unchanged)".into() }
            else { "•".repeat(s.chars().count()) + if show_cursor { "_" } else { "" } }
        }
        (_, FieldValue::Text(s)) => {
            let mut out = s.clone();
            if show_cursor {
                let byte = s.char_indices().nth(cursor).map_or(s.len(), |(b, _)| b);
                out.insert(byte, '_');
            }
            out
        }
        _ => String::new(),
    }
}
```

> Executor: this render is a first cut — adjust to real theme color field names (`colors.error`, `colors.fg`, `colors.fg_muted`, `colors.accent`, `colors.bg_alt`) by grepping `struct ThemeColors`. Remove the `Line` dummy if unused. `as u16` casts on `txt.len()` need `u16::try_from(...).unwrap_or(0)` to satisfy clippy.

- [ ] **Step 3: Wire render into layout (top-most)**

In `src/ui/layout.rs`, after the emote picker render (line ~156):

```rust
    // Wizard overlay (top-most modal when open).
    super::wizard::render(frame, frame.area(), app);
```

- [ ] **Step 4: Key routing**

In `src/app/input.rs` `handle_key`, near the emote-picker guard (line ~122):

```rust
        if self.wizard.is_some() {
            self.handle_wizard_key(key);
            return;
        }
```

Add the handler method (near `handle_emote_picker_key`):

```rust
fn handle_wizard_key(&mut self, key: event::KeyEvent) {
    use crate::ui::wizard::Focus;
    let Some(w) = self.wizard.as_mut() else { return; };
    match (key.modifiers, key.code) {
        (_, KeyCode::Esc) => { self.wizard = None; }
        (KeyModifiers::SHIFT, KeyCode::BackTab) | (_, KeyCode::BackTab) => w.focus_prev(),
        (_, KeyCode::Tab) => w.focus_next(),
        (_, KeyCode::Up) => w.focus_prev(),
        (_, KeyCode::Down) => w.focus_next(),
        (_, KeyCode::Left) => {
            if matches!(w.focus, Focus::Field(_)) && w.is_select_focused() { w.cycle_focused(false); }
            else { w.set_page(w.page.saturating_sub(1)); }
        }
        (_, KeyCode::Right) => {
            if matches!(w.focus, Focus::Field(_)) && w.is_select_focused() { w.cycle_focused(true); }
            else if w.page + 1 < w.num_pages { w.set_page(w.page + 1); }
        }
        (_, KeyCode::Char(' ')) if w.is_toggle_focused() => w.activate_focused(),
        (_, KeyCode::Backspace) => w.backspace(),
        (_, KeyCode::Enter) => {
            match w.focus {
                Focus::Save => self.wizard_save(),
                Focus::Cancel => { self.wizard = None; }
                Focus::Field(_) => w.focus_next(),
            }
        }
        (m, KeyCode::Char(c)) if !m.contains(KeyModifiers::CONTROL) && w.is_text_focused() => {
            w.insert_char(c);
        }
        _ => {}
    }
}

fn wizard_save(&mut self) {
    let Some(w) = self.wizard.as_ref() else { return; };
    match crate::ui::wizard::server::build(w, &self.config.servers) {
        Ok(built) => {
            let env_path = crate::constants::env_path();
            let cfg_path = crate::constants::config_path();
            let id = built.id.clone();
            if let Err(e) = crate::commands::handlers_admin::apply_server_config(
                &mut self.config, &cfg_path, &env_path, &built.id, built.config, built.password, built.sasl_pass,
            ) {
                if let Some(w) = self.wizard.as_mut() { w.error = Some(format!("Save failed: {e}")); }
                return;
            }
            self.cached_config_toml = None;
            self.wizard = None;
            crate::commands::handlers_admin::add_local_event(self, &format!("Server '{id}' saved"));
        }
        Err(msg) => { if let Some(w) = self.wizard.as_mut() { w.error = Some(msg); } }
    }
}
```

Add small predicate helpers on `WizardState` in `src/ui/wizard/mod.rs`:

```rust
#[must_use] pub fn is_select_focused(&self) -> bool {
    matches!(self.focus, Focus::Field(i) if matches!(self.fields[i].kind, FieldKind::Select(_)))
}
#[must_use] pub fn is_toggle_focused(&self) -> bool {
    matches!(self.focus, Focus::Field(i) if matches!(self.fields[i].kind, FieldKind::Toggle))
}
#[must_use] pub fn is_text_focused(&self) -> bool {
    matches!(self.focus, Focus::Field(i) if matches!(self.fields[i].kind, FieldKind::Text | FieldKind::Masked | FieldKind::Number))
}
```

- [ ] **Step 5: Mouse routing**

In `src/app/input.rs` `handle_mouse`, before the emote-picker block (so the wizard, being top-most, wins):

```rust
        if self.wizard.is_some() {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.handle_wizard_click(mouse.column, mouse.row);
            }
            return;
        }
```

Handler:

```rust
fn handle_wizard_click(&mut self, col: u16, row: u16) {
    use crate::ui::wizard::Focus;
    let hit_pt = |r: ratatui::layout::Rect| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height;
    // Save / Cancel
    if let Some(w) = self.wizard.as_ref() {
        if w.save_rect.is_some_and(hit_pt) { self.wizard_save(); return; }
        if w.cancel_rect.is_some_and(hit_pt) { self.wizard = None; return; }
    }
    let Some(w) = self.wizard.as_mut() else { return; };
    // Tabs
    if let Some(&(p, _)) = w.tab_rects.iter().find(|(_, r)| hit_pt(*r)) { w.set_page(p); return; }
    // Fields
    if let Some(&(i, _)) = w.field_rects.iter().find(|(_, r)| hit_pt(*r)) {
        w.focus = Focus::Field(i);
        w.sync_cursor_to_focus_pub();
        if w.is_toggle_focused() { w.activate_focused(); }
        else if w.is_select_focused() { w.cycle_focused(true); }
    }
}
```

> Executor: `sync_cursor_to_focus` is private; expose a `pub fn sync_cursor_to_focus_pub(&mut self)` or make the original `pub`. Also `field_rects`/`tab_rects` borrow vs `self.wizard_save()` mutable borrow — structure so the immutable read of rects completes before the `&mut self` call (as written: check buttons first via `as_ref`, return; then re-borrow `as_mut`).

- [ ] **Step 6: `/wizard` command + registry**

In `src/commands/handlers_ui.rs`:

```rust
pub(crate) fn cmd_wizard(app: &mut App, args: &[String]) {
    match args.first().map(String::as_str) {
        Some("server") => app.open_server_wizard(args.get(1).map(String::as_str)),
        _ => add_local_event(app, "Usage: /wizard server [id]   — open the add/edit-server form"),
    }
}
```

> Executor: confirm `add_local_event` import path in `handlers_ui.rs` (it lives in `handlers_admin`); use the same helper the other UI handlers use.

In `src/commands/registry.rs`, add an entry (Connection category):

```rust
        (
            "wizard",
            CommandDef {
                handler: cmd_wizard,
                description: "Open a guided form (server)",
                aliases: &[],
                category: CommandCategory::Connection,
            },
        ),
```

Ensure `cmd_wizard` is imported in `registry.rs`'s `use` block alongside the other `handlers_ui` imports.

- [ ] **Step 7: Build + test + clippy** — `make test`, `make clippy`. Fix every new warning (casts → `u16::try_from`, `#[must_use]`, `match`→`if let`, etc.).

- [ ] **Step 8: Manual smoke (documented, not automated)** — note in commit body: launch `target/release/repartee`, run `/wizard server`, verify the modal opens, Tab cycles, Space toggles TLS, Left/Right switches pages and cycles SASL mechanism, mouse clicks focus/toggle/press, Save writes the server, `/wizard server <id>` pre-fills.

- [ ] **Step 9: Review + commit**

```bash
git add -A && git commit -m "feat(tui): /wizard server overlay — render, key+mouse routing, save"
```
Run `/code-review`, fix all findings before Task 5.

---

## Task 5: Web protocol `SaveServer` + app handler

**Files:**
- Modify: `src/web/protocol.rs`
- Modify: `web-ui/src/protocol.rs`
- Modify: `src/app/web.rs`

- [ ] **Step 1: Add `SaveServer` to the server-side `WebCommand`**

In `src/web/protocol.rs`, add a variant to `enum WebCommand` (match the existing serde tag/style — grep the enum's `#[serde(...)]`):

```rust
    /// Add or edit a server from the web wizard. `id` empty/None = add (id is
    /// derived from `network`); present = edit that server.
    SaveServer {
        #[serde(default)]
        id: Option<String>,
        network: String,
        address: String,
        #[serde(default)]
        port: Option<u16>,
        tls: bool,
        tls_verify: bool,
        autoconnect: bool,
        #[serde(default)]
        channels: String,
        #[serde(default)]
        nick: String,
        #[serde(default)]
        username: String,
        #[serde(default)]
        realname: String,
        #[serde(default)]
        bind_ip: String,
        #[serde(default)]
        encoding: String,
        #[serde(default)]
        sasl_user: String,
        #[serde(default)]
        sasl_mechanism: String,
        #[serde(default)]
        autosendcmd: String,
        #[serde(default)]
        client_cert_path: String,
        #[serde(default)]
        auto_reconnect: bool,
        #[serde(default)]
        reconnect_delay: String,
        #[serde(default)]
        reconnect_max_retries: String,
        // Credentials: None = unchanged (edit), Some("") = remove, Some(v) = set.
        #[serde(default)]
        password: Option<String>,
        #[serde(default)]
        sasl_pass: Option<String>,
    },
```

Mirror the EXACT same variant in `web-ui/src/protocol.rs`.

- [ ] **Step 2: Handle it in `src/app/web.rs`**

In the `match cmd` block, add an arm that builds a `ServerConfig` and calls the shared helper. To avoid duplicating `build` logic, add a small constructor in `server.rs` that takes raw web fields:

In `src/ui/wizard/server.rs`:

```rust
/// Build a server from raw web-wizard fields (parallel to `build`, no WizardState).
/// `id_in` empty = add (derive + uniquify); non-empty = edit that id.
#[allow(clippy::too_many_arguments)]
pub fn build_from_web(
    servers: &HashMap<String, ServerConfig>,
    id_in: Option<&str>, network: &str, address: &str, port: Option<u16>,
    tls: bool, tls_verify: bool, autoconnect: bool, channels: &str,
    nick: &str, username: &str, realname: &str, bind_ip: &str, encoding: &str,
    sasl_user: &str, sasl_mechanism: &str, autosendcmd: &str, client_cert_path: &str,
    auto_reconnect: bool, reconnect_delay: &str, reconnect_max_retries: &str,
    password: Option<&str>, sasl_pass: Option<&str>,
) -> Result<BuiltServer, String> {
    if network.trim().is_empty() { return Err("Network Name is required".into()); }
    if address.trim().is_empty() { return Err("Server address is required".into()); }
    let mut port = port.unwrap_or(if tls { 6697 } else { 6667 });
    if tls && port == 6667 { port = 6697; }
    let edit = id_in.is_some_and(|s| !s.is_empty());
    let id = if edit { id_in.unwrap().to_string() }
             else { unique_id(&slugify(network), servers) };
    let mech = match sasl_mechanism { "PLAIN" => Some("PLAIN".into()), "EXTERNAL" => Some("EXTERNAL".into()), _ => None };
    let parse_opt = |s: &str| -> Option<String> { let t = s.trim(); (!t.is_empty()).then(|| t.to_string()) };
    let mut config = ServerConfig {
        label: network.trim().to_string(),
        address: address.trim().to_string(),
        port, tls, tls_verify, autoconnect,
        channels: channels.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect(),
        nick: parse_opt(nick), username: parse_opt(username), realname: parse_opt(realname),
        password: None, sasl_user: parse_opt(sasl_user), sasl_pass: None,
        bind_ip: parse_opt(bind_ip), encoding: parse_opt(encoding),
        auto_reconnect: Some(auto_reconnect),
        reconnect_delay: parse_opt(reconnect_delay).and_then(|s| s.parse().ok()),
        reconnect_max_retries: parse_opt(reconnect_max_retries).and_then(|s| s.parse().ok()),
        autosendcmd: parse_opt(autosendcmd), sasl_mechanism: mech,
        client_cert_path: parse_opt(client_cert_path),
    };
    let existing = if edit { servers.get(&id) } else { None };
    let pw = web_cred(password, existing.and_then(|s| s.password.clone()), &mut config.password);
    let sp = web_cred(sasl_pass, existing.and_then(|s| s.sasl_pass.clone()), &mut config.sasl_pass);
    Ok(BuiltServer { id, config, password: pw, sasl_pass: sp })
}

fn web_cred(incoming: Option<&str>, existing: Option<String>, out: &mut Option<String>) -> CredUpdate {
    match incoming {
        None => { *out = existing; CredUpdate::Keep }        // unchanged
        Some("") => { *out = None; CredUpdate::Remove }       // explicit clear
        Some(v) => { *out = Some(v.to_string()); CredUpdate::Set(v.to_string()) }
    }
}
```

Then in `src/app/web.rs`:

```rust
WebCommand::SaveServer {
    id, network, address, port, tls, tls_verify, autoconnect, channels, nick,
    username, realname, bind_ip, encoding, sasl_user, sasl_mechanism, autosendcmd,
    client_cert_path, auto_reconnect, reconnect_delay, reconnect_max_retries,
    password, sasl_pass,
} => {
    match crate::ui::wizard::server::build_from_web(
        &self.config.servers, id.as_deref(), &network, &address, port, tls, tls_verify,
        autoconnect, &channels, &nick, &username, &realname, &bind_ip, &encoding,
        &sasl_user, &sasl_mechanism, &autosendcmd, &client_cert_path, auto_reconnect,
        &reconnect_delay, &reconnect_max_retries, password.as_deref(), sasl_pass.as_deref(),
    ) {
        Ok(built) => {
            let env_path = crate::constants::env_path();
            let cfg_path = crate::constants::config_path();
            let bid = built.id.clone();
            if let Err(e) = crate::commands::handlers_admin::apply_server_config(
                &mut self.config, &cfg_path, &env_path, &built.id, built.config,
                built.password, built.sasl_pass,
            ) {
                tracing::warn!("web SaveServer failed: {e}");
            } else {
                self.cached_config_toml = None;
                tracing::info!("web wizard saved server '{bid}'");
            }
        }
        Err(msg) => tracing::warn!("web SaveServer rejected: {msg}"),
    }
}
```

> Executor: match the exact `self`/handler shape in `src/app/web.rs` (it iterates `WebCommand` in a method — reuse the surrounding `self`). Confirm `build_from_web`'s long arg list is acceptable; `#[allow(clippy::too_many_arguments)]` already added. Consider grouping into a struct later (YAGNI now).

- [ ] **Step 3: Test — round-trip a `SaveServer` JSON applies identically**

In `src/app/web.rs` tests (or a focused test in `server.rs`):

```rust
#[test]
fn build_from_web_add_matches_expectations() {
    use std::collections::HashMap;
    let servers = HashMap::new();
    let built = crate::ui::wizard::server::build_from_web(
        &servers, None, "Libera.Chat", "irc.libera.chat", None, true, true, true,
        "#rust, #repartee", "kofany", "", "", "", "utf-8", "kofany", "PLAIN", "", "",
        true, "", "", Some("pw"), Some("sp"),
    ).unwrap();
    assert_eq!(built.id, "libera_chat");
    assert_eq!(built.config.port, 6697);
    assert_eq!(built.config.channels, vec!["#rust", "#repartee"]);
    assert!(matches!(built.password, crate::commands::handlers_admin::CredUpdate::Set(ref v) if v == "pw"));
    assert_eq!(built.config.sasl_mechanism.as_deref(), Some("PLAIN"));
}
```

- [ ] **Step 4: test + clippy** — `make test`, `make clippy`. Fix new warnings.
- [ ] **Step 5: Review + commit**

```bash
git add -A && git commit -m "feat(web): WebCommand::SaveServer + shared build_from_web persistence"
```
Run `/code-review`, fix all findings before Task 6.

---

## Task 6: Web Leptos modal + "+ Add network" button

**Files:**
- Create: `web-ui/src/components/wizard.rs`
- Modify: `web-ui/src/components/mod.rs` (`pub mod wizard;`)
- Modify: `web-ui/src/state.rs` (signals for open/edit-id)
- Modify: `web-ui/src/components/layout.rs` (mount modal + button + `/wizard` intercept)

- [ ] **Step 1: State signals**

In `web-ui/src/state.rs`, add to the app state struct (mirror existing `RwSignal` fields):

```rust
/// Whether the server wizard modal is open, and the edit id (None = add).
pub wizard_open: RwSignal<bool>,
pub wizard_edit_id: RwSignal<Option<String>>,
```

Initialise in the constructor: `wizard_open: RwSignal::new(false), wizard_edit_id: RwSignal::new(None),`.

- [ ] **Step 2: Modal component**

Create `web-ui/src/components/wizard.rs` — a Leptos component with Basics/Advanced tabs, inputs bound to local signals, Save emitting `WebCommand::SaveServer` over the socket, Cancel closing. Use the same WS-send helper other components use (grep for how `input.rs` sends `WebCommand`).

```rust
use leptos::prelude::*;
use crate::state::AppStateCtx;       // adjust to the real context type
use crate::protocol::WebCommand;

#[component]
pub fn ServerWizard() -> impl IntoView {
    let st = expect_context::<AppStateCtx>();
    let open = st.wizard_open;
    let edit_id = st.wizard_edit_id;

    // Local field signals.
    let network = RwSignal::new(String::new());
    let address = RwSignal::new(String::new());
    let port = RwSignal::new(String::new());
    let tls = RwSignal::new(false);
    let tls_verify = RwSignal::new(true);
    let bind_ip = RwSignal::new(String::new());
    let nick = RwSignal::new(String::new());
    let channels = RwSignal::new(String::new());
    let sasl_user = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let sasl_pass = RwSignal::new(String::new());
    let sasl_mechanism = RwSignal::new("Auto".to_string());
    let username = RwSignal::new(String::new());
    let realname = RwSignal::new(String::new());
    let encoding = RwSignal::new(String::new());
    let autoconnect = RwSignal::new(true);
    let auto_reconnect = RwSignal::new(true);
    let reconnect_delay = RwSignal::new(String::new());
    let reconnect_max_retries = RwSignal::new(String::new());
    let autosendcmd = RwSignal::new(String::new());
    let client_cert_path = RwSignal::new(String::new());
    let page = RwSignal::new(0u8);

    let send = st.send_command();  // adjust to the real sender API

    let on_save = move |_| {
        let cmd = WebCommand::SaveServer {
            id: edit_id.get(),
            network: network.get(), address: address.get(),
            port: port.get().trim().parse::<u16>().ok(),
            tls: tls.get(), tls_verify: tls_verify.get(), autoconnect: autoconnect.get(),
            channels: channels.get(), nick: nick.get(), username: username.get(),
            realname: realname.get(), bind_ip: bind_ip.get(), encoding: encoding.get(),
            sasl_user: sasl_user.get(), sasl_mechanism: sasl_mechanism.get(),
            autosendcmd: autosendcmd.get(), client_cert_path: client_cert_path.get(),
            auto_reconnect: auto_reconnect.get(),
            reconnect_delay: reconnect_delay.get(), reconnect_max_retries: reconnect_max_retries.get(),
            // empty masked field => unchanged (None) in edit, "" stays "" meaning remove only if user cleared.
            password: { let v = password.get(); if v.is_empty() { None } else { Some(v) } },
            sasl_pass: { let v = sasl_pass.get(); if v.is_empty() { None } else { Some(v) } },
        };
        send(cmd);
        open.set(false);
    };

    view! {
        <Show when=move || open.get() fallback=|| ()>
            <div class="wizard-backdrop" on:click=move |_| open.set(false)></div>
            <div class="wizard-modal" on:click=|e| e.stop_propagation()>
                <div class="wizard-head">
                    <h3>{move || if edit_id.get().is_some() { "Edit Server" } else { "Add Server" }}</h3>
                    <span class="wizard-x" on:click=move |_| open.set(false)>"✕"</span>
                </div>
                <div class="wizard-tabs">
                    <button class="wizard-tab" class:active=move || page.get()==0 on:click=move |_| page.set(0)>"Basics"</button>
                    <button class="wizard-tab" class:active=move || page.get()==1 on:click=move |_| page.set(1)>"Advanced"</button>
                </div>
                <div class="wizard-body">
                    <Show when=move || page.get()==0 fallback=|| ()>
                        {text_row("Network Name", network)}
                        {text_row("Server address / IP", address)}
                        {text_row("Port", port)}
                        {check_row("Use TLS/SSL", tls)}
                        {check_row("Verify TLS certificate", tls_verify)}
                        {text_row("Bind IP", bind_ip)}
                    </Show>
                    <Show when=move || page.get()==1 fallback=|| ()>
                        {text_row("Nick", nick)}
                        {text_row("Username", username)}
                        {text_row("Realname", realname)}
                        {text_row("Channels (comma-separated)", channels)}
                        {pass_row("Server password", password)}
                        {text_row("SASL user", sasl_user)}
                        {pass_row("SASL pass", sasl_pass)}
                        {select_row("SASL mechanism", sasl_mechanism, &["Auto","PLAIN","EXTERNAL"])}
                        {text_row("Encoding", encoding)}
                        {check_row("Autoconnect", autoconnect)}
                        {check_row("Auto-reconnect", auto_reconnect)}
                        {text_row("Reconnect delay (s)", reconnect_delay)}
                        {text_row("Reconnect max retries", reconnect_max_retries)}
                        {text_row("Autosendcmd", autosendcmd)}
                        {text_row("Client cert path", client_cert_path)}
                    </Show>
                </div>
                <div class="wizard-foot">
                    <button class="wizard-btn s" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button class="wizard-btn p" on:click=on_save>"Save"</button>
                </div>
            </div>
        </Show>
    }
}

fn text_row(label: &'static str, sig: RwSignal<String>) -> impl IntoView {
    view! { <div class="wizard-row"><label>{label}</label>
        <input prop:value=move || sig.get() on:input=move |e| sig.set(event_target_value(&e)) /></div> }
}
fn pass_row(label: &'static str, sig: RwSignal<String>) -> impl IntoView {
    view! { <div class="wizard-row"><label>{label}</label>
        <input type="password" prop:value=move || sig.get() on:input=move |e| sig.set(event_target_value(&e)) /></div> }
}
fn check_row(label: &'static str, sig: RwSignal<bool>) -> impl IntoView {
    view! { <label class="wizard-check"><input type="checkbox" prop:checked=move || sig.get()
        on:change=move |e| sig.set(event_target_checked(&e)) />{label}</label> }
}
fn select_row(label: &'static str, sig: RwSignal<String>, opts: &'static [&'static str]) -> impl IntoView {
    view! { <div class="wizard-row"><label>{label}</label>
        <select on:change=move |e| sig.set(event_target_value(&e))>
            {opts.iter().map(|o| view!{ <option value=*o selected=move || sig.get()==*o>{*o}</option> }).collect_view()}
        </select></div> }
}
```

> Executor: this is scaffolding against Leptos 0.7 idioms — reconcile with the real `state.rs` context type and WS-send API (grep `expect_context`, `WebCommand::SendMessage` usage in `web-ui/src/components/input.rs`). Edit-prefill from existing servers is NOT available client-side (the web has no full ServerConfig list); for edit, leaving masked fields blank = unchanged is already correct, and other fields can start blank or we skip web-edit-prefill (web edit still works via `/wizard server <id>` typed → but prefill needs server data). DECISION: web wizard prefill is out of scope; web edit simply overwrites the named id with what's entered (document this). The "+ Add network" button covers the primary web flow.

- [ ] **Step 3: Mount modal + "+ Add network" button + `/wizard` intercept**

In `web-ui/src/components/layout.rs`:
- Mount `<ServerWizard/>` once at the top level.
- Add a "+ Add network" button near the buffer/connection list that does `st.wizard_edit_id.set(None); st.wizard_open.set(true);`.
- Where the input command is parsed/sent, intercept a leading `/wizard server` and open the modal instead of sending (grep how `/`-commands are currently handled in `input.rs`; if all commands go to the server as `RunCommand`, the simplest parity is: the server-side `cmd_wizard` won't help the web since it opens the TUI overlay — so intercept client-side). Implementation:

```rust
// in the input submit handler, before sending:
if let Some(rest) = text.strip_prefix("/wizard server") {
    let id = rest.trim();
    st.wizard_edit_id.set(if id.is_empty() { None } else { Some(id.to_string()) });
    st.wizard_open.set(true);
    return; // don't send to server
}
```

- [ ] **Step 4: CSS**

Append to `web-ui/styles/base.css`:

```css
.wizard-backdrop { position:fixed; inset:0; background:rgba(0,0,0,.5); z-index:40; }
.wizard-modal { position:fixed; top:50%; left:50%; transform:translate(-50%,-50%); width:min(460px,92vw);
  background:var(--bg-alt,#161922); border:1px solid var(--border,#2a2f3a); border-radius:10px; z-index:41;
  box-shadow:0 12px 40px rgba(0,0,0,.5); }
.wizard-head { display:flex; align-items:center; justify-content:space-between; padding:12px 16px; border-bottom:1px solid var(--border,#2a2f3a); }
.wizard-head h3 { margin:0; font-size:15px; }
.wizard-x { cursor:pointer; color:var(--fg-muted,#565f73); }
.wizard-tabs { display:flex; gap:4px; padding:10px 16px 0; }
.wizard-tab { padding:6px 14px; border:none; background:transparent; color:var(--fg-muted,#9aa5b8); cursor:pointer; border-radius:7px 7px 0 0; }
.wizard-tab.active { background:var(--bg,#1f2430); color:var(--fg,#fff); }
.wizard-body { padding:14px 16px; display:flex; flex-direction:column; gap:10px; max-height:55vh; overflow:auto; }
.wizard-row { display:flex; flex-direction:column; gap:4px; }
.wizard-row label { font-size:11px; text-transform:uppercase; letter-spacing:.04em; color:var(--fg-muted,#9aa5b8); }
.wizard-row input, .wizard-row select { background:var(--bg,#0e1016); border:1px solid var(--border,#2a2f3a); border-radius:6px; padding:7px 9px; color:var(--fg,#e6edf3); }
.wizard-check { display:flex; align-items:center; gap:7px; }
.wizard-foot { display:flex; justify-content:flex-end; gap:8px; padding:12px 16px; border-top:1px solid var(--border,#2a2f3a); }
.wizard-btn { padding:7px 16px; border-radius:7px; border:none; cursor:pointer; }
.wizard-btn.p { background:var(--accent,#7aa2f7); color:#11131a; }
.wizard-btn.s { background:var(--border,#2a2f3a); color:var(--fg,#cdd3de); }
.add-network-btn { width:100%; border:1px dashed var(--border,#3a4150); background:transparent; color:var(--fg-muted,#9aa5b8); border-radius:6px; padding:5px; cursor:pointer; }
```

> Executor: align the CSS variable names with what `base.css` already defines (grep `--bg`, `--accent`); fall back to literals if the project doesn't use CSS variables.

- [ ] **Step 5: Build WASM** — `make wasm`. Expected: compiles; commit the regenerated `static/web/` + `web-ui/dist/` artifacts as the prior emote work did.

- [ ] **Step 6: test + clippy** — `make test`, `make clippy` (native). Expected: pass.
- [ ] **Step 7: Review + commit**

```bash
git add -A && git commit -m "feat(web): server wizard modal + Add-network button + /wizard intercept"
```
Run `/code-review`, fix all findings before Task 7.

---

## Task 7: Docs + full-branch review + verification

**Files:**
- Modify: `README.md` (changelog bullet)
- Modify: `src/commands/docs.rs` if it documents `/wizard` (optional; grep for command help).

- [ ] **Step 1: README changelog**

Add a bullet under the current unreleased/next changelog section in `README.md`:

```markdown
- Added `/wizard server [id]` — a guided popup form (TUI + web) to add or edit a
  server with mouse + keyboard, Basics/Advanced pages, TLS and bind-IP options, and
  SASL. Credentials are stored in `.env`. Manual `/server add` is unchanged.
```

- [ ] **Step 2: Optional `/wizard` help entry** — if `src/commands/docs.rs` has structured help, add a `/wizard` entry mirroring `/server`'s.

- [ ] **Step 3: Full verification**

```bash
make test     # all pass
make clippy   # 0 new warnings vs the documented baseline
make wasm     # web compiles
make release  # release profile builds
```

- [ ] **Step 4: Full-branch `/code-review`** — run `/code-review` over the entire branch diff vs `main`. Triage and fix all findings. Re-run `make test` + `make clippy` after fixes.

- [ ] **Step 5: Final commit**

```bash
git add -A && git commit -m "docs: changelog for /wizard server; final review fixes"
```

---

## Self-Review (plan vs spec)

- **Reusable toolkit (spec §toolkit):** Task 2 (`mod.rs` engine, generic field model, no server semantics). ✓
- **`/wizard server` add + edit (spec §triggers, §actions):** Tasks 3–4 (`build_wizard` Add/Edit, `cmd_wizard`, `open_server_wizard`). ✓
- **Manual `/server add` unchanged + creds to `.env` (spec §data flow):** Task 1 (refactor reuses `apply_server_config`). ✓
- **Essentials/Advanced two-page layout — Option B (spec §layout):** Task 4 render (tab row, page filter) + Task 6 web tabs. ✓
- **Field types incl. SASL Select (spec §field types):** Task 2 `FieldKind`, Task 3 `SASL_MECHS`. ✓
- **Mouse support (spec §toolkit):** Task 4 `handle_wizard_click` + recorded rects; Task 6 native HTML. ✓
- **Credentials masked, `.env` only, edit "unchanged" sentinel (spec §credentials):** Task 1 `CredUpdate`, Task 3 `was_touched`→Keep/Set/Remove, Task 6 empty=None. ✓
- **id slug + uniqueness, edit id read-only (spec §id rules):** Task 3 `slugify`/`unique_id`, `schema(edit)` readonly. ✓
- **Validation: required, port range, numbers, TLS port bump (spec §validation):** Task 3 `build`. ✓
- **Web structured `SaveServer`, not a command string (spec §data flow):** Task 5. ✓
- **"+ Add network" button + `/wizard server` in web (spec §triggers):** Task 6. ✓
- **Tests across logic/serializers/validation/creds/web (spec §tests):** Tasks 1–5 inline tests. ✓

**Known deviations (documented):** Web wizard does **not** pre-fill from an existing server (the web client has no full `ServerConfig` list); web edit overwrites the named id with entered values, masked-blank = unchanged. TUI edit pre-fills fully. This is acceptable per YAGNI and noted in Task 6.

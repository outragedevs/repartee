# repartee Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a fully themed terminal IRC client UI in Rust/ratatui, then wire up IRC connectivity.

**Architecture:** TEA pattern (Model → Message → Update → View) with tokio async event loop. State is UI-agnostic for future web frontend. Theme engine is an irssi-compatible format string parser ported from kokoirc's TypeScript implementation.

**Tech Stack:** Rust 2024, ratatui 0.30, crossterm 0.29, tokio 1.50, irc 1.1, color-eyre, serde, toml

**Reference:** Design doc at `docs/plans/2026-03-05-repartee-ui-design.md`
**Reference codebase:** kokoirc at `~/dev/kokoirc` (TypeScript source of truth)

---

## Phase 1: Project Scaffold

### Task 1: Initialize Cargo project and dependencies

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `src/constants.rs`
- Create: `.gitignore`

**Step 1: Create Cargo.toml**

```toml
[package]
name = "repartee"
version = "0.1.0"
edition = "2024"

[dependencies]
ratatui = "0.30"
crossterm = { version = "0.29", features = ["event-stream"] }
tokio = { version = "1.50", features = ["full"] }
irc = "1.1"
color-eyre = "0.6"
serde = { version = "1.0", features = ["derive"] }
toml = "1.0"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
futures = "0.3"
chrono = { version = "0.4", features = ["serde"] }
dirs = "6"
unicode-width = "0.2"
thiserror = "2.0"
indexmap = { version = "2", features = ["serde"] }

[profile.release]
lto = true
codegen-units = 1
panic = "abort"
strip = true
```

**Step 2: Create src/constants.rs**

```rust
pub const APP_NAME: &str = "repartee";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

use std::path::PathBuf;

pub fn home_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(format!(".{}", APP_NAME))
}

pub fn config_path() -> PathBuf {
    home_dir().join("config.toml")
}

pub fn theme_dir() -> PathBuf {
    home_dir().join("themes")
}

pub fn env_path() -> PathBuf {
    home_dir().join(".env")
}

pub fn log_dir() -> PathBuf {
    home_dir().join("logs")
}

pub fn scripts_dir() -> PathBuf {
    home_dir().join("scripts")
}
```

**Step 3: Create src/main.rs (minimal hello world)**

```rust
mod constants;

use color_eyre::eyre::Result;

fn main() -> Result<()> {
    color_eyre::install()?;
    println!("{} v{}", constants::APP_NAME, constants::APP_VERSION);
    Ok(())
}
```

**Step 4: Create .gitignore**

```
/target
*.db
*.db-shm
*.db-wal
.env
/.agents
/.claude/skills
skills-lock.json
memory.db*
```

**Step 5: Build and verify**

Run: `cargo build`
Expected: Compiles with no errors

**Step 6: Initialize git and commit**

```bash
git init
git add Cargo.toml Cargo.lock src/ .gitignore CLAUDE.md docs/
git commit -m "feat: initialize repartee project scaffold"
```

---

### Task 2: Create module stubs for all directories

**Files:**
- Create: `src/state/mod.rs`, `src/state/buffer.rs`, `src/state/connection.rs`, `src/state/sorting.rs`, `src/state/events.rs`
- Create: `src/config/mod.rs`, `src/config/defaults.rs`, `src/config/env.rs`
- Create: `src/theme/mod.rs`, `src/theme/loader.rs`, `src/theme/parser.rs`
- Create: `src/irc/mod.rs`, `src/irc/events.rs`, `src/irc/formatting.rs`, `src/irc/flood.rs`, `src/irc/netsplit.rs`, `src/irc/ignore.rs`
- Create: `src/commands/mod.rs`, `src/commands/parser.rs`, `src/commands/registry.rs`, `src/commands/helpers.rs`
- Create: `src/ui/mod.rs`, `src/ui/layout.rs`, `src/ui/topic_bar.rs`, `src/ui/chat_view.rs`, `src/ui/message_line.rs`, `src/ui/buffer_list.rs`, `src/ui/nick_list.rs`, `src/ui/status_line.rs`, `src/ui/input.rs`, `src/ui/styled_text.rs`
- Create: `src/scripting/mod.rs`, `src/scripting/api.rs`, `src/scripting/event_bus.rs`
- Create: `src/image_preview/mod.rs`
- Create: `src/app.rs`

**Step 1:** Create all directories and empty module files with just `// TODO` comments. Each `mod.rs` should declare its submodules. Wire all modules into `main.rs`.

**Step 2: Build and verify**

Run: `cargo build`
Expected: Compiles (all modules empty but wired)

**Step 3: Commit**

```bash
git add -A
git commit -m "feat: create module structure for all components"
```

---

## Phase 2: State & Config Types

### Task 3: Implement state types (Buffer, Message, NickEntry, Connection)

**Files:**
- Implement: `src/state/buffer.rs`
- Implement: `src/state/connection.rs`
- Test: `src/state/buffer.rs` (inline tests)

**Reference:** `~/dev/kokoirc/src/types/index.ts`

**Step 1: Write tests for buffer ID creation and activity level ordering**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_buffer_id_lowercases() {
        assert_eq!(make_buffer_id("libera", "#Rust"), "libera/#rust");
    }

    #[test]
    fn activity_level_ordering() {
        assert!(ActivityLevel::Mention > ActivityLevel::Activity);
        assert!(ActivityLevel::Activity > ActivityLevel::Highlight);
        assert!(ActivityLevel::None < ActivityLevel::Events);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib state::buffer`
Expected: FAIL — types not defined

**Step 3: Implement the types**

Implement in `src/state/buffer.rs`:
- `BufferType` enum (Server, Channel, Query, Special) with `SortGroup`
- `ActivityLevel` enum (None=0, Events=1, Highlight=2, Activity=3, Mention=4) — derive `PartialOrd, Ord`
- `MessageType` enum (Message, Action, Event, Notice, Ctcp)
- `Message` struct
- `NickEntry` struct
- `ListEntry` struct
- `Buffer` struct with `HashMap<String, NickEntry>` for users
- `make_buffer_id()` function

Implement in `src/state/connection.rs`:
- `ConnectionStatus` enum (Connecting, Connected, Disconnected, Error)
- `Connection` struct

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS

**Step 5: Commit**

```bash
git add src/state/
git commit -m "feat: implement core state types (Buffer, Message, Connection)"
```

---

### Task 4: Implement AppState and state mutations

**Files:**
- Implement: `src/state/mod.rs`
- Implement: `src/state/events.rs`
- Test: `src/state/events.rs` (inline tests)

**Reference:** `~/dev/kokoirc/src/core/state/store.ts`

**Step 1: Write tests for state mutations**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_buffer_and_set_active() { ... }

    #[test]
    fn add_message_to_buffer() { ... }

    #[test]
    fn activity_only_escalates() { ... }

    #[test]
    fn activation_resets_activity() { ... }

    #[test]
    fn remove_buffer_falls_back() { ... }
}
```

**Step 2: Implement AppState struct and mutation methods**

`AppState` in `src/state/mod.rs`:
- `connections: HashMap<String, Connection>`
- `buffers: IndexMap<String, Buffer>`
- `active_buffer_id: Option<String>`
- `previous_buffer_id: Option<String>`
- `config: AppConfig` (use placeholder type for now)
- `theme: ThemeFile` (use placeholder type for now)

`src/state/events.rs` — methods on AppState:
- `add_connection()`, `remove_connection()`, `update_connection_status()`
- `add_buffer()`, `remove_buffer()`, `set_active_buffer()`
- `add_message()`, `add_message_with_activity()`
- `set_activity()` (only escalates)
- `update_nick()`, `add_nick()`, `remove_nick()`
- `set_topic()`
- `sorted_buffers()` → returns buffers in display order
- `active_buffer()` → `Option<&Buffer>`
- `next_buffer()`, `prev_buffer()`

**Step 3: Run tests**

Run: `cargo test --lib state`
Expected: PASS

**Step 4: Commit**

```bash
git add src/state/
git commit -m "feat: implement AppState with all mutation methods"
```

---

### Task 5: Implement buffer and nick sorting

**Files:**
- Implement: `src/state/sorting.rs`
- Test: `src/state/sorting.rs` (inline tests)

**Reference:** `~/dev/kokoirc/src/core/state/sorting.ts`

**Step 1: Write tests**

```rust
#[test]
fn sort_buffers_server_before_channel() { ... }

#[test]
fn sort_buffers_alphabetical_within_group() { ... }

#[test]
fn sort_nicks_by_prefix_rank() { ... }

#[test]
fn sort_nicks_alphabetical_same_prefix() { ... }
```

**Step 2: Implement**

- `sort_buffers(buffers) -> Vec<&Buffer>` — by connection label, then sort group, then name
- `sort_nicks(nicks, prefix_order) -> Vec<&NickEntry>` — by prefix rank, then nick alpha

**Step 3: Run tests, commit**

Run: `cargo test --lib state::sorting`

```bash
git add src/state/sorting.rs
git commit -m "feat: implement buffer and nick sorting"
```

---

### Task 6: Implement config loading

**Files:**
- Implement: `src/config/mod.rs`
- Implement: `src/config/defaults.rs`
- Implement: `src/config/env.rs`
- Test: `src/config/mod.rs` (inline tests)

**Reference:** `~/dev/kokoirc/src/types/config.ts`, `~/dev/kokoirc/src/core/config/defaults.ts`, `~/dev/kokoirc/config/config.toml`

**Step 1: Define AppConfig struct tree** (all `#[derive(Deserialize, Serialize, Clone)]`)

```rust
pub struct AppConfig {
    pub general: GeneralConfig,
    pub display: DisplayConfig,
    pub sidepanel: SidepanelConfig,
    pub statusbar: StatusbarConfig,
    pub servers: HashMap<String, ServerConfig>,
    pub aliases: HashMap<String, String>,
}
```

Match all fields from kokoirc's config.ts.

**Step 2: Implement DEFAULT_CONFIG in defaults.rs**

Match kokoirc's defaults.ts values.

**Step 3: Implement load_config() and save_config()**

- `load_config(path) -> Result<AppConfig>` — reads TOML, merges with defaults
- `save_config(path, config) -> Result<()>` — writes back to TOML
- `load_env(path) -> HashMap<String, String>` — parses KEY=VALUE .env file

**Step 4: Write test that loads a sample config TOML string**

**Step 5: Run tests, commit**

```bash
git add src/config/
git commit -m "feat: implement config loading with TOML support"
```

---

## Phase 3: Theme Engine

### Task 7: Implement theme types and loader

**Files:**
- Implement: `src/theme/mod.rs`
- Implement: `src/theme/loader.rs`
- Test: `src/theme/loader.rs` (inline tests)

**Reference:** `~/dev/kokoirc/src/types/theme.ts`, `~/dev/kokoirc/src/core/theme/loader.ts`

**Step 1: Define types**

```rust
pub struct ThemeFile {
    pub meta: ThemeMeta,
    pub colors: ThemeColors,
    pub abstracts: HashMap<String, String>,
    pub formats: ThemeFormats,
}

pub struct ThemeColors {
    pub bg: Color, pub bg_alt: Color, pub border: Color,
    pub fg: Color, pub fg_muted: Color, pub fg_dim: Color,
    pub accent: Color, pub cursor: Color,
}

pub struct StyledSpan {
    pub text: String,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool, pub italic: bool, pub underline: bool, pub dim: bool,
}
```

**Step 2: Implement load_theme()**

- Parse TOML
- Merge with DEFAULT_THEME (hardcoded Nightfall values from kokoirc)
- Convert hex strings to `ratatui::style::Color::Rgb(r,g,b)`

**Step 3: Write test that loads kokoirc's default.theme**

Copy `~/dev/kokoirc/themes/default.theme` to `themes/default.theme` in the project.

Run: `cargo test --lib theme`

**Step 4: Commit**

```bash
git add src/theme/ themes/
git commit -m "feat: implement theme types and TOML loader"
```

---

### Task 8: Implement format string parser — variable substitution

**Files:**
- Implement: `src/theme/parser.rs` (first part)
- Test: `src/theme/parser.rs` (inline tests)

**Reference:** `~/dev/kokoirc/src/core/theme/parser.ts` lines 73-145

**Step 1: Write tests for substitute_vars**

```rust
#[test]
fn substitute_positional() {
    assert_eq!(substitute_vars("$0 says $1", &["Alice", "hello"]), "Alice says hello");
}

#[test]
fn substitute_star() {
    assert_eq!(substitute_vars("$*", &["a", "b", "c"]), "a b c");
}

#[test]
fn substitute_padded_right() {
    assert_eq!(substitute_vars("$[8]0", &["nick"]), "nick    ");
}

#[test]
fn substitute_padded_left() {
    assert_eq!(substitute_vars("$[-8]0", &["nick"]), "    nick");
}
```

**Step 2: Implement substitute_vars()**

Direct port of kokoirc's `substituteVars()` function.

**Step 3: Run tests, commit**

```bash
git add src/theme/parser.rs
git commit -m "feat: implement format string variable substitution"
```

---

### Task 9: Implement format string parser — abstraction expansion

**Files:**
- Modify: `src/theme/parser.rs`
- Test: inline tests

**Reference:** `~/dev/kokoirc/src/core/theme/parser.ts` lines 148-244

**Step 1: Write tests**

```rust
#[test]
fn resolve_simple_abstract() {
    let mut abs = HashMap::new();
    abs.insert("nick".into(), "%_$*%_".into());
    assert_eq!(resolve_abstractions("{nick Alice}", &abs), "%_Alice%_");
}

#[test]
fn resolve_nested_abstractions() {
    let mut abs = HashMap::new();
    abs.insert("msgnick".into(), "<$0$1>".into());
    abs.insert("ownnick".into(), "[$*]".into());
    let result = resolve_abstractions("{msgnick @ {ownnick me}}", &abs);
    assert_eq!(result, "<@[me]>");
}

#[test]
fn resolve_max_depth_does_not_infinite_loop() {
    let mut abs = HashMap::new();
    abs.insert("a".into(), "{a}".into());  // self-referencing
    let _ = resolve_abstractions("{a}", &abs);  // should not hang
}
```

**Step 2: Implement resolve_abstractions()**

Port `resolveAbstractions()`, `findMatchingBrace()`, `splitAbstractionArgs()` from kokoirc.

**Step 3: Run tests, commit**

```bash
git add src/theme/parser.rs
git commit -m "feat: implement abstraction expansion for theme format strings"
```

---

### Task 10: Implement format string parser — color/style parsing

**Files:**
- Modify: `src/theme/parser.rs`
- Test: inline tests

**Reference:** `~/dev/kokoirc/src/core/theme/parser.ts` lines 257-518

This is the big one — the full `parse_format_string()` function.

**Step 1: Write tests**

```rust
#[test]
fn parse_plain_text() {
    let spans = parse_format_string("hello", &[]);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].text, "hello");
}

#[test]
fn parse_hex_fg_color() {
    let spans = parse_format_string("%Z7aa2f7text%N", &[]);
    assert_eq!(spans[0].fg, Some(Color::Rgb(0x7a, 0xa2, 0xf7)));
    assert_eq!(spans[0].text, "text");
}

#[test]
fn parse_hex_bg_color() {
    let spans = parse_format_string("%z1a1b26text%N", &[]);
    assert_eq!(spans[0].bg, Some(Color::Rgb(0x1a, 0x1b, 0x26)));
}

#[test]
fn parse_irssi_letter_colors() {
    let spans = parse_format_string("%rhello%N", &[]);
    assert_eq!(spans[0].fg, Some(Color::Rgb(0xaa, 0x00, 0x00)));
}

#[test]
fn parse_bold_toggle() {
    let spans = parse_format_string("%_bold%_ normal", &[]);
    assert!(spans[0].bold);
    assert!(!spans[1].bold);
}

#[test]
fn parse_mirc_color_codes() {
    // \x034 = red fg
    let spans = parse_format_string("\x034hello\x03", &[]);
    assert_eq!(spans[0].fg, Some(Color::Rgb(0xff, 0x00, 0x00)));
}

#[test]
fn parse_mirc_hex_color() {
    let spans = parse_format_string("\x04FF9955hello\x04", &[]);
    assert_eq!(spans[0].fg, Some(Color::Rgb(0xFF, 0x99, 0x55)));
}

#[test]
fn parse_reset() {
    let spans = parse_format_string("%Rhello%Nworld", &[]);
    assert!(spans[1].fg.is_none());
}

#[test]
fn full_kokoirc_timestamp_format() {
    // From default.theme: timestamp = "%Z6e738d$*%Z7aa2f7%N"
    let spans = parse_format_string("%Z6e738d12:34:56%Z7aa2f7%N", &[]);
    assert_eq!(spans[0].text, "12:34:56");
    assert_eq!(spans[0].fg, Some(Color::Rgb(0x6e, 0x73, 0x8d)));
}
```

**Step 2: Implement parse_format_string()**

Port the full char-by-char walker from kokoirc's `parseFormatString()`.

Includes:
- COLOR_MAP: irssi letter → Color::Rgb mapping
- MIRC_COLORS: 99-entry palette → Color::Rgb
- All `%X` code handling
- All `\xNN` mIRC control char handling
- Style state tracking (fg, bg, bold, italic, underline, dim)
- Flush mechanism for building StyledSpan vec

**Step 3: Run tests**

Run: `cargo test --lib theme::parser`
Expected: ALL PASS

**Step 4: Commit**

```bash
git add src/theme/parser.rs
git commit -m "feat: implement full format string parser with irssi and mIRC support"
```

---

### Task 11: Integration test — parse kokoirc default theme end-to-end

**Files:**
- Create: `tests/theme_integration.rs`

**Step 1: Write integration test**

Load `themes/default.theme`, resolve a message format with abstractions, parse the result, verify spans have correct colors/styles. Test the full pipeline: load_theme → resolve_abstractions → substitute_vars → parse_format_string.

```rust
#[test]
fn render_own_message_from_default_theme() {
    let theme = load_theme("themes/default.theme").unwrap();
    let format = theme.formats.messages.get("own_msg").unwrap();
    let resolved = resolve_abstractions(format, &theme.abstracts);
    let substituted = substitute_vars(&resolved, &["mynick", "hello world", " "]);
    let spans = parse_format_string(&substituted, &[]);
    // Verify: nick should be green (#9ece6a), text should be light (#c0caf5)
    assert!(spans.iter().any(|s| s.text.contains("mynick")));
    assert!(spans.iter().any(|s| s.text.contains("hello world")));
}
```

**Step 2: Run tests, commit**

Run: `cargo test --test theme_integration`

```bash
git add tests/
git commit -m "test: add theme integration test with default.theme"
```

---

## Phase 4: TUI Shell

### Task 12: Implement terminal setup and teardown

**Files:**
- Implement: `src/ui/mod.rs`
- Modify: `src/main.rs`

**Reference:** ratatui-tui skill at `.agents/skills/ratatui-tui/SKILL.md`

**Step 1: Implement terminal setup**

In `src/ui/mod.rs`:
- `setup_terminal()` → enables raw mode, alternate screen, mouse capture, returns `Terminal<CrosstermBackend>`
- `restore_terminal()` → disables raw mode, alternate screen, mouse capture
- Panic hook that restores terminal before printing panic

In `src/main.rs`:
- Set up tracing subscriber
- Install color-eyre
- Install panic hook
- Setup terminal → run app → restore terminal

**Step 2: Verify it compiles and runs (shows blank alternate screen, exits on Ctrl+C)**

Run: `cargo run`
Expected: Blank terminal, Ctrl+C exits cleanly

**Step 3: Commit**

```bash
git add src/ui/mod.rs src/main.rs
git commit -m "feat: implement terminal setup with panic hook and mouse capture"
```

---

### Task 13: Implement styled_text converter

**Files:**
- Implement: `src/ui/styled_text.rs`

**Step 1: Implement conversion**

```rust
use ratatui::text::{Line, Span};
use ratatui::style::{Style, Modifier, Color};
use crate::theme::StyledSpan;

pub fn styled_spans_to_line(spans: &[StyledSpan]) -> Line<'_> {
    // Convert Vec<StyledSpan> → ratatui Line with proper Style on each Span
    // Handle bold, italic, underline, dim modifiers
    // Handle fg/bg Color mapping
}
```

**Step 2: Write inline test**

```rust
#[test]
fn converts_bold_colored_span() {
    let spans = vec![StyledSpan {
        text: "hello".into(),
        fg: Some(Color::Rgb(0x7a, 0xa2, 0xf7)),
        bg: None,
        bold: true, italic: false, underline: false, dim: false,
    }];
    let line = styled_spans_to_line(&spans);
    // Verify the ratatui Span has correct style
}
```

**Step 3: Run tests, commit**

```bash
git add src/ui/styled_text.rs
git commit -m "feat: implement StyledSpan to ratatui Line converter"
```

---

### Task 14: Implement AppLayout — the main layout skeleton

**Files:**
- Implement: `src/ui/layout.rs`
- Implement: `src/app.rs` (basic App struct with mock state)

**Reference:** `~/dev/kokoirc/src/ui/layout/AppLayout.tsx`

**Step 1: Implement App struct with mock data**

In `src/app.rs`: Create `App` struct that holds `AppState` populated with mock data:
- 1 mock connection ("IRCnet", connected)
- 3 mock buffers: Status, #channel, query
- Each with a few mock messages
- A few mock nicks in the channel buffer
- Load real theme from `themes/default.theme`

**Step 2: Implement draw() in layout.rs**

```rust
pub fn draw(frame: &mut Frame, app: &App) {
    let colors = &app.state.theme.colors;

    let [topic_area, main_area, bottom_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(3),
    ]).areas(frame.area());

    // Main: sidebar | chat | nicklist
    // Bottom: Block with top border containing statusline + input
    // Each component renders into its area
}
```

Wire `draw()` into the main loop: terminal.draw(|f| layout::draw(f, &app)).

**Step 3: Verify it renders the skeleton layout (empty panels with borders)**

Run: `cargo run`
Expected: Layout visible with borders, topic area, sidebars, bottom area

**Step 4: Commit**

```bash
git add src/ui/layout.rs src/app.rs src/main.rs
git commit -m "feat: implement main layout skeleton with mock data"
```

---

## Phase 5: UI Components

### Task 15: Implement TopicBar

**Files:**
- Implement: `src/ui/topic_bar.rs`

**Reference:** `~/dev/kokoirc/src/ui/layout/TopicBar.tsx`

Renders: `channelname — topic text` with parsed IRC formatting in topic. Background = `bg_alt`. Channel name in `accent` color.

**Step 1: Implement render function**

```rust
pub fn render_topic_bar(frame: &mut Frame, area: Rect, app: &App) { ... }
```

**Step 2: Wire into layout.rs, verify visually**

Run: `cargo run`
Expected: Topic bar shows mock channel name and topic

**Step 3: Commit**

```bash
git add src/ui/topic_bar.rs src/ui/layout.rs
git commit -m "feat: implement TopicBar component"
```

---

### Task 16: Implement BufferList (left sidebar)

**Files:**
- Implement: `src/ui/buffer_list.rs`

**Reference:** `~/dev/kokoirc/src/ui/sidebar/BufferList.tsx`

Renders: connection headers + numbered buffer items, themed per activity level using format strings from `theme.formats.sidepanel`.

**Step 1: Implement render function**

```rust
pub fn render_buffer_list(frame: &mut Frame, area: Rect, app: &App) { ... }
```

- Group by connection (render header when connectionId changes)
- Sequential refNum starting at 1
- Format key: `item_selected` for active, `item_activity_N` for others
- Truncate names to fit panel width
- Use ListState for scrolling

**Step 2: Wire into layout.rs, verify visually**

**Step 3: Commit**

```bash
git add src/ui/buffer_list.rs src/ui/layout.rs
git commit -m "feat: implement BufferList sidebar with themed activity levels"
```

---

### Task 17: Implement NickList (right sidebar)

**Files:**
- Implement: `src/ui/nick_list.rs`

**Reference:** `~/dev/kokoirc/src/ui/sidebar/NickList.tsx`

Renders: user count header + sorted nicks with prefix symbols, themed per mode using `theme.formats.nicklist`.

**Step 1: Implement render function**

```rust
pub fn render_nick_list(frame: &mut Frame, area: Rect, app: &App) { ... }
```

- Header: `N users` in fg_muted
- Format keys: owner/admin/op/halfop/voice/normal
- Only show for Channel buffers
- Use ListState for scrolling

**Step 2: Wire into layout.rs, verify visually**

**Step 3: Commit**

```bash
git add src/ui/nick_list.rs src/ui/layout.rs
git commit -m "feat: implement NickList sidebar with themed prefix modes"
```

---

### Task 18: Implement MessageLine rendering

**Files:**
- Implement: `src/ui/message_line.rs`

**Reference:** `~/dev/kokoirc/src/ui/chat/MessageLine.tsx`

The message rendering pipeline:
1. Format timestamp with `abstracts.timestamp`
2. Determine format key (own_msg/pubmsg/pubmsg_mention/action/event)
3. Compute nick alignment (right-pad/left-pad to nick_column_width)
4. Resolve abstractions, substitute params, parse format string
5. Output: `Vec<StyledSpan>` for the full line

**Step 1: Implement**

```rust
pub fn render_message(
    msg: &Message,
    is_own: bool,
    theme: &ThemeFile,
    config: &AppConfig,
) -> Vec<StyledSpan> { ... }
```

**Step 2: Write tests for own message, public message, event, action**

**Step 3: Run tests, commit**

```bash
git add src/ui/message_line.rs
git commit -m "feat: implement MessageLine rendering with nick alignment"
```

---

### Task 19: Implement ChatView (scrollable message area)

**Files:**
- Implement: `src/ui/chat_view.rs`

**Reference:** `~/dev/kokoirc/src/ui/chat/ChatView.tsx`

**Step 1: Implement ScrollState and render function**

```rust
pub struct ScrollState {
    pub offset: usize,
    pub sticky: bool,
}

pub fn render_chat_view(frame: &mut Frame, area: Rect, app: &App) { ... }
```

- Renders messages from bottom up based on scroll offset
- Uses message_line::render_message for each line
- Shows "No active buffer" when no buffer selected
- Handles word wrapping using unicode-width

**Step 2: Wire into layout.rs, verify visually with mock messages**

**Step 3: Commit**

```bash
git add src/ui/chat_view.rs src/ui/layout.rs
git commit -m "feat: implement ChatView with scroll state and sticky scroll"
```

---

### Task 20: Implement StatusLine

**Files:**
- Implement: `src/ui/status_line.rs`

**Reference:** `~/dev/kokoirc/src/ui/statusbar/StatusLine.tsx`

**Step 1: Implement render function**

```rust
pub fn render_status_line(frame: &mut Frame, area: Rect, app: &App) { ... }
```

Items: time, nick_info, channel_info, lag, active_windows. Separator from config. Wrapped in `[` `]`.

Activity colors:
- Events: `#9ece6a` green
- Highlight: `#f7768e` red
- Activity: `#e0af68` yellow
- Mention: `#bb9af7` purple

Lag colors:
- \> 5s: red, > 2s: yellow, else: green

**Step 2: Wire into layout.rs, verify visually**

**Step 3: Commit**

```bash
git add src/ui/status_line.rs src/ui/layout.rs
git commit -m "feat: implement StatusLine with configurable items"
```

---

### Task 21: Implement CommandInput

**Files:**
- Implement: `src/ui/input.rs`

**Reference:** `~/dev/kokoirc/src/ui/input/CommandInput.tsx`

**Step 1: Implement InputState and render function**

```rust
pub struct InputState {
    pub value: String,
    pub cursor_pos: usize,
    pub history: Vec<String>,
    pub history_index: Option<usize>,
    pub tab_state: Option<TabCompletionState>,
}

pub fn render_input(frame: &mut Frame, area: Rect, app: &App) { ... }
```

- Prompt from config template with `$server`, `$channel`, `$nick`, `$buffer` substitution
- Cursor rendering (block cursor at position)
- Text editing: insert char, backspace, delete, home, end, left, right

**Step 2: Wire into layout.rs, verify visually (cursor visible, prompt shows)**

**Step 3: Commit**

```bash
git add src/ui/input.rs src/ui/layout.rs
git commit -m "feat: implement CommandInput with prompt and cursor"
```

---

## Phase 6: Event Handling

### Task 22: Implement async event loop

**Files:**
- Modify: `src/app.rs`
- Modify: `src/main.rs`

**Step 1: Implement the main event loop**

```rust
pub async fn run(app: &mut App) -> Result<()> {
    let mut terminal = ui::setup_terminal()?;
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));

    loop {
        terminal.draw(|f| ui::layout::draw(f, app))?;

        tokio::select! {
            Some(Ok(event)) = events.next() => {
                app.handle_terminal_event(event);
            }
            _ = tick.tick() => {
                app.handle_tick();
            }
        }

        if app.should_quit { break; }
    }

    ui::restore_terminal(&mut terminal)?;
    Ok(())
}
```

**Step 2: Implement handle_terminal_event()**

Route `Event::Key`, `Event::Mouse`, `Event::Resize` to appropriate handlers.

**Step 3: Verify — app runs, renders, Ctrl+Q quits cleanly**

Run: `cargo run`

**Step 4: Commit**

```bash
git add src/app.rs src/main.rs
git commit -m "feat: implement async event loop with terminal events and tick"
```

---

### Task 23: Implement keyboard handling

**Files:**
- Modify: `src/app.rs`

**Step 1: Implement all keybindings**

- Ctrl+Q → quit
- Esc+1..9, Esc+0 → switch buffer (with 500ms Esc window)
- Alt+1..9 → switch buffer
- Esc+Left/Right → prev/next buffer
- Tab → tab completion
- Up/Down → history
- PageUp/PageDown → scroll
- End → snap to bottom
- Enter → submit
- Printable chars → insert into input
- Backspace/Delete → edit input
- Home/End in input → move cursor

**Step 2: Verify each keybinding works**

Run: `cargo run` and test interactively

**Step 3: Commit**

```bash
git add src/app.rs
git commit -m "feat: implement all keyboard shortcuts"
```

---

### Task 24: Implement mouse handling

**Files:**
- Modify: `src/app.rs`
- Modify: `src/ui/layout.rs` (store component Rects for hit-testing)

**Step 1: Store rendered Rects**

Add a `UiRegions` struct that stores the `Rect` of each component from the last render. Updated each frame.

**Step 2: Implement mouse event handling**

- Click in buffer list area → determine which item, switch buffer
- Click in nick list area → determine which nick, open query
- Scroll wheel in chat area → adjust scroll offset
- Scroll wheel in buffer/nick list → scroll list
- Drag on border → resize sidepanel, persist to config

**Step 3: Verify mouse interactions**

Run: `cargo run` and test with mouse

**Step 4: Commit**

```bash
git add src/app.rs src/ui/layout.rs
git commit -m "feat: implement full mouse support (click, scroll, drag resize)"
```

---

### Task 25: Implement tab completion

**Files:**
- Modify: `src/ui/input.rs`

**Reference:** `~/dev/kokoirc/src/ui/input/CommandInput.tsx` lines 145-256

**Step 1: Implement TabCompletionState and cycling**

Three modes:
1. Nick completion: partial → matches from buffer users, `: ` suffix at start of line
2. Command completion: `/par` → `/part `
3. Subcommand completion: `/server li` → `/server list `

Tab cycles through matches. Any non-Tab key resets.

**Step 2: Write tests for each completion mode**

**Step 3: Run tests, commit**

```bash
git add src/ui/input.rs
git commit -m "feat: implement tab completion for nicks, commands, and subcommands"
```

---

### Task 26: Implement command input history

**Files:**
- Modify: `src/ui/input.rs`

**Step 1: Implement history navigation**

- On submit: push to history (max 100 entries)
- Up: move to older entry
- Down: move to newer entry, or clear if at start

**Step 2: Verify interactively**

**Step 3: Commit**

```bash
git add src/ui/input.rs
git commit -m "feat: implement command input history navigation"
```

---

## Phase 7: Command System (basic)

### Task 27: Implement command parser and basic commands

**Files:**
- Implement: `src/commands/parser.rs`
- Implement: `src/commands/mod.rs`
- Implement: `src/commands/registry.rs`
- Implement: `src/commands/helpers.rs`

**Reference:** `~/dev/kokoirc/src/core/commands/parser.ts`, `registry.ts`

**Step 1: Implement command parser**

- `/command args` parsing
- GREEDY_COMMANDS set (msg, notice, me, quit, topic, kick, kb, close, disconnect, set, alias)
- Return `ParsedCommand { name, args }`

**Step 2: Implement initial commands (UI-only, no IRC needed)**

Start with commands that work without IRC:
- `/help` — show command list
- `/quit` — set should_quit
- `/clear` — clear active buffer messages
- `/close` — close active buffer
- `/set` — view/modify config values
- `/reload` — reload config + theme
- `/alias`, `/unalias` — manage aliases

**Step 3: Write tests for parser**

**Step 4: Commit**

```bash
git add src/commands/
git commit -m "feat: implement command parser and basic UI commands"
```

---

## Phase 8: Visual Polish

### Task 28: Copy default theme and verify full rendering

**Files:**
- Create: `themes/default.theme` (copy from kokoirc)

**Step 1: Ensure themes/default.theme matches kokoirc's theme exactly**

**Step 2: Run the app with mock data and visually verify:**

- Topic bar renders with correct colors
- Buffer list shows connection headers and numbered items with activity colors
- Chat view shows messages with proper nick alignment, timestamps, colors
- Nick list shows sorted users with mode prefixes
- Status line shows time, nick, channel info
- Input prompt renders with correct colors and working cursor
- All borders visible with correct border color
- Background colors correct throughout

**Step 3: Fix any visual discrepancies**

**Step 4: Commit**

```bash
git add themes/ src/
git commit -m "feat: visual polish — verify full themed rendering matches kokoirc"
```

---

## Phase 9: IRC Connection Layer

### Task 29: Implement IRC connection manager

**Files:**
- Implement: `src/irc/mod.rs`

**Reference:** `~/dev/kokoirc/src/core/irc/client.ts`

**Step 1: Implement connection manager**

```rust
pub struct IrcHandle {
    pub conn_id: String,
    pub client: irc::client::Client,
    pub tx: mpsc::UnboundedSender<IrcEvent>,
}

pub async fn connect_server(config: &ServerConfig, env: &HashMap<String, String>)
    -> Result<(IrcHandle, mpsc::UnboundedReceiver<IrcEvent>)>
```

Spawns a tokio task that reads from the IRC stream and forwards via mpsc.

**Step 2: Add IRC message channel to the main event loop's select!**

**Step 3: Commit**

```bash
git add src/irc/mod.rs src/app.rs
git commit -m "feat: implement IRC connection manager with tokio tasks"
```

---

### Task 30: Implement IRC event handlers

**Files:**
- Implement: `src/irc/events.rs`

**Reference:** `~/dev/kokoirc/src/core/irc/events.ts`

**Step 1: Implement event routing**

Map IRC messages to state mutations:
- `JOIN` → add buffer, add nick, add event message
- `PART` → remove nick (or remove buffer if self), add event message
- `QUIT` → remove nick from all buffers, add event message
- `PRIVMSG` → add message, detect highlights, update activity
- `NOTICE` → route to channel/query/status
- `NICK` → update nick in all buffers
- `KICK` → remove nick (or remove buffer if self)
- `TOPIC` → update buffer topic
- `MODE` → update buffer/user modes
- `353` (NAMES) → populate nick list
- `PING/PONG` → lag measurement
- Connection events → update connection status

**Step 2: Commit**

```bash
git add src/irc/events.rs
git commit -m "feat: implement IRC event handlers for all major events"
```

---

### Task 31: Implement IRC-dependent commands

**Files:**
- Modify: `src/commands/registry.rs`

**Step 1: Add all IRC commands**

- `/join`, `/part`, `/msg`, `/me`, `/nick`, `/quit`, `/topic`, `/names`
- `/invite`, `/notice`, `/mode`, `/kick`, `/ban`, `/unban`, `/kb`
- `/op`, `/deop`, `/voice`, `/devoice`
- `/whois`, `/wii`, `/connect`, `/disconnect`, `/server`
- `/quote`, `/oper`, `/kill`, `/wallops`
- `/ignore`, `/unignore`

**Step 2: Commit**

```bash
git add src/commands/registry.rs
git commit -m "feat: implement all IRC slash commands"
```

---

### Task 32: Implement IRC formatting helpers

**Files:**
- Implement: `src/irc/formatting.rs`

**Reference:** `~/dev/kokoirc/src/core/irc/formatting.ts`

- `strip_irc_formatting(text) → String`
- `format_timestamp(dt, format) → String`
- `build_prefix_map(isupport_prefix) → HashMap`
- `get_highest_prefix(modes, prefix_order) → String`
- `build_mode_string(modes) → String`

**Step 1: Implement with tests**

**Step 2: Commit**

```bash
git add src/irc/formatting.rs
git commit -m "feat: implement IRC formatting helpers"
```

---

## Phase 10: Protection Systems

### Task 33: Implement antiflood detection

**Files:**
- Implement: `src/irc/flood.rs`

**Reference:** `~/dev/kokoirc/src/core/irc/antiflood.ts`

CTCP flood (5/5s → 60s block), ident flood, duplicate text flood, nick change flood.

**Step 1: Implement with tests**

**Step 2: Commit**

```bash
git add src/irc/flood.rs
git commit -m "feat: implement antiflood detection"
```

---

### Task 34: Implement netsplit detection

**Files:**
- Implement: `src/irc/netsplit.rs`

**Reference:** `~/dev/kokoirc/src/core/irc/netsplit.ts`

Detect QUIT messages matching `"host1.tld host2.tld"`, batch and display after 5s.

**Step 1: Implement with tests**

**Step 2: Commit**

```bash
git add src/irc/netsplit.rs
git commit -m "feat: implement netsplit detection and batching"
```

---

### Task 35: Implement ignore system

**Files:**
- Implement: `src/irc/ignore.rs`

**Reference:** `~/dev/kokoirc/src/core/irc/ignore.ts`

Nick/host mask matching with wildcard support, levels (MSGS, PUBLIC, etc.), optional channel restriction.

**Step 1: Implement with tests**

**Step 2: Commit**

```bash
git add src/irc/ignore.rs
git commit -m "feat: implement ignore system with wildcard mask matching"
```

---

## Phase 11: Scripting & Event Bus Stubs

### Task 36: Implement EventBus skeleton

**Files:**
- Implement: `src/scripting/event_bus.rs`
- Implement: `src/scripting/api.rs`
- Implement: `src/scripting/mod.rs`

**Step 1: Define traits and basic implementation**

```rust
pub trait EventHandler: Send + Sync {
    fn handle(&self, event: &Event) -> EventResult;
}

pub struct EventBus {
    handlers: HashMap<String, Vec<Box<dyn EventHandler>>>,
}

impl EventBus {
    pub fn emit(&self, event_name: &str, event: &Event) -> bool { ... }
    pub fn on(&mut self, event_name: &str, handler: Box<dyn EventHandler>) { ... }
}
```

**Step 2: Wire EventBus into App so IRC events and commands emit through it**

**Step 3: Commit**

```bash
git add src/scripting/
git commit -m "feat: implement EventBus skeleton for future scripting API"
```

---

### Task 37: Stub image_preview module

**Files:**
- Implement: `src/image_preview/mod.rs`

Just the module with placeholder types and a `// TODO: port from kokoirc` comment.

**Step 1: Commit**

```bash
git add src/image_preview/
git commit -m "feat: stub image_preview module for future implementation"
```

---

## Phase 12: Integration & End-to-End

### Task 38: End-to-end test — connect to IRC and chat

**Step 1: Update config.toml with a test server**

**Step 2: Run the app, verify:**

- Connects to IRC server
- Status buffer shows connection messages (MOTD, etc.)
- `/join #test` creates channel buffer, populates nick list
- Typing text sends messages, own messages appear
- Other users' messages appear with correct formatting
- `/part` closes buffer
- `/quit` disconnects cleanly
- Activity levels update in buffer list
- Status bar shows lag, channel info, time
- Mouse clicks work (switch buffer, nick query)
- Scrolling works (PageUp/Down, mouse wheel)
- Tab completion works for nicks and commands

**Step 3: Fix any issues found**

**Step 4: Final commit**

```bash
git add -A
git commit -m "feat: complete repartee v0.1.0 — full IRC client with themed TUI"
```

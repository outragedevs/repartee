# Shell Buffer Implementation Plan

**Branch**: `feat/shell-buffer`
**Date**: 2026-03-16

## Overview

Embed a full PTY-backed shell inside Repartee as a new buffer type. Users get a real terminal experience (zsh, bash, vim, htop) without detaching from the IRC client.

## Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `portable-pty` | 0.9.0 | PTY allocation + command spawning (from WezTerm) |
| `vt100` | 0.16.2 | Terminal emulation — parses ANSI/VT100 into screen buffer |

Both are mature, well-maintained crates. No new transitive C dependencies.

## Architecture

```
App
 ├── state.buffers["shell/0"] = Buffer { buffer_type: Shell, ... }
 ├── shell_mgr: ShellManager
 │    └── shells: HashMap<String, ShellSession>
 │         ├── parser: vt100::Parser        (terminal emulator)
 │         ├── writer: Box<dyn Write+Send>  (PTY master writer)
 │         ├── child: Box<dyn Child+Send>   (process handle)
 │         └── scrollback_offset: usize
 └── shell_rx: mpsc::UnboundedReceiver<ShellEvent>
      └── ShellEvent::Output { id, bytes }
      └── ShellEvent::Exited { id, status }
```

### Data Flow

```
Shell process stdout → async reader task → ShellEvent::Output → shell_rx
  → ShellManager::process_output(id, bytes) → vt100::Parser::process()
  → next frame: render shell buffer from parser.screen()

Keyboard input → handle_key() → detect active shell buffer
  → ShellManager::write(id, bytes) → PTY master writer
```

## Implementation Tasks

### Task 1: Add dependencies + module skeleton

**Files**: `Cargo.toml`, `src/shell/mod.rs`, `src/shell/types.rs`, `src/main.rs`

Add to `Cargo.toml`:
```toml
portable-pty = "0.9.0"
vt100 = "0.16.2"
```

Create `src/shell/mod.rs` with:
- `pub mod types;`
- `ShellManager` struct (empty impl)

Create `src/shell/types.rs` with:
- `ShellEvent` enum: `Output { id: String, bytes: Vec<u8> }`, `Exited { id: String, status: Option<u32> }`

Wire `mod shell;` in `main.rs`.

### Task 2: BufferType::Shell + sort order

**Files**: `src/state/buffer.rs`

Add `Shell` variant to `BufferType` enum. Sort group 6 (after Special=5) — shell buffers appear at the bottom of the sidebar.

Update the `sort_group` test to include the new variant.

### Task 3: ShellSession + ShellManager core

**Files**: `src/shell/mod.rs`, `src/shell/types.rs`

`ShellSession` struct:
```rust
pub struct ShellSession {
    parser: vt100::Parser,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send>,
    /// Buffer ID this session is attached to.
    buffer_id: String,
    /// Counter for unique shell IDs.
    id: String,
}
```

`ShellManager` struct:
```rust
pub struct ShellManager {
    sessions: HashMap<String, ShellSession>,
    event_tx: mpsc::UnboundedSender<ShellEvent>,
    next_id: u32,
}
```

Methods:
- `new(event_tx) -> Self`
- `open(cols, rows, command: Option<&str>) -> Result<String, String>` — allocates PTY via `portable_pty::native_pty_system()`, spawns `$SHELL` (or specified command), creates `vt100::Parser::new(rows, cols, 1000)` for scrollback, spawns async reader task, returns shell ID
- `close(id: &str)` — kills child process, drops session
- `write(id: &str, data: &[u8])` — writes to PTY master
- `resize(id: &str, cols: u16, rows: u16)` — resizes PTY + vt100 parser
- `process_output(id: &str, bytes: &[u8])` — feeds bytes to `vt100::Parser::process()`
- `screen(id: &str) -> Option<&vt100::Screen>` — returns screen for rendering
- `is_finished(id: &str) -> bool` — checks `child.try_wait()`

### Task 4: Async reader task

**Files**: `src/shell/mod.rs`

When a shell is opened, spawn a blocking reader task:

```rust
let reader = pair.master.try_clone_reader()?;
let tx = self.event_tx.clone();
let id = id.clone();
std::thread::spawn(move || {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => {
                let _ = tx.send(ShellEvent::Exited { id, status: None });
                break;
            }
            Ok(n) => {
                let _ = tx.send(ShellEvent::Output {
                    id: id.clone(),
                    bytes: buf[..n].to_vec(),
                });
            }
        }
    }
});
```

Use `std::thread::spawn` (not `tokio::spawn_blocking`) because `portable-pty` reader is a blocking `Read` that may not be `Send` across all platforms. The std thread sends events via the mpsc channel — same pattern as our terminal reader.

### Task 5: Wire into App — channels + event loop

**Files**: `src/app.rs`

Add fields to `App`:
```rust
pub shell_mgr: shell::ShellManager,
shell_rx: mpsc::UnboundedReceiver<shell::ShellEvent>,
```

Initialize in `App::new()`.

Add `tokio::select!` arm in the main event loop:
```rust
shell_ev = self.shell_rx.recv() => {
    if let Some(ev) = shell_ev {
        self.handle_shell_event(ev);
    }
},
```

`handle_shell_event`:
- `Output { id, bytes }` → `shell_mgr.process_output(&id, &bytes)` — ratatui redraws on next frame
- `Exited { id, status }` → add exit message to buffer, mark session as finished, optionally auto-close

### Task 6: /shell command

**Files**: `src/commands/handlers_ui.rs`, `src/commands/registry.rs`, `src/commands/parser.rs`, `docs/commands/shell.md`

Register command:
```rust
("shell", CommandDef {
    handler: cmd_shell,
    description: "Open a shell terminal",
    aliases: &["sh"],
    category: CommandCategory::UI,
})
```

Add `"shell"` to `GREEDY_COMMANDS` in `parser.rs` (so arguments aren't split).

Subcommands:
- `/shell` or `/shell open` — open new shell with `$SHELL`
- `/shell cmd <command>` — open shell running specific command (e.g. `/shell cmd htop`)
- `/shell close` — close current shell buffer (kill process)
- `/shell list` — list open shells

Implementation:
```rust
fn cmd_shell(app: &mut App, args: &[String]) {
    let sub = args.first().map(String::as_str).unwrap_or("open");
    match sub {
        "open" | "" => { /* open $SHELL */ }
        "cmd" => { /* open with specified command */ }
        "close" => { /* close active shell */ }
        "list" => { /* list shells */ }
        _ => { /* treat as "cmd" arg */ }
    }
}
```

On open: create buffer with `BufferType::Shell`, set as active, call `shell_mgr.open()`.

### Task 7: Shell buffer rendering (ratatui widget)

**Files**: `src/ui/shell_view.rs`, `src/ui/chat_view.rs`

New `src/ui/shell_view.rs`:

```rust
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let buf = app.state.active_buffer()?;
    let screen = app.shell_mgr.screen(&buf.id)?;

    // Map vt100 screen cells → ratatui buffer cells directly
    for row in 0..area.height.min(screen.size().0 as u16) {
        for col in 0..area.width.min(screen.size().1 as u16) {
            let cell = screen.cell(row as u16, col as u16)?;
            let ratatui_cell = frame.buffer_mut().cell_mut(Position::new(
                area.x + col, area.y + row
            ))?;
            ratatui_cell.set_char(cell.contents().chars().next().unwrap_or(' '));
            ratatui_cell.set_fg(vt100_color_to_ratatui(cell.fgcolor()));
            ratatui_cell.set_bg(vt100_color_to_ratatui(cell.bgcolor()));
            // Apply attributes: bold, italic, underline, inverse
            apply_attrs(ratatui_cell, &cell);
        }
    }

    // Render cursor
    if screen.hide_cursor() {
        // hidden
    } else {
        let (crow, ccol) = screen.cursor_position();
        frame.set_cursor_position(Position::new(
            area.x + ccol as u16,
            area.y + crow as u16,
        ));
    }
}
```

Color mapping function `vt100_color_to_ratatui`:
- `vt100::Color::Default` → `Color::Reset`
- `vt100::Color::Idx(n)` → `Color::Indexed(n)`
- `vt100::Color::Rgb(r, g, b)` → `Color::Rgb(r, g, b)`

In `chat_view.rs` render dispatch: if `buf.buffer_type == BufferType::Shell`, delegate to `shell_view::render()` instead.

### Task 8: Input routing — shell mode

**Files**: `src/app.rs`

In `handle_key()`, before the normal key dispatch, check if the active buffer is a shell:

```rust
if self.is_active_shell_buffer() {
    // Global escape: Ctrl+] exits shell input mode (configurable prefix)
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char(']') {
        // Toggle: focus moves to IRC command input
        self.shell_input_active = false;
        return;
    }
    // Alt+N buffer switching still works in shell mode
    if key.modifiers.contains(KeyModifiers::ALT) && matches!(key.code, KeyCode::Char(c) if c.is_ascii_digit()) {
        // fall through to normal handler
    } else {
        // Forward everything else to shell PTY
        self.forward_key_to_shell(key);
        return;
    }
}
```

`forward_key_to_shell(key)` serializes the `KeyEvent` to terminal bytes:
- `KeyCode::Char(c)` with no modifiers → UTF-8 bytes of `c`
- `KeyCode::Char(c)` with Ctrl → control character (`c as u8 - b'a' + 1`)
- `KeyCode::Enter` → `\r`
- `KeyCode::Backspace` → `\x7f`
- `KeyCode::Tab` → `\t`
- `KeyCode::Esc` → `\x1b`
- Arrow keys → `\x1b[A`, `\x1b[B`, `\x1b[C`, `\x1b[D`
- Home/End/PgUp/PgDn → standard xterm sequences
- Function keys → standard xterm sequences

Add `shell_input_active: bool` field on `App`. Set to `true` when switching to a shell buffer, `false` on Ctrl+] or switching to a non-shell buffer.

### Task 9: Resize handling

**Files**: `src/app.rs`, `src/shell/mod.rs`

When the terminal resizes or sidepanel visibility changes, recalculate the chat area dimensions and call `shell_mgr.resize(id, cols, rows)` for all active shells.

`ShellManager::resize` calls both:
1. `pair.master.resize(PtySize { rows, cols, .. })` — tells the OS
2. `parser.set_size(rows, cols)` — tells vt100

Hook into the existing resize handling path (search for `terminal.resize` or `cached_term_cols`).

### Task 10: Shell buffer lifecycle

**Files**: `src/app.rs`, `src/commands/handlers_ui.rs`

- `/close` on a shell buffer: call `shell_mgr.close(id)`, then `state.remove_buffer(id)`
- Shell process exits naturally: `ShellEvent::Exited` → add "[Process exited with status N]" message to the buffer, leave buffer open for the user to read output and close manually
- On app quit (`/quit`): iterate all shells, kill child processes gracefully (SIGHUP then SIGKILL after timeout)

### Task 11: Status line integration

**Files**: `src/ui/status_line.rs`

When a shell buffer is active, show `[shell: zsh]` or `[shell: htop]` in the status line where channel name normally appears. Use the shell command name from the session.

### Task 12: Documentation

**Files**: `docs/commands/shell.md`, `docs/src/content/configuration.md`, `docs/src/content/faq.md`

Command doc `docs/commands/shell.md`:
```markdown
---
category: UI
description: Open an embedded terminal
---
# /shell
## Syntax
    /shell [open|cmd|close|list] [command]
## Aliases
    /sh
## Description
Open an embedded shell terminal inside Repartee. [...]
```

Add to FAQ: "Can I run shell commands without detaching?"

Rebuild docs with `bun run docs/build.ts`.

### Task 13: Tests

**Files**: `src/shell/mod.rs`

Unit tests:
- `shell_manager_open_creates_session` — open a shell, verify session exists
- `shell_manager_close_removes_session` — close, verify removed
- `shell_manager_process_output_updates_screen` — feed known bytes, verify screen content
- `vt100_color_mapping` — test all color variants map correctly
- `key_to_bytes_basic` — verify char/enter/backspace/arrow serialization
- `key_to_bytes_ctrl` — verify Ctrl+C → \x03, Ctrl+D → \x04, etc.
- `resize_updates_parser` — verify parser dimensions after resize

Integration test (if feasible without a real TTY):
- Spawn `/bin/echo hello`, read output, verify "hello" appears on vt100 screen

## Task Order

```
Task 1  (deps + skeleton)
  ↓
Task 2  (BufferType::Shell)
  ↓
Task 3  (ShellSession + ShellManager)
  ↓
Task 4  (async reader)
  ↓
Task 5  (wire into App event loop)
  ↓
Task 6  (/shell command)
  ↓
Task 7  (shell_view renderer)     ← can demo basic output here
  ↓
Task 8  (input routing)           ← interactive shell works here
  ↓
Task 9  (resize)
  ↓
Task 10 (lifecycle / cleanup)
  ↓
Task 11 (status line)
  ↓
Task 12 (docs)
  ↓
Task 13 (tests)
```

## Design Decisions

### Why `std::thread::spawn` for PTY reader (not tokio)?
`portable-pty`'s `Read` impl is blocking and may hold OS file descriptors that aren't safe to use with `tokio::spawn_blocking`'s thread pool. A dedicated std thread per shell (same pattern as our terminal reader) is simpler and guaranteed safe.

### Why Ctrl+] as escape prefix?
Matches `telnet` convention. Ctrl+] is rarely used in shells, unlike Ctrl+A (screen), Ctrl+B (tmux), or Ctrl+\ (SIGQUIT). Configurable later via `/set shell.escape_key`.

### Why vt100 crate over alacritty_terminal?
- `vt100` is ~5x smaller, designed for embedding (not running a full terminal app)
- Clean `Screen::cell(row, col)` API maps perfectly to ratatui's buffer
- No GPU/rendering dependencies
- `alacritty_terminal` pulls in too much for our use case

### Why sort group 6 (after Special)?
Shell buffers are auxiliary — not IRC channels, not queries. Placing them at the bottom keeps the IRC-focused sidebar clean. Users can still Alt+N to reach them quickly.

### Scrollback
`vt100::Parser::new(rows, cols, scrollback_lines)` — set scrollback to 1000 lines. When shell buffer is scrolled up, render from the parser's scrollback buffer. Shift+PgUp/PgDn for scroll (same UX as chat buffers).

### Memory
Each `vt100::Parser` holds a screen buffer (~rows × cols × ~8 bytes per cell) plus scrollback. For 80×24 + 1000 scrollback lines: ~80KB per shell. Negligible.

## Risks

| Risk | Mitigation |
|------|------------|
| `portable-pty` macOS sandbox issues | Crate is battle-tested (WezTerm uses it). If issues arise, fall back to raw `openpty` + `forkpty` |
| Shell output flood (e.g. `yes`) | vt100 parser handles this — it only keeps screen + scrollback. No unbounded growth |
| Key encoding edge cases | Start with basic set (chars, arrows, ctrl), extend as needed. Use crossterm's existing key mapping as reference |
| Full-screen apps (vim) in shell | vt100 handles alternate screen buffer natively. No special code needed |

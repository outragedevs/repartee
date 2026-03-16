# Web Shell Rendering with beamterm — Implementation Prompt

**Branch**: `feat/web-shell-beamterm`
**Date**: 2026-03-16
**Feed this file as context to start the implementation session.**

## What

Replace the current HTML `<span>` grid shell renderer in the Repartee web frontend with **beamterm-renderer** (WebGL2 GPU-accelerated terminal renderer). The backend already has full PTY + vt100 terminal emulation working. The web frontend needs a proper terminal renderer that can display full-screen TUI programs (btop, vim, weechat) without artifacts.

## Why

The current HTML approach (Option A) has fundamental problems:
- **Screen clearing artifacts** — DOM diffing leaves stale `<span>` elements when the shell clears the screen
- **Font rendering** — CSS font-family fallback chain can't reliably render box-drawing characters (`│`, `─`, `┌`) and NerdFont icons
- **Performance** — creating/destroying thousands of DOM nodes at 10fps causes jank
- **No atomic screen updates** — the browser renders intermediate DOM states, causing flicker

beamterm solves all of these:
- GPU-accelerated WebGL2 rendering in a single instanced draw call
- Dynamic font atlas with correct Unicode box-drawing and emoji support
- Sub-millisecond render times for 17K+ cells
- Atomic frame updates — the canvas shows nothing until the full frame is rendered
- Pure Rust WASM — integrates directly with our Leptos frontend, no JS interop

## Current State

### Backend (DONE — no changes needed)
- `src/shell/mod.rs`: `ShellManager` with portable-pty + vt100, PTY reader thread, HVP→CUP rewrite
- `src/shell/mod.rs`: `screen_to_web()` serializes vt100 screen as `ShellScreenRow` with RLE-compressed `ShellSpan` structs
- `src/web/protocol.rs`: `WebEvent::ShellScreen { buffer_id, rows, cursor_row, cursor_col, cursor_visible }` and `WebCommand::ShellInput { buffer_id, data (base64) }`
- `src/app.rs`: `maybe_broadcast_shell_screen()` throttled to 100ms (10fps), `force_broadcast_shell_screen()` on buffer switch, `ShellInput` handler decodes base64 and writes to PTY
- TUI shell works perfectly — btop, vim, irssi, weechat all render correctly

### Frontend (REPLACE)
- `web-ui/src/components/shell_view.rs`: current HTML span-based renderer — **replace with beamterm canvas**
- `web-ui/src/protocol.rs`: `ShellScreenRow`, `ShellSpan`, `ShellScreenData` types — **keep, used for data transport**
- `web-ui/src/state.rs`: `shell_screen: RwSignal<Option<ShellScreenData>>` — **keep, feeds beamterm**
- `web-ui/styles/base.css`: `.shell-terminal` CSS — **update for canvas**

### What stays unchanged
- Backend protocol (ShellScreen events, ShellInput commands)
- Backend serialization (screen_to_web with styled spans)
- State management (shell_screen signal in AppState)
- Keyboard input capture and base64 encoding
- Buffer type detection and routing

## Architecture

```
Backend (unchanged):
  PTY → vt100 parser → screen_to_web() → ShellScreenRow[] → WebSocket JSON

Frontend (new):
  WebSocket → handle_event(ShellScreen) → shell_screen signal updated
       ↓
  ShellView component detects signal change
       ↓
  Convert ShellSpan[] → beamterm CellData[]
       ↓
  terminal.update_cells(cells) → terminal.render_frame()
       ↓
  WebGL2 canvas renders terminal grid (GPU, <1ms)

Input (unchanged):
  Browser keydown → key_event_to_bytes() → base64 → ShellInput → WebSocket → PTY
```

## Implementation Tasks

### Task 1: Add beamterm-renderer dependency
- Add `beamterm-renderer` (latest version from crates.io) to `web-ui/Cargo.toml`
- Use stock upstream from junkdog — NOT the @kofany/beamterm-terx npm fork
- Verify it compiles to WASM with `trunk build`

### Task 2: Replace ShellView component
- Remove the HTML `<span>` grid renderer from `shell_view.rs`
- Create a `<canvas>` element for beamterm
- Initialize `Terminal::builder("#shell-canvas").build()` on component mount
- On `shell_screen` signal change: convert `ShellSpan[]` → `CellData[]`, call `update_cells()` + `render_frame()`
- Handle component disposal cleanly (drop the Terminal)

### Task 3: Color conversion
- Convert CSS hex colors from `ShellSpan.fg`/`ShellSpan.bg` (e.g. `"#ff0000"`) to beamterm's `u32` RGB format (`0xff0000`)
- Handle `"ansi(N)"` indexed colors — map to standard 256-color palette
- Handle empty string (default color) — use beamterm's theme defaults
- Handle inverse attribute — swap fg/bg

### Task 4: Font configuration
- Configure beamterm with a monospace font (Menlo, Consolas, or bundled)
- Ensure box-drawing characters render correctly
- Set appropriate font size (13-14px to match the IRC chat area)

### Task 5: Canvas sizing and resize
- Size the canvas to fill the chat area
- Handle browser window resize — call beamterm's resize method
- The PTY size is computed server-side (`compute_chat_area_size`) and the backend handles SIGWINCH

### Task 6: Keyboard focus
- Auto-focus the canvas when shell buffer is active
- The existing input.rs global keydown listener already skips focus-steal for shell buffers
- Keyboard input capture and base64 encoding already work — just wire to the canvas element

### Task 7: CSS cleanup
- Update `.shell-terminal` to style the canvas container
- Remove span-related CSS (`.shell-row`, `.shell-placeholder`)
- Ensure the canvas blends with the rest of the UI (dark background, no borders)

### Task 8: Test and verify
- Run `cargo clippy --all-targets` — 0 warnings
- Run `cargo test` — all tests pass
- Build with `trunk build --release`
- Verify btop, vim, weechat render correctly in the web frontend
- Verify keyboard input works (typing, arrow keys, Ctrl+C, etc.)

## Key Crate Info

**beamterm-renderer** (crates.io): Pure Rust WASM WebGL2 terminal renderer
- GitHub: https://github.com/junkdog/beamterm
- API: `Terminal::builder(canvas_selector).build()`, `update_cells(iter)`, `render_frame()`
- Cell type: `CellData::new(symbol, FontStyle, GlyphEffect, fg_rgb, bg_rgb)`
- ~1MB WASM binary
- Uses instanced WebGL2 draw calls for entire grid in one pass
- Dynamic font atlas from system/bundled fonts

## Reference Files

Read these before starting:
- `web-ui/src/components/shell_view.rs` — current implementation to replace
- `web-ui/src/protocol.rs` — ShellScreenRow, ShellSpan, ShellScreenData types
- `web-ui/src/state.rs` — shell_screen signal, handle_event ShellScreen handler
- `web-ui/src/components/chat_view.rs` — dispatch to ShellView for shell buffers
- `web-ui/src/components/input.rs` — global keydown listener shell-awareness
- `src/shell/mod.rs` — screen_to_web() serialization (backend, don't modify)
- `src/web/protocol.rs` — ShellScreen/ShellInput protocol types (backend, don't modify)
- `src/app.rs` — search for "shell" to see all backend integration points (don't modify)
- https://github.com/junkdog/beamterm/tree/main/examples/terminal-emulator — reference for vt100→beamterm cell mapping

## Constraints

- Use `/rust-best-practices` and `/rust-engineer` guidelines
- Use stock beamterm from junkdog (latest version on crates.io) — no forks
- Do NOT modify backend code — only `web-ui/` files
- Branch: `feat/web-shell-beamterm` (create from main)
- Run clippy, tests, and trunk build before declaring done
- Commit and push when complete

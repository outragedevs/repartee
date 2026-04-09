# AGENTS.md

Repository-specific guidance for AI agents working on Repartee.

## Build & Run

- **Always use `make`**, never raw `cargo` or `trunk` commands:
  - `make release` — native release binary
  - `make build` — native dev build (faster, no WASM rebuild)
  - `make test` — `cargo test -p repartee`
  - `make clippy` — `cargo clippy -p repartee --all-targets`
  - `make wasm` or `make web` — Leptos WASM frontend (must run before `make release` if web UI changed)
  - `make all` — clean + WASM + release (full rebuild)
- **Order**: `make clippy` before `make test`. Both must pass with 0 warnings.
- **No `rustfmt.toml`** — use `cargo fmt` with defaults.
- **No pinned toolchain** — requires Rust 1.85+ (2024 edition).
- Runtime data lives in `~/.repartee/` (config, `.env`, themes, logs, sessions, scripts, dicts, certs).

## Workspace Structure

- **Root crate** (`.`): `repartee` — the main binary
- **`web-ui/`**: `repartee-web` — Leptos WASM frontend (separate workspace member, `publish = false`)
- `make test` and `make clippy` target only `-p repartee`; web-ui is not tested by default.

## Naming — Must Use Constants

- The binary/app name is **repartee** (alias: `reptee`).
- **All** strings referencing the app name must use `constants::APP_NAME` — never hardcode `"repartee"` in string literals.
- Paths: `~/.repartee/` — derived from `constants::home_dir()`, never hardcoded.

## Architecture

- **TEA pattern**: `App` (Model) → Events (Message) → `App::run()` (Update) → `ui::render()` (View).
- **Fork model** (`src/main.rs`): Parent = terminal shim, child = headless daemon. Fork happens before tokio runtime.
  - `repartee a [pid]` — attach to running daemon
  - `repartee -d` — start headless (no fork)
- **`src/app/mod.rs`** (~1358 lines) is the central controller with 13 domain submodules managing a `tokio::select!` event loop.
- **`src/state/`** is UI-agnostic — **no ratatui imports allowed**.
- **`src/commands/`**: Handler functions use the signature `fn(&mut App, &[String])` (function pointers). Handler files: `handlers_irc.rs`, `handlers_ui.rs`, `handlers_dcc.rs`, `handlers_admin.rs`.

## Key Dependencies & Quirks

- **IRC**: `irc-repartee` v1.5.0 (custom fork on crates.io). Has `bind_ip` config field and immediate-send flush fix.
- **SQLite**: `rusqlite` with `bundled-full` — no external libsqlite3 needed, but makes releases slower to compile.
- **Lua**: `mlua` with `lua54` + `vendored` — Lua 5.4 is compiled from source.
- **Session protocol**: `postcard` (serde) for detach/reattach serialization over Unix sockets.
- **Image preview**: `ratatui-image` with Kitty/iTerm2/Sixel protocol detection.
- **Web server**: `axum` with WSS + TLS (self-signed via `rcgen`).
- **PTY**: `portable-pty` for embedded `/shell` terminal.
- **Release profile**: LTO=true, codegen-units=1, panic=abort, strip=true — expect long compile times.

## Conventions

- **Error handling**: `color-eyre` for app errors, `thiserror` for library-style error types.
- **Logging**: `tracing` only — never `log`, `println!`, or `eprintln!` (except in `main.rs` fork plumbing).
- **No comments** in code unless explicitly requested.
- **Clippy**: enforced at pedantic=warn, nursery=warn, perf=deny, redundant_clone=deny. Zero warnings policy.
- **Edition**: Rust 2024 — use edition idioms (`use<>` return-position, gen blocks, etc.).

## MemPalace & Skills

MemPalace is persistent project memory — use it to keep the context window small while retaining deep project knowledge across sessions.

**Always:**
- **Search before reading files.** Use `mempalace_mempalace_search` to check if architecture decisions, gotchas, or in-progress work are already stored. Prefer stored context over re-reading large source files.
- **File what you learn.** After discovering something non-obvious (quirks, bugs, design rationale, migration steps), store it with `mempalace_mempalace_add_drawer`. Future sessions start informed without burning context.
- **Use the knowledge graph for entities.** Use `mempalace_mempalace_kg_add` for facts like "`irc-repartee` is a custom fork with `bind_ip` support" or "`src/state/` must stay UI-agnostic".

**Wing:** `repartee`
**Rooms:** `src`, `frontend`, `configuration`, `documentation`, `commands_docs`, `general`

**Reference project — erssi:** `~/dev/erssi/` is the owner's highly modified irssi fork. It's the reference implementation for IRC protocol behavior, theme formatting, and UI patterns. When asked how erssi does something, always check MemPalace's `erssi` wing first (`mempalace_mempalace_search`) before reading source files from `~/dev/erssi/`. File anything you learn from erssi back into the `erssi` wing so future sessions don't need to re-read it.

**Installed skills** (in `.agents/skills/`): `coding-guidelines`, `ratatui-tui`, `rust-async-patterns`, `rust-best-practices`, `rust-engineer`. Load relevant skills before editing Rust or TUI code.

## CI

- Release-only workflow on tag push (`v*`): macOS ARM64, Linux AMD64/ARM64, FreeBSD AMD64.
- Linux builds require `libchafa-dev` and `libglib2.0-dev`; macOS requires `chafa` from Homebrew.
- No CI for tests or clippy — run locally before pushing.

## Common Mistakes to Avoid

- Don't use raw `cargo build` — use `make build` or `make release`.
- Don't hardcode `"repartee"` — use `APP_NAME` constant.
- Don't import ratatui in `src/state/` — that module is UI-agnostic.
- Don't put credentials in `config.toml` — they go in `.env` (loaded at runtime).
- Don't forget to run `make wasm` before `make release` if the web UI changed.
- The `make test` target runs `cargo test -p repartee` only — not the web-ui crate.
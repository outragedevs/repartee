# Repartee

Rust IRC client — a port of kokoirc (~/dev/kokoirc) from TypeScript/OpenTUI/Bun to Rust/ratatui/tokio.

- **Website**: https://repart.ee
- **Repo**: https://github.com/outragedevs/repartee

## Naming

The app name is **Repartee** (binary: `repartee`, alias: `reptee`).

```rust
pub const APP_NAME: &str = "repartee";
```

- Config/data directory: `~/.repartee/`
- Binary installed at: `/usr/local/bin/repartee` (symlink to `target/release/repartee`)
- Alias: `/usr/local/bin/reptee` (symlink to same binary)
- All paths, config dirs, CTCP version strings, etc. must reference the `APP_NAME` constant — do NOT hardcode the name in strings.

## Build

- **Workspace**: `Cargo.toml` has two members: `.` (main binary) and `web-ui/` (Leptos WASM frontend)
- **Makefile**: All builds go through `make` targets — never use raw cargo/trunk commands
  - `make all` — clean + WASM + release
  - `make release` — native release binary
  - `make wasm` / `make web` — Leptos WASM frontend
  - `make test` / `make clippy` — testing and linting
- **CI**: GitHub Actions release workflow on tag push (`v*`) — macOS ARM64, Linux AMD64/ARM64, FreeBSD AMD64

## Architecture

- **Pattern**: TEA (Model → Message → Update → View)
- **TUI**: ratatui 0.30+ with crossterm backend
- **Async**: tokio with crossterm event-stream
- **IRC**: `irc-repartee` v1.5.0 on crates.io (published fork of `irc` crate with bind_address, rustls fix, immediate flush)
  - **Bind address**: `Config::bind_address` — bind to specific local IP (our config field: `bind_ip`)
  - **Immediate send flush**: outgoing messages flush immediately via spawned tokio task (not buffered until next poll)
- **Config**: TOML (`config.toml`), same format as kokoirc
- **Credentials**: `.env` file (never written to config.toml)
- **Theming**: TOML `.theme` files with irssi-compatible format strings
  - `%Z` RRGGBB = 24-bit foreground color
  - `%z` RRGGBB = 24-bit background color
  - `%X` single-letter irssi color codes
  - `{abstract args}` template expansion
  - `$0-$9`, `$*`, `$[N]0` variable substitution
  - mIRC control characters (\x02, \x03, \x04, \x0F, \x16, \x1D, \x1E, \x1F)

### Module Layout

```
src/
├── app/           # TEA controller — 13 domain submodules (backlog, dcc, image, input, irc, maintenance, mentions, scripting, session, shell, web, who, mod)
├── commands/      # Command parser + handler groups (IRC, UI, DCC, admin) + settings + registry
├── config/        # TOML config + .env credentials
├── dcc/           # DCC CHAT (active + passive/reverse)
├── image_preview/ # Kitty/iTerm2/Sixel image preview with async fetch + cache
├── irc/           # IRC protocol (IRCv3 caps, SASL, ISUPPORT, batch, extban, flood, netsplit, ignore, formatting)
├── scripting/     # Lua 5.4 engine (mlua), EventBus, ScriptAPI
├── session/       # Detach/reattach session persistence (postcard protocol)
├── shell/         # Embedded PTY terminal (/shell command)
├── spellcheck/    # Hunspell spell checking (spellbook crate)
├── state/         # UI-agnostic state (buffers, connections, events, sorting)
├── storage/       # SQLite + WAL + FTS5, optional AES-256-GCM, async batched writer
├── theme/         # irssi-compatible theme engine (loader, parser)
├── ui/            # ratatui rendering — 13 view components
├── web/           # axum HTTPS + WSS server, auth, broadcasting, snapshots
├── nick_color.rs  # Deterministic per-nick coloring (djb2 hash, HSL palettes)
└── main.rs        # tokio event loop + select! arms
web-ui/            # Leptos WASM frontend (separate workspace crate)
```

## Reference Projects

- **kokoirc** (`~/dev/kokoirc`): Primary reference for features, UI, theming, config format
- **erssi** (`~/dev/erssi`): Reference for irssi theme format and sidepanel rendering

## Conventions

- Use `color-eyre` for error handling
- Use `tracing` for logging (not `log` or `println!`)
- Follow Rust 2024 edition idioms
- Prefer `thiserror` for library error types
- Clippy: pedantic=warn, nursery=warn, perf=deny, redundant_clone=deny (0 warnings policy)
- Commands use function pointer handlers: `fn(&mut App, &[String])`
- State is UI-agnostic — no ratatui imports in `state/`

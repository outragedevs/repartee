# Repartee

Rust IRC client — a port of kokoirc (~/dev/kokoirc) from TypeScript/OpenTUI/Bun to Rust/ratatui/tokio.

## Naming

The app name is **Repartee** (binary: `repartee`, alias: `reptee`).

```rust
pub const APP_NAME: &str = "repartee";
```

- Config/data directory: `~/.repartee/`
- Binary installed at: `/usr/local/bin/repartee` (symlink to `target/release/repartee`)
- Alias: `/usr/local/bin/reptee` (symlink to same binary)
- All paths, config dirs, CTCP version strings, etc. must reference the `APP_NAME` constant — do NOT hardcode the name in strings.

## Architecture

- **TUI**: ratatui 0.30+ with crossterm backend
- **Async**: tokio with crossterm event-stream
- **IRC**: `irc` crate v1.1.0
- **Config**: TOML (`config.toml`), same format as kokoirc
- **Credentials**: `.env` file (never written to config.toml)
- **Theming**: TOML `.theme` files with irssi-compatible format strings
  - `%Z` RRGGBB = 24-bit foreground color
  - `%z` RRGGBB = 24-bit background color
  - `%X` single-letter irssi color codes
  - `{abstract args}` template expansion
  - `$0-$9`, `$*`, `$[N]0` variable substitution
  - mIRC control characters (\x02, \x03, \x04, \x0F, \x16, \x1D, \x1E, \x1F)

## Reference Projects

- **kokoirc** (`~/dev/kokoirc`): Primary reference for features, UI, theming, config format
- **erssi** (`~/dev/erssi`): Reference for irssi theme format and sidepanel rendering

## Conventions

- Use `color-eyre` for error handling
- Use `tracing` for logging (not `log` or `println!`)
- Follow Rust 2024 edition idioms
- Prefer `thiserror` for library error types

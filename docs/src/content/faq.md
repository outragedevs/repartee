# FAQ

## What is rustirc?

rustirc is a terminal IRC client written in Rust, inspired by irssi and built as a port of [kokoirc](https://github.com/kofany/kokoIRC) (TypeScript/OpenTUI/Bun) to Rust/ratatui/tokio.

## Why Rust?

- **Performance**: ~5MB binary, instant startup, minimal memory usage
- **Safety**: Memory-safe without garbage collection
- **Concurrency**: tokio async runtime handles multiple connections efficiently
- **Reliability**: Rust's type system catches bugs at compile time
- **Distribution**: Single static binary, no runtime dependencies

## How does rustirc compare to kokoirc?

| Feature | kokoirc | rustirc |
|---|---|---|
| Language | TypeScript | Rust |
| TUI framework | OpenTUI/React | ratatui |
| Runtime | Bun | Native binary |
| Binary size | ~68MB | ~5MB |
| Scripting | TypeScript | Lua 5.4 |
| Config format | TOML | TOML (same format) |
| Theme format | irssi-compatible | irssi-compatible (same) |

The config and theme formats are compatible — you can copy your kokoirc config to rustirc with minimal changes.

## How do I migrate from kokoirc?

1. Copy `~/.kokoirc/config.toml` to `~/.rustirc/config.toml`
2. Copy `~/.kokoirc/.env` to `~/.rustirc/.env`
3. Copy `~/.kokoirc/themes/` to `~/.rustirc/themes/`
4. Scripts need to be rewritten from TypeScript to Lua

## How do I migrate from irssi?

rustirc uses irssi-compatible format strings, so your theme knowledge transfers directly. The key differences:

- Config is TOML instead of irssi's custom format
- Scripts are Lua instead of Perl
- Most `/commands` work the same

## Where are logs stored?

`~/.rustirc/logs/messages.db` — a SQLite database with optional AES-256-GCM encryption.

## Can I use multiple IRC networks?

Yes. Add multiple `[servers.*]` sections to your config. Each gets its own connection and set of channel buffers.

## Does rustirc support IRCv3?

The underlying `irc` crate supports IRCv3 capabilities including SASL, server-time, and CAP negotiation.

## How do I report bugs?

Open an issue on the [GitHub repository](https://github.com/kofany/rustirc/issues).

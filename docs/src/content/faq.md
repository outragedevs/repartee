# FAQ

## What is repartee?

repartee is a terminal IRC client written in Rust, inspired by irssi and built as a port of [kokoirc](https://github.com/kofany/kokoIRC) (TypeScript/OpenTUI/Bun) to Rust/ratatui/tokio.

## Why Rust?

- **Performance**: ~5MB binary, instant startup, minimal memory usage
- **Safety**: Memory-safe without garbage collection
- **Concurrency**: tokio async runtime handles multiple connections efficiently
- **Reliability**: Rust's type system catches bugs at compile time
- **Distribution**: Single static binary, no runtime dependencies

## How does repartee compare to kokoirc?

| Feature | kokoirc | repartee |
|---|---|---|
| Language | TypeScript | Rust |
| TUI framework | OpenTUI/React | ratatui |
| Runtime | Bun | Native binary |
| Binary size | ~68MB | ~5MB |
| Scripting | TypeScript | Lua 5.4 |
| Config format | TOML | TOML (same format) |
| Theme format | irssi-compatible | irssi-compatible (same) |

The config and theme formats are compatible — you can copy your kokoirc config to repartee with minimal changes.

## How do I migrate from kokoirc?

1. Copy `~/.kokoirc/config.toml` to `~/.repartee/config.toml`
2. Copy `~/.kokoirc/.env` to `~/.repartee/.env`
3. Copy `~/.kokoirc/themes/` to `~/.repartee/themes/`
4. Scripts need to be rewritten from TypeScript to Lua

## How do I migrate from irssi?

repartee uses irssi-compatible format strings, so your theme knowledge transfers directly. The key differences:

- Config is TOML instead of irssi's custom format
- Scripts are Lua instead of Perl
- Most `/commands` work the same

## Where are logs stored?

`~/.repartee/logs/messages.db` — a SQLite database with optional AES-256-GCM encryption.

## Can I use multiple IRC networks?

Yes. Add multiple `[servers.*]` sections to your config. Each gets its own connection and set of channel buffers.

## Does repartee support IRCv3?

Yes — repartee has comprehensive IRCv3 support negotiated at connection time:

- **server-time**, **echo-message**, **away-notify**, **account-notify**, **chghost**, **cap-notify**
- **multi-prefix** (e.g. `@+nick`), **extended-join**, **userhost-in-names**, **message-tags**
- **invite-notify**, **BATCH** (netsplit/netjoin grouping)
- **SASL**: PLAIN, EXTERNAL (client certificate), SCRAM-SHA-256
- **WHOX**: auto-detected for account name and full host tracking
- **Extban**: `$a:account` ban type with `/ban -a` shorthand

## What does /cycle do?

`/cycle` parts and immediately rejoins a channel. Useful for refreshing your nick list, re-triggering auto-op, or clearing stale channel state. Channel keys are preserved. Alias: `/rejoin`.

## How do I report bugs?

Open an issue on the [GitHub repository](https://github.com/kofany/repartee/issues).

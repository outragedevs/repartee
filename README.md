# Repartee

**A modern terminal IRC client built with Rust, Ratatui, and Tokio.**

Inspired by irssi. Designed for the future.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2024%20edition-orange.svg)](https://www.rust-lang.org)
[![Crates.io](https://img.shields.io/crates/v/repartee.svg)](https://crates.io/crates/repartee)
[![GitHub Pages](https://img.shields.io/badge/docs-online-brightgreen.svg)](https://outragedevs.github.io/repartee/)

---

## Demo

Terminal, mobile web, and desktop web — all in real-time sync:

[![Repartee Demo](https://img.youtube.com/vi/okU4WKF5GDI/maxresdefault.jpg)](https://www.youtube.com/watch?v=okU4WKF5GDI)

> TUI (left) | Mobile web (center) | Desktop web (right) — 1:1 state sync across all interfaces.

---

## Features

- **Full IRC protocol** — channels, queries, CTCP, TLS, channel modes, ban/except/invex lists
- **IRCv3** — server-time, echo-message, away-notify, account-notify, chghost, multi-prefix, BATCH netsplit grouping, message-tags, and more
- **SASL** — PLAIN, EXTERNAL (client certificate), and SCRAM-SHA-256
- **irssi-style navigation** — Esc+1–9 window switching, aliases, familiar `/commands`
- **Mouse support** — click buffers and nicks, scroll chat history
- **Lua 5.4 scripting** — event bus, custom commands, full IRC and state access, sandboxed per-script environments
- **Persistent logging** — SQLite with WAL, FTS5 full-text search, optional AES-256-GCM encryption
- **Netsplit detection** — batches join/part floods into single events
- **Flood protection** — blocks CTCP spam and nick-change floods automatically
- **Theming** — irssi-compatible format strings with 24-bit color support and custom abstracts
- **Web frontend** — built-in HTTPS web UI with mobile support, real-time sync with the terminal, swipe gestures, 5 themes
- **DCC CHAT** — direct client-to-client messaging with active and passive (reverse) connections
- **Spell check** — inline correction with Hunspell dictionaries, multilingual, Tab to cycle suggestions
- **Detach & reattach** — detach from your terminal and reattach later; IRC connections stay alive
- **Extban** — `$a:account` ban type with `/ban -a` shorthand
- **Single binary** — ~15MB, zero runtime dependencies (SQLite, Lua, and WASM frontend bundled)

---

## Installation

### From source

```bash
git clone https://github.com/outragedevs/repartee.git
cd repartee
cargo build --release
./target/release/repartee
```

### Requirements

- **Rust 1.85+** (2024 edition) — install via [rustup](https://rustup.rs)
- A terminal with 256-color or truecolor support (iTerm2, Alacritty, kitty, WezTerm, etc.)
- A modern web browser for the web frontend (optional)

---

## Quick Start

Launch repartee:

```bash
repartee
```

Add a server and connect:

```
/server add libera irc.libera.chat
/connect libera
/join #repartee
```

Or edit `~/.repartee/config.toml` directly:

```toml
[servers.libera]
label    = "Libera"
address  = "irc.libera.chat"
port     = 6697
tls      = true
autoconnect = true
channels = ["#repartee"]
```

---

## Key Bindings

| Key | Action |
|-----|--------|
| `Esc + 1–9` | Switch to buffer |
| `Ctrl+N` / `Ctrl+P` | Next / previous buffer |
| `Tab` | Nick completion |
| `Up` / `Down` | Input history |
| `Mouse click` | Select buffer or nick |
| `Mouse wheel` | Scroll chat |
| `Ctrl+\` | Detach from terminal |
| `Ctrl+Z` | Detach from terminal |

---

## Directory Layout

```
~/.repartee/
  config.toml        # main configuration
  .env               # credentials (SASL passwords, log encryption key)
  themes/            # custom .theme files
  scripts/           # Lua scripts
  logs/messages.db   # chat logs (SQLite)
  sessions/          # Unix sockets for detached sessions
```

---

## Sessions & Detach

repartee can run in the background while you close your terminal:

```bash
# Detach: press Ctrl+\ or type /detach — terminal is restored
# Reattach from any terminal:
repartee a

# Or start headless (no terminal needed):
repartee -d
repartee a       # attach when ready
```

Everything survives detach — IRC connections, scrollback, scripts, and channel state.

---

## Scripting

Scripts are Lua 5.4 files placed in `~/.repartee/scripts/`:

```lua
meta = {
    name        = "hello",
    version     = "1.0",
    description = "Greet users on join",
}

function setup(api)
    api.on("irc.join", function(event)
        if event.nick ~= api.our_nick() then
            api.irc.say(event.channel, "Welcome, " .. event.nick .. "!")
        end
    end)
end
```

Load at runtime:

```
/script load hello
```

Or autoload in config:

```toml
[scripts]
autoload = ["hello"]
```

---

## Theming

Themes are TOML files in `~/.repartee/themes/` using irssi-compatible format strings with 24-bit color extensions:

```toml
[colors]
bg        = "1a1b26"
fg        = "a9b1d6"
highlight = "e0af68"
nick_self = "7aa2f7"

[abstracts]
pubmsg  = "{pubmsgnick $0}$1"
own_msg = "{ownmsgnick $0}$1"
```

Set the active theme:

```toml
[general]
theme = "mytheme"
```

---

## Documentation

Full documentation is available at **[outragedevs.github.io/repartee](https://outragedevs.github.io/repartee/)**.

- [Installation](https://outragedevs.github.io/repartee/installation.html)
- [First Connection](https://outragedevs.github.io/repartee/first-connection.html)
- [Configuration Reference](https://outragedevs.github.io/repartee/configuration.html)
- [Command List](https://outragedevs.github.io/repartee/commands.html)
- [Web Frontend](https://outragedevs.github.io/repartee/web-frontend.html)
- [Sessions & Detach](https://outragedevs.github.io/repartee/sessions.html)
- [Scripting API](https://outragedevs.github.io/repartee/scripting-api.html)
- [Theming](https://outragedevs.github.io/repartee/theming.html)
- [Logging & Search](https://outragedevs.github.io/repartee/logging.html)

---

## License

MIT — see [LICENSE](LICENSE).

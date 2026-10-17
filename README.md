<div align="center">

# Repartee

**A modern terminal IRC client — inspired by irssi, built with Rust, Ratatui and Tokio.**

[![Crates.io](https://img.shields.io/crates/v/repartee.svg?logo=rust)](https://crates.io/crates/repartee)
[![Downloads](https://img.shields.io/crates/d/repartee.svg)](https://crates.io/crates/repartee)
[![Checks](https://img.shields.io/github/actions/workflow/status/outragedevs/repartee/dependencies.yml?branch=main&label=checks)](https://github.com/outragedevs/repartee/actions/workflows/dependencies.yml)
[![MSRV](https://img.shields.io/badge/MSRV-1.91-orange.svg)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

[Website](https://repart.ee/) ·
[Documentation](https://repart.ee/docs) ·
[Installation](#installation) ·
[Changelog](CHANGELOG.md) ·
[Releases](https://github.com/outragedevs/repartee/releases)

[![Repartee demo: terminal, mobile web and desktop web in real-time sync](https://img.youtube.com/vi/okU4WKF5GDI/maxresdefault.jpg)](https://www.youtube.com/watch?v=okU4WKF5GDI)

<sub>Terminal (left), mobile web (center) and desktop web (right), kept in 1:1 sync.</sub>

</div>

---

Repartee keeps the speed and keyboard-driven workflow of irssi and adds what a
client needs today: full IRCv3, bouncer integration, end-to-end encryption, a
browser frontend that mirrors the terminal, and sessions that survive closing
your terminal. It ships as a single binary with SQLite, Lua and the web
frontend built in.

## Highlights

**Protocol**

- Full IRC support — channels, queries, CTCP, TLS, channel modes, ban/except/invex lists, `$a:account` extbans
- [IRCv3](https://repart.ee/docs/reference/ircv3) — `server-time`, `echo-message`, `chathistory`, `multiline`, `+typing`, labeled responses, `MONITOR`, `SETNAME`, message redaction and more
- SASL — PLAIN, EXTERNAL (client certificate), SCRAM-SHA-1/256/512 and ECDSA-NIST256P-CHALLENGE, strongest mechanism picked automatically
- First-class bouncer support — network discovery and binding, Soju management via `/bouncer`, server-side history and search, WebPush
- DCC CHAT with active and passive (reverse) connections

**Interface**

- irssi-style navigation, aliases and familiar `/commands`, with rebindable keys via `/bind`
- Mouse support, nick completion, multi-line composing and inline spell checking (Hunspell)
- Inline image previews (Kitty, iTerm2 and Sixel graphics) and animated emotes
- Embedded PTY shell (`/shell`) — run vim or btop without leaving the client
- irssi-compatible [themes](https://repart.ee/docs/configuration/themes) with 24-bit color and deterministic nick colors

**Everywhere**

- [Detach and reattach](https://repart.ee/docs/features/sessions) — connections, scrollback and scripts stay alive in the background
- Built-in HTTPS [web frontend](https://repart.ee/docs/features/web-frontend) with mobile support, synchronized live with the terminal
- [Persistent logs](https://repart.ee/docs/features/logging) in SQLite with full-text search and optional AES-256-GCM encryption
- [End-to-end encryption](https://repart.ee/docs/rpe2e) for channels and queries
- Optional [live translation](https://repart.ee/docs/features/translation) of conversations through AI providers
- [Lua 5.4 scripting](https://repart.ee/docs/features/lua-scripting) with an event bus and sandboxed per-script environments

## Installation

### Pre-built binaries

Download the archive for your platform from the
[latest release](https://github.com/outragedevs/repartee/releases/latest):

| Platform       | Archive                          |
| -------------- | -------------------------------- |
| macOS ARM64    | `repartee-macos-arm64.tar.gz`    |
| Linux x86_64   | `repartee-linux-amd64.tar.gz`    |
| Linux ARM64    | `repartee-linux-arm64.tar.gz`    |
| FreeBSD x86_64 | `repartee-freebsd-amd64.tar.gz`  |

### Cargo

```sh
cargo install --locked repartee
```

### From source

```sh
git clone https://github.com/outragedevs/repartee.git
cd repartee
make release
./target/release/repartee
```

### Requirements

- Rust 1.91 or newer to build — install it with [rustup](https://rustup.rs)
- A terminal with 256-color or truecolor support, such as Ghostty, WezTerm, kitty, iTerm2 or Alacritty
- A modern browser, if you use the web frontend

## Quick start

```sh
repartee            # start the client
repartee --help     # every launch flag, subcommand and path
```

Add a network, connect and join a channel:

```text
/server add libera irc.libera.chat
/connect libera
/join #repartee
```

Or describe the network in `~/.repartee/config.toml`:

```toml
[servers.libera]
label       = "Libera"
address     = "irc.libera.chat"
port        = 6697
tls         = true
autoconnect = true
channels    = ["#repartee"]
```

The [first connection guide](https://repart.ee/docs/quick-start) covers SASL,
certificates and bouncers.

## Key bindings

| Key                            | Action                              |
| ------------------------------ | ----------------------------------- |
| `Alt+1`…`Alt+9` or `Esc 1`…`9` | Switch to window 1–9                |
| `Alt+0`                        | Status window                       |
| `Alt+←` / `Alt+→`              | Previous / next window              |
| `Alt+A`                        | Next window with activity           |
| `Alt+L`                        | Bare display for selecting text     |
| `Tab`                          | Complete nicks, commands and emotes |
| `↑` / `↓`                      | Input history                       |
| `PgUp` / `PgDn`                | Scroll the chat                     |
| `Alt+Enter`                    | New line in a multi-line message    |
| `Ctrl+G`                       | Emote picker                        |
| `Ctrl+]`                       | Leave shell input mode              |
| `Ctrl+Z` or `/detach`          | Detach from the terminal            |

`/bind` lists and changes the configurable bindings.

## Sessions

Repartee keeps running when you close the terminal:

```sh
repartee -d         # start headless
repartee a          # attach from any terminal
```

Press `Ctrl+Z` or type `/detach` to leave a running session; IRC connections,
scrollback, scripts and channel state stay intact.

## Configuration

Everything lives in `~/.repartee/`:

```text
~/.repartee/
├── config.toml        main configuration
├── .env               credentials (SASL passwords, log encryption key)
├── themes/            custom .theme files
├── scripts/           Lua scripts
├── logs/messages.db   chat logs (SQLite)
└── sessions/          sockets for detached sessions
```

<details>
<summary><b>Source address on multi-homed hosts</b></summary>

<br>

The outgoing IRC source address is chosen in this order:

1. `servers.<id>.bind_ip` in `config.toml`, also settable with `/server add … -bind=<ip>` or `/connect -bind=<ip>`
2. `repartee -h <ip>` — a runtime override modelled after `irssi -h`
3. `general.default_bind_ip` in `config.toml`, or `/set general.default_bind_ip <ip>`
4. The operating system's default route

```sh
repartee -h 192.0.2.10        # one session from a specific address
repartee --bind=2001:db8::1
repartee -h 192.0.2.10 -d     # combined with --detach
```

The command-line flag applies to that session only and is never written back
to `config.toml`; a server's own `bind_ip` always wins.

</details>

<details>
<summary><b>Lua scripting</b></summary>

<br>

Scripts are Lua 5.4 files in `~/.repartee/scripts/`:

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

Load it with `/script load hello`, or autoload it from `config.toml`:

```toml
[scripts]
autoload = ["hello"]
```

See the [Lua API reference](https://repart.ee/docs/reference/lua-api) and
[examples](https://repart.ee/docs/reference/scripting-examples).

</details>

<details>
<summary><b>Themes</b></summary>

<br>

Themes are TOML files in `~/.repartee/themes/` using irssi-compatible format
strings with 24-bit color extensions:

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

Activate one with `theme = "mytheme"` in the `[general]` section.

</details>

## Documentation

The full documentation lives at **[repart.ee/docs](https://repart.ee/docs)**:

| Getting started | Features | Reference |
| --- | --- | --- |
| [Installation](https://repart.ee/docs/installation) | [IRC](https://repart.ee/docs/features/irc) | [Commands](https://repart.ee/docs/reference/commands) |
| [Quick start](https://repart.ee/docs/quick-start) | [Web frontend](https://repart.ee/docs/features/web-frontend) | [IRCv3 support](https://repart.ee/docs/reference/ircv3) |
| [Configuration](https://repart.ee/docs/configuration) | [Sessions](https://repart.ee/docs/features/sessions) | [Lua API](https://repart.ee/docs/reference/lua-api) |
| [Themes](https://repart.ee/docs/configuration/themes) | [Logging and search](https://repart.ee/docs/features/logging) | [Architecture](https://repart.ee/docs/architecture) |
| [FAQ](https://repart.ee/docs/faq) | [Translation](https://repart.ee/docs/features/translation) | [RPE2E encryption](https://repart.ee/docs/rpe2e) |

## Development

All builds go through `make`:

| Target                  | Purpose                                        |
| ----------------------- | ---------------------------------------------- |
| `make release`          | Optimized native binary                        |
| `make wasm`             | Leptos web frontend                            |
| `make clippy`           | Lints (pedantic and nursery, zero warnings)    |
| `make test`             | Test suite                                     |
| `make dependency-check` | Dependency policy for the whole workspace      |

`make dependency-check` needs Python 3.9+. It rejects the retired
`proc-macro-error` packages anywhere in the dependency graph, including
renamed, optional and target-specific entries, and fails on a stale
`Cargo.lock`. It runs before `make clippy` and `make test` and on every pull
request; it is not a substitute for a general security audit.

## What's new in 2.2.1

- **Dependency cleanup and updates.** Remove redundant dependency wrappers, disable unused image and IRC configuration features, use AWS-LC for certificate generation, and replace retired macro dependencies with maintained releases.
- **Compatible data and scripting upgrades.** Refresh the cryptographic libraries, bundled SQLite, Linux allocator, and Lua integration, with fixed fixtures for existing encrypted data and database migrations. Lua remains on 5.4 and preserves fractional integer argument handling.
- **Terminal rendering updates.** Update Ratatui, ratatui-image, and the browser terminal renderer. Retire the old rendering pin while preserving inline images, and handle redraw and resize queries in the detached daemon.
- **Web shell fixes.** Start a browser shell in the tab that opened it and keep wide characters aligned with the server's terminal columns.
- **Ad-hoc network labels.** Connections opened with `/connect` adopt the server's network name again.

Earlier releases are described in the [changelog](CHANGELOG.md).

## License

Repartee is released under the [MIT License](LICENSE).

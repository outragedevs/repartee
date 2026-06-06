# Repartee

**A modern terminal IRC client built with Rust, Ratatui, and Tokio.**

Inspired by irssi. Designed for the future.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2024%20edition-orange.svg)](https://www.rust-lang.org)
[![Crates.io](https://img.shields.io/crates/v/repartee.svg)](https://crates.io/crates/repartee)
[![Website](https://img.shields.io/badge/web-repart.ee-brightgreen.svg)](https://repart.ee/)

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
- **Nick coloring** — deterministic per-nick colors (WeeChat-style) with HSL hue wheel for truecolor, 256-color and 16-color fallbacks, auto-detected terminal capability, configurable saturation/lightness
- **Theming** — irssi-compatible format strings with 24-bit color support and custom abstracts
- **Web frontend** — built-in HTTPS web UI with mobile support, real-time sync with the terminal, swipe gestures, 5 themes
- **DCC CHAT** — direct client-to-client messaging with active and passive (reverse) connections
- **Spell check** — inline correction with Hunspell dictionaries, multilingual, Tab to cycle suggestions, computing/IT dictionary with 7,400+ terms, replace and highlight modes
- **Embedded shell** — full PTY terminal inside Repartee (`/shell`) — run vim, btop, irssi without leaving the client. Also available in the web frontend via beamterm WebGL2 renderer with Nerd Font, mouse selection, Ctrl+/- font resize, and clipboard paste
- **Detach & reattach** — detach from your terminal and reattach later; IRC connections stay alive
- **Extban** — `$a:account` ban type with `/ban -a` shorthand
- **Single binary** — ~20MB (SQLite, Lua, and WASM frontend bundled). No external C libraries required for image preview

---

## Installation

### Pre-built binaries

Download from [GitHub Releases](https://github.com/outragedevs/repartee/releases/latest):

| Platform | Binary |
|----------|--------|
| macOS ARM64 | `repartee-macos-arm64.tar.gz` |
| Linux x86_64 | `repartee-linux-amd64.tar.gz` |
| Linux ARM64 | `repartee-linux-arm64.tar.gz` |
| FreeBSD x86_64 | `repartee-freebsd-amd64.tar.gz` |

### From crates.io

```bash
cargo install repartee
```

### From source

```bash
git clone https://github.com/outragedevs/repartee.git
cd repartee
make release
./target/release/repartee
```

### Requirements

- **Build**: Rust 1.85+ (2024 edition) — install via [rustup](https://rustup.rs)
- A terminal with 256-color or truecolor support (iTerm2, Alacritty, kitty, WezTerm, Ghostty, Subterm, etc.)
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
| `Ctrl+]` | Exit shell input mode |
| `Ctrl+Z` | Detach from terminal |
| `/detach` or `/dt` | Detach from terminal |

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
# Detach: press Ctrl+Z or type /detach — terminal is restored
# Reattach from any terminal:
repartee a

# Or start headless (no terminal needed):
repartee -d
repartee a       # attach when ready
```

Everything survives detach — IRC connections, scrollback, scripts, and channel state.

---

## Bind address (multi-homed hosts)

On a host with multiple local IPs, repartee picks the outgoing IRC source address using this precedence:

1. **`servers.<id>.bind_ip`** in `config.toml` (or set inline with `/server add ... -bind=<ip>` / `/connect -bind=<ip>`)
2. **`repartee -h <ip>`** — runtime override (modelled after `irssi -h`)
3. **`general.default_bind_ip`** in `config.toml` (or `/set general.default_bind_ip <ip>`)
4. OS default (kernel routing table)

```bash
# One-off connection from a specific source IP (doesn't touch config):
repartee -h 192.0.2.10
repartee --bind=2001:db8::1
repartee -h 192.0.2.10 -d   # combine with --detach

# Make 192.0.2.10 the default for every server lacking its own bind_ip:
/set general.default_bind_ip 192.0.2.10
```

The CLI flag only applies for that session — it never writes back to `config.toml`. A server entry with its own `bind_ip` is always honoured first, so per-server overrides are stable across CLI invocations.

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

Full documentation is available at **[repart.ee/docs](https://repart.ee/docs)**.

- [Installation](https://repart.ee/docs/installation)
- [Quick Start](https://repart.ee/docs/quick-start)
- [Configuration](https://repart.ee/docs/configuration)
- [Themes](https://repart.ee/docs/configuration/themes)
- [Commands](https://repart.ee/docs/reference/commands)
- [Web Frontend](https://repart.ee/docs/features/web-frontend)
- [Sessions & Detach](https://repart.ee/docs/features/sessions)
- [Lua Scripting](https://repart.ee/docs/features/lua-scripting)
- [Lua API Reference](https://repart.ee/docs/reference/lua-api)
- [Logging & Search](https://repart.ee/docs/features/logging)

---

## Changelog

### v1.4.3

- **Web UI: emote/emoji picker now inserts on mobile.** Both the desktop and mobile layouts mount their own composer, so picking an emote routed the inserted `:name:`/emoji to the hidden desktop input; mobile saw nothing. The insertion now targets the visible composer.
- **Web UI: active-buffer view is synced 1:1 again across the TUI and every web session.** Switching the active channel/query anywhere — the terminal, any browser tab, the phone — now propagates everywhere, restoring the original shared-view behaviour. (Shell buffers stay per-session, since each web session owns its own terminal.)
- **Web UI: fixed chat flicker with images.** The whole chat area was re-rendering — recreating every preview `<img>` — whenever the buffer list churned (unread counts/activity from traffic on other channels), which on Chrome flashed each image off and back and jumped the scroll. The chat now only re-renders on an actual shell/non-shell switch.

### v1.4.2

- **Web UI: IRC formatting now renders in the channel topic.** The chat parser already handled mIRC/irssi colour, bold, italic and underline, but the topic bar rendered raw text — so a formatted topic showed control bytes instead of styles. The topic now runs through the same parser (URLs in topics are clickable too), and the mobile topic breadcrumb strips control codes so no raw bytes leak. The web mIRC colour palette was also extended from 16 to the full 99 colours, matching the TUI.
- **Web UI: emote & emoji pickers.** A **GG emote picker** (animated `:name:` GIF thumbnails with a live filter) opens from a toolbar button, the `/emoji` command (and `/emote`, `/emotes` aliases), or **Ctrl+G**, and `:name:` now **tab-completes** in the composer. A separate **Unicode emoji picker** (category tabs + search) opens from its own button on desktop — on mobile it's hidden, since phones already provide a system emoji keyboard. Both insert at the caret.

### v1.4.1

- **Fix: inline emotes rendered as blank cells on `cargo install` builds.** `ratatui-core 0.1.1` rewrote the buffer's skip/multi-width cell handling, which breaks the skip-cell mechanism `ratatui-image` relies on to draw inline graphics — so emote thumbnails (and the `/emote` picker) showed only one or two images and empty gaps for the rest. Source and CI builds were shielded by `Cargo.lock`, but `cargo install` ignores the lockfile and pulled the newer crate. `ratatui-core` is versioned independently and floats under `ratatui`'s requirement, so the fix pins **`ratatui-core = "=0.1.0"`** directly (pinning the main `ratatui` crate is not enough). macOS/Linux source builds were never affected.
- **Fix: multi-codepoint emoji in chat are now measured by grapheme cluster.** Line wrapping and inline-emote placement summed per-`char` widths, but ratatui lays text out by grapheme clusters — so a ZWJ family emoji (👨‍👩‍👧), a skin-tone modifier (👍🏽) or a variation-selector glyph (❤️) was mis-measured. This could split an emoji across wrapped lines or position an inline emote image on the wrong cells when such an emoji preceded it on the same line. Both paths now tokenize with `unicode-segmentation` and measure each cluster with `UnicodeWidthStr`, matching ratatui's own layout.

### v1.4.0

- **Built-in `:name:` emotes — 183 curated GG7 GIFs, embedded in the binary, rendered inline in both the TUI and the web UI.** Type `:usmiech:` (or the English alias `:smile:`) and it renders as an animated emote in place. The TUI composites GIF frames onto a cell-sized, theme-coloured background with an idle-friendly ~20 fps animation clock (pinned sleep, no busy-loop when nothing moves); the web UI serves them as inline `<img>` from `/emotes/{name}.gif`. An **emote picker overlay** (`Ctrl+G`) with keyboard + mouse navigation and graphical thumbnails, plus `/emote` (alias `/emoji`) to insert by name. Tab-completion offers `:name:` prefixes (closing colon + space). Names work in **Polish or English**, case-insensitive, selectable with `/set emotes.lang en|pl`. New `[emotes]` config section (`enabled`, `render` mode). The whole set — GIFs and the PL→EN alias table — is compiled in via `rust-embed`, so nothing is fetched at runtime.
- **`/wizard server` — a guided add/edit-server form, in the TUI and the web UI.** A friendly popup for "mIRC users" who don't want to memorise `/server add` flags. Fixed-size modal with **Basics** (Network Name, address/IP, port, TLS + verify-cert toggles, bind IP) and **Advanced** (nick, channels, SASL user/pass/mechanism, encoding, reconnect, autosendcmd, client cert) pages. Full mouse support — click fields, toggle checkboxes, switch pages, press buttons — plus keyboard (Tab/Shift-Tab, Space, ←→, Enter, Esc). In the TUI, `/wizard server` adds and `/wizard server <id>` edits an existing server pre-filled (the id field is locked). In the web UI, a **"+ Add network"** button or typing `/wizard server` opens an add form. Credentials are written to `.env`, never `config.toml`; on edit, an untouched password field leaves the stored secret intact. Manual `/server add` is unchanged. Built on a reusable wizard-form toolkit, so future `/wizard` forms can reuse it.
- **`/kick` no longer needs a `:` before the reason.** Nicks are now a single comma-separated token and everything after the first space is the reason — `/kick spammer stop flooding` and `/kick alice,bob,carol go away` both work, no colon required (a leading `:` is still accepted for backward compatibility). This removes the ambiguity of space-separated nick lists. **Behaviour change:** `/kick a b c` now kicks `a` with reason `"b c"` (previously kicked all three); multi-target kick is comma-only. Tab-completion treats a comma as a nick-list separator, so `/kick alice,b<TAB>` completes only the segment after the last comma.
- **`/help` is fully compiled into the binary.** Command documentation was previously read from `docs/commands/*.md` on disk at runtime (searching the executable's directory and the CWD), so an installed binary needed the `docs/` tree beside it. The docs are now embedded via `rust-embed` — `repartee` is a single self-contained file for help, themes, web UI, emotes, and the default config; only your own `~/.repartee/` files live on disk.
- **Web add-server wizard fixes** — a failing or invalid `SaveServer` now shows its error toast only to the web session that submitted the form, instead of every connected client. The `.env` credential purge on `/server remove` is gated on a successful `config.toml` save (and the in-memory server entry is restored if the save fails), so a write error can't leave credentials and config out of sync; purge failures are surfaced rather than swallowed. Fresh web clients now receive `emotes_enabled` in `SyncInit`, so they honour the config without a reload.

### v1.3.3

- **TUI no longer freezes for seconds on busy IRC networks** — the Lua-script state snapshot was being deep-cloned (every connection, every buffer, every nick across every buffer, plus the full app `config.toml`) after every IRC event, every keystroke, every shim event, and every script action. On a power-user setup (~10 networks × 30 channels × 200 nicks) that's 20–50 ms per call, and a 200-event channel burst stacked into 4–10 seconds of frozen UI on `tokio::current_thread`. The fix: `update_script_snapshot()` early-returns when no Lua scripts are loaded (the dominant case), and the eager rebuild was dropped from the event arms — the tick arm (1 s) is now the single rebuild site. New `ScriptManager::has_loaded_scripts()` is the correct guard (the prior `!script_commands.is_empty()` missed event-only scripts). `tracing::warn!` fires if a rebuild ever exceeds 50 ms, so future snapshot growth surfaces in logs before users feel it.
- **Mobile web UX rewrite — adopt TheLounge's scroll-pin pattern.** Third pass at the mobile chat-view jitter; earlier rounds layered fixes on top of an architecture that was the actual problem. CSS: `html, body { height: 100% }` (not `dvh` — animated frame-by-frame during URL-bar/keyboard transitions and triggered our resize handlers on every frame), `body { touch-action: none }` to block pull-to-refresh, `.bottom-bar` is now a plain `flex: 0 0 auto` item (not `position: sticky`, which intermittently detached during iOS Safari URL-bar collapse), and `content-visibility: auto` is gone from `.chat-line` and `.msg-preview-card.loaded` — that was the headline jitter source, off-screen lines were 40 px placeholders that re-measured on viewport entry and grew `scrollHeight` underneath the pin. Viewport meta drops `interactive-widget=resizes-content` (Chrome-Android only, net negative). Rust scroll-pin: drop `ResizeObserver` and `VisualViewport` listeners entirely in favour of a single throttled (RAF-coalesced) `window.resize`; new `skip_next_scroll` flag so programmatic pins don't re-enter `on_scroll` and stale-flip `is_at_bottom` (this is the fix for "I close the keyboard and chat doesn't snap back to the bottom"); new `pin_scheduled` debounce so message bursts produce at most one pin per frame; `SCROLL_THRESHOLD` 100 → 30 (matches TheLounge); image-preview `onload` pins inline from JS (replaces the deleted `ResizeObserver`). Net result: mobile scroll behaves like TheLounge / Obsidian / Slack — no jitter, deterministic snap-to-bottom on keyboard close.

### v1.3.2

- **Rock-solid mobile scroll stickiness** — the web chat view on mobile no longer "jumps up several lines and then back down" on every new message. Four root causes addressed together: (1) `overflow-anchor: none` on `.chat-messages` so the browser's scroll-anchor heuristic stops fighting the manual pin; (2) a `ResizeObserver` on a new `.chat-messages-inner` wrapper re-pins `scrollTop` on EVERY content-height change (image decode, font swap, content-visibility re-measure, new line), replacing the single-rAF append handler that raced layout; (3) `interactive-widget=resizes-content` in the viewport meta so Chrome 108+ Android shrinks the layout viewport when the keyboard opens; (4) a parallel `visualViewport.resize`/`scroll` listener so iOS Safari (which does NOT fire `window.resize` on keyboard open) gets the same pin-if-at-bottom treatment. Tested behaviour now matches TheLounge / Slack / Discord stickiness.
- **Mobile compound improvements** — `SCROLL_THRESHOLD` raised 40 → 100 px (a single wrapped mobile line is often 30–50 px); `.chat-line` `contain-intrinsic-size` raised to `auto 40px` so the virtualization placeholder no longer underestimates real mobile heights; `.app` dropped `position: fixed; inset: 0` (conflicted with `100dvh` during iOS URL-bar collapse); `html, body` switched to `100dvh`; `.bottom-bar` made `position: sticky; bottom: 0` so safe-area-inset collapse on keyboard-open no longer shifts the chat list; `scroll-padding-bottom: 60px` so iOS auto-scroll-into-focus snaps above the bar instead of mid-message.
- **`<For>` key correctness** — chat-message keying was switched to `(msg.id, msg.timestamp)` with a discriminator so date-separator rows (which share `id = 0` by design) no longer collide into a single reused DOM node, while real messages still key by their unique `msg.id` so backlog timestamp re-stamps cannot force a re-mount of the entire list.

### v1.3.1

- **`/reload` now re-reads `~/.repartee/.env`** — previously `/reload` only picked up `config.toml` and the theme, so a freshly-added `SHRINK_API_KEY` (or rotated server `PASS` / SASL secret / `WEB_SESSION_SECRET`) stayed invisible until restart. The handler now mirrors the startup credential path. When the shrink API key transitions empty → populated, a themed message tells the user to restart explicitly, because the shrink workers were never spawned at startup and cannot be safely rebuilt from a command handler.
- **Irssi-style instant `/wc`** — closing a channel buffer no longer waits for the server-side PART echo. The buffer disappears immediately and the PART is fire-and-forget. The eventual server echo is a clean no-op (`remove_buffer` is now idempotent, so no duplicate `BufferClosed` event reaches the web frontend).
- **`/op`, `/deop`, `/voice`, `/devoice` no longer silently drop nicks past 3** — server `MAXMODES` caps each MODE line at 3 parameter modes, but the previous handler packed every arg into one line. The new chunker splits per the irssi convention — first line carries 2 nicks, every subsequent line up to 3 (`4 → [2,2]`, `5 → [2,3]`, `6 → [2,3,1]`, `7 → [2,3,2]`, …). All resulting MODE lines are queued back-to-back with no await between sends, so the server applies the full batch as a single burst.
- **`/kick` accepts up to 6 nicks plus a multi-word `:reason`** — `kick` left the greedy-command list so multiple nicks tokenise naturally. Reason is everything from the first `:`-prefixed token onward (leading `:` stripped, rest joined with spaces); no `:` means every argument is a nick. Hard cap of 6 nicks. Five nicks split as `[2, 3]`, six as `[2, 4]`; multi-target uses the comma-list form `KICK #chan a,b,c :reason`. Same back-to-back burst send as the mode commands.

### v1.3.0

- **URL shortening via `shr.al`** — long URLs are transparently shortened by an external API both **before they hit the wire** (outgoing) and **before they hit your screen** (incoming). Display: outgoing renders as `https://shr.al/abc`; incoming renders as `https://shr.al/abc [original-host.tld]` so you can see where the original link points. Independent `shrink.outgoing_enabled` / `shrink.incoming_enabled` toggles. Default threshold is `min_url_length = 50` (hard floor 25) and tunable via `/set shrink.min_url_length`.
- **`/shrink <url>` command** — explicitly shorten a URL without sending it; result is copied into the input line. `/help shrink` shows full usage.
- **Hardened against URL-display spoofing** — `host_of` strips `user:pass@` userinfo before extracting the displayed `[host]` hint, so a phishing URL like `https://trusted.com:x@evil.com/path` correctly renders as `[evil.com]` instead of `[trusted.com]`. Portless IPv6 literals (`[2001:db8::1]`) are preserved intact.
- **Quality-of-life shrink internals** — in-memory LRU cache (default 500 entries) avoids re-billing API calls for repeated URLs; 2-second timeout with silent fallback to the original URL on any failure (network, API, panic); per-message URLs are pre-extracted at dispatch time so a `/set shrink.min_url_length` change mid-flight cannot desync the gate from the worker; all per-message worker bodies are wrapped in `catch_unwind` so a panic in one shortening never bricks the feature for the rest of the session.
- **E2E REKEY fix for PMs** — REKEY NOTICEs produced by lazy session rotation in encrypted private messages are no longer silently dropped (regression that pre-dated this release: the buffer key was being reconstructed from the `@peer_handle` pseudochannel instead of the actual nick-keyed buffer).
- **Secrets stay in `.env`** — `SHRINK_API_KEY` is loaded exclusively from the `.env` file and never written to `config.toml`.

### v1.2.0

- **Runtime bind-IP override** — new `repartee -h <ip>` / `--bind <ip>` / `--bind=<ip>` CLI flag (modelled on `irssi -h`) lets multi-homed hosts pick the outgoing IRC source address for a session without editing config. New `general.default_bind_ip` config key (also settable via `/set`) provides a host-wide default. Precedence: per-server `bind_ip` → CLI `-h` → `general.default_bind_ip` → OS default. The CLI flag never persists.
- **No more duplicate web messages** — every line is now deduplicated by message id at both the live-event and backlog-fetch layers, and the `SyncInit` handler no longer triggers two competing `FetchMessages` round-trips per resync. Previously every reconnect or lag-recovery showed each chat line twice.
- **No more web chat-view jitter** — image previews now reserve a fixed 320×200 (desktop) / aspect-ratio (mobile) box before loading, so async thumbnail loads no longer reflow the bottom-anchored chat. The message list switched to keyed `<For>` rendering, so appending a single new line stops rebuilding the entire 1000-line DOM. Browser-native `content-visibility` keeps off-screen lines out of layout/paint.
- **Per-element web reactivity** — timestamp format, nick column width, nick truncation, nick colors, and dismissed-preview state are wrapped in fine-grained reactive closures, so settings changes and preview dismissals update only the affected DOM nodes instead of the whole message list.
- **Multi-tab opt-out** — set `web_follow_tui_buffer = false` in localStorage to make a browser tab stop following the TUI's active-buffer flips (default behaviour is preserved for single-tab + TUI workflows).
- **Resilient web reconnect** — a transient network blip on initial page load no longer kicks the user back to the login form; the client retries with exponential backoff for up to five attempts before giving up. Resize listener is now cleaned up on component unmount, eliminating a per-login closure leak.
- **Eager web-state snapshot** — `SyncInit` no longer reads up-to-1-second-old state when a new WS session connects between background ticks. Snapshot is now refreshed inline whenever a structural event (`BufferCreated`, `BufferClosed`, `ActiveBufferChanged`, `ConnectionStatus`, `SettingsChanged`) is broadcast.
- **Backend lock and asset cache cleanup** — replaced `std::sync::RwLock` with `parking_lot::RwLock` on the web-state snapshot (no risk of blocking a tokio worker on contention). Trunk-hashed static assets now ship with `Cache-Control: public, max-age=31536000, immutable`; unhashed paths get a 1-hour cache.

### v1.1.1

- **Refreshed bundled web UI** — `cargo install repartee@1.1.0` shipped the v1.0.x WASM bundle because `static/web/` in the published source predated the web-improvements work. This release re-bundles the actual v1.1.0 frontend (login form with username field, clickable links, image preview rendering) so `cargo install` users get the working UI without having to clone the repo and run `make wasm` themselves.

### v1.1.0

- **Web frontend improvements** — added persistent web sessions, a login form, clickable chat links, and server-generated image previews.
- **Image preview SSRF hardening** — `/api/preview` now blocks private, loopback, link-local, cloud metadata, and other non-public targets across direct URLs, redirects, and `og:image` follow-up fetches.

### v1.0.1

- **Log browser event rendering fix** — `repartee l` now renders persisted event text directly, so JOIN/PART/MODE/KICK/NICK/QUIT lines keep their original nick/channel/mode details instead of expanding empty event templates.
- **Fan-out event reference resolution** — secondary-channel QUIT/NICK log rows now resolve their `ref_id` primary row when reading history, preventing blank timestamp-only lines in multi-channel event history.

### v1.0.0

- **Read-only log browser** — `repartee l` / `repartee logs` opens the SQLite history directly, without connecting to IRC or starting scripts/web services. It supports network headers, day separators, paged scrollback loading, `/search`, and encrypted-log fallback behavior.
- **Paste-burst connection hardening** — updated to `irc-repartee` 1.5.1 so internal IRC PING/PONG traffic uses a priority lane and cannot be starved behind flood-throttled paste backlogs.
- **More tolerant local ping timeout** — repartee now sets the IRC client ping response timeout to 60 seconds instead of inheriting the upstream 20-second default.

### v0.9.4

- **Live ban-list tracking** — channel `+b/-b` mode changes now keep the local ban list current, and joined channels silently sync `MODE #channel b` so `/unban` numeric references and wildcard removals work without repeatedly refreshing `/ban`.
- **Themeable WHOIS replies** — WHOIS numerics now carry event keys and structured parameters for `[formats.events]`, with default and Spring theme entries for the common WHOIS lines.
- **Complete server-add configuration** — `/server add` now exposes the practical connection parameters in command parsing and help, including TLS verification, reconnect behavior, SASL, autosend commands, bind IP, encoding, password, and client certificate path.

### v0.9.3

- **IRCnet reop mode handling** — updated to `irc-proto-repartee` 1.2.2 so channel mode `+R/-R` is parsed as the reop list mode with masks, while registered-only remains lowercase `+r`.
- **Batched list modes** — `/reop`, `/except`, `/invex`, and their removal commands now batch multiple masks as `+RRR/-RRR`, `+eee/-eee`, and `+III/-III` according to the server `MODES` limit.
- **Mode display regression tests** — added coverage for `r/R` rendering and multi-mask list-mode display.

### v0.9.2

- **Memory bounds and long-session hardening** — capped oversized input history entries, bounded socket output buffering, capped storage writer pending rows, and limited IRCv3 batch retention to prevent unbounded memory growth under bursty or degraded conditions.
- **Web UI memory discipline** — capped client-side messages per buffer and reduced broad signal cloning in the chat, layout, and nick-list components.
- **Web build refresh** — regenerated bundled web frontend assets for the release.

### v0.9.1

- **Web session isolation and auth hardening** — web shell snapshots and active-buffer state are now isolated per session, WebSocket auth no longer sends bearer tokens in the URL, and session validation enforces client IP continuity.
- **Secret storage hardening** — E2E private material stored in SQLite is now encrypted at rest, with compatibility for existing databases during migration.
- **Local runtime hardening** — `~/.repartee` runtime files now use owner-only permissions where required, TLS/private env material is written securely, and local attach rejects peers from a different Unix UID.

### v0.9.0

- **RPE2E end-to-end encryption** — native E2E for repartee with per-peer trust, pending accept/decline flow, reciprocal key exchange, key export/import, revoke/reverify, and channel/query support.
- **Cross-client interoperability** — bundled companion scripts for WeeChat and Irssi/Eerssi now speak the same compact RPE2E v1 wire format, including long-message chunking, UTF-8/emoji handling, auto-handshake, and debug output directly in the current buffer.
- **Operational hardening** — multiple state-handling fixes landed across repartee and companion scripts: atomic E2E export/import where applicable, safer forget/reverify cleanup, improved multi-peer handshake tracking, and plaintext bypass for bot-style commands starting with `.` or `!`.

### v0.8.5

- **Critical: fix 3 GB OOM crash on long mouse-wheel scroll** — the chat view's render loop could walk the entire message buffer on every frame when `scroll_offset` exceeded available content. Under sustained wheel scrolling this produced 200–600 MB/s of allocation churn, fragmenting glibc's arena until the kernel (or `systemd-oomd`) killed the process at ~3 GB RSS. The loop is now capped at `buffer_len × 16` visual lines so it terminates in `O(buffer_len)` regardless of scroll position. Observed on Debian with v0.8.4 after a week of uptime; pre-existing bug, not a 0.8.4 regression, but v0.8.4 had enough baseline churn to make it reachable in normal use.
- **jemalloc on Linux** — `tikv-jemallocator` is now the global allocator on Linux builds via `#[cfg(target_os = "linux")]`. Defense-in-depth against glibc ptmalloc2 fragmentation in long-running sessions (weeks of uptime). macOS keeps `libsystem_malloc`, FreeBSD keeps its native jemalloc — builds on those platforms are byte-identical to v0.8.4, the dependency is not pulled into their build graph at all.
- **Regression tests** — six unit tests lock in the render-budget invariant so the OOM cannot regress silently; tests are organized under `compute_render_budget::...` for IDE filtering and carry diagnostic assert messages.

### v0.8.4

- **Web: sticky scroll** — auto-scroll now only scrolls to bottom when you're already there; scroll-up to read backlog stays put. Scroll-to-bottom button (▼) appears when scrolled up
- **Web: event_key parity** — web frontend now receives per-event-type keys (join, part, quit, kick, kicked, nick_change, topic_changed, mode, connected, disconnected, chghost, account) for themed icons and colors instead of fragile text heuristics
- **Web: notice rendering** — notices now render with `-nick- text` format and distinct cyan styling
- **Web: nick truncation** — accounts for mode prefix width (`@`, `+`) like the TUI does
- **Kick notification** — when kicked from a channel, the message now appears in the server status window, the channel buffer (before removal), and the landing buffer (where you end up). Themed as `kicked` event with red highlight
- **`/kick` and `/kb` accept `#channel`** — you can now specify a target channel: `/kick #otherchan nick reason`
- **Web: in-memory FetchMessages** — initial buffer loads serve from in-memory messages first, ensuring recent events (like kick notifications) are visible immediately even before the log writer flushes to SQLite
- **`event_key` persisted to DB** — backward-compatible migration adds `event_key TEXT` column so historical messages retain their event type for themed rendering
- **CI: WASM build step** — release workflow now builds the WASM frontend in a separate job, ensuring every release binary includes the latest web UI
- Removed dead `MessageType::Ctcp` variant

### v0.8.3

- Web buffer sync reliability fixes

---

## License

MIT — see [LICENSE](LICENSE).

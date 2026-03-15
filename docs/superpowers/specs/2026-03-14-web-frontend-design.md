# Web Frontend Design

**Date:** 2026-03-14
**Branch:** `feat/web-frontend`
**Status:** Approved

## Overview

A web frontend for Repartee that runs inside the same process as the IRC client. Terminal and web share a single `AppState` — read a message on web, it's marked read on terminal, and vice versa. The web server is an additional output alongside the existing ratatui terminal backend, not a replacement.

## Architecture

### Single-Process Model

```
repartee (single process, single PID)
├── Core
│   ├── AppState (shared)
│   ├── IRC connections (tokio TCP)
│   ├── Storage (SQLite)
│   ├── ScriptManager
│   ├── DCC / Spellcheck
│   ├── EventBus
│   └── MentionTracker (new)
├── Terminal Backend (unchanged)
│   ├── ratatui renderer
│   ├── crossterm events
│   └── Session/shim (detach/reattach)
└── Web Backend (new)
    ├── axum HTTPS server
    ├── WebSocket (WSS) upgrade
    ├── Auth + rate-limited sessions
    ├── Static file serving (Leptos WASM)
    └── (Future: cloudflared tunnel)
```

The web server starts when the process starts and runs independently of the terminal lifecycle. Whether the terminal is attached, detached, or reattached — the web server keeps running. The detach/reattach session system (`src/session/`) is completely unchanged.

### Event Flow

When an IRC event arrives:
1. Existing event handling processes it into `AppState`
2. Terminal redraws via ratatui (unchanged)
3. A lightweight JSON event is broadcast to all connected WebSocket clients

Client actions (`SendMessage`, `RunCommand`) reuse the same command dispatch logic as terminal keyboard input — no duplicate command handling. Each web session maintains its own `active_buffer_id` independently of the terminal's `AppState.active_buffer_id`. Commands from web clients execute in the context of that session's active buffer.

## WebSocket Protocol

### Connection Flow

1. **Login**: `POST /api/login` with password → rate limit check → secure session cookie
2. **WebSocket upgrade**: `GET /wss` with session cookie → validate → upgrade to WSS
3. **SyncInit**: Server sends buffer metadata only (no message bodies) for all buffers:
   - Buffer id (format: `"connection_id/buffer_name"`, lowercase), name, type, topic, unread count, activity level
   - Nick lists per channel buffer (nick, prefix, modes, away status)
   - Connection statuses
   - Unread mention count
4. **On-demand messages**: Client requests last 50 messages when opening a buffer
5. **Live event stream**: Continuous broadcast of state changes

All WebSocket connections use WSS (TLS) — no plaintext `ws://` fallback.

### Lazy Loading

On connect, only buffer metadata is sent. With ~100 buffers across 3 networks, this keeps the initial payload small. Messages are fetched on demand:

- Opening a buffer: `FetchMessages {buffer_id, limit: 50, before: null}`
- Scrolling up: `FetchMessages {buffer_id, limit: 50, before: oldest_loaded_timestamp}`
- Server queries SQLite log DB: `WHERE network = ? AND buffer = ? AND timestamp < ? ORDER BY timestamp DESC LIMIT 50`
- The `before` cursor is an integer timestamp (microseconds since epoch), matching the existing `get_messages()` pagination in `storage/query.rs`
- `buffer_id` (`"connection_id/buffer_name"`) is split into `(network, buffer)` for the query

Only the actively viewed buffer's history is loaded.

### Server → Client Events

```
SyncInit          — full buffer metadata on connect
NewMessage        — {buffer_id, message}
TopicChanged      — {buffer_id, topic, set_by}
NickEvent         — {buffer_id, kind, nick, ...}  (kinds: join, part, quit, nick_change, mode_change, away_change)
BufferCreated     — {buffer metadata}
BufferClosed      — {buffer_id}
ActivityChanged   — {buffer_id, level, unread}
ConnectionStatus  — {conn_id, status}
MentionAlert      — {buffer_id, message}
Messages          — response to FetchMessages
NickList          — response to FetchNickList (full nick list for a channel)
MentionsList      — response to FetchMentions
```

### Client → Server Events

```
SendMessage       — {buffer_id, text}
SwitchBuffer      — {buffer_id}  (session-local, does NOT change terminal's active buffer)
MarkRead          — {buffer_id, up_to: timestamp}  (precise: marks read up to this timestamp)
FetchMessages     — {buffer_id, limit, before: timestamp|null}
FetchNickList     — {buffer_id}  (request full nick list for a channel)
FetchMentions     — {}
RunCommand        — {buffer_id, text}  (buffer_id provides command context)
```

### Per-Session Active Buffer

Each WebSocket session tracks its own `active_buffer_id` independently. The terminal has its own via `AppState.active_buffer_id`. These do not interfere — switching buffers on web does not move the terminal cursor, and vice versa.

`SwitchBuffer` updates the session-local active buffer. `RunCommand` includes `buffer_id` so commands execute in the correct context regardless of the terminal's state.

### Read Sync

- Web client sends `MarkRead {buffer_id, up_to: timestamp}` when user views a buffer
- Server clears unread count for messages up to that timestamp, avoiding race conditions with new messages arriving concurrently
- Terminal sees the updated `buffer.unread_count` and `buffer.activity` on next redraw
- Reverse: switching buffers in terminal broadcasts `ActivityChanged` to web clients

### Reconnection

- WebSocket heartbeat via ping/pong frames (30s interval, 10s timeout)
- On disconnect, client auto-reconnects with exponential backoff (1s, 2s, 4s, max 30s)
- Re-auth via existing session cookie (if not expired) or fresh login
- On reconnect, server sends fresh `SyncInit` — client reconciles with local state
- Messages received while disconnected are available via `FetchMessages` (they're in SQLite)

## Desktop UI Layout

Three-column layout matching the terminal:

```
┌─────────────────────────────────────────────────┐
│ #rust — Welcome to #rust                        │  topic bar (bg_alt)
├────────┬───────────────────────────┬────────────┤
│ Libera │ 14:23  @ferris❯ Has any… │ @ferris    │
│ 1.#rust│ 14:24   alice❯ Yeah, it… │ @rustbot   │
│ 2.#lin…│ 14:25  +bob❯ The RPITIT… │ +bob       │
│        │ 14:25  * ferris nods…     │  alice     │
│ OFTC   │ 14:26  @ferris❯ Agreed…  │  charlie   │
│ 5.#deb…│ 14:27   charlie❯ What…   │  kofany    │
│ 6.#sway│ 14:29  +bob❯ kofany: …   │            │
│        │ 14:30   dave❯ URGENT: …  │            │
│ IRCnet │ 14:31   charlie❯ That's… │            │
│ 7.#pol…│ 14:32   kofany❯ thanks!… │            │
├────────┴───────────────────────────┴────────────┤
│ [14:36|kofany(+ix)|#rust(+nt)|Lag: 0.2s|Act: …]│  status line
│ ❯ |                                             │  input
└─────────────────────────────────────────────────┘
```

### Chat Area — Right-Aligned Nick Column

CSS grid with 3 columns: `[timestamp 42px] [nick 100px right-aligned] [text flex]`

- Nick column is right-aligned — short nicks pad from left, long nicks fill to right edge
- The `❯` separator sits at the right edge of the nick column
- Text always starts at the exact same column position
- Events (join/part/quit), actions (`* nick`), notices (`-nick-`) span the full text area — no nick column
- Own messages: green nick (`#9ece6a`), brighter text (`#c0caf5`)

### Event Arrows (UTF-8)

- Join: `→` (green `#9ece6a`)
- Part: `←` (yellow `#e0af68`)
- Quit: `←` (red `#f7768e`)
- Nick change: `↔` (purple `#bb9af7`)

### Status Bar

irssi-style: `[time|nick(+modes)|#channel(+modes)|Lag: N.Ns|Act: 3,4,7]`

Activity numbers colored by level (web-specific enhancement over terminal, which groups some levels):
- green = Events
- yellow = Activity
- red = Highlight
- purple = Mention

### Line Density

- `line-height: 1.35` default (configurable via `/set web.line_height`)
- Zero vertical padding on chat rows
- Monospace font stack: JetBrains Mono → Fira Code → SF Mono → Cascadia Code → system monospace
- Full emoji/UTF-8 rendering via browser

### Timestamp Format

Configurable via `/set web.timestamp_format` — same format string as terminal's `general.timestamp_format`. Users can choose with or without seconds.

## Mobile UI Layout

### Default View — Full-Width Chat

```
┌──────────────────────────┐
│ ☰  #rust (+nt) — Welc… 2 👥│  top bar (channel + topic snippet + mentions badge)
├──────────────────────────┤
│ 14:23 @ferris❯ Has any…  │
│ 14:24 alice❯ Yeah, it's… │
│ 14:25 +bob❯ The RPITIT…  │  inline nicks (no column grid)
│ 14:25 * ferris nods 👍    │
│ …~20 lines visible…      │
├──────────────────────────┤
│ [kofany|Act: 3,4,7]      │  compact status
│ [Message...          ] ⏎  │  input
└──────────────────────────┘
```

- Nicks inline (no right-aligned column — not enough horizontal space)
- Topic merged into top bar as single line
- Compact status bar (nick + activity only)

### Slide-Out Buffer List (Left)

Swipe right from left edge OR tap `☰`:
- Slides out over chat with dimmed overlay
- 220px wide, shows all buffers grouped by network
- Activity colors match desktop
- Active buffer highlighted with blue left border
- Mentions badge at top
- Settings and Theme buttons at bottom
- Tap buffer to switch + auto-close panel

### Slide-Out Nick List (Right)

Swipe left from right edge OR tap `👥`:
- Slides out from right with dimmed overlay
- 180px wide, shows nicks grouped by mode (Ops/Voiced/Users) with counts
- Away users dimmed with `(away)` label
- Tap overlay to close

### Gestures

- Swipe right (from left edge) → open buffer list
- Swipe left (from right edge) → open nick list
- Tap overlay → close any open panel
- Scroll up → lazy-load older messages from DB
- Pull down (at top) → load more history

## `/mentions` System

### Storage

New `mentions` table in SQLite:
- `id` — primary key
- `timestamp` — when the mention occurred
- `buffer_id` — which buffer
- `channel` — channel name
- `nick` — who mentioned us
- `text` — message text
- `read_at` — NULL until read, set when `/mentions` is used

Messages with `highlight: true` are auto-inserted on arrival.

### Commands

- `/mentions` — show unread mentions as a list (timestamp, #channel, nick, text), marks all as read
- Web: mentions badge count in top bar (purple pill), tap opens mentions view, viewing clears them
- Bidirectional: persisted to DB, synced between terminal and web

### Persistence

Mentions persist across restarts with a `last_checked_at` timestamp. If you've been away for hours, `/mentions` shows everything you missed.

## Themes

### 5 Built-in Web Themes

| Theme | Type | Inspiration |
|-------|------|-------------|
| **Nightfall** | Dark (default) | Repartee terminal theme — exact color match |
| **Catppuccin Mocha** | Dark | Catppuccin Mocha palette |
| **Tokyo Storm** | Dark | Tokyo Night Storm variant |
| **Gruvbox Light** | Light | Gruvbox Light palette |
| **Catppuccin Latte** | Light | Catppuccin Latte palette |

### Implementation

Web themes are CSS custom properties mapping the same color slots as terminal themes:
- `--bg`, `--bg-alt`, `--border`, `--fg`, `--fg-muted`, `--fg-dim`, `--accent`, `--cursor`
- Plus semantic colors for activity levels, nick modes, events

Theme switcher button accessible from settings. Configurable via `/set web.theme nightfall`.

Terminal `.theme` files and web themes are independent but use the same color semantics.

## Authentication & Security

### Login

- Single password set via `/set web.password` (stored in `.env`)
- `POST /api/login` with password → secure session cookie
- Rate limiting: exponential backoff after failed attempts (prevents brute force)
- Secure session management with expiry

### TLS

- axum serves HTTPS via `rustls`
- Self-signed certificate auto-generated on first run (stored in `~/.repartee/certs/`)
- Users can provide their own cert via `/set web.tls_cert` and `/set web.tls_key`
- All connections are TLS — no plaintext HTTP/WS fallback

### Future: Cloudflared

Config namespace reserved:
- `/set web.cloudflare_token` — API token from Cloudflare dashboard
- `/set web.cloudflare_tunnel_name` — tunnel name

Cloudflared handles TLS termination for remote access. Local TLS remains for direct LAN access.

## Configuration

New `/set web.*` settings. Non-secret settings live in `config.toml` (following existing pattern for `[dcc]`, `[spellcheck]`). Only `password` and `cloudflare_token` are stored in `.env`.

```toml
# config.toml
[web]
enabled = true
bind_address = "127.0.0.1"    # default localhost, set 0.0.0.0 for LAN
port = 8443
tls_cert = ""                  # auto-generated self-signed if empty
tls_key = ""
timestamp_format = "%H:%M"
line_height = 1.35
theme = "nightfall"

# Future
cloudflare_tunnel_name = ""
```

```env
# .env (secrets only)
WEB_PASSWORD=
CLOUDFLARE_TOKEN=
```

## Tech Stack

| Layer | Technology | Notes |
|-------|-----------|-------|
| Web server | axum | tokio-native, first-class WS support |
| TLS | rustls + rcgen | rcgen for self-signed cert generation |
| WebSocket | axum WS | WSS only |
| Frontend | Leptos (CSR → WASM) | Single-language Rust stack |
| Auth | password + secure cookie | Rate-limited |
| State sync | JSON over WSS | Event-driven |
| Message history | SQLite queries | Existing storage DB |
| Mentions | New SQLite table | Persisted across restarts |
| Themes | CSS custom properties | 5 built-in themes |

### New Cargo Dependencies

- `axum` — HTTP/WS server
- `axum-extra` — cookie/session support
- `tower` — middleware (rate limiting)
- `rcgen` — self-signed TLS cert generation
- `leptos` — WASM frontend framework

All other deps (tokio, serde_json, rusqlite, rustls) already in the project.

### Build Pipeline

The Leptos WASM frontend is a separate build target (`wasm32-unknown-unknown`), compiled via `trunk` or `cargo-leptos`. The resulting WASM binary + JS glue + HTML are embedded into the main binary at compile time using `rust-embed` (or `include_dir`), so the release artifact remains a single binary with no external static files. The build process:

1. `trunk build --release` → produces `dist/` with WASM + JS + HTML
2. Main binary build embeds `dist/` contents via `rust-embed`
3. axum serves embedded assets at `/` routes

### Encrypted Storage Compatibility

The web frontend's `FetchMessages` queries go through the existing `storage/query.rs` which handles AES-256-GCM decryption transparently. Since the web server runs in the same process, it has access to the encryption key. No special handling needed.

## What Stays Unchanged

- Terminal backend (ratatui/crossterm) — zero changes
- Session/shim system (detach/reattach) — zero changes
- IRC connection handling — zero changes
- Storage system — only addition is `mentions` table
- Scripting engine — zero changes
- Command dispatch — reused by web `RunCommand`

## Responsive Breakpoint

- **Desktop** (≥768px): Three-column layout with nick column grid
- **Mobile** (<768px): Full-width chat, inline nicks, slide-out panels

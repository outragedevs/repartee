# Web Frontend Phase 2 — Full UI + WebSocket Handler

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the web frontend with real-time WebSocket communication, full desktop/mobile Leptos UI, 5 themes, and bidirectional state sync.

**Architecture:** Rust backend (`src/web/ws.rs`) handles WebSocket sessions with per-session active buffer. IRC events broadcast to all web clients. Leptos WASM frontend (`web-ui/`) connects via WSS, renders desktop (3-column) and mobile (slide-out panels) layouts. CSS custom properties power 5 themes.

**Tech Stack:** axum WebSocket, tokio broadcast, Leptos 0.7 CSR, gloo-net WebSocket, CSS custom properties

**Spec:** `docs/superpowers/specs/2026-03-14-web-frontend-design.md`
**Phase 1:** `docs/superpowers/plans/2026-03-14-web-frontend-plan.md` (complete)

---

## File Structure

### New files (Rust backend)

| File | Responsibility |
|------|---------------|
| `src/web/ws.rs` | WebSocket upgrade handler, per-session loop, SyncInit, command dispatch |
| `src/web/snapshot.rs` | Build `SyncInit` payload from `AppState`, convert state types to wire types |

### New files (Leptos frontend)

| File | Responsibility |
|------|---------------|
| `web-ui/src/ws.rs` | WebSocket client — connect, send/receive JSON, auto-reconnect |
| `web-ui/src/state.rs` | Client-side state store — signals for buffers, messages, nicks, mentions |
| `web-ui/src/theme.rs` | CSS theme definitions, theme switcher logic |
| `web-ui/src/components/layout.rs` | Root layout — desktop vs mobile detection, responsive switching |
| `web-ui/src/components/topic_bar.rs` | Topic bar component |
| `web-ui/src/components/buffer_list.rs` | Buffer list sidebar (desktop) + slide-out panel (mobile) |
| `web-ui/src/components/chat_view.rs` | Chat message area — right-aligned nick grid, lazy scroll |
| `web-ui/src/components/nick_list.rs` | Nick list sidebar (desktop) + slide-out panel (mobile) |
| `web-ui/src/components/status_line.rs` | irssi-style status bar |
| `web-ui/src/components/input.rs` | Message input field |
| `web-ui/src/components/login.rs` | Login form |
| `web-ui/src/components/mentions.rs` | Mentions badge + list view |
| `web-ui/src/components/mod.rs` | Component module root |
| `web-ui/styles/themes.css` | 5 theme definitions as CSS custom properties |
| `web-ui/styles/base.css` | Layout, typography, chat grid, animations |

### Modified files

| File | Change |
|------|--------|
| `src/web/mod.rs` | Add `pub mod ws; pub mod snapshot;` |
| `src/web/server.rs` | Add `/ws` route, pass `AppHandle` state snapshot + storage |
| `src/app.rs` | Broadcast WebEvents from `handle_irc_event`, implement `handle_web_command`, auto-record mentions |
| `web-ui/src/main.rs` | Wire new modules |
| `web-ui/src/app.rs` | Replace placeholder with real layout |
| `web-ui/Cargo.toml` | Add `chrono` for timestamp formatting |
| `web-ui/index.html` | Link CSS files |

---

## Chunk 1: WebSocket Handler + Event Broadcasting

### Task 1: State snapshot builder

**Files:**
- Create: `src/web/snapshot.rs`
- Modify: `src/web/mod.rs`

Build `SyncInit` payload from live `AppState`. This converts internal types to wire types.

- [ ] **Step 1: Create `src/web/snapshot.rs`**

```rust
use crate::state::AppState;
use crate::state::buffer::BufferType;
use crate::state::connection::ConnectionStatus;
use crate::web::protocol::*;

/// Build a SyncInit event from the current AppState.
pub fn build_sync_init(state: &AppState, mention_count: u32) -> WebEvent {
    let buffers: Vec<BufferMeta> = state.buffers.values()
        .map(|b| BufferMeta {
            id: b.id.clone(),
            connection_id: b.connection_id.clone(),
            name: b.name.clone(),
            buffer_type: match b.buffer_type {
                BufferType::Server => "server",
                BufferType::Channel => "channel",
                BufferType::Query => "query",
                BufferType::DccChat => "dcc_chat",
                BufferType::Special => "special",
            }.to_string(),
            topic: b.topic.clone(),
            unread_count: b.unread_count,
            activity: b.activity as u8,
            nick_count: u32::try_from(b.users.len()).unwrap_or(u32::MAX),
        })
        .collect();

    let connections: Vec<ConnectionMeta> = state.connections.values()
        .map(|c| ConnectionMeta {
            id: c.id.clone(),
            label: c.label.clone(),
            nick: c.nick.clone(),
            connected: c.status == ConnectionStatus::Connected,
        })
        .collect();

    WebEvent::SyncInit { buffers, connections, mention_count }
}

/// Build a NickList event for a specific buffer.
pub fn build_nick_list(state: &AppState, buffer_id: &str) -> Option<WebEvent> {
    let buf = state.buffers.get(buffer_id)?;
    let nicks: Vec<WireNick> = buf.users.values()
        .map(|n| WireNick {
            nick: n.nick.clone(),
            prefix: n.prefix.clone(),
            modes: n.modes.clone(),
            away: n.away,
        })
        .collect();
    Some(WebEvent::NickList { buffer_id: buffer_id.to_string(), nicks })
}

/// Convert a state Message to a WireMessage.
pub fn message_to_wire(msg: &crate::state::buffer::Message) -> WireMessage {
    WireMessage {
        id: msg.id,
        timestamp: msg.timestamp.timestamp(),
        msg_type: msg.message_type.as_str().to_string(),
        nick: msg.nick.clone(),
        nick_mode: msg.nick_mode.clone(),
        text: msg.text.clone(),
        highlight: msg.highlight,
    }
}
```

- [ ] **Step 2: Write tests**

```rust
#[cfg(test)]
mod tests {
    // test build_sync_init with empty state
    // test build_nick_list returns None for unknown buffer
    // test message_to_wire roundtrip
}
```

- [ ] **Step 3: Run tests, commit**

---

### Task 2: WebSocket handler

**Files:**
- Create: `src/web/ws.rs`
- Modify: `src/web/server.rs` (add `/ws` route)
- Modify: `src/web/server.rs` (`AppHandle` add `state` + `storage` fields)

- [ ] **Step 1: Add `state` and `storage` to `AppHandle`**

In `src/web/server.rs`, add:
```rust
pub state: Arc<std::sync::RwLock<crate::state::AppState>>,
pub storage: Option<Arc<crate::storage::Storage>>,
```

Update `App::run()` in `src/app.rs` to pass these when creating the handle.

Note: `AppState` currently lives directly on `App` (`self.state`). To share it with the web server, wrap it in `Arc<RwLock<AppState>>`. This is a significant refactor — all `self.state.xxx` becomes `self.state.read().xxx` or `self.state.write().xxx`. **Alternative**: use a periodic snapshot approach — copy state to a shared `Arc<RwLock<StateSnapshot>>` on each tick. This avoids touching every `self.state` call site.

**Chosen approach**: Periodic snapshot. Add `web_state_snapshot: Arc<RwLock<WebStateSnapshot>>` to `App`. Update it in the 1s tick. The WebSocket handler reads from this snapshot for `SyncInit`/`FetchNickList`. For `FetchMessages`, it accesses `Storage.db` directly (already `Arc<Mutex<Connection>>`).

```rust
/// Lightweight read-only snapshot of AppState for web handlers.
pub struct WebStateSnapshot {
    pub buffers: Vec<BufferMeta>,
    pub connections: Vec<ConnectionMeta>,
    pub nick_lists: HashMap<String, Vec<WireNick>>,
    pub mention_count: u32,
}
```

- [ ] **Step 2: Create `src/web/ws.rs`**

WebSocket upgrade handler:
```rust
pub async fn ws_handler(
    ws: axum::extract::WebSocketUpgrade,
    State(state): State<Arc<AppHandle>>,
    // Validate session token from query param
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}
```

`handle_socket` per-session loop:
- Subscribe to `broadcaster`
- Send `SyncInit` from snapshot
- `tokio::select!` loop:
  - `broadcast_rx.recv()` → forward as JSON to client
  - `socket.recv()` → parse `WebCommand`, send through `web_cmd_tx`
  - Ping interval (30s) → send ping, track pong
  - Pong timeout (10s) → disconnect

- [ ] **Step 3: Add `/ws` route to server.rs**

```rust
.route("/ws", get(ws::ws_handler))
```

- [ ] **Step 4: Write test for SyncInit delivery**

- [ ] **Step 5: Commit**

---

### Task 3: Event broadcasting from IRC handler

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: Add `broadcast_web_event()` helper on `App`**

```rust
fn broadcast_web(&self, event: WebEvent) {
    let _ = self.web_broadcaster.send(event);
}
```

- [ ] **Step 2: Add broadcast calls in `handle_irc_event`**

After each state mutation, broadcast the corresponding `WebEvent`:

- `IrcEvent::Message` → `WebEvent::NewMessage` + `WebEvent::ActivityChanged`
- `IrcEvent::TopicChanged` → `WebEvent::TopicChanged`
- `IrcEvent::Join` → `WebEvent::NickEvent { kind: Join }`
- `IrcEvent::Part` → `WebEvent::NickEvent { kind: Part }`
- `IrcEvent::Quit` → `WebEvent::NickEvent { kind: Quit }` (per affected buffer)
- `IrcEvent::NickChanged` → `WebEvent::NickEvent { kind: NickChange }`
- `IrcEvent::ModeChanged` → `WebEvent::NickEvent { kind: ModeChange }`
- `IrcEvent::Connected` → `WebEvent::ConnectionStatus`
- `IrcEvent::Disconnected` → `WebEvent::ConnectionStatus`

- [ ] **Step 3: Auto-record mentions on highlight messages**

In `add_message()` or where `highlight: true` is set, insert into mentions table:
```rust
if msg.highlight {
    if let Some(ref storage) = self.storage {
        if let Ok(db) = storage.db.lock() {
            let (network, buffer) = split_buffer_id(&buf_id);
            let _ = crate::storage::query::insert_mention(
                &db, msg.timestamp.timestamp(), &network, &buffer,
                &msg.nick.clone().unwrap_or_default(), &buf.name, &msg.text,
            );
        }
    }
    self.broadcast_web(WebEvent::MentionAlert { ... });
}
```

- [ ] **Step 4: Update `web_state_snapshot` in tick**

In the 1s tick, rebuild the snapshot from current `AppState`.

- [ ] **Step 5: Commit**

---

### Task 4: Implement `handle_web_command` dispatch

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: Replace the TODO dispatch with real implementation**

```rust
fn handle_web_command(&mut self, cmd: WebCommand, session_id: &str) {
    match cmd {
        WebCommand::SendMessage { buffer_id, text } => {
            // Parse buffer_id to get connection, send via IRC
            self.web_send_message(&buffer_id, &text);
        }
        WebCommand::SwitchBuffer { buffer_id } => {
            // Session-local — no state change needed server-side.
            // Client handles this locally.
        }
        WebCommand::MarkRead { buffer_id, up_to } => {
            self.web_mark_read(&buffer_id, up_to);
        }
        WebCommand::FetchMessages { buffer_id, limit, before } => {
            // Query SQLite, send Messages response via broadcaster (targeted)
            self.web_fetch_messages(&buffer_id, limit, before, session_id);
        }
        WebCommand::FetchNickList { buffer_id } => {
            // Read from AppState, send NickList response
        }
        WebCommand::FetchMentions => {
            // Query SQLite, send MentionsList response
        }
        WebCommand::RunCommand { buffer_id, text } => {
            // Dispatch through command system with buffer context
            self.web_run_command(&buffer_id, &text);
        }
    }
}
```

- [ ] **Step 2: Implement `web_send_message`**

Split `buffer_id` into `(connection_id, buffer_name)`, find the IRC handle, send PRIVMSG.

- [ ] **Step 3: Implement `web_mark_read`**

Update `buffer.unread_count` and `buffer.activity` in `AppState`. Broadcast `ActivityChanged`.

- [ ] **Step 4: Implement `web_fetch_messages`**

Query `storage::query::get_messages()` with the `before` cursor. Convert to `Vec<WireMessage>`. Send `WebEvent::Messages` response. Note: responses to specific sessions need a targeted send mechanism — either a per-session `mpsc` channel or tag the `WebEvent` with a session ID.

- [ ] **Step 5: Implement `web_run_command`**

Temporarily set `active_buffer_id` to the web session's buffer, call `commands::dispatch()`, restore. Or extract the command dispatch to accept a buffer_id parameter.

- [ ] **Step 6: Commit**

---

## Chunk 2: Leptos Frontend — State + WebSocket Client

### Task 5: WebSocket client module

**Files:**
- Create: `web-ui/src/ws.rs`

- [ ] **Step 1: Create WebSocket client**

```rust
use gloo_net::websocket::{Message, futures::WebSocket};
use futures::{SinkExt, StreamExt};

pub fn connect(token: &str) -> WebSocket {
    let location = web_sys::window().unwrap().location();
    let host = location.host().unwrap();
    let url = format!("wss://{host}/ws?token={token}");
    WebSocket::open(&url).unwrap()
}
```

- [ ] **Step 2: Message parsing and dispatch**

Parse incoming JSON into `WebEvent` variants, dispatch to state store signals.

- [ ] **Step 3: Auto-reconnect logic**

On disconnect, exponential backoff (1s, 2s, 4s, max 30s). Re-send auth token.

- [ ] **Step 4: Commit**

---

### Task 6: Client-side state store

**Files:**
- Create: `web-ui/src/state.rs`

- [ ] **Step 1: Define state signals**

```rust
use leptos::prelude::*;
use std::collections::HashMap;

pub struct AppState {
    pub connected: RwSignal<bool>,
    pub buffers: RwSignal<Vec<BufferMeta>>,
    pub connections: RwSignal<Vec<ConnectionMeta>>,
    pub active_buffer: RwSignal<Option<String>>,
    pub messages: RwSignal<HashMap<String, Vec<WireMessage>>>,
    pub nick_lists: RwSignal<HashMap<String, Vec<WireNick>>>,
    pub mention_count: RwSignal<u32>,
    pub token: RwSignal<Option<String>>,
    pub theme: RwSignal<String>,
}
```

- [ ] **Step 2: Event handlers that update signals from WebSocket messages**

`handle_sync_init`, `handle_new_message`, `handle_activity_changed`, etc.

- [ ] **Step 3: Provide state via Leptos context**

Use `provide_context(state)` at the app root so all components can access it.

- [ ] **Step 4: Commit**

---

### Task 7: Login component

**Files:**
- Create: `web-ui/src/components/login.rs`

- [ ] **Step 1: Create login form**

Password input + submit button. Calls `POST /api/login`, stores token in state, triggers WebSocket connect.

- [ ] **Step 2: Error display for wrong password / rate limited**

- [ ] **Step 3: Commit**

---

## Chunk 3: Leptos Frontend — Desktop Layout

### Task 8: CSS themes and base styles

**Files:**
- Create: `web-ui/styles/themes.css`
- Create: `web-ui/styles/base.css`
- Modify: `web-ui/index.html`

- [ ] **Step 1: Create `themes.css` with 5 theme definitions**

CSS custom properties for each theme:
```css
[data-theme="nightfall"] {
    --bg: #1a1b26; --bg-alt: #16161e; --border: #292e42;
    --fg: #a9b1d6; --fg-muted: #565f89; --fg-dim: #292e42;
    --accent: #7aa2f7; --cursor: #7aa2f7;
    --green: #9ece6a; --yellow: #e0af68; --red: #f7768e;
    --purple: #bb9af7; --cyan: #7dcfff; --bright: #c0caf5;
}
[data-theme="catppuccin-mocha"] { ... }
[data-theme="tokyo-storm"] { ... }
[data-theme="gruvbox-light"] { ... }
[data-theme="catppuccin-latte"] { ... }
```

- [ ] **Step 2: Create `base.css` with layout and typography**

Grid layouts, monospace font stack, line-height, chat message grid (`42px | 100px | flex`), status bar, input styling, slide-out panel animations.

- [ ] **Step 3: Link CSS in `index.html`**

```html
<link data-trunk rel="css" href="styles/themes.css" />
<link data-trunk rel="css" href="styles/base.css" />
```

- [ ] **Step 4: Commit**

---

### Task 9: Desktop layout components

**Files:**
- Create: all `web-ui/src/components/*.rs`
- Modify: `web-ui/src/app.rs`

- [ ] **Step 1: Create `components/mod.rs`**

Module declarations for all components.

- [ ] **Step 2: Create `components/layout.rs`**

Root layout — detects viewport width, renders desktop (≥768px) or mobile (<768px). Desktop: 3-column grid (buffer_list | chat | nick_list). Topic bar on top, status+input on bottom.

- [ ] **Step 3: Create `components/topic_bar.rs`**

Channel name (bold accent) + separator + topic text. On mobile: merged into top bar.

- [ ] **Step 4: Create `components/buffer_list.rs`**

Network headers (bold accent), numbered buffer entries with activity colors. Active buffer highlighted. Click to switch. On mobile: slide-out panel from left.

- [ ] **Step 5: Create `components/chat_view.rs`**

CSS grid: `[timestamp 42px] [nick 100px right-aligned] [text flex]`
- Regular messages: timestamp, mode+nick❯, text
- Events: timestamp, full-width (→ join, ← part/quit, ↔ nick)
- Actions: `* nick action text`
- Notices: `-nick- text`
- Mentions: purple background tint
- Highlights: red background tint
- Own messages: green nick, bright text
- Scroll up: trigger `FetchMessages` with cursor

- [ ] **Step 6: Create `components/nick_list.rs`**

Grouped by mode (ops/voiced/users) with headers. Away users dimmed. On mobile: slide-out from right.

- [ ] **Step 7: Create `components/status_line.rs`**

`[time|nick(+modes)|#channel(+modes)|Lag|Act: 3,4,7]` with colored activity numbers.

- [ ] **Step 8: Create `components/input.rs`**

Text input + send button. Enter/button sends `SendMessage`. `/` prefix sends `RunCommand`. On mobile: rounded input with ⏎ button.

- [ ] **Step 9: Create `components/mentions.rs`**

Badge showing unread count. Click opens mentions list overlay. List shows timestamp, channel, nick, text. Viewing sends `FetchMentions` and marks read.

- [ ] **Step 10: Wire everything in `app.rs`**

Replace placeholder with:
```rust
view! {
    <Show when=move || state.token.get().is_some() fallback=Login>
        <Layout />
    </Show>
}
```

- [ ] **Step 11: Commit**

---

## Chunk 4: Mobile Layout + Polish

### Task 10: Mobile slide-out panels

**Files:**
- Modify: `web-ui/src/components/layout.rs`
- Modify: `web-ui/src/components/buffer_list.rs`
- Modify: `web-ui/src/components/nick_list.rs`
- Modify: `web-ui/styles/base.css`

- [ ] **Step 1: Add mobile detection signal**

```rust
let is_mobile = Signal::derive(move || {
    window().inner_width().unwrap().as_f64().unwrap_or(1024.0) < 768.0
});
```

- [ ] **Step 2: Implement slide-out buffer list**

- ☰ hamburger button in top bar
- Swipe right from left edge (touch events)
- Dimmed overlay backdrop
- 220px panel, animated slide
- Tap buffer → switch + auto-close

- [ ] **Step 3: Implement slide-out nick list**

- 👥 button in top bar
- Swipe left from right edge
- 180px panel, grouped by mode with headers

- [ ] **Step 4: Mobile-specific layout adjustments**

- Inline nicks (no column grid)
- Compact status bar (nick + activity only)
- Topic merged into top bar
- Touch-sized input with send button

- [ ] **Step 5: Commit**

---

### Task 11: Theme switcher

**Files:**
- Modify: `web-ui/src/state.rs`
- Create: `web-ui/src/components/theme_picker.rs`

- [ ] **Step 1: Theme switcher component**

Button in buffer list panel footer. Opens picker showing 5 themes with color swatches. Click applies `data-theme` attribute to `<html>` element. Saves preference to `localStorage`.

- [ ] **Step 2: Load theme from localStorage on startup**

- [ ] **Step 3: Commit**

---

### Task 12: Build, embed, and test

**Files:**
- Modify: trunk build output embedded in binary

- [ ] **Step 1: Build WASM frontend**

```bash
cd web-ui && trunk build --release
```

- [ ] **Step 2: Rebuild main binary**

```bash
cargo build --release -p repartee
```

The `rust-embed` macro picks up the new `web-ui/dist/` contents automatically.

- [ ] **Step 3: Manual integration test**

1. Set `WEB_PASSWORD=test123` in `~/.repartee/.env`
2. Set `[web] enabled = true` in config.toml
3. Start repartee
4. Open `https://localhost:8443` in browser
5. Accept self-signed cert warning
6. Login with password
7. Verify: buffer list shows, chat area renders messages
8. Send a message from web → appears in terminal
9. Send a message from terminal → appears in web
10. Switch buffers, verify read sync
11. Test mobile layout (resize browser to <768px)
12. Test slide-out panels
13. Switch themes

- [ ] **Step 4: Run full test suite**

```bash
cargo test -p repartee
```

- [ ] **Step 5: Commit all**

---

## Dependencies

```
Task 1 (snapshot) ──→ Task 2 (ws handler) ──→ Task 3 (broadcasting) ──→ Task 4 (cmd dispatch)
                                                                              │
Task 5 (ws client) ──→ Task 6 (state store) ──→ Task 7 (login) ─────────────┤
                                                                              │
Task 8 (CSS themes) ──→ Task 9 (desktop components) ──→ Task 10 (mobile) ───┤
                                                                              │
                                                         Task 11 (theme sw) ─┤
                                                                              ↓
                                                         Task 12 (build+test)
```

**Parallelizable**: Tasks 1-4 (backend) and Tasks 5-8 (frontend foundation) can run in parallel since they're in separate crates.

## Notes for Implementer

- **Do NOT modify terminal rendering code** — ratatui stays untouched
- **Periodic snapshot, not shared `Arc<RwLock<AppState>>`** — avoids touching every `self.state` call site
- **Per-session responses** (FetchMessages, NickList, MentionsList): tag `WebEvent` with optional `session_id`. The ws handler filters: if `session_id` is `Some` and doesn't match, skip forwarding.
- **CSS custom properties** for theming — one `data-theme` attribute on `<html>`, all colors derived. No Rust code changes needed to add themes.
- **Monospace font stack**: `'JetBrains Mono', 'Fira Code', 'SF Mono', 'Cascadia Code', monospace`
- **Line-height default 1.35**, configurable via `/set web.line_height`
- **Timestamp format configurable** via `/set web.timestamp_format`
- All `WebEvent` variants already defined in `src/web/protocol.rs` — no new types needed

# Web Frontend Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an embedded web frontend to Repartee that shares state 1:1 with the terminal client over WSS.

**Architecture:** axum HTTPS server runs inside the same process, broadcasting IRC events as JSON over WebSocket to Leptos WASM clients. Each web session has its own active buffer. Messages lazy-loaded from SQLite.

**Tech Stack:** axum, axum-extra (cookies), tower (rate limit), rcgen (TLS cert gen), rustls-pemfile, leptos (WASM), rust-embed, serde_json

**Spec:** `docs/superpowers/specs/2026-03-14-web-frontend-design.md`

---

## File Structure

### New files

| File | Responsibility |
|------|---------------|
| `src/web/mod.rs` | Module root, `WebServer` struct, startup/shutdown |
| `src/web/server.rs` | axum router setup, TLS config, static file serving |
| `src/web/auth.rs` | Login endpoint, session cookies, rate limiting |
| `src/web/ws.rs` | WebSocket upgrade, per-session state, message dispatch |
| `src/web/protocol.rs` | `WebEvent` (server→client) and `WebCommand` (client→server) enums, JSON serialization |
| `src/web/broadcast.rs` | `WebBroadcaster` — fan-out channel for web events |
| `src/web/tls.rs` | Self-signed cert generation via rcgen, cert loading |
| `src/web/mentions.rs` | `MentionTracker` — insert/query/mark-read in SQLite |
| `web-ui/` | Leptos frontend crate (separate `Cargo.toml`, `wasm32-unknown-unknown` target) |

### Modified files

| File | Change |
|------|--------|
| `Cargo.toml` | Add axum, axum-extra, tower, rcgen, rustls-pemfile deps; add `web-ui` workspace member |
| `src/main.rs` | Add `mod web;` |
| `src/app.rs` | Add `web_broadcast_tx` field, broadcast events in IRC handler, add web event select arm |
| `src/config/mod.rs` | Add `WebConfig` struct, `[web]` section |
| `src/config/defaults.rs` | Default values for `WebConfig` |
| `src/config/env.rs` | Load `WEB_PASSWORD` from `.env` |
| `src/commands/settings.rs` | Register `web.*` setting paths |
| `src/commands/registry.rs` | Register `/mentions` command |
| `src/commands/handlers_admin.rs` | Implement `cmd_mentions` |
| `src/storage/db.rs` | Create `mentions` table |
| `src/storage/query.rs` | Add `get_mentions()`, `insert_mention()`, `mark_mentions_read()` |
| `src/constants.rs` | Add `certs_dir()` helper |

---

## Chunk 1: WebSocket Protocol Types + Config

### Task 1: Define WebSocket protocol types

**Files:**
- Create: `src/web/protocol.rs`
- Create: `src/web/mod.rs`

- [ ] **Step 1: Create `src/web/mod.rs` module root**

```rust
pub mod protocol;
```

- [ ] **Step 2: Create `src/web/protocol.rs` with event/command enums**

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Server → Client events (JSON over WSS).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WebEvent {
    SyncInit {
        buffers: Vec<BufferMeta>,
        connections: Vec<ConnectionMeta>,
        mention_count: u32,
    },
    NewMessage {
        buffer_id: String,
        message: WireMessage,
    },
    TopicChanged {
        buffer_id: String,
        topic: Option<String>,
        set_by: Option<String>,
    },
    NickEvent {
        buffer_id: String,
        kind: NickEventKind,
        nick: String,
        new_nick: Option<String>,
        prefix: Option<String>,
        modes: Option<String>,
        away: Option<bool>,
        message: Option<String>,
    },
    BufferCreated {
        buffer: BufferMeta,
    },
    BufferClosed {
        buffer_id: String,
    },
    ActivityChanged {
        buffer_id: String,
        activity: u8,
        unread_count: u32,
    },
    ConnectionStatus {
        conn_id: String,
        label: String,
        connected: bool,
        nick: String,
    },
    MentionAlert {
        buffer_id: String,
        message: WireMessage,
    },
    Messages {
        buffer_id: String,
        messages: Vec<WireMessage>,
        has_more: bool,
    },
    NickList {
        buffer_id: String,
        nicks: Vec<WireNick>,
    },
    MentionsList {
        mentions: Vec<WireMention>,
    },
    Error {
        message: String,
    },
}

/// Client → Server commands (JSON over WSS).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WebCommand {
    SendMessage { buffer_id: String, text: String },
    SwitchBuffer { buffer_id: String },
    MarkRead { buffer_id: String, up_to: i64 },
    FetchMessages { buffer_id: String, limit: u32, before: Option<i64> },
    FetchNickList { buffer_id: String },
    FetchMentions,
    RunCommand { buffer_id: String, text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferMeta {
    pub id: String,
    pub connection_id: String,
    pub name: String,
    pub buffer_type: String,
    pub topic: Option<String>,
    pub unread_count: u32,
    pub activity: u8,
    pub nick_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionMeta {
    pub id: String,
    pub label: String,
    pub nick: String,
    pub connected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireMessage {
    pub id: u64,
    pub timestamp: i64,
    pub msg_type: String,
    pub nick: Option<String>,
    pub nick_mode: Option<String>,
    pub text: String,
    pub highlight: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireNick {
    pub nick: String,
    pub prefix: String,
    pub modes: String,
    pub away: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireMention {
    pub id: i64,
    pub timestamp: i64,
    pub buffer_id: String,
    pub channel: String,
    pub nick: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NickEventKind {
    Join,
    Part,
    Quit,
    NickChange,
    ModeChange,
    AwayChange,
}
```

- [ ] **Step 3: Register module in `src/main.rs`**

Add `mod web;` after `mod ui;` in `src/main.rs`.

- [ ] **Step 4: Verify it compiles**

Run: `cargo check`
Expected: compiles with no errors (unused warnings OK)

- [ ] **Step 5: Commit**

```bash
git add src/web/ src/main.rs
git commit -m "feat(web): add WebSocket protocol types"
```

---

### Task 2: Add `WebConfig` to config system

**Files:**
- Modify: `src/config/mod.rs`
- Modify: `src/config/defaults.rs`
- Modify: `src/config/env.rs`
- Modify: `src/commands/settings.rs`

- [ ] **Step 1: Add `WebConfig` struct to `src/config/mod.rs`**

Add after `SpellcheckConfig`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_web_bind")]
    pub bind_address: String,
    #[serde(default = "default_web_port")]
    pub port: u16,
    #[serde(default)]
    pub tls_cert: String,
    #[serde(default)]
    pub tls_key: String,
    #[serde(default = "default_web_timestamp")]
    pub timestamp_format: String,
    #[serde(default = "default_web_line_height")]
    pub line_height: f32,
    #[serde(default = "default_web_theme")]
    pub theme: String,
    #[serde(default)]
    pub cloudflare_tunnel_name: String,
    /// Loaded from .env, not config.toml.
    #[serde(skip)]
    pub password: String,
}

fn default_web_bind() -> String { "127.0.0.1".to_string() }
fn default_web_port() -> u16 { 8443 }
fn default_web_timestamp() -> String { "%H:%M".to_string() }
fn default_web_line_height() -> f32 { 1.35 }
fn default_web_theme() -> String { "nightfall".to_string() }
```

Add `web: WebConfig` field to `AppConfig`.

- [ ] **Step 2: Add defaults in `src/config/defaults.rs`**

Add `WebConfig` default to `default_config()`.

- [ ] **Step 3: Load `WEB_PASSWORD` from `.env` in `src/config/env.rs`**

In `apply_credentials` or a new `apply_web_credentials` function, read `WEB_PASSWORD` from env vars and set `config.web.password`.

- [ ] **Step 4: Register web settings in `src/commands/settings.rs`**

Add `web.enabled`, `web.bind_address`, `web.port`, `web.tls_cert`, `web.tls_key`, `web.timestamp_format`, `web.line_height`, `web.theme`, `web.password` (credential), `web.cloudflare_tunnel_name` to `get_setting_paths()` and `get_config_value()` / `set_config_value()`.

- [ ] **Step 5: Verify it compiles and passes tests**

Run: `cargo test -- --quiet`
Expected: all existing tests pass, no regressions

- [ ] **Step 6: Commit**

```bash
git add src/config/ src/commands/settings.rs
git commit -m "feat(web): add WebConfig and /set web.* settings"
```

---

### Task 3: TLS cert generation

**Files:**
- Create: `src/web/tls.rs`
- Modify: `src/web/mod.rs`
- Modify: `src/constants.rs`
- Modify: `Cargo.toml`

- [ ] **Step 1: Add deps to `Cargo.toml`**

```toml
rcgen = "0.14"
rustls-pemfile = "2"
tokio-rustls = "0.26"
```

- [ ] **Step 2: Add `certs_dir()` to `src/constants.rs`**

```rust
pub fn certs_dir() -> std::path::PathBuf {
    home_dir().join("certs")
}
```

- [ ] **Step 3: Create `src/web/tls.rs`**

Functions:
- `load_or_generate_tls_config(cert_path: &str, key_path: &str) -> Result<ServerConfig>` — if cert/key paths are non-empty, load them; otherwise generate self-signed to `~/.repartee/certs/` and load those.
- `generate_self_signed() -> Result<(PathBuf, PathBuf)>` — uses rcgen to create cert+key, writes to `certs_dir()`, returns paths.

- [ ] **Step 4: Write test for self-signed cert generation**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_self_signed_creates_files() {
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = generate_self_signed_to(dir.path()).unwrap();
        assert!(cert.exists());
        assert!(key.exists());
        // Verify they parse as valid PEM
        let cert_pem = std::fs::read(&cert).unwrap();
        let key_pem = std::fs::read(&key).unwrap();
        assert!(cert_pem.starts_with(b"-----BEGIN CERTIFICATE-----"));
        assert!(key_pem.starts_with(b"-----BEGIN PRIVATE KEY-----"));
    }
}
```

- [ ] **Step 5: Run test**

Run: `cargo test web::tls -- -v`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml src/constants.rs src/web/tls.rs src/web/mod.rs
git commit -m "feat(web): TLS self-signed cert generation"
```

---

### Task 4: Web event broadcaster

**Files:**
- Create: `src/web/broadcast.rs`
- Modify: `src/web/mod.rs`

- [ ] **Step 1: Create `src/web/broadcast.rs`**

```rust
use tokio::sync::broadcast;
use super::protocol::WebEvent;

/// Fan-out channel for broadcasting events to all connected web clients.
pub struct WebBroadcaster {
    tx: broadcast::Sender<WebEvent>,
}

impl WebBroadcaster {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Send an event to all connected clients. Returns number of receivers.
    /// If no clients are connected, the event is silently dropped.
    pub fn send(&self, event: WebEvent) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    /// Subscribe to receive events. Each WebSocket session calls this.
    pub fn subscribe(&self) -> broadcast::Receiver<WebEvent> {
        self.tx.subscribe()
    }
}
```

- [ ] **Step 2: Write test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn broadcast_to_multiple_receivers() {
        let bc = WebBroadcaster::new(16);
        let mut rx1 = bc.subscribe();
        let mut rx2 = bc.subscribe();

        let event = WebEvent::BufferClosed { buffer_id: "test/chan".into() };
        let count = bc.send(event);
        assert_eq!(count, 2);

        let ev1 = rx1.recv().await.unwrap();
        let ev2 = rx2.recv().await.unwrap();
        // Both received the event
        assert!(matches!(ev1, WebEvent::BufferClosed { .. }));
        assert!(matches!(ev2, WebEvent::BufferClosed { .. }));
    }

    #[test]
    fn broadcast_no_receivers_does_not_panic() {
        let bc = WebBroadcaster::new(16);
        let count = bc.send(WebEvent::BufferClosed { buffer_id: "x".into() });
        assert_eq!(count, 0);
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test web::broadcast -- -v`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/web/broadcast.rs src/web/mod.rs
git commit -m "feat(web): event broadcaster for WebSocket fan-out"
```

---

### Task 5: Auth module — login, sessions, rate limiting

**Files:**
- Create: `src/web/auth.rs`
- Modify: `Cargo.toml`

- [ ] **Step 1: Add deps**

```toml
axum = "0.8"
axum-extra = { version = "0.10", features = ["cookie-signed"] }
tower = { version = "0.5", features = ["limit"] }
```

- [ ] **Step 2: Create `src/web/auth.rs`**

Implement:
- `SessionStore` — `HashMap<String, Session>` behind a `Mutex`, each session has token + expiry + IP
- `RateLimiter` — per-IP attempt counter with exponential backoff (1s, 2s, 4s, 8s, max 60s, reset after success)
- `login_handler(Json<LoginRequest>, State<AppState>) -> Response` — validates password, creates session, sets signed cookie
- `validate_session(cookie) -> Option<Session>` — checks token exists and not expired
- Helper: `generate_session_token() -> String` — 32-byte random hex

Tests:
- Rate limiter blocks after N failures
- Rate limiter resets on success
- Session expiry works
- Invalid password returns 401

- [ ] **Step 3: Run tests**

Run: `cargo test web::auth -- -v`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml src/web/auth.rs src/web/mod.rs
git commit -m "feat(web): auth module with login, sessions, rate limiting"
```

---

### Task 6: WebSocket handler

**Files:**
- Create: `src/web/ws.rs`
- Modify: `src/web/mod.rs`

- [ ] **Step 1: Create `src/web/ws.rs`**

Implement:
- `ws_handler(ws: WebSocketUpgrade, State) -> Response` — validates session cookie, upgrades to WSS
- `handle_socket(socket, state)` async fn:
  - Creates per-session `active_buffer_id: Option<String>`
  - Subscribes to `WebBroadcaster`
  - Sends `SyncInit` with buffer metadata + nick lists
  - Runs select loop:
    - `broadcast_rx.recv()` → forward `WebEvent` as JSON to client
    - `socket.recv()` → parse `WebCommand`, dispatch:
      - `SendMessage` → queue to IRC via existing `irc_tx`
      - `SwitchBuffer` → update session-local active buffer, send messages
      - `MarkRead` → update `buffer.unread_count` in `AppState`
      - `FetchMessages` → query SQLite, respond with `Messages`
      - `FetchNickList` → read from `AppState`, respond with `NickList`
      - `FetchMentions` → query SQLite, respond with `MentionsList`
      - `RunCommand` → dispatch through existing command system with buffer context
    - Ping/pong heartbeat (30s interval)

- [ ] **Step 2: Write test for SyncInit generation**

Test that `build_sync_init(app_state)` correctly serializes buffer metadata.

- [ ] **Step 3: Commit**

```bash
git add src/web/ws.rs src/web/mod.rs
git commit -m "feat(web): WebSocket handler with per-session state"
```

---

### Task 7: axum server setup

**Files:**
- Create: `src/web/server.rs`
- Modify: `src/web/mod.rs`

- [ ] **Step 1: Create `src/web/server.rs`**

Implement:
- `WebServer::start(config: &WebConfig, app_handle: AppHandle) -> Result<()>`
  - Build axum router:
    - `POST /api/login` → `auth::login_handler`
    - `GET /ws` → `ws::ws_handler`
    - `GET /*` → static file serving (for Leptos WASM assets, placeholder for now)
  - Configure TLS via `tls::load_or_generate_tls_config()`
  - Bind to `config.bind_address:config.port`
  - Spawn as tokio task, return `JoinHandle`

- [ ] **Step 2: Define `AppHandle` shared state type**

```rust
/// Shared state passed to axum handlers.
/// Contains everything web handlers need to read/write app state.
pub struct AppHandle {
    pub broadcaster: Arc<WebBroadcaster>,
    pub web_cmd_tx: mpsc::UnboundedSender<(WebCommand, SessionId)>,
    pub password_hash: String,
    pub session_store: Arc<Mutex<SessionStore>>,
    pub rate_limiter: Arc<Mutex<RateLimiter>>,
    // Read-only access for SyncInit / FetchNickList:
    pub state_snapshot: Arc<RwLock<StateSnapshot>>,
    // SQLite access for FetchMessages:
    pub storage: Option<Arc<crate::storage::Storage>>,
}
```

- [ ] **Step 3: Commit**

```bash
git add src/web/server.rs src/web/mod.rs
git commit -m "feat(web): axum HTTPS server with routing"
```

---

### Task 8: Wire web server into `App`

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: Add web fields to `App` struct**

```rust
/// Web event broadcaster — sends events to all connected web clients.
pub web_broadcaster: Arc<web::broadcast::WebBroadcaster>,
/// Receiver for commands from web clients.
pub web_cmd_rx: mpsc::UnboundedReceiver<(web::protocol::WebCommand, String)>,
/// Sender side (cloned into web handlers).
pub web_cmd_tx: mpsc::UnboundedSender<(web::protocol::WebCommand, String)>,
```

- [ ] **Step 2: Initialize in `App::new()`**

Create broadcaster, channels. If `config.web.enabled && !config.web.password.is_empty()`, spawn the web server.

- [ ] **Step 3: Add `web_cmd_rx` arm to `tokio::select!` loop in `run()`**

```rust
web_cmd = self.web_cmd_rx.recv() => {
    if let Some((cmd, session_id)) = web_cmd {
        self.handle_web_command(cmd, &session_id);
    }
},
```

- [ ] **Step 4: Implement `handle_web_command()`**

Dispatch each `WebCommand` variant. For `SendMessage`, inject the message as if typed in the terminal. For `RunCommand`, dispatch through `crate::commands::dispatch()` with the provided `buffer_id` as context.

- [ ] **Step 5: Add `broadcast_web_event()` calls in IRC event handlers**

In `handle_irc_event()`, after processing each event into `AppState`, call `self.web_broadcaster.send(event)` with the corresponding `WebEvent`. Key events:
- New message → `NewMessage`
- Topic change → `TopicChanged`
- Join/Part/Quit/Nick → `NickEvent`
- Buffer created/closed → `BufferCreated`/`BufferClosed`
- Activity change → `ActivityChanged`
- Connection status → `ConnectionStatus`

- [ ] **Step 6: Verify it compiles**

Run: `cargo check`

- [ ] **Step 7: Run full test suite**

Run: `cargo test -- --quiet`
Expected: all tests pass, no regressions

- [ ] **Step 8: Commit**

```bash
git add src/app.rs
git commit -m "feat(web): wire web server and event broadcasting into App"
```

---

## Chunk 2: Mentions System

### Task 9: Mentions DB schema and queries

**Files:**
- Modify: `src/storage/db.rs`
- Modify: `src/storage/query.rs`

- [ ] **Step 1: Add `mentions` table creation to `src/storage/db.rs`**

```rust
const CREATE_MENTIONS: &str = "
CREATE TABLE IF NOT EXISTS mentions (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp INTEGER NOT NULL,
    network   TEXT NOT NULL,
    buffer    TEXT NOT NULL,
    channel   TEXT NOT NULL,
    nick      TEXT NOT NULL,
    text      TEXT NOT NULL,
    read_at   INTEGER
)";

const CREATE_MENTIONS_IDX: &str = "
CREATE INDEX IF NOT EXISTS idx_mentions_unread
ON mentions (read_at) WHERE read_at IS NULL";
```

Add to `init_db()` function.

- [ ] **Step 2: Add query functions to `src/storage/query.rs`**

```rust
pub fn insert_mention(
    db: &Connection,
    timestamp: i64,
    network: &str,
    buffer: &str,
    channel: &str,
    nick: &str,
    text: &str,
) -> rusqlite::Result<i64> { ... }

pub fn get_unread_mentions(db: &Connection) -> rusqlite::Result<Vec<MentionRow>> { ... }

pub fn get_unread_mention_count(db: &Connection) -> rusqlite::Result<u32> { ... }

pub fn mark_mentions_read(db: &Connection) -> rusqlite::Result<usize> { ... }
```

- [ ] **Step 3: Write tests**

```rust
#[test]
fn insert_and_query_mentions() { ... }

#[test]
fn mark_mentions_read_sets_timestamp() { ... }

#[test]
fn unread_count_accurate() { ... }
```

- [ ] **Step 4: Run tests**

Run: `cargo test storage::query -- -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/storage/db.rs src/storage/query.rs
git commit -m "feat(web): mentions table and query functions"
```

---

### Task 10: MentionTracker and `/mentions` command

**Files:**
- Create: `src/web/mentions.rs`
- Modify: `src/commands/registry.rs`
- Modify: `src/commands/handlers_admin.rs`
- Modify: `src/app.rs`

- [ ] **Step 1: Create `src/web/mentions.rs`**

```rust
/// Tracks highlight mentions for the /mentions command and web badge.
/// Wraps the SQLite mention queries with in-memory unread count cache.
pub struct MentionTracker {
    unread_count: u32,
}
```

Methods: `record_mention()` (inserts to DB + increments count), `get_unread_count()`, `mark_all_read()`.

- [ ] **Step 2: Wire into `App` — auto-record on highlight messages**

In `add_message()` or `handle_irc_event()`, when a message has `highlight: true`, call `mention_tracker.record_mention()`.

- [ ] **Step 3: Register `/mentions` command**

Add to `src/commands/registry.rs` and implement `cmd_mentions` in `src/commands/handlers_admin.rs`:
- No args: show unread mentions list, mark all read
- Displays as: `[timestamp] #channel <nick> text`

- [ ] **Step 4: Write test**

Test that highlight messages auto-record, `/mentions` displays and clears.

- [ ] **Step 5: Run tests**

Run: `cargo test -- --quiet`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/web/mentions.rs src/commands/ src/app.rs src/web/mod.rs
git commit -m "feat(web): /mentions command with DB persistence"
```

---

## Chunk 3: Leptos Frontend (placeholder)

### Task 11: Scaffold Leptos WASM frontend crate

**Files:**
- Create: `web-ui/Cargo.toml`
- Create: `web-ui/src/main.rs`
- Create: `web-ui/src/app.rs`
- Create: `web-ui/index.html`
- Modify: `Cargo.toml` (workspace)

This task sets up the frontend crate structure. The full Leptos implementation (components, layouts, themes) is a separate phase — this creates the scaffold so the build pipeline works.

- [ ] **Step 1: Create workspace**

Convert root `Cargo.toml` to workspace with `members = [".", "web-ui"]`.

- [ ] **Step 2: Create `web-ui/Cargo.toml`**

```toml
[package]
name = "repartee-web"
version = "0.1.0"
edition = "2024"

[dependencies]
leptos = { version = "0.7", features = ["csr"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
web-sys = { version = "0.3", features = ["WebSocket", "MessageEvent", "ErrorEvent", "CloseEvent"] }
wasm-bindgen = "0.2"
gloo-net = { version = "0.6", features = ["websocket"] }
```

- [ ] **Step 3: Create minimal `web-ui/src/main.rs` + `app.rs`**

A "hello world" Leptos app that connects to `/ws` and displays connection status. Enough to verify the build pipeline.

- [ ] **Step 4: Create `web-ui/index.html`**

Trunk entry point HTML.

- [ ] **Step 5: Install trunk and build**

Run: `cargo install trunk && cd web-ui && trunk build --release`
Expected: `web-ui/dist/` contains WASM + JS + HTML

- [ ] **Step 6: Commit**

```bash
git add web-ui/ Cargo.toml
git commit -m "feat(web): scaffold Leptos WASM frontend crate"
```

---

### Task 12: Embed WASM assets in binary

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/web/server.rs`

- [ ] **Step 1: Add `rust-embed` dep**

```toml
rust-embed = "8"
```

- [ ] **Step 2: Embed `web-ui/dist/` in server.rs**

```rust
#[derive(rust_embed::Embed)]
#[folder = "web-ui/dist/"]
struct WebAssets;
```

Serve via axum fallback handler — any request not matching `/api/*` or `/ws` serves from embedded assets.

- [ ] **Step 3: Test that assets serve**

Build the full binary, start it, verify `curl -k https://localhost:8443/` returns the HTML.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml src/web/server.rs
git commit -m "feat(web): embed WASM assets in binary via rust-embed"
```

---

## Chunk 4: Integration & Docs

### Task 13: End-to-end integration test

**Files:**
- Tests in existing test modules

- [ ] **Step 1: Manual integration test**

1. Build: `cargo build --release`
2. Set password: add `WEB_PASSWORD=test123` to `~/.repartee/.env`
3. Set config: add `[web]\nenabled = true` to `config.toml`
4. Start: `./target/release/repartee`
5. Verify: `curl -k https://localhost:8443/` returns HTML
6. Verify: `curl -k -X POST -d '{"password":"test123"}' https://localhost:8443/api/login` returns session token
7. Verify: WebSocket connects with session cookie
8. Verify: Sending a message from terminal appears in web client
9. Verify: Sending a message from web client appears in terminal

- [ ] **Step 2: Verify rate limiting**

5 failed login attempts → 6th blocked with 429.

- [ ] **Step 3: Commit any fixes**

---

### Task 14: Update docs and help

**Files:**
- Create: `docs/commands/mentions.md`
- Modify: `docs/src/content/configuration.md`
- Modify: `docs/commands/set.md` (if web settings need documenting)

- [ ] **Step 1: Create `/mentions` command docs**

```markdown
---
category: General
description: Show and clear unread mentions
---

# /mentions

Show all unread highlight mentions across all buffers, then mark them as read.

## Usage

\```
/mentions
\```

## Output

Each mention shows: `[timestamp] #channel <nick> message text`

After displaying, all mentions are marked as read. The mention counter resets to 0.

## Web Integration

On the web frontend, the mention count appears as a badge. Viewing mentions on either terminal or web clears them on both.
```

- [ ] **Step 2: Update configuration docs**

Add `[web]` section to `docs/src/content/configuration.md` with all settings documented.

- [ ] **Step 3: Rebuild docs**

Run: `bun run docs/build.ts`

- [ ] **Step 4: Commit**

```bash
git add docs/
git commit -m "docs: add web frontend and /mentions documentation"
```

---

## Dependencies Between Tasks

```
Task 1 (protocol types) ─┬─→ Task 4 (broadcaster) ──→ Task 6 (ws handler) ──→ Task 8 (wire into app)
                          │                                                         ↑
Task 2 (config) ──────────┤                                                         │
                          │                                                         │
Task 3 (TLS) ────────────┴─→ Task 7 (axum server) ─────────────────────────────────┘
                                                                                    │
Task 5 (auth) ──────────────→ Task 7 (axum server)                                 │
                                                                                    │
Task 9 (mentions DB) ──────→ Task 10 (mention tracker) ────────────────────────────┘
                                                                                    │
Task 11 (Leptos scaffold) ─→ Task 12 (embed assets) ──→ Task 13 (e2e test) ──→ Task 14 (docs)
```

**Parallelizable groups:**
- Tasks 1, 2, 3 can run in parallel (no deps)
- Tasks 4, 5 can run in parallel (both depend only on Task 1)
- Task 9 can run in parallel with Tasks 3-8

## Notes for Implementer

- **Do NOT modify terminal rendering code** — ratatui/crossterm stays untouched
- **Do NOT modify session/shim code** — detach/reattach stays untouched
- The Leptos frontend (Task 11) is a **scaffold only** in this plan. Full UI implementation (desktop layout, mobile layout, themes, gestures) is a separate Phase 2 plan.
- `WebEvent` must implement `Clone` for `broadcast::channel` — all fields are owned types
- axum handlers receive state via `axum::extract::State<Arc<AppHandle>>`
- The `web_cmd_tx`/`web_cmd_rx` channel pattern matches existing `irc_tx`/`irc_rx`, `dcc_tx`/`dcc_rx`, `dict_tx`/`dict_rx` patterns in the codebase
- Password comparison must use constant-time comparison to prevent timing attacks

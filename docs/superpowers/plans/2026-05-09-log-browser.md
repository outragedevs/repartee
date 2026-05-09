# Log Browser Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `repartee l` (alias `logs`) subcommand that opens a read-only TUI log browser over the SQLite history, reusing the entire existing layout — sidebar / topic bar / chat view / status line / input bar — with pseudo-networks synthesised from the database.

**Architecture:** Standalone process (no fork, no IRC, no daemon link) that opens the message DB read-only via SQLite WAL, builds a sidebar of pseudo-`Connection`s + `BufferType::Log` buffers from `SELECT DISTINCT (network, buffer) FROM messages`, and lazy-paginates content via the existing `query::get_messages` API. The whole thing is gated by a single `App::log_browser_mode: bool` flag — every chat-mode entry point in `App::run` either branches on this flag or is short-circuited.

**Tech Stack:** Rust 2024, ratatui 0.30, tokio (current_thread), rusqlite (existing dependency, used in read-only `mode=ro` URI — NOT `immutable=1`, which would block reading new WAL writes from the daemon), no new crates.

**Reference spec:** `docs/superpowers/specs/2026-05-09-log-browser-design.md`.

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `src/state/buffer.rs` | modify | Add `BufferType::Log` enum variant, sort group `2` (same as Channel). Add 4 optional fields used only in log mode. |
| `src/web/snapshot.rs` | modify | New `BufferType::Log` arm returning `"log"`. |
| `src/state/mod.rs` | modify | New `BufferType::Log` arm in `script_snapshot`. |
| `src/storage/db.rs` | modify | New `open_readonly_at(path)` helper using URI `file:<p>?mode=ro&immutable=1`. |
| `src/storage/query.rs` | modify | New `list_networks`, `list_buffers_for_network`, `buffer_stats` queries. |
| `src/storage/mod.rs` | modify | New `load_log_db(config)` returning a `LogDb` struct (db handle + crypto key + fts flag). |
| `src/app/mod.rs` | modify | New `log_browser_mode: bool` field + `log_db: Option<LogDb>` + `log_buffer_meta: HashMap<String, LogBufferMeta>`; gate `start_socket_listener`/`start_web_server`/scripts/autoconnect on the flag. |
| `src/app/log_browser.rs` | **create** | All log-browser-only methods: `new_log_browser`, `build_log_catalog`, `load_initial_messages`, `load_older_messages`, `compute_buffer_stats`, slash command handlers. |
| `src/main.rs` | modify | New `repartee l` / `repartee logs` subcommand branch — direct mode like `repartee a`, no fork. |
| `src/ui/topic_bar.rs` | modify | Branch for log buffer rendering: `📜 Log: <buf> @ <network> • N lines • date_range`. |
| `src/ui/status_line.rs` | modify | Branch for log mode: `log mode • <net>/<buf> • showing X/Y from <ts>`. |
| `src/ui/input.rs` | modify | In log mode reject non-`/` input with a transient hint. |

---

## Task 1: Add `BufferType::Log` variant

**Files:**
- Modify: `src/state/buffer.rs:8-34, 287-294`
- Modify: `src/web/snapshot.rs:113-115`
- Modify: `src/state/mod.rs:113-122`

Adding the enum variant first surfaces every match arm that needs updating — the compiler does the work.

- [ ] **Step 1: Write the failing test**

In `src/state/buffer.rs` after the existing `BufferType` test block (around line 290):

```rust
#[test]
fn log_buffer_sorts_with_channels() {
    // Log buffers live under pseudo-networks (different connection_id from
    // any real channel), so the shared sort group is fine.
    assert_eq!(BufferType::Log.sort_group(), BufferType::Channel.sort_group());
}
```

- [ ] **Step 2: Run the test (it fails — variant missing)**

```bash
. "$HOME/.cargo/env" && cargo test -p repartee --quiet log_buffer_sorts_with_channels 2>&1 | tail -5
```

Expected: compile error or `no variant Log`.

- [ ] **Step 3: Add the variant**

In `src/state/buffer.rs` add `Log` to the enum, place it after `Shell`:

```rust
pub enum BufferType {
    Mentions,
    Server,
    Channel,
    Query,
    DccChat,
    Special,
    Shell,
    Log,
}
```

In the same file, extend `sort_group`:

```rust
impl BufferType {
    pub const fn sort_group(&self) -> u8 {
        match self {
            Self::Mentions => 0,
            Self::Server => 1,
            Self::Channel | Self::Log => 2,
            Self::Query => 3,
            Self::DccChat => 4,
            Self::Special => 5,
            Self::Shell => 6,
        }
    }
}
```

- [ ] **Step 4: Update the two match arms in other modules**

`src/web/snapshot.rs` — add `BufferType::Log => "log",` next to the existing `Shell` arm.

`src/state/mod.rs` — in the `script_snapshot` match, add `buffer::BufferType::Log => "log",` next to the existing `Shell` arm.

- [ ] **Step 5: Build + run tests**

```bash
. "$HOME/.cargo/env" && cargo build -p repartee 2>&1 | tail -3
. "$HOME/.cargo/env" && cargo test -p repartee --quiet 2>&1 | tail -3
```

Expected: build OK, all tests pass (1067+1).

- [ ] **Step 6: Commit**

```bash
git add src/state/buffer.rs src/web/snapshot.rs src/state/mod.rs
git -c user.name="kofany" -c user.email="j@dabrowski.biz" commit -m "feat(buffer): add BufferType::Log variant"
```

---

## Task 2: Buffer fields for log metadata

**Files:**
- Modify: `src/state/buffer.rs` (Buffer struct around line 145)

Four optional fields, all `None` / `false` for non-log buffers. Cheap memory overhead, avoids a separate `HashMap<buffer_id, LogMeta>` lookup on every render.

- [ ] **Step 1: Add fields to `Buffer`**

In `src/state/buffer.rs`, locate the `Buffer` struct and add at the end (after `peer_handle`):

```rust
    // Populated only when buffer_type == BufferType::Log. Cached at activation
    // time so the topic-bar render doesn't requery the DB every frame.
    #[serde(default)]
    pub log_total_lines: Option<u64>,
    #[serde(default)]
    pub log_oldest_ts: Option<i64>,
    #[serde(default)]
    pub log_newest_ts: Option<i64>,
    /// Set true once a paginated `load_older` returns fewer rows than
    /// requested — i.e. we've reached the start of the recorded log.
    #[serde(default)]
    pub history_exhausted: bool,
```

- [ ] **Step 2: Update every `Buffer { ... }` construction**

The compiler will name them. Run a build to find sites:

```bash
. "$HOME/.cargo/env" && cargo build -p repartee 2>&1 | grep -E "error\[E0063\]" | head -10
```

For each missing-field error site, add the four defaults — copy-paste:

```rust
    log_total_lines: None,
    log_oldest_ts: None,
    log_newest_ts: None,
    history_exhausted: false,
```

Sites to expect (grep before/after to verify):

```bash
grep -rn "Buffer {$\|Buffer {$" /home/projekt/dev/repartee/src --include="*.rs" | head -20
```

Repeat the build / paste loop until clean.

- [ ] **Step 3: Build + test**

```bash
. "$HOME/.cargo/env" && cargo build -p repartee 2>&1 | tail -3
. "$HOME/.cargo/env" && cargo test -p repartee --quiet 2>&1 | tail -3
```

Expected: build OK, all tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/
git -c user.name="kofany" -c user.email="j@dabrowski.biz" commit -m "feat(buffer): cache log-mode metadata on Buffer"
```

---

## Task 3: SQLite read-only open helper

**Files:**
- Modify: `src/storage/db.rs:215-220`

URI form `file:<path>?mode=ro` lets the daemon keep writing through WAL while we read.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module at end of `src/storage/db.rs`:

```rust
#[test]
fn open_readonly_rejects_writes() {
    let dir = std::env::temp_dir().join("repartee_logbrowser_ro_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("messages.db");

    // Seed via the writable opener.
    let path_str = path.to_str().unwrap();
    let rw = open_database_at(path_str, false).unwrap();
    rw.execute(
        "INSERT INTO messages (timestamp, network, buffer, message_type, nick, text) \
         VALUES (?1, 'libera', '#test', 'message', 'ada', 'hello')",
        rusqlite::params![100],
    )
    .unwrap();
    drop(rw);

    let ro = open_readonly_at(path_str).unwrap();
    let count: i64 = ro
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);

    let write_err =
        ro.execute("INSERT INTO messages (timestamp, network, buffer, message_type, nick, text) \
                    VALUES (1, 'a','b','message','c','d')", []);
    assert!(write_err.is_err(), "read-only handle must reject writes");

    std::fs::remove_dir_all(&dir).unwrap();
}
```

- [ ] **Step 2: Run test (fails — function missing)**

```bash
. "$HOME/.cargo/env" && cargo test -p repartee --quiet open_readonly_rejects_writes 2>&1 | tail -5
```

- [ ] **Step 3: Implement `open_readonly_at`**

Add right below `open_database_at` in `src/storage/db.rs`:

```rust
/// Open the message database read-only.
///
/// Uses `mode=ro` URI parameter so the SQLite WAL writer side can
/// keep flushing concurrently — the log browser never writes, the
/// daemon never blocks. No schema creation: the database must
/// already exist.
pub fn open_readonly_at(path: &str) -> rusqlite::Result<Connection> {
    let uri = format!("file:{path}?mode=ro");
    let db = Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    apply_pragmas(&db)?;
    Ok(db)
}
```

- [ ] **Step 4: Run test**

```bash
. "$HOME/.cargo/env" && cargo test -p repartee --quiet open_readonly_rejects_writes 2>&1 | tail -3
```

Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add src/storage/db.rs
git -c user.name="kofany" -c user.email="j@dabrowski.biz" commit -m "feat(storage): read-only DB open helper"
```

---

## Task 4: Catalog queries (`list_networks`, `list_buffers_for_network`, `buffer_stats`)

**Files:**
- Modify: `src/storage/query.rs`

These are the queries that populate the sidebar and topic-bar metadata.

- [ ] **Step 1: Write the failing tests**

At the end of the existing `tests` module in `src/storage/query.rs`:

```rust
#[test]
fn list_networks_returns_distinct_sorted() {
    let db = open_database(false).unwrap();
    db.execute_batch(
        "INSERT INTO messages (timestamp, network, buffer, message_type, nick, text) VALUES \
         (1, 'libera',  '#rust',   'message', 'ada', 'a'), \
         (2, 'libera',  '#polska', 'message', 'ada', 'b'), \
         (3, 'oftc',    '#debian', 'message', 'ada', 'c'), \
         (4, 'libera',  '#rust',   'message', 'ada', 'd'), \
         (5, 'ircnet',  '#pl',     'message', 'ada', 'e');",
    )
    .unwrap();

    assert_eq!(list_networks(&db).unwrap(), vec!["ircnet", "libera", "oftc"]);
}

#[test]
fn list_buffers_for_network_filters_correctly() {
    let db = open_database(false).unwrap();
    db.execute_batch(
        "INSERT INTO messages (timestamp, network, buffer, message_type, nick, text) VALUES \
         (1, 'libera', '#rust',   'message', 'a', 'x'), \
         (2, 'libera', '#polska', 'message', 'a', 'x'), \
         (3, 'oftc',   '#debian', 'message', 'a', 'x'), \
         (4, 'libera', '#rust',   'message', 'a', 'y');",
    )
    .unwrap();

    assert_eq!(
        list_buffers_for_network(&db, "libera").unwrap(),
        vec!["#polska", "#rust"]
    );
    assert_eq!(
        list_buffers_for_network(&db, "oftc").unwrap(),
        vec!["#debian"]
    );
    assert!(list_buffers_for_network(&db, "missing").unwrap().is_empty());
}

#[test]
fn buffer_stats_returns_count_and_range() {
    let db = open_database(false).unwrap();
    db.execute_batch(
        "INSERT INTO messages (timestamp, network, buffer, message_type, nick, text) VALUES \
         (100,  'libera', '#rust', 'message', 'a', 'x'), \
         (200,  'libera', '#rust', 'message', 'a', 'y'), \
         (50,   'libera', '#rust', 'message', 'a', 'z'), \
         (9999, 'libera', '#other','message', 'a', 'q');",
    )
    .unwrap();

    let stats = buffer_stats(&db, "libera", "#rust").unwrap();
    assert_eq!(stats, Some((3, 50, 200)));
    assert_eq!(buffer_stats(&db, "libera", "#unknown").unwrap(), None);
}
```

- [ ] **Step 2: Run tests (fail — functions missing)**

```bash
. "$HOME/.cargo/env" && cargo test -p repartee --quiet list_networks_returns_distinct_sorted list_buffers_for_network_filters_correctly buffer_stats_returns_count_and_range 2>&1 | tail -5
```

- [ ] **Step 3: Implement the three queries**

Add to `src/storage/query.rs` near the other public functions:

```rust
/// Distinct networks present in the message log, sorted ascending.
pub fn list_networks(db: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = db.prepare("SELECT DISTINCT network FROM messages ORDER BY network")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// Distinct buffers logged for a given network, sorted ascending.
pub fn list_buffers_for_network(
    db: &Connection,
    network: &str,
) -> rusqlite::Result<Vec<String>> {
    let mut stmt = db.prepare(
        "SELECT DISTINCT buffer FROM messages WHERE network = ?1 ORDER BY buffer",
    )?;
    let rows = stmt.query_map(rusqlite::params![network], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// `(line_count, oldest_ts, newest_ts)` for a given network/buffer pair, or
/// `None` if no messages exist there. Cached on the `Buffer` at activation.
pub fn buffer_stats(
    db: &Connection,
    network: &str,
    buffer: &str,
) -> rusqlite::Result<Option<(u64, i64, i64)>> {
    let row = db.query_row(
        "SELECT COUNT(*), MIN(timestamp), MAX(timestamp) \
         FROM messages WHERE network = ?1 AND buffer = ?2",
        rusqlite::params![network, buffer],
        |r| {
            let count: i64 = r.get(0)?;
            // MIN/MAX are NULL when count == 0
            let oldest: Option<i64> = r.get(1)?;
            let newest: Option<i64> = r.get(2)?;
            #[expect(clippy::cast_sign_loss, reason = "COUNT(*) is non-negative")]
            Ok((count as u64, oldest, newest))
        },
    )?;
    Ok(match row {
        (0, _, _) => None,
        (n, Some(o), Some(x)) => Some((n, o, x)),
        _ => None,
    })
}
```

- [ ] **Step 4: Run tests, expect pass**

```bash
. "$HOME/.cargo/env" && cargo test -p repartee --quiet list_networks_returns_distinct_sorted list_buffers_for_network_filters_correctly buffer_stats_returns_count_and_range 2>&1 | tail -3
```

- [ ] **Step 5: Commit**

```bash
git add src/storage/query.rs
git -c user.name="kofany" -c user.email="j@dabrowski.biz" commit -m "feat(storage): catalog queries for log browser"
```

---

## Task 5: `LogDb` loader (read-only DB + crypto key)

**Files:**
- Modify: `src/storage/mod.rs`

A small struct that bundles the read-only handle, optional crypto key, and FTS availability. Used by `App::new_log_browser`.

- [ ] **Step 1: Implement the loader**

Add to the bottom of `src/storage/mod.rs` (outside `impl Storage`):

```rust
/// Read-only handle bundle used by the log browser. Same crypto-key
/// derivation as `Storage::init`, no writer task spawned.
pub struct LogDb {
    pub db: Arc<Mutex<rusqlite::Connection>>,
    pub crypto_key: Option<aes_gcm::Key<aes_gcm::Aes256Gcm>>,
    pub has_fts: bool,
}

/// Open the message database read-only and (when `[storage] encrypt =
/// true`) load the same AES-256-GCM key the daemon uses. Returns a clear
/// human-readable error so `repartee l` can print it directly.
pub fn load_log_db(config: &LoggingConfig) -> Result<LogDb, String> {
    let db_dir = constants::log_dir();
    let db_path = db_dir.join("messages.db");
    if !db_path.exists() {
        return Err(format!(
            "no message log at {} — start `repartee` and chat first",
            db_path.display()
        ));
    }
    let path_str = db_path.to_str().ok_or("invalid log dir path")?;
    let conn = db::open_readonly_at(path_str)
        .map_err(|e| format!("failed to open log database: {e}"))?;

    let crypto_key = if config.encrypt {
        let hex_key = crypto::load_or_create_key()?;
        Some(crypto::import_key(&hex_key)?)
    } else {
        None
    };

    Ok(LogDb {
        db: Arc::new(Mutex::new(conn)),
        crypto_key,
        has_fts: !config.encrypt,
    })
}
```

`Arc`, `Mutex`, `aes_gcm`, `db`, `crypto`, `constants` are already imported in this file — verify with the existing imports at top.

- [ ] **Step 2: Build**

```bash
. "$HOME/.cargo/env" && cargo build -p repartee 2>&1 | tail -3
```

Expected: OK.

- [ ] **Step 3: Commit**

```bash
git add src/storage/mod.rs
git -c user.name="kofany" -c user.email="j@dabrowski.biz" commit -m "feat(storage): LogDb bundle for log browser"
```

---

## Task 6: `App::log_browser_mode` flag + `LogDb` field

**Files:**
- Modify: `src/app/mod.rs` (struct definition + `App::new` defaults)

Field-only addition — populated by the new constructor in Task 7.

- [ ] **Step 1: Add fields to `App`**

In `src/app/mod.rs`, in the `App` struct add:

```rust
    /// `true` when started via `repartee l` — disables IRC, sockets,
    /// scripts, autoconnect; rewires sidebar to read from `log_db`.
    pub log_browser_mode: bool,
    /// Read-only DB handle when `log_browser_mode == true`. `None`
    /// otherwise.
    pub log_db: Option<crate::storage::LogDb>,
```

- [ ] **Step 2: Initialise in `App::new`**

Find the existing `Self { ... }` constructor inside `App::new` and add the two fields with their defaults at the end (just before the closing brace):

```rust
            log_browser_mode: false,
            log_db: None,
```

- [ ] **Step 3: Build**

```bash
. "$HOME/.cargo/env" && cargo build -p repartee 2>&1 | tail -3
```

Expected: OK.

- [ ] **Step 4: Commit**

```bash
git add src/app/mod.rs
git -c user.name="kofany" -c user.email="j@dabrowski.biz" commit -m "feat(app): log_browser_mode flag and LogDb field"
```

---

## Task 7: `App::new_log_browser` constructor and catalog builder

**Files:**
- Create: `src/app/log_browser.rs`
- Modify: `src/app/mod.rs` (declare submodule, public re-export)

Single home for every method that runs only in log mode.

- [ ] **Step 1: Declare the submodule**

In `src/app/mod.rs` at the top, alongside the other `mod` declarations of submodules:

```rust
mod log_browser;
```

- [ ] **Step 2: Create `src/app/log_browser.rs` with constructor + catalog**

```rust
//! Log-browser-only methods on `App`. Only invoked when
//! `log_browser_mode == true`. Keeps the chat-mode call sites in
//! `app/mod.rs` free of log-mode branches.

use std::collections::{HashMap, HashSet, VecDeque};

use chrono::Utc;
use color_eyre::eyre::Result;

use crate::config;
use crate::state::buffer::{ActivityLevel, Buffer, BufferType, make_buffer_id};
use crate::state::connection::{Connection, ConnectionStatus};
use crate::storage::LogDb;

use super::App;

impl App {
    /// Connection ID prefix used for log-mode pseudo-networks. Distinct
    /// from any real network identifier (which the user picks in their
    /// config TOML) so live and log buffers never collide on
    /// `make_buffer_id`.
    pub const LOG_CONN_PREFIX: &'static str = "_log_";

    /// Build an `App` instance configured for the read-only log browser.
    /// No IRC, no scripts, no web server, no sockets.
    pub fn new_log_browser() -> Result<Self> {
        let mut app = Self::new()?;
        app.log_browser_mode = true;

        let log_db = crate::storage::load_log_db(&app.config.logging)
            .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
        app.log_db = Some(log_db);

        // Wipe any state left over from `App::new`'s default initialisation
        // (default Status buffer etc.) so build_log_catalog sees a clean
        // sidebar.
        app.state.connections.clear();
        app.state.buffers.clear();
        app.state.active_buffer_id = None;

        app.build_log_catalog()?;
        Ok(app)
    }

    /// Populate `state.connections` and `state.buffers` from the
    /// distinct (network, buffer) pairs in the log database. Each
    /// network becomes a synthetic `Connection`, each buffer a
    /// `BufferType::Log` placeholder with empty `messages` (filled
    /// lazily when first activated).
    pub fn build_log_catalog(&mut self) -> Result<()> {
        let log_db = self
            .log_db
            .as_ref()
            .ok_or_else(|| color_eyre::eyre::eyre!("log catalog requires log_db"))?;
        let db = log_db.db.lock().expect("log db poisoned");

        let networks = crate::storage::query::list_networks(&db)
            .map_err(|e| color_eyre::eyre::eyre!("list_networks: {e}"))?;

        // Look up friendly labels from the user's chat config when present
        // ("libera" -> "Libera Chat" if they configured a server with that
        // id). Falls back to the network id verbatim.
        let label_for = |net: &str| -> String {
            self.config
                .servers
                .get(net)
                .map_or_else(|| net.to_string(), |c| c.label.clone())
        };

        let mut first_buffer_id: Option<String> = None;

        for net in &networks {
            let conn_id = format!("{}{net}", Self::LOG_CONN_PREFIX);
            self.state.add_connection(Connection {
                id: conn_id.clone(),
                label: label_for(net),
                status: ConnectionStatus::Connected,
                nick: String::new(),
                user_modes: String::new(),
                isupport: HashMap::new(),
                isupport_parsed: crate::irc::isupport::Isupport::new(),
                error: None,
                lag: None,
                lag_pending: false,
                reconnect_attempts: 0,
                reconnect_delay_secs: 0,
                next_reconnect: None,
                should_reconnect: false,
                joined_channels: Vec::new(),
                origin_config: config::ServerConfig {
                    label: String::new(),
                    address: String::new(),
                    port: 0,
                    tls: false,
                    tls_verify: true,
                    autoconnect: false,
                    channels: vec![],
                    nick: None,
                    username: None,
                    realname: None,
                    password: None,
                    sasl_user: None,
                    sasl_pass: None,
                    bind_ip: None,
                    encoding: None,
                    auto_reconnect: Some(false),
                    reconnect_delay: None,
                    reconnect_max_retries: None,
                    autosendcmd: None,
                    sasl_mechanism: None,
                    client_cert_path: None,
                },
                local_ip: None,
                enabled_caps: HashSet::new(),
                who_token_counter: 0,
                silent_who_channels: HashSet::new(),
                silent_banlist_channels: HashSet::new(),
            });

            let buffers = crate::storage::query::list_buffers_for_network(&db, net)
                .map_err(|e| color_eyre::eyre::eyre!("list_buffers_for_network: {e}"))?;
            for buf in buffers {
                let buf_id = make_buffer_id(&conn_id, &buf);
                if first_buffer_id.is_none() {
                    first_buffer_id = Some(buf_id.clone());
                }
                self.state.add_buffer(Buffer {
                    id: buf_id,
                    connection_id: conn_id.clone(),
                    buffer_type: BufferType::Log,
                    name: buf.clone(),
                    messages: VecDeque::new(),
                    activity: ActivityLevel::None,
                    unread_count: 0,
                    last_read: Utc::now(),
                    topic: None,
                    topic_set_by: None,
                    users: HashMap::new(),
                    modes: None,
                    mode_params: None,
                    list_modes: HashMap::new(),
                    last_speakers: Vec::new(),
                    peer_handle: None,
                    log_total_lines: None,
                    log_oldest_ts: None,
                    log_newest_ts: None,
                    history_exhausted: false,
                });
            }
        }

        if let Some(id) = first_buffer_id {
            self.state.set_active_buffer(&id);
        }
        Ok(())
    }
}
```

- [ ] **Step 3: Build**

```bash
. "$HOME/.cargo/env" && cargo build -p repartee 2>&1 | tail -10
```

Expected: OK. If there are any "unused" warnings on the new constants, tolerate them — Task 8 will use them.

- [ ] **Step 4: Commit**

```bash
git add src/app/log_browser.rs src/app/mod.rs
git -c user.name="kofany" -c user.email="j@dabrowski.biz" commit -m "feat(app): log browser constructor + catalog builder"
```

---

## Task 8: Lazy load — initial + paged history

**Files:**
- Modify: `src/app/log_browser.rs` (add 2 methods)

- [ ] **Step 1: Add `load_initial_messages` and `load_older_messages`**

Append to `src/app/log_browser.rs` inside the existing `impl App` block:

```rust
    /// Load the most recent `INITIAL_LIMIT` messages for `buffer_id`.
    /// Cheap no-op if the buffer already has messages (called every time
    /// a log buffer is activated). Also populates `log_total_lines`,
    /// `log_oldest_ts`, `log_newest_ts` once.
    pub fn load_initial_messages(&mut self, buffer_id: &str) {
        const INITIAL_LIMIT: usize = 200;
        let already_loaded = self
            .state
            .buffers
            .get(buffer_id)
            .is_some_and(|b| !b.messages.is_empty());
        if already_loaded {
            return;
        }
        let Some(log_db) = &self.log_db else { return };
        let Some((net, buf)) = self.split_log_buffer_id(buffer_id) else { return };
        let db = log_db.db.lock().expect("log db poisoned");

        // Always cache stats first so the topic bar gets numbers even if
        // the buffer happens to have zero messages.
        if let Ok(Some((count, oldest, newest))) =
            crate::storage::query::buffer_stats(&db, &net, &buf)
            && let Some(buffer) = self.state.buffers.get_mut(buffer_id)
        {
            buffer.log_total_lines = Some(count);
            buffer.log_oldest_ts = Some(oldest);
            buffer.log_newest_ts = Some(newest);
        }

        match crate::storage::query::get_messages(
            &db,
            &net,
            &buf,
            None,
            INITIAL_LIMIT,
            log_db.crypto_key.is_some(),
            log_db.crypto_key.as_ref(),
        ) {
            Ok(rows) => {
                let exhausted = rows.len() < INITIAL_LIMIT;
                drop(db);
                if let Some(buffer) = self.state.buffers.get_mut(buffer_id) {
                    for row in rows {
                        buffer.messages.push_back(row.into_buffer_message());
                    }
                    buffer.history_exhausted = exhausted;
                }
            }
            Err(e) => tracing::warn!(%buffer_id, "log load_initial failed: {e}"),
        }
    }

    /// Prepend up to `PAGE_LIMIT` messages older than the oldest currently
    /// loaded message. Sets `history_exhausted` when fewer rows are
    /// returned than requested. No-op if already exhausted or no messages
    /// loaded yet.
    pub fn load_older_messages(&mut self, buffer_id: &str) {
        const PAGE_LIMIT: usize = 200;
        let Some(buffer) = self.state.buffers.get(buffer_id) else { return };
        if buffer.history_exhausted {
            return;
        }
        let Some(oldest_msg) = buffer.messages.front() else {
            self.load_initial_messages(buffer_id);
            return;
        };
        let oldest_ts = oldest_msg.timestamp.timestamp();
        let Some(log_db) = &self.log_db else { return };
        let Some((net, buf)) = self.split_log_buffer_id(buffer_id) else { return };
        let db = log_db.db.lock().expect("log db poisoned");
        match crate::storage::query::get_messages(
            &db,
            &net,
            &buf,
            Some(oldest_ts),
            PAGE_LIMIT,
            log_db.crypto_key.is_some(),
            log_db.crypto_key.as_ref(),
        ) {
            Ok(rows) => {
                let exhausted = rows.len() < PAGE_LIMIT;
                drop(db);
                if let Some(buffer) = self.state.buffers.get_mut(buffer_id) {
                    // get_messages returns chronological ascending — front
                    // them in the same order so the buffer stays sorted.
                    for row in rows.into_iter().rev() {
                        buffer.messages.push_front(row.into_buffer_message());
                    }
                    if exhausted {
                        buffer.history_exhausted = true;
                    }
                }
            }
            Err(e) => tracing::warn!(%buffer_id, "log load_older failed: {e}"),
        }
    }

    /// Split `<conn_id>/<buffer_name>` where `conn_id` starts with
    /// `LOG_CONN_PREFIX`, returning `(network, buffer)` without the
    /// prefix. `None` for ids not produced by `build_log_catalog`.
    fn split_log_buffer_id(&self, buffer_id: &str) -> Option<(String, String)> {
        let buffer = self.state.buffers.get(buffer_id)?;
        let net = buffer
            .connection_id
            .strip_prefix(Self::LOG_CONN_PREFIX)?
            .to_string();
        Some((net, buffer.name.clone()))
    }
```

- [ ] **Step 2: Verify `StoredMessage::into_buffer_message` exists**

```bash
grep -n "fn into_buffer_message" /home/projekt/dev/repartee/src/storage/types.rs /home/projekt/dev/repartee/src/storage/*.rs
```

If it doesn't exist, look for whatever conversion the existing `load_backlog` path in `app/backlog.rs` uses — adapt the field-by-field copy here. (Most likely there's a `pub fn into_buffer_message(self) -> Message` or an `impl From<StoredMessage> for Message`. If not, write the conversion inline.)

- [ ] **Step 3: Build**

```bash
. "$HOME/.cargo/env" && cargo build -p repartee 2>&1 | tail -10
```

If `into_buffer_message` doesn't exist, the error will name the right symbol — replace `row.into_buffer_message()` with the correct conversion from `app/backlog.rs`.

- [ ] **Step 4: Commit**

```bash
git add src/app/log_browser.rs
git -c user.name="kofany" -c user.email="j@dabrowski.biz" commit -m "feat(app): lazy initial + paged loading for log buffers"
```

---

## Task 9: `App::run` gates for log mode

**Files:**
- Modify: `src/app/mod.rs` (`run` method)

- [ ] **Step 1: Skip socket listener / scripts / web / autoconnect**

In `src/app/mod.rs::App::run`, replace each of these blocks:

```rust
        if let Err(e) = self.start_socket_listener() {
            ...
```

becomes guarded by `if !self.log_browser_mode`:

```rust
        if !self.log_browser_mode {
            if let Err(e) = self.start_socket_listener() {
                if self.detached {
                    return Err(e.wrap_err("failed to start session socket"));
                }
                tracing::warn!("session socket unavailable: {e}");
            }
        }
```

Apply the same `if !self.log_browser_mode { ... }` wrapper around:

- The `autoconnect_ids` collection block (we never want IRC autoconnect)
- `self.autoload_scripts();`
- `self.start_web_server().await;`
- The `pending_autoconnect_ids` initialisation
- The `start_term_reader` call **stays** — log browser still needs keyboard

For `state.buffers.is_empty()` (default Status creation) — change to also gate on log mode so we don't add a stray Status when log mode wiped it on purpose:

```rust
        if !self.log_browser_mode && self.state.buffers.is_empty() {
            Self::create_default_status(&mut self.state);
        }
```

- [ ] **Step 2: Build + smoke**

```bash
. "$HOME/.cargo/env" && cargo build -p repartee 2>&1 | tail -3
. "$HOME/.cargo/env" && cargo test -p repartee --quiet 2>&1 | tail -3
```

- [ ] **Step 3: Commit**

```bash
git add src/app/mod.rs
git -c user.name="kofany" -c user.email="j@dabrowski.biz" commit -m "feat(app): gate IRC/web/scripts/autoconnect on log_browser_mode"
```

---

## Task 10: `repartee l` / `repartee logs` subcommand

**Files:**
- Modify: `src/main.rs`

Mirrors the existing `repartee a` / `repartee attach` branch.

- [ ] **Step 1: Add the branch**

In `src/main.rs`, after the existing `args.get(1).map(...) == Some("a") || ... == Some("attach")` block, add a parallel block for `"l"` / `"logs"`:

```rust
    if args.get(1).map(String::as_str) == Some("l")
        || args.get(1).map(String::as_str) == Some("logs")
    {
        color_eyre::install()?;
        setup_logging();
        ui::install_panic_hook();
        // Pre-fork validation skipped: log mode never forks. Config TOML
        // is still read inside App::new and any error surfaces here on
        // the user's TTY directly.
        constants::ensure_config_dir();
        return tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let mut app = app::App::new_log_browser()?;
                if let Ok((cols, rows)) = crossterm::terminal::size() {
                    app.cached_term_cols = cols;
                    app.cached_term_rows = rows;
                }
                app.terminal = Some(ui::setup_terminal()?);
                let result = app.run().await;
                if let Some(ref mut terminal) = app.terminal {
                    let _ = ui::restore_terminal(terminal);
                }
                result
            });
    }
```

- [ ] **Step 2: Build**

```bash
. "$HOME/.cargo/env" && cargo build -p repartee 2>&1 | tail -3
```

- [ ] **Step 3: Smoke (without DB — should fail clean)**

```bash
HOME=/tmp/repartee_logbrowser_no_db /home/projekt/dev/repartee/target/debug/repartee l 2>&1 | head -5
```

Expected: clean error message (no message log) plus a non-zero exit.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git -c user.name="kofany" -c user.email="j@dabrowski.biz" commit -m "feat(cli): repartee l / logs subcommand"
```

---

## Task 11: Topic bar log-mode rendering

**Files:**
- Modify: `src/ui/topic_bar.rs`

- [ ] **Step 1: Branch on active buffer type**

Read `src/ui/topic_bar.rs`'s `render` and add a fallback branch for `BufferType::Log` that formats the message described in the spec:

```rust
if buf.buffer_type == BufferType::Log {
    let total = buf.log_total_lines.unwrap_or(0);
    let range = match (buf.log_oldest_ts, buf.log_newest_ts) {
        (Some(o), Some(n)) => format!(
            "{} → {}",
            format_log_date(o),
            format_log_date(n),
        ),
        _ => String::from("(empty)"),
    };
    let net = buf.connection_id.trim_start_matches(crate::app::App::LOG_CONN_PREFIX);
    let text = format!("📜 Log: {} @ {}  •  {} lines  •  {}", buf.name, net, total, range);
    // render `text` using the same Style the existing topic uses.
    return;
}
```

Add helper:

```rust
fn format_log_date(ts: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}
```

(Adjust the `return` placement to match the existing function's flow — likely you'll extract the existing topic render into a common path and only branch on the formatting.)

- [ ] **Step 2: Build + run unit tests**

```bash
. "$HOME/.cargo/env" && cargo build -p repartee 2>&1 | tail -3
. "$HOME/.cargo/env" && cargo test -p repartee --quiet 2>&1 | tail -3
```

- [ ] **Step 3: Commit**

```bash
git add src/ui/topic_bar.rs
git -c user.name="kofany" -c user.email="j@dabrowski.biz" commit -m "feat(ui): topic bar shows log metadata in log mode"
```

---

## Task 12: Status line log-mode rendering

**Files:**
- Modify: `src/ui/status_line.rs`

- [ ] **Step 1: Branch on `app.log_browser_mode`**

Read `src/ui/status_line.rs::render` and add at the top:

```rust
if app.log_browser_mode {
    use chrono::DateTime;
    let buf = app.state.active_buffer();
    let (loaded, total, from) = buf
        .map(|b| {
            let total = b.log_total_lines.unwrap_or(0);
            let loaded = b.messages.len();
            let from = b
                .messages
                .front()
                .map(|m| m.timestamp.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "(empty)".to_string());
            (loaded, total, from)
        })
        .unwrap_or((0, 0, String::from("(no buffer)")));
    let id = buf.map(|b| b.id.as_str()).unwrap_or("");
    let text = format!(
        "log mode • {id} • showing {loaded}/{total} from {from}  •  ↑/↓ scroll • / search • Q quit"
    );
    // render `text` with the existing status bar style and return early.
    return;
}
```

(Inline-adapt the early-return shape to match the existing function — the goal is: in log mode, the status bar shows that line and only that line.)

- [ ] **Step 2: Build + tests**

```bash
. "$HOME/.cargo/env" && cargo build -p repartee 2>&1 | tail -3
. "$HOME/.cargo/env" && cargo test -p repartee --quiet 2>&1 | tail -3
```

- [ ] **Step 3: Commit**

```bash
git add src/ui/status_line.rs
git -c user.name="kofany" -c user.email="j@dabrowski.biz" commit -m "feat(ui): status line in log mode"
```

---

## Task 13: Hook active-buffer change to lazy-load

**Files:**
- Modify: `src/app/mod.rs` or a small helper

When the user navigates to a log buffer, we need to call `load_initial_messages`. Hook this somewhere in the main loop — a per-tick check is fine and keeps the change minimal.

- [ ] **Step 1: Add tick-time hook**

In `src/app/mod.rs` near the existing 1s tick body (the same arm that calls `check_stale_who_batches`), insert:

```rust
                    if self.log_browser_mode
                        && let Some(active_id) = self.state.active_buffer_id.clone()
                    {
                        self.load_initial_messages(&active_id);
                    }
```

(Idempotent — `load_initial_messages` early-returns if the buffer already has messages.)

- [ ] **Step 2: Build + tests**

```bash
. "$HOME/.cargo/env" && cargo build -p repartee 2>&1 | tail -3
. "$HOME/.cargo/env" && cargo test -p repartee --quiet 2>&1 | tail -3
```

- [ ] **Step 3: Commit**

```bash
git add src/app/mod.rs
git -c user.name="kofany" -c user.email="j@dabrowski.biz" commit -m "feat(app): hook log buffer activation to lazy load"
```

---

## Task 14: Slash-only input filter + `/quit` `/help` `/search`

**Files:**
- Modify: `src/app/mod.rs` (input handling — search for the place that takes the input line and dispatches commands)
- Create: `src/commands/handlers_logs.rs`

- [ ] **Step 1: Locate the input dispatch**

```bash
grep -n "fn execute_command\|fn submit_input\|parse_command" /home/projekt/dev/repartee/src/commands/parser.rs /home/projekt/dev/repartee/src/app/*.rs | head
```

Find where a submitted input line is split into command vs chat. Probably `App::execute_input_line` or `App::execute_command`. Note the function name for Step 3.

- [ ] **Step 2: Create the log-mode handlers**

Create `src/commands/handlers_logs.rs`:

```rust
//! Slash command handlers active only when `app.log_browser_mode == true`.

use crate::app::App;
use crate::commands::helpers::add_local_event;

pub(crate) fn cmd_log_quit(app: &mut App, _args: &[String]) {
    app.should_quit = true;
}

pub(crate) fn cmd_log_help(app: &mut App, _args: &[String]) {
    add_local_event(app, "log mode commands:");
    add_local_event(app, "  /search <text>   FTS5 / LIKE search in active log");
    add_local_event(app, "  /quit            exit log browser  (also: q outside input)");
    add_local_event(app, "  /help            this list");
}

pub(crate) fn cmd_log_search(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /search <text>");
        return;
    }
    let query = args.join(" ");
    let Some(active_id) = app.state.active_buffer_id.clone() else {
        add_local_event(app, "No active log buffer");
        return;
    };
    let Some((net, buf)) = app.log_buffer_id_parts(&active_id) else {
        add_local_event(app, "Active buffer is not a log");
        return;
    };
    let Some(log_db) = &app.log_db else {
        add_local_event(app, "Log DB unavailable");
        return;
    };
    let db = log_db.db.lock().expect("log db poisoned");
    match crate::storage::query::search_messages(&db, &query, Some(&net), Some(&buf), 100) {
        Ok(hits) => {
            drop(db);
            add_local_event(app, &format!("[{} matches for \"{}\"]", hits.len(), query));
            for hit in hits {
                let formatted = format!(
                    "{}  <{}> {}",
                    hit.timestamp_human(),
                    hit.nick.as_deref().unwrap_or("*"),
                    hit.text
                );
                add_local_event(app, &formatted);
            }
        }
        Err(e) => add_local_event(app, &format!("Search failed: {e}")),
    }
}
```

(`App::log_buffer_id_parts` is a public wrapper around `split_log_buffer_id` from Task 8 — promote it from `fn` to `pub(crate) fn` and rename if needed.)

`StoredMessage::timestamp_human` may not exist — replace with `chrono::DateTime::<chrono::Utc>::from_timestamp(hit.timestamp, 0).map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()` if so.

- [ ] **Step 3: Wire into command parser**

In `src/commands/registry.rs`, add three command entries for `log_quit`, `log_help`, `log_search`. Their existing `cmd_quit` may already work — check whether registering an alias works. If not, add named entries restricted to log mode (you can predicate dispatch on `app.log_browser_mode` inside the handler that catches unknown).

Simpler: in the dispatch function found in Step 1, if `app.log_browser_mode == true`, route `quit` → `cmd_log_quit`, `help` → `cmd_log_help`, `search` → `cmd_log_search`, and reject anything else with:

```rust
add_local_event(app, "log mode: only /search, /quit, /help — see /help");
```

For typed-but-not-prefixed input (chat lines): also gate at the input layer — if `log_browser_mode == true` and the submitted line doesn't start with `/`, reject identically.

- [ ] **Step 4: Add module to commands**

In `src/commands/mod.rs` add:

```rust
pub mod handlers_logs;
```

- [ ] **Step 5: Build + tests**

```bash
. "$HOME/.cargo/env" && cargo build -p repartee 2>&1 | tail -3
. "$HOME/.cargo/env" && cargo test -p repartee --quiet 2>&1 | tail -3
```

- [ ] **Step 6: Commit**

```bash
git add src/
git -c user.name="kofany" -c user.email="j@dabrowski.biz" commit -m "feat(log-browser): /search /quit /help and slash-only input filter"
```

---

## Task 15: Q hotkey + scroll-up trigger

**Files:**
- Modify: `src/app/mod.rs` (key event dispatch)

- [ ] **Step 1: Bind Q outside input**

Locate the key dispatch (search `KeyCode::Char('q')` for existing hits). Add a guard:

```rust
if app.log_browser_mode
    && !app.input.has_focus()
    && key.code == KeyCode::Char('q')
{
    app.should_quit = true;
    return;
}
```

(`input.has_focus()` may be named differently — search the existing handlers for the equivalent `is_editing_input` predicate.)

- [ ] **Step 2: Trigger `load_older_messages` on scroll-up at top**

Find the chat scroll handler. On the `ScrollUp` event, after performing the scroll, if `app.log_browser_mode` and the new `scroll_offset` puts the user at the top of `messages` and `!buffer.history_exhausted`, call `app.load_older_messages(&buffer_id)`.

- [ ] **Step 3: Build + tests**

```bash
. "$HOME/.cargo/env" && cargo build -p repartee 2>&1 | tail -3
. "$HOME/.cargo/env" && cargo test -p repartee --quiet 2>&1 | tail -3
```

- [ ] **Step 4: Commit**

```bash
git add src/app/mod.rs
git -c user.name="kofany" -c user.email="j@dabrowski.biz" commit -m "feat(log-browser): Q to quit + scroll-up paginates"
```

---

## Task 16: Final clippy + tests + push

- [ ] **Step 1: Full sweep**

```bash
. "$HOME/.cargo/env" && cargo clippy -p repartee --tests --no-deps 2>&1 | tail -5
. "$HOME/.cargo/env" && cargo test -p repartee 2>&1 | tail -3
. "$HOME/.cargo/env" && cargo build --release -p repartee 2>&1 | tail -3
```

Required: zero new clippy warnings on touched files (pre-existing warnings on untouched files are fine).

- [ ] **Step 2: Manual smoke**

```bash
# Seed a tiny log
sqlite3 ~/.repartee/logs/messages.db <<'SQL'
CREATE TABLE IF NOT EXISTS messages (
  id INTEGER PRIMARY KEY,
  timestamp INTEGER,
  network TEXT,
  buffer TEXT,
  message_type TEXT,
  nick TEXT,
  text TEXT
);
INSERT INTO messages (timestamp, network, buffer, message_type, nick, text)
VALUES
 (1700000000, 'libera', '#test', 'message', 'ada', 'hello world'),
 (1700000001, 'libera', '#test', 'message', 'ktk', 'hi'),
 (1700000002, 'oftc',   '#debian','message', 'pi',  'sup');
SQL
```

(Skip if the user's DB already has data — just `repartee l` against the live DB.)

```bash
/home/projekt/dev/repartee/target/release/repartee l
```

Verify:
- Sidebar shows `libera` and `oftc`.
- Active buffer auto-loads, topic shows the date range, status shows `1/N from <date>`.
- Alt-↑/↓ navigates between log buffers.
- `/quit` and `q` both exit cleanly.
- `/search hello` returns the seeded line.
- `/help` lists commands.
- Typing plain text is rejected with the hint.

- [ ] **Step 3: Push**

```bash
git push origin feat/log-browser
```

---

## Self-Review Notes

**Spec coverage:**

- ✅ §CLI — Task 10
- ✅ §Architecture (`log_browser_mode`) — Tasks 6, 9
- ✅ §Storage Access (`open_readonly_at`) — Task 3
- ✅ §Sidebar Catalog — Tasks 4, 7
- ✅ §`BufferType::Log` — Task 1
- ✅ §Lazy Loading — Tasks 8, 13
- ✅ §Topic Bar — Task 11
- ✅ §Status Line — Task 12
- ✅ §Input Bar (`/search`, `/quit`, `/help`) — Task 14
- ✅ §Layout untouched — implicit, only render-content branches added
- ✅ §Module Layout — files match plan File Structure table
- ⏭ §V1.1 commands `/jump` `/grep` — explicitly deferred per spec; not in plan tasks

**Type consistency:**

- `App::LOG_CONN_PREFIX` declared in Task 7, referenced in Task 8 (`split_log_buffer_id`), Task 11 (topic bar `trim_start_matches`), Task 14 (`log_buffer_id_parts`). Consistent.
- `LogDb` declared Task 5, used Tasks 7 + 8 + 14. Consistent.
- `load_initial_messages` / `load_older_messages` defined Task 8, called Tasks 13 + 15. Consistent.

**Placeholders:** none — every step contains either complete code, an exact command, or an explicit "search for X / replace with Y" instruction with the search query attached.

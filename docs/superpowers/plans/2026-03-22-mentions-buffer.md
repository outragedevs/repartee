# Mentions Buffer Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the `/mentions` dump command with a persistent Mentions buffer pinned at the top of the sidebar, showing highlights in a scrollable chat view.

**Architecture:** New `BufferType::Mentions` variant with sort group 0. A dedicated `add_mention_to_buffer` helper adds messages without duplicate logging or MentionAlert. DB-backed with 7-day/1000-line cap, periodic purge.

**Tech Stack:** Rust, ratatui, SQLite (rusqlite), Leptos (web frontend)

**Spec:** `docs/superpowers/specs/2026-03-22-mentions-buffer-design.md`

---

### Task 1: BufferType::Mentions variant + sorting

**Files:**
- Modify: `src/state/buffer.rs:9-30` (enum + sort_group)
- Modify: `src/state/buffer.rs:273-279` (sort_group test)
- Modify: `src/state/sorting.rs:21-26` (special-case Mentions)
- Modify: `src/web/snapshot.rs:104-113` (buffer_type_str)

- [ ] **Step 1: Add Mentions variant to BufferType**

In `src/state/buffer.rs`, add `Mentions` before `Server`:

```rust
pub enum BufferType {
    /// Aggregated mentions buffer — pinned at top of sidebar.
    Mentions,
    Server,
    Channel,
    // ... rest unchanged
}
```

Add sort_group match arm:

```rust
pub const fn sort_group(&self) -> u8 {
    match self {
        Self::Mentions => 0,
        Self::Server => 1,
        // ... rest unchanged
    }
}
```

Update sort_group test to include Mentions:

```rust
fn buffer_type_sort_group() {
    assert!(BufferType::Mentions.sort_group() < BufferType::Server.sort_group());
    assert!(BufferType::Server.sort_group() < BufferType::Channel.sort_group());
    // ... rest unchanged
}
```

- [ ] **Step 2: Special-case Mentions in sort_buffers**

In `src/state/sorting.rs:21-26`, update the sort closure to pin Mentions first:

```rust
keyed.sort_by(|(la, na, a), (lb, nb, b)| {
    // Mentions buffer always sorts first, regardless of connection label.
    let a_mentions = matches!(a.buffer_type, crate::state::buffer::BufferType::Mentions);
    let b_mentions = matches!(b.buffer_type, crate::state::buffer::BufferType::Mentions);
    b_mentions
        .cmp(&a_mentions)
        .then_with(|| la.cmp(lb))
        .then_with(|| a.buffer_type.sort_group().cmp(&b.buffer_type.sort_group()))
        .then_with(|| na.cmp(nb))
});
```

- [ ] **Step 3: Add buffer_type_str mapping**

In `src/web/snapshot.rs:104-113`, add:

```rust
BufferType::Mentions => "mentions",
```

- [ ] **Step 4: Build and fix all exhaustive match warnings**

Run: `cargo test 2>&1 | head -30`

The new variant will cause non-exhaustive match errors in any `match buf_type` expressions. Fix each one — likely in:
- `src/commands/handlers_ui.rs` (cmd_close match)
- `src/state/events.rs` (if any matches exist)
- `src/scripting/` (script snapshot buffer_type mapping)

For `cmd_close`, add:

```rust
crate::state::buffer::BufferType::Mentions => {
    app.config.display.mentions_buffer = false;
    crate::config::save_config(&crate::constants::config_path(), &app.config).ok();
    app.state.buffers.remove(&buf_id);
    app.state.switch_to_previous_or_first();
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test`
Expected: all pass

- [ ] **Step 6: Commit**

```
git add src/state/buffer.rs src/state/sorting.rs src/web/snapshot.rs src/commands/handlers_ui.rs
git commit -m "feat(mentions): BufferType::Mentions variant, sort group 0, always-first sorting"
```

---

### Task 2: Config + /set wiring

**Files:**
- Modify: `src/config/mod.rs:105-140` (DisplayConfig)
- Modify: `src/commands/settings.rs` (get/set + tab completion)

- [ ] **Step 1: Add mentions_buffer to DisplayConfig**

In `src/config/mod.rs` DisplayConfig struct, add:

```rust
/// Show the Mentions buffer at the top of the buffer list.
pub mentions_buffer: bool,
```

In `Default for DisplayConfig`, add: `mentions_buffer: true,`

- [ ] **Step 2: Wire get/set in settings.rs**

Add `"display.mentions_buffer"` to the config path routing in `set_config_value` and `get_config_value`. Add to tab completion list.

In the `/set` handler, after successful set, add lifecycle handling:

```rust
if path == "display.mentions_buffer" {
    if app.config.display.mentions_buffer {
        app.create_mentions_buffer(); // will implement in Task 4
    } else {
        // Switch away if active, then remove
        if app.state.active_buffer_id.as_deref() == Some("_mentions") {
            app.state.switch_to_previous_or_first();
        }
        app.state.buffers.remove("_mentions");
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test`
Expected: all pass (create_mentions_buffer doesn't exist yet — the /set handler won't be called during tests)

- [ ] **Step 4: Commit**

```
git add src/config/mod.rs src/commands/settings.rs
git commit -m "feat(mentions): display.mentions_buffer config + /set wiring"
```

---

### Task 3: Database queries

**Files:**
- Modify: `src/storage/db.rs:82-84` (add timestamp index)
- Modify: `src/storage/query.rs:270-335` (new queries)

- [ ] **Step 1: Add timestamp index**

In `src/storage/db.rs`, add after `CREATE_MENTIONS_IDX`:

```rust
const CREATE_MENTIONS_TIMESTAMP_IDX: &str = "
CREATE INDEX IF NOT EXISTS idx_mentions_timestamp
ON mentions (timestamp)";
```

Add to `create_schema`:

```rust
db.execute_batch(CREATE_MENTIONS_TIMESTAMP_IDX)?;
```

- [ ] **Step 2: Add new query functions**

In `src/storage/query.rs`, add:

```rust
/// Load recent mentions for the mentions buffer.
/// Returns up to `limit` mentions newer than `since_ts` (Unix timestamp), oldest first.
pub fn load_recent_mentions(
    db: &Connection,
    since_ts: i64,
    limit: u32,
) -> rusqlite::Result<Vec<super::types::MentionRow>> {
    let mut stmt = db.prepare(
        "SELECT id, timestamp, network, buffer, channel, nick, text
         FROM mentions
         WHERE timestamp > ?1
         ORDER BY timestamp ASC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![since_ts, limit], |row| {
        Ok(super::types::MentionRow {
            id: row.get(0)?,
            timestamp: row.get(1)?,
            network: row.get(2)?,
            buffer: row.get(3)?,
            channel: row.get(4)?,
            nick: row.get(5)?,
            text: row.get(6)?,
        })
    })?;
    rows.collect()
}

/// Delete mentions older than the given Unix timestamp.
pub fn purge_old_mentions(db: &Connection, before_ts: i64) -> rusqlite::Result<usize> {
    db.execute(
        "DELETE FROM mentions WHERE timestamp < ?1",
        params![before_ts],
    )
}

/// Delete ALL mentions (used by /clear on mentions buffer).
pub fn truncate_mentions(db: &Connection) -> rusqlite::Result<usize> {
    db.execute("DELETE FROM mentions", [])
}
```

- [ ] **Step 3: Write tests for new queries**

```rust
#[test]
fn load_recent_mentions_returns_newest_within_window() {
    let db = setup_test_db();
    let now = chrono::Utc::now().timestamp();
    insert_mention(&db, now - 100, "net", "buf", "#ch", "nick", "old").unwrap();
    insert_mention(&db, now - 50, "net", "buf", "#ch", "nick", "mid").unwrap();
    insert_mention(&db, now, "net", "buf", "#ch", "nick", "new").unwrap();

    let rows = load_recent_mentions(&db, now - 200, 1000).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].text, "old"); // oldest first
    assert_eq!(rows[2].text, "new");
}

#[test]
fn load_recent_mentions_respects_limit() {
    let db = setup_test_db();
    let now = chrono::Utc::now().timestamp();
    for i in 0..10 {
        insert_mention(&db, now + i, "net", "buf", "#ch", "nick", &format!("msg{i}")).unwrap();
    }
    let rows = load_recent_mentions(&db, now - 1, 5).unwrap();
    assert_eq!(rows.len(), 5);
}

#[test]
fn purge_old_mentions_deletes_expired() {
    let db = setup_test_db();
    let now = chrono::Utc::now().timestamp();
    insert_mention(&db, now - 1000, "net", "buf", "#ch", "nick", "old").unwrap();
    insert_mention(&db, now, "net", "buf", "#ch", "nick", "new").unwrap();
    let deleted = purge_old_mentions(&db, now - 500).unwrap();
    assert_eq!(deleted, 1);
    let remaining = load_recent_mentions(&db, 0, 1000).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].text, "new");
}

#[test]
fn truncate_mentions_deletes_all() {
    let db = setup_test_db();
    let now = chrono::Utc::now().timestamp();
    insert_mention(&db, now, "net", "buf", "#ch", "nick", "msg").unwrap();
    truncate_mentions(&db).unwrap();
    let remaining = load_recent_mentions(&db, 0, 1000).unwrap();
    assert!(remaining.is_empty());
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test storage`
Expected: all pass

- [ ] **Step 5: Commit**

```
git add src/storage/db.rs src/storage/query.rs
git commit -m "feat(mentions): DB queries — load_recent, purge_old, truncate + timestamp index"
```

---

### Task 4: Mentions buffer creation + history load

**Files:**
- Modify: `src/app.rs` (create_mentions_buffer, load_mentions_history, startup wiring)
- Modify: `src/state/events.rs` (add_mention_to_buffer helper)

- [ ] **Step 1: Add add_mention_to_buffer to AppState**

In `src/state/events.rs`, add a new method on `AppState`:

```rust
/// Add a mention message to the `_mentions` buffer.
///
/// Unlike `add_message_with_activity`, this:
/// - Does NOT log to the messages DB (mention is already in the mentions table)
/// - Does NOT push a `MentionAlert` web event (avoids double-counting the badge)
/// - DOES push `NewMessage` for web clients
/// - DOES set `ActivityLevel::Mention` on the buffer
pub fn add_mention_to_buffer(&mut self, message: Message) {
    let buffer_id = "_mentions";
    let wire = crate::web::snapshot::message_to_wire(&message);
    self.pending_web_events
        .push(crate::web::protocol::WebEvent::NewMessage {
            buffer_id: buffer_id.to_string(),
            message: wire,
        });
    if let Some(buf) = self.buffers.get_mut(buffer_id) {
        buf.messages.push_back(message);
        // Cap at 1000 messages.
        while buf.messages.len() > 1000 {
            buf.messages.pop_front();
        }
        let is_active = self.active_buffer_id.as_deref() == Some(buffer_id);
        if !is_active && buf.activity < ActivityLevel::Mention {
            buf.activity = ActivityLevel::Mention;
            buf.unread_count += 1;
            self.pending_web_events
                .push(crate::web::protocol::WebEvent::ActivityChanged {
                    buffer_id: buffer_id.to_string(),
                    activity: ActivityLevel::Mention as u8,
                    unread_count: buf.unread_count,
                });
        }
    }
}
```

- [ ] **Step 2: Add create_mentions_buffer + load_mentions_history to App**

In `src/app.rs`, add methods:

```rust
/// Buffer ID for the mentions aggregation buffer.
const MENTIONS_BUFFER_ID: &'static str = "_mentions";

/// Create the mentions buffer if it doesn't already exist.
fn create_mentions_buffer(&mut self) {
    if self.state.buffers.contains_key(Self::MENTIONS_BUFFER_ID) {
        return;
    }
    let buf = crate::state::buffer::Buffer {
        id: Self::MENTIONS_BUFFER_ID.to_string(),
        connection_id: String::new(),
        buffer_type: crate::state::buffer::BufferType::Mentions,
        name: "Mentions".to_string(),
        messages: std::collections::VecDeque::new(),
        activity: crate::state::buffer::ActivityLevel::None,
        unread_count: 0,
        last_read: chrono::Utc::now(),
        topic: None,
        topic_set_by: None,
        users: std::collections::HashMap::new(),
        modes: None,
        mode_params: None,
        list_modes: std::collections::HashMap::new(),
        last_speakers: Vec::new(),
    };
    self.state
        .buffers
        .insert(Self::MENTIONS_BUFFER_ID.to_string(), buf);
    self.load_mentions_history();
}

/// Load recent mentions from DB into the mentions buffer.
fn load_mentions_history(&mut self) {
    let Some(storage) = &self.storage else { return };
    let Ok(db) = storage.db.lock() else { return };
    let seven_days_ago = chrono::Utc::now().timestamp() - 7 * 24 * 3600;
    let Ok(rows) = crate::storage::query::load_recent_mentions(&db, seven_days_ago, 1000) else {
        return;
    };
    drop(db);
    let Some(buf) = self.state.buffers.get_mut(Self::MENTIONS_BUFFER_ID) else {
        return;
    };
    for row in rows {
        buf.messages.push_back(Self::mention_row_to_message(&row));
    }
}

/// Convert a MentionRow to a Message for the mentions buffer.
fn mention_row_to_message(row: &crate::storage::types::MentionRow) -> crate::state::buffer::Message {
    let is_channel = row.channel.starts_with('#')
        || row.channel.starts_with('&')
        || row.channel.starts_with('!')
        || row.channel.starts_with('+');
    let text = if is_channel {
        format!("{} {}❯ {}", row.channel, row.nick, row.text)
    } else {
        format!("{}❯ {}", row.nick, row.text)
    };
    crate::state::buffer::Message {
        id: 0,
        timestamp: chrono::DateTime::from_timestamp(row.timestamp, 0)
            .unwrap_or_else(chrono::Utc::now),
        message_type: crate::state::buffer::MessageType::Message,
        nick: Some(row.network.clone()),
        nick_mode: None,
        text,
        highlight: true,
        event_key: None,
        event_params: None,
        log_msg_id: None,
        log_ref_id: None,
        tags: None,
    }
}
```

- [ ] **Step 3: Wire into startup**

In `App::run()`, after autoload scripts and before the event loop, add:

```rust
if self.config.display.mentions_buffer {
    self.create_mentions_buffer();
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: all pass

- [ ] **Step 5: Commit**

```
git add src/state/events.rs src/app.rs
git commit -m "feat(mentions): create_mentions_buffer, load history, add_mention_to_buffer helper"
```

---

### Task 5: Wire mention events into the buffer

**Files:**
- Modify: `src/irc/events.rs:739-855` (handle_privmsg — push to mentions buffer)

- [ ] **Step 1: Add mention-to-buffer push after PRIVMSG highlight detection**

In `handle_privmsg`, after the `add_message_with_activity` call for the channel buffer (around line 854-855), add:

```rust
// Push to mentions buffer if this is a highlight.
if is_mention && state.buffers.contains_key("_mentions") {
    let conn_label = state
        .connections
        .get(conn_id)
        .map_or(conn_id, |c| c.label.as_str())
        .to_string();
    let is_channel = target_is_channel;
    let mention_text = if is_channel {
        format!("{} {}❯ {}", target, nick, text)
    } else {
        format!("{}❯ {}", nick, text)
    };
    let mention_msg = Message {
        id: state.message_counter,
        timestamp: chrono::Utc::now(),
        message_type: MessageType::Message,
        nick: Some(conn_label),
        nick_mode: None,
        text: mention_text,
        highlight: true,
        event_key: None,
        event_params: None,
        log_msg_id: None,
        log_ref_id: None,
        tags: None,
    };
    state.message_counter += 1;
    state.add_mention_to_buffer(mention_msg);
}
```

Do the same for the ACTION (CTCP ACTION) mention path around line 770.

- [ ] **Step 2: Run tests**

Run: `cargo test`
Expected: all pass

- [ ] **Step 3: Commit**

```
git add src/irc/events.rs
git commit -m "feat(mentions): wire PRIVMSG/ACTION highlights into _mentions buffer"
```

---

### Task 6: /mentions command + /clear + /close handling

**Files:**
- Modify: `src/commands/handlers_admin.rs:1092-1148` (repurpose cmd_mentions)
- Modify: `src/commands/handlers_ui.rs:166-170` (cmd_clear mentions DB truncate)
- Modify: `src/commands/handlers_ui.rs:172-243` (cmd_close Mentions arm)

- [ ] **Step 1: Repurpose cmd_mentions**

Replace the entire `cmd_mentions` function:

```rust
pub(crate) fn cmd_mentions(app: &mut App, _args: &[String]) {
    if !app.config.display.mentions_buffer {
        add_local_event(
            app,
            &format!("{C_ERR}Mentions buffer is disabled — /set display.mentions_buffer true{C_RST}"),
        );
        return;
    }
    if !app.state.buffers.contains_key(App::MENTIONS_BUFFER_ID) {
        app.create_mentions_buffer();
    }
    app.state.set_active_buffer(App::MENTIONS_BUFFER_ID);
    app.scroll_offset = 0;
}
```

- [ ] **Step 2: Handle /clear on mentions buffer**

In `cmd_clear`, add DB truncation:

```rust
pub(crate) fn cmd_clear(app: &mut App, _args: &[String]) {
    let is_mentions = app
        .state
        .active_buffer()
        .is_some_and(|b| b.buffer_type == crate::state::buffer::BufferType::Mentions);
    if let Some(buf) = app.state.active_buffer_mut() {
        buf.messages.clear();
        buf.messages.shrink_to(0);
    }
    // Truncate the mentions DB table when clearing the mentions buffer.
    if is_mentions {
        if let Some(storage) = &app.storage {
            if let Ok(db) = storage.db.lock() {
                crate::storage::query::truncate_mentions(&db).ok();
            }
        }
    }
}
```

- [ ] **Step 3: Handle /close on mentions buffer**

In `cmd_close`, add a match arm for `BufferType::Mentions`:

```rust
crate::state::buffer::BufferType::Mentions => {
    app.config.display.mentions_buffer = false;
    let cfg_path = crate::constants::config_path();
    crate::config::save_config(&cfg_path, &app.config).ok();
    app.state.buffers.remove(&buf_id);
    app.state.switch_to_previous_or_first();
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: all pass

- [ ] **Step 5: Commit**

```
git add src/commands/handlers_admin.rs src/commands/handlers_ui.rs
git commit -m "feat(mentions): /mentions switches to buffer, /clear truncates DB, /close disables"
```

---

### Task 7: Buffer list rendering — skip number + header for Mentions

**Files:**
- Modify: `src/ui/buffer_list.rs:30-60`

- [ ] **Step 1: Skip connection header and number for Mentions**

In the buffer list render loop, add early handling for Mentions:

After the `DEFAULT_CONN_ID` skip (line 39-41), add:

```rust
// Mentions buffer: render without number prefix or connection header.
if buf.buffer_type == crate::state::buffer::BufferType::Mentions {
    // Render using activity-level theme (same as numbered items, but no number).
    let activity = buf.activity as u8;
    let fmt_key = format!("item_activity_{activity}");
    let fmt = sidepanel
        .get(fmt_key.as_str())
        .or_else(|| sidepanel.get("item"))
        .cloned()
        .unwrap_or_else(|| "$0".to_string());
    let resolved = resolve_abstractions(&fmt, abstracts, 0);
    let spans = parse_format_string(&resolved, &[&buf.name]);
    // ... render spans to line (follow existing pattern for item rendering)
    continue;
}
```

Also skip connection header when `buf.connection_id.is_empty()`:

```rust
if buf.connection_id != last_conn_id && !buf.connection_id.is_empty() {
```

- [ ] **Step 2: Run app visually to verify sidebar**

Run: `cargo run`
Expected: Mentions buffer appears at top with no number, no blank header

- [ ] **Step 3: Run tests + clippy**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: all pass, 0 warnings

- [ ] **Step 4: Commit**

```
git add src/ui/buffer_list.rs
git commit -m "feat(mentions): buffer list renders Mentions without number or connection header"
```

---

### Task 8: Periodic purge + mention_count fix

**Files:**
- Modify: `src/app.rs` (purge in tick, mention_count in snapshot)

- [ ] **Step 1: Add periodic mention purge**

Add a `last_mention_purge: Instant` field to `App`, initialized to `Instant::now()`.

In the 1-second tick arm (near `maybe_purge_old_events`), add:

```rust
self.maybe_purge_old_mentions();
```

Implement the method (follows same pattern as `maybe_purge_old_events`):

```rust
/// Purge mentions older than 7 days from DB and in-memory buffer.
fn maybe_purge_old_mentions(&mut self) {
    if self.last_mention_purge.elapsed() < Duration::from_secs(3600) {
        return;
    }
    self.last_mention_purge = Instant::now();

    let seven_days_ago = chrono::Utc::now().timestamp() - 7 * 24 * 3600;

    // Purge from DB.
    if let Some(storage) = &self.storage {
        let db = std::sync::Arc::clone(&storage.db);
        tokio::task::spawn_blocking(move || {
            if let Ok(db) = db.lock() {
                crate::storage::query::purge_old_mentions(&db, seven_days_ago).ok();
            }
        });
    }

    // Prune in-memory buffer.
    if let Some(buf) = self.state.buffers.get_mut(Self::MENTIONS_BUFFER_ID) {
        let cutoff = chrono::DateTime::from_timestamp(seven_days_ago, 0)
            .unwrap_or_else(chrono::Utc::now);
        buf.messages.retain(|m| m.timestamp >= cutoff);
        // Enforce 1000 cap.
        while buf.messages.len() > 1000 {
            buf.messages.pop_front();
        }
    }
}
```

- [ ] **Step 2: Fix mention_count in web snapshot**

In the 1-second tick arm where `build_sync_init` is called, replace the hardcoded `0` with actual count from DB:

```rust
let mention_count = self.storage.as_ref().and_then(|s| {
    s.db.lock().ok().and_then(|db| {
        crate::storage::query::get_unread_mention_count(&db).ok()
    })
}).unwrap_or(0);
let init = crate::web::snapshot::build_sync_init(&self.state, mention_count, &self.config.web.timestamp_format);
```

- [ ] **Step 3: Run tests + clippy**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: all pass

- [ ] **Step 4: Commit**

```
git add src/app.rs
git commit -m "feat(mentions): periodic 7-day purge, fix web mention_count from DB"
```

---

### Task 9: Web frontend — pin mentions buffer to top

**Files:**
- Modify: `web-ui/src/components/buffer_list.rs`

- [ ] **Step 1: Pin mentions buffer in web sidebar**

In the buffer list component, when rendering buffers, sort mentions-type buffer to top:

```rust
// In the buffer rendering closure, check buffer_type:
let is_mentions = buf.buffer_type == "mentions";
// Render without connection group header, pin to top via CSS or sort order.
```

The server-side `sort_buffers` already pins it first, so `SyncInit` delivers it first. The web just needs to skip the connection header for `buffer_type == "mentions"`.

- [ ] **Step 2: Build web frontend**

Run: `cd web-ui && trunk build --release`
Copy dist to static/web.

- [ ] **Step 3: Commit**

```
git add web-ui/src/components/buffer_list.rs static/web/
git commit -m "feat(mentions): web frontend pins mentions buffer to top"
```

---

### Task 10: Final integration test + cleanup

**Files:**
- All modified files

- [ ] **Step 1: Run full test suite**

Run: `cargo test`
Expected: all pass (should be 860+ tests now)

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: 0 warnings

- [ ] **Step 3: Manual smoke test**

1. Start repartee, verify Mentions buffer at top of sidebar
2. Receive a highlight on a channel — verify it appears in Mentions buffer
3. Switch to Mentions buffer — verify activity clears
4. `/clear` in Mentions buffer — verify messages cleared and DB empty
5. `/set display.mentions_buffer false` — verify buffer disappears
6. `/set display.mentions_buffer true` — verify buffer reappears with history
7. `/close` on Mentions buffer — verify it disappears and config saved

- [ ] **Step 4: Commit all remaining changes**

```
git add -A
git commit -m "feat(mentions): mentions buffer complete — persistent, sortable, purgeable"
```

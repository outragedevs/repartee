# Web Server Hot Restart Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow `/set web.enabled`, `/set web.port`, `/set web.bind_address`, and `/set web.password` to take effect immediately without restarting Repartee.

**Architecture:** Extract the web server startup block from `App::run()` into `App::start_web_server()` and add `App::stop_web_server()`. The `/set` handler calls stop+start when any `web.*` setting that affects the server lifecycle changes. Shared state (`web_broadcaster`, `web_cmd_tx/rx`) survives restart; per-session state (sessions, rate limiter, snapshot) is recreated. Existing WebSocket connections are dropped on restart (clients reconnect automatically via their persistent session logic).

**Tech Stack:** Rust, tokio (task abort/spawn), axum, existing `web::server::start()`.

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `src/app.rs` | **Modify** | Extract `start_web_server()` + `stop_web_server()` methods from inline startup block |
| `src/commands/settings.rs` | **Modify** | Call `stop_web_server()` + `start_web_server()` on lifecycle-affecting `/set web.*` changes |

---

## Chunk 1: Extract and Wire

### Task 1: Extract `start_web_server()` and `stop_web_server()` methods

**Files:**
- Modify: `src/app.rs`

The startup block at lines 1540-1586 is currently inlined in `App::run()`. Extract it into an `async` method. Add a stop method that aborts the server task and clears per-session state.

- [ ] **Step 1: Add `stop_web_server()` method to `App`**

Add near the other web-related methods (after `drain_pending_web_events`):

```rust
/// Stop the web server if running. Aborts the accept loop task and
/// clears per-session state (sessions, rate limiter, snapshot).
/// The `web_broadcaster` and `web_cmd_tx/rx` channel survive — they
/// are owned by `App` and reused across restarts.
fn stop_web_server(&mut self) {
    if let Some(handle) = self.web_server_handle.take() {
        handle.abort();
        tracing::info!("web server stopped");
    }
    self.web_sessions = None;
    self.web_rate_limiter = None;
    self.web_state_snapshot = None;
}
```

- [ ] **Step 2: Add `start_web_server()` async method**

Extract the block from `App::run()` lines 1540-1586 into a method. Note: `start()` is async so this method must be async too.

```rust
/// Start the web server (HTTPS + WebSocket). Creates fresh session
/// store, rate limiter, and state snapshot. Reuses the existing
/// `web_broadcaster` and `web_cmd_tx` channel.
///
/// Does nothing if `web.enabled` is false or `web.password` is empty.
async fn start_web_server(&mut self) {
    if !self.config.web.enabled {
        return;
    }
    if self.config.web.password.is_empty() {
        tracing::warn!("web.enabled=true but web.password is empty — set WEB_PASSWORD in .env");
        crate::commands::helpers::add_local_event(
            self,
            "web.enabled=true but web.password is empty — set WEB_PASSWORD in .env",
        );
        return;
    }

    let sessions = std::sync::Arc::new(tokio::sync::Mutex::new(
        crate::web::auth::SessionStore::with_hours(self.config.web.session_hours),
    ));
    let limiter = std::sync::Arc::new(tokio::sync::Mutex::new(
        crate::web::auth::RateLimiter::new(),
    ));
    self.web_sessions = Some(std::sync::Arc::clone(&sessions));
    self.web_rate_limiter = Some(std::sync::Arc::clone(&limiter));

    let snapshot = std::sync::Arc::new(std::sync::RwLock::new(
        crate::web::server::WebStateSnapshot {
            buffers: Vec::new(),
            connections: Vec::new(),
            mention_count: 0,
            active_buffer_id: None,
            timestamp_format: self.config.web.timestamp_format.clone(),
        },
    ));
    self.web_state_snapshot = Some(std::sync::Arc::clone(&snapshot));

    let handle = std::sync::Arc::new(crate::web::server::AppHandle {
        broadcaster: std::sync::Arc::clone(&self.web_broadcaster),
        web_cmd_tx: self.web_cmd_tx.clone(),
        password: self.config.web.password.clone(),
        session_store: sessions,
        rate_limiter: limiter,
        web_state_snapshot: Some(snapshot),
    });

    match crate::web::server::start(&self.config.web, handle).await {
        Ok(h) => {
            self.web_server_handle = Some(h);
            tracing::info!(
                "web frontend at https://{}:{}",
                self.config.web.bind_address,
                self.config.web.port
            );
            crate::commands::helpers::add_local_event(
                self,
                &format!(
                    "Web server listening on https://{}:{}",
                    self.config.web.bind_address, self.config.web.port
                ),
            );
        }
        Err(e) => {
            tracing::error!("failed to start web server: {e}");
            crate::commands::helpers::add_local_event(
                self,
                &format!("Failed to start web server: {e}"),
            );
        }
    }
}
```

- [ ] **Step 3: Replace inline startup in `App::run()` with method call**

Replace lines 1540-1586 in `App::run()`:

```rust
// Before (inlined block):
if self.config.web.enabled && !self.config.web.password.is_empty() {
    // ... 45 lines of setup ...
} else if self.config.web.enabled && self.config.web.password.is_empty() {
    tracing::warn!("web.enabled=true but web.password is empty — set WEB_PASSWORD in .env");
}

// After:
self.start_web_server().await;
```

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: PASS (no behavioral change — same code, just extracted)

- [ ] **Step 5: Commit**

```bash
git add src/app.rs
git commit -m "refactor: extract start_web_server/stop_web_server methods"
```

---

### Task 2: Wire `/set web.*` to hot restart

**Files:**
- Modify: `src/commands/settings.rs`

The `/set` handler in `cmd_set()` needs to call `stop_web_server()` + `start_web_server()` when any lifecycle-affecting web setting changes.

**Lifecycle-affecting settings:** `web.enabled`, `web.port`, `web.bind_address`, `web.password`, `web.tls_cert`, `web.tls_key`, `web.session_hours`.

**Non-lifecycle settings** (already handled via broadcast): `web.timestamp_format`, `web.line_height`, `web.theme`, `web.nick_column_width`, `web.nick_max_length`.

**Problem:** `cmd_set` is a sync function (`fn cmd_set(app: &mut App, args: &[String])`) but `start_web_server` is async. We can't `.await` in a sync context.

**Solution:** Use a flag on `App` that the main event loop checks. The pattern already exists for `should_quit` and `should_detach`. Add `web_restart_pending: bool`. The main loop checks it after processing commands and calls the async methods.

- [ ] **Step 1: Add `web_restart_pending` field to `App`**

In `src/app.rs`, add to the `App` struct (near `web_server_handle`):

```rust
/// Flag to trigger web server restart in the next event loop iteration.
/// Set by `/set web.*` when a lifecycle-affecting setting changes.
pub web_restart_pending: bool,
```

Initialize to `false` in `App::new()`.

- [ ] **Step 2: Handle the flag in the main event loop**

In `src/app.rs`, in the main `tokio::select!` loop, after the terminal event arm processes commands (where `should_quit` is checked), add:

```rust
if self.web_restart_pending {
    self.web_restart_pending = false;
    self.stop_web_server();
    self.start_web_server().await;
}
```

Find the right location: after `self.handle_key_event(key)` or `self.handle_submit()` returns, before the next `select!` iteration. The terminal event arm already has a block after processing input — add it there.

- [ ] **Step 3: Set the flag in `/set` handler**

In `src/commands/settings.rs`, in `cmd_set()`, after the existing web broadcast block (around line 680), add:

```rust
// Hot restart web server when lifecycle settings change.
if matches!(path,
    "web.enabled" | "web.port" | "web.bind_address" | "web.password"
    | "web.tls_cert" | "web.tls_key" | "web.session_hours"
) {
    app.web_restart_pending = true;
    if path != "web.enabled" || raw == "true" {
        ev(app, &format!("{C_DIM}Web server will restart...{C_RST}"));
    }
}
```

Also remove the old "Restart to start the web server." message from the `web.password` handler (line 613).

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/app.rs src/commands/settings.rs
git commit -m "feat: hot restart web server on /set web.* changes"
```

---

### Task 3: User feedback + edge cases

**Files:**
- Modify: `src/app.rs` (add status message for disable case)

- [ ] **Step 1: Handle `/set web.enabled false` gracefully**

When `web.enabled` is set to `false`, `stop_web_server()` runs but `start_web_server()` exits early (the enabled check). Add a user-facing message in `stop_web_server()`:

```rust
fn stop_web_server(&mut self) {
    if let Some(handle) = self.web_server_handle.take() {
        handle.abort();
        tracing::info!("web server stopped");
        crate::commands::helpers::add_local_event(self, "Web server stopped");
    }
    self.web_sessions = None;
    self.web_rate_limiter = None;
    self.web_state_snapshot = None;
}
```

- [ ] **Step 2: Run full test suite**

Run: `cargo test`
Expected: PASS

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: No new warnings

- [ ] **Step 4: Commit**

```bash
git add src/app.rs
git commit -m "feat: user feedback messages for web server start/stop"
```

---

## Summary

| Task | Description | Files |
|------|-------------|-------|
| 1 | Extract `start_web_server()` / `stop_web_server()` | `src/app.rs` |
| 2 | Wire `/set web.*` to hot restart via flag | `src/app.rs`, `src/commands/settings.rs` |
| 3 | User feedback + edge cases | `src/app.rs` |

**Total: 3 tasks, ~60 lines of new code, 0 new dependencies.**

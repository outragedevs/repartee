# Minor Issues — Codebase Audit 2026-03-09

## M1 — `extract_nick` clones from borrowed prefix on every message

- **File:** `src/irc/formatting.rs:170-191`
- **Status:** [x] SKIPPED (large signature change; flagged for future refactor)
- **Issue:** Both `extract_nick` and `extract_nick_userhost` clone nick, user, and host strings from the IRC `Prefix`. Callers mostly use results as `&str`. Returning `(&str, &str, &str)` lifetied to the prefix would eliminate per-message allocations.
- **Scope:** Large signature change across `events.rs`. Flag for future refactor.

---

## M2 — `Instant::now()` called per iteration in nick-change loop

- **File:** `src/irc/events.rs:1338`
- **Status:** [x] DONE
- **Issue:** `should_suppress_nick_flood(buf_id, Instant::now())` is called once per buffer in the `affected` loop. For nick changes affecting many channels, this means many syscalls.
- **Fix:** Capture `let now = Instant::now();` before the loop.

---

## M3 — `write!(msg_text, ...).unwrap()` on infallible String write

- **File:** `src/irc/events.rs:212, 1978`
- **Status:** [x] DONE
- **Issue:** `write!` on `String` is infallible but `.unwrap()` is present. Violates the project guideline against `unwrap()` in production.
- **Fix:** Use `let _ = write!(...)` or `msg_text.push_str(&format!(...))`.

---

## M4 — `Style::default().fg(x)` throughout UI instead of Stylize trait

- **File:** All UI files (`status_line.rs`, `input.rs`, `topic_bar.rs`, `buffer_list.rs`, `nick_list.rs`, `layout.rs`, `chat_view.rs`, `styled_text.rs`)
- **Status:** [x] SKIPPED (large-scale cosmetic refactor; low risk, low priority)
- **Issue:** ratatui 0.30+ exports the `Stylize` trait. `Style::default().fg(color)` is verbose and non-idiomatic. The Stylize approach (`.bold()`, `.cyan()`, `.dim()`) is the recommended pattern.
- **Scope:** Large-scale cosmetic change across all render files.

---

## M5 — `unwrap_or(Color::Black/White)` fallbacks

- **File:** All render files
- **Status:** [x] DONE
- **Issue:** `hex_to_color(&colors.bg).unwrap_or(Color::Black)` — hardcoded black/white don't adapt to terminal themes. CLAUDE.md guideline says avoid hardcoded `Color::White/Black`.
- **Fix:** Use `Color::Reset` (terminal default) as fallback: `hex_to_color(&colors.bg).unwrap_or(Color::Reset)`.

---

## M6 — `Vec<Constraint>` heap-allocated for max 3 elements

- **File:** `src/ui/layout.rs:50-57`
- **Status:** [x] DONE
- **Issue:** Allocates a `Vec` for at most 3 `Constraint` values. Fixed-size array or direct branch-per-case eliminates the allocation.
- **Fix:** Match on `(left_visible, show_nicklist)` and call `Layout::horizontal` with inline arrays.

---

## M7 — Timer `JoinHandle`s not aborted on shutdown

- **File:** `src/app.rs:351, 2861-2882`
- **Status:** [x] DONE
- **Issue:** Dropping a `JoinHandle` detaches but does not cancel the task. Script timer tasks continue running after app quits, sending on a dropped channel.
- **Fix:** Abort all timer handles on shutdown:

```rust
for (_, handle) in self.active_timers.drain() {
    handle.abort();
}
```

---

## M8 — Untracked forwarder task in `connect_server_async`

- **File:** `src/app.rs:823-829`
- **Status:** [x] DONE
- **Issue:** The event-forwarding task's `JoinHandle` is thrown away. If `irc_rx` receiver is dropped, the task spins silently failing. Inconsistent with the `spawn_reconnect` path.
- **Fix:** Store the handle or restructure to match the reconnect pattern.

---

## M9 — `get_command_names()` allocates Vec per call

- **File:** `src/commands/registry.rs:100-112`
- **Status:** [x] DONE
- **Issue:** Builds and sorts a fresh `Vec<&'static str>` each call. The main command table uses `LazyLock` but this helper does not.
- **Fix:** Make it a `LazyLock<Vec<&'static str>>` returning `&'static [&'static str]`.

---

## M10 — `APP_VERSION` and `env_path()` have `#[allow(dead_code)]`

- **File:** `src/constants.rs:2-3, 31-34`
- **Status:** [x] DONE
- **Issue:** If genuinely unused, remove them. If needed (e.g. CTCP VERSION), wire them in and remove the `#[allow(dead_code)]`.

---

## M11 — `ensure_config_dir` discards all errors silently

- **File:** `src/constants.rs:46-65`
- **Status:** [x] DONE
- **Issue:** All `create_dir_all`, `save_config`, and `fs::write` calls use `let _ =` to discard errors. If the config directory cannot be created, the app proceeds silently and fails later with a confusing error.
- **Fix:** Replace `let _ =` with `if let Err(e) = ... { tracing::warn!(...) }`.

---

## M12 — `_has_fts` parameter unused in `flush_blocking`

- **File:** `src/storage/writer.rs:87`
- **Status:** [x] DONE
- **Issue:** The parameter is accepted but never used (prefixed with `_`). FTS is maintained by database triggers, so no manual FTS insertion is needed.
- **Fix:** Remove `has_fts`/`_has_fts` from `flush_blocking`, `writer_loop`, and `LogWriterHandle::spawn`.

---

## M13 — `migrate_schema` swallows all errors

- **File:** `src/storage/db.rs:75-82`
- **Status:** [x] DONE
- **Issue:** All `ALTER TABLE` errors are silently swallowed, not just "duplicate column" errors. Permissions, corruption, or wrong table name would be invisible.
- **Fix:**

```rust
if let Err(e) = db.execute_batch(&sql) {
    if !e.to_string().contains("duplicate column name") {
        tracing::warn!("migration warning for '{col}': {e}");
    }
}
```

---

## M14 — `open_database` (in-memory) is `pub` but test-only

- **File:** `src/storage/db.rs:84`
- **Status:** [x] DONE
- **Issue:** The in-memory variant is public but never called from production code.
- **Fix:** Change to `pub(crate)` or gate with `#[cfg(test)]`.

---

## M15 — `block_on` inside `spawn_blocking` in image preview

- **File:** `src/image_preview/mod.rs:145-150`
- **Status:** [x] DONE
- **Issue:** `handle.block_on(fetch::fetch_image(...))` inside `spawn_blocking` is fragile. If the runtime is `current_thread` (some test contexts), this deadlocks.
- **Fix:** Restructure so `fetch_image` is awaited outside `spawn_blocking`, and only the CPU-bound decode+encode step is in `spawn_blocking`.

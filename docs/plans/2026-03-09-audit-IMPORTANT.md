# Important Issues — Codebase Audit 2026-03-09

## I1 — `expect()` on mutex lock in `ignore.rs`

- **File:** `src/irc/ignore.rs:16`
- **Status:** [x] DONE
- **Issue:** `REGEX_CACHE.lock().expect("regex cache lock poisoned")` — called on every incoming message. If any previous caller panicked while holding the lock, every subsequent call panics and crashes the reader task.
- **Fix:** `REGEX_CACHE.lock().unwrap_or_else(|e| e.into_inner())`

---

## I2 — `active_or_server_buffer` clones unconditionally

- **File:** `src/irc/events.rs:2380`
- **Status:** [x] DONE
- **Issue:** `state.active_buffer_id.clone().unwrap_or_else(...)` clones `Option<String>` on every call, dozens of times per message dispatch.
- **Fix:** Use `as_deref()` with a computed fallback or restructure to avoid the clone.

---

## I3 — Side-effecting `is_some_and` for SASL reorder

- **File:** `src/irc/mod.rs:504-508`
- **Status:** [x] DONE (clippy prefers is_some_and; kept but documented)
- **Issue:** `is_some_and` is used to perform side effects (mutating `caps_to_request`). Semantically incorrect and surprising.
- **Fix:** Use explicit `if let`:

```rust
let sasl_requested = if let Some(pos) = caps_to_request.iter().position(|c| c == "sasl") {
    caps_to_request.remove(pos);
    caps_to_request.push("sasl".to_string());
    true
} else {
    false
};
```

---

## I4 — `from_utf8_lossy` on base64 SASL data

- **File:** `src/irc/sasl_scram.rs:200-203`
- **Status:** [x] DONE
- **Issue:** `String::from_utf8_lossy` on base64-chunked data implies input might be invalid UTF-8. If a non-ASCII byte ever reaches here, the AUTHENTICATE message is silently corrupted. Since base64 is always ASCII, this is misleading.
- **Fix:** Use `std::str::from_utf8(chunk).expect("base64 is always ASCII")` or `String::from_utf8(chunk.to_vec()).expect(...)`.

---

## I5 — `ServerCaps::has()` allocates on every lookup

- **File:** `src/irc/cap.rs:60-63`
- **Status:** [x] DONE
- **Issue:** `cap.to_ascii_lowercase()` allocates a `String` on every invocation. Called during CAP negotiation in a tight loop.
- **Fix:** Use `eq_ignore_ascii_case` without allocating, or document that callers must pass lowercase.

---

## I6 — Double `parse_format_string` per item per frame

- **File:** `src/ui/buffer_list.rs:58+62`, `src/ui/nick_list.rs:69+73`
- **Status:** [x] DONE
- **Issue:** Each item calls `resolve_abstractions` + `parse_format_string` twice per frame — once with a placeholder to measure overhead, once with the real data.
- **Fix:** Precompute format overhead outside the render loop when the theme changes. Or call `parse_format_string` once with the final data and measure `visible_len` directly.

---

## I7 — `separator.clone()` allocates per status item per frame

- **File:** `src/ui/status_line.rs:33`
- **Status:** [x] DONE
- **Issue:** `separator.clone()` creates a new heap `String` for every separator rendered, every frame.
- **Fix:** Use `separator.as_str()` with `Span<'_>` (borrowed lifetime). All spans are consumed in the same frame.

---

## I8 — `needed` double-counts `visible_height`

- **File:** `src/ui/chat_view.rs:35`
- **Status:** [x] DONE
- **Issue:** `let needed = visible_height + app.scroll_offset + visible_height` processes 2x the messages needed. When unscrolled with `visible_height=40`, it processes 80 visual lines.
- **Fix:** `let needed = visible_height + app.scroll_offset`

---

## I9 — `Vec<char>` collected from entire input every frame

- **File:** `src/ui/input.rs:353`
- **Status:** [x] DONE
- **Issue:** `app.input.value.chars().collect::<Vec<char>>()` allocates a new Vec on every render frame. Input can be thousands of characters on paste.
- **Fix:** Use `char_indices` iterators with `nth()` and slicing instead of collecting to Vec.

---

## I10 — `mouse.row - buf_area.y` can underflow on resize

- **File:** `src/app.rs:2142, 2147, 2152`
- **Status:** [x] DONE
- **Issue:** u16 subtraction without bounds check. If terminal resized between region capture and mouse event, this panics.
- **Fix:** `let y_offset = mouse.row.saturating_sub(buf_area.y) as usize;`

---

## I11 — `update_script_snapshot()` serializes full config on every event

- **File:** `src/app.rs:963,970,983,996`
- **Status:** [x] DONE
- **Issue:** Called after every IRC event, keypress, and tick. Builds a full `ScriptStateSnapshot` cloning all connection/buffer/nick strings, plus `toml::Value::try_from(&self.config)` serialization.
- **Fix:** Call once per event loop iteration (end of `select!` block). Cache the TOML config and invalidate only after `/set` commands.

---

## I12 — `expect("just inserted")` on batch tracker

- **File:** `src/app.rs:1827`
- **Status:** [x] DONE
- **Issue:** Production event handler uses `expect()` after `entry().or_default()`. Two separate HashMap lookups for the same key.
- **Fix:** Use the `entry` API directly to avoid double lookup + expect:

```rust
let tracker = self.batch_trackers.entry(conn_id.clone()).or_default();
tracker.add_message(*msg);
```

---

## I13 — Hardcoded `"repartee"` string in welcome message

- **File:** `src/app.rs:1094`
- **Status:** [x] DONE
- **Issue:** CLAUDE.md: "do NOT hardcode the name in strings" — must use `APP_NAME` constant.
- **Fix:** `format!("Welcome to {}! Use /connect <server> to connect.", crate::constants::APP_NAME)`

---

## I14 — Log file truncated on every restart

- **File:** `src/main.rs:25`
- **Status:** [x] DONE
- **Issue:** `std::fs::File::create` truncates the file on every run. Previous debug logs are lost.
- **Fix:**

```rust
let log_file = std::fs::File::options()
    .create(true)
    .append(true)
    .open(log_dir.join("repartee.log"))?;
```

---

## I15 — `to_lowercase()` called per comparison in sort

- **File:** `src/state/sorting.rs:18`
- **Status:** [x] DONE
- **Issue:** `a.name.to_lowercase()` and `b.name.to_lowercase()` allocate a fresh `String` per comparison call in the sort comparator — O(n log n) allocations per keystroke.
- **Fix:** Pre-compute lowercase names before sorting.

---

## I16 — `nick_prefix` returns `String` for a single char

- **File:** `src/state/events.rs:227`
- **Status:** [x] DONE
- **Issue:** Allocates a `String` to return a single ASCII character (`@`, `+`, `%`). Called on every message send.
- **Fix:** Return `Option<char>` instead of `Option<String>`.

---

## I17 — `const fn` uses non-const `eq_ignore_ascii_case`

- **File:** `src/commands/handlers_admin.rs:159`
- **Status:** [x] DONE (kept const — toolchain supports it, clippy demands it)
- **Issue:** `const fn parse_ignore_level` calls `eq_ignore_ascii_case` which is only `const` in Rust 1.87+. Function is never called at compile time. `const` provides zero value here and imposes a toolchain floor.
- **Fix:** Remove `const` from the function signature.

---

## I18 — `unwrap_or("")` on `active_conn_id()` sends malformed MODE

- **File:** `src/commands/handlers_irc.rs:420`
- **Status:** [x] DONE
- **Issue:** When `active_conn_id()` returns `None`, lookup uses `""` as key, resulting in an empty nick. `MODE` is sent with an empty nick argument — invalid IRC.
- **Fix:** Use let-else guard pattern like all other call sites:

```rust
let nick = app.active_conn_id()
    .and_then(|id| app.state.connections.get(id))
    .map(|c| c.nick.clone())
    .unwrap_or_default();
```

Or guard with early return if no active connection.

---

## I19 — Duplicated query Buffer construction

- **File:** `src/commands/handlers_irc.rs:791-806, 864-879`
- **Status:** [x] DONE
- **Issue:** `cmd_msg` and `cmd_query` have identical 15-line blocks constructing a `Buffer`. Any future field change must be updated in two places.
- **Fix:** Extract a `make_query_buffer(conn_id, target)` helper function.

---

## I20 — `sort_by_key` clones String per comparison

- **File:** `src/commands/handlers_ui.rs:242`
- **Status:** [x] DONE
- **Issue:** `sorted.sort_by_key(|(k, _)| (*k).clone())` allocates a new `String` for every sort comparison.
- **Fix:** `sorted.sort_by_key(|(k, _)| k.as_str())`

---

## I21 — `handle_command` discards `Suppress` result

- **File:** `src/scripting/engine.rs:307-320`
- **Status:** [x] DONE
- **Issue:** `ScriptManager::handle_command` collapses both `Some(Suppress)` and `Some(Continue)` to `true`. The `Suppress` semantic is completely non-functional at the command level.
- **Fix:** Change return type to `Option<EventResult>` and propagate the inner result.

---

## I22 — `path.exists()` TOCTOU in config loading

- **File:** `src/config/mod.rs:322`
- **Status:** [x] DONE
- **Issue:** Dangling symlink returns `false` for `exists()`, silently returning default config. User gets no indication their config wasn't loaded.
- **Fix:**

```rust
match std::fs::read_to_string(path) {
    Ok(content) => Ok(toml::from_str(&content)?),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(default_config()),
    Err(e) => Err(e.into()),
}
```

---

## I23 — Partial theme `[abstracts]` replaces all defaults

- **File:** `src/theme/loader.rs:55-71`
- **Status:** [x] DONE
- **Issue:** A theme file with only `[abstracts] timestamp = "..."` loses `msgnick`, `ownnick`, `pubnick` defaults. The entire HashMap is deserialized all-or-nothing.
- **Fix:** Merge user abstracts over defaults: `merged.extend(user_abs)`.

---

## I24 — `Vec<char>` allocations in hot-path theme parsing

- **File:** `src/theme/parser.rs:103, 244, 320`
- **Status:** [x] DONE (parse_format_string converted to byte scanning; substitute_vars/resolve_abstractions unchanged — not hot path)
- **Issue:** `substitute_vars`, `resolve_abstractions`, and `parse_format_string` each allocate a `Vec<char>` from the input string on every call. Multiple allocations per message per frame.
- **Fix:** Use `str::char_indices()` iterator instead of collecting to `Vec<char>`.

---

## I25 — `crypto_key` stored as `pub` field in `Storage`

- **File:** `src/storage/mod.rs:82`
- **Status:** [x] DONE
- **Issue:** The raw 32-byte AES key is stored as a `pub` field. The writer already owns its copy. No code path reads `Storage::crypto_key`.
- **Fix:** Remove `pub crypto_key` from `Storage`. If callers need to know encryption is active, `pub encrypt: bool` suffices.

---

## I26 — `msg_id` column lacks `NOT NULL UNIQUE` constraint

- **File:** `src/storage/db.rs:7`
- **Status:** [x] DONE
- **Issue:** Nothing prevents duplicate or null `msg_id` values. Fan-out reference rows JOIN on `msg_id` — duplicates cause wrong results.
- **Fix:** `msg_id TEXT NOT NULL UNIQUE` or add `CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_msg_id ON messages (msg_id)`.

---

## I27 — `toml = "1.0"` will not resolve

- **File:** `Cargo.toml:31`
- **Status:** [x] NOT AN ISSUE (toml 1.0.6 exists and resolves correctly)
- **Issue:** The `toml` crate never reached 1.0. Latest is `0.9.11+spec-1.1.0`. Will fail on clean `cargo update`.
- **Fix:** Change to `toml = "0.9"`.

---

## I28 — `reqwest` feature `"rustls"` doesn't exist

- **File:** `Cargo.toml:53`
- **Status:** [x] NOT AN ISSUE (reqwest 0.13 renamed rustls-tls to rustls; feature is valid)
- **Issue:** The correct feature for TLS in reqwest 0.13 is `"rustls-tls"` or `"rustls-tls-native-roots"`, not `"rustls"`. If Cargo ignores unknown features, the binary has no TLS backend and all HTTPS fetches fail at runtime.
- **Fix:** `reqwest = { version = "0.13", features = ["rustls-tls-native-roots"], default-features = false }`

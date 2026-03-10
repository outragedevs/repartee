# Critical Issues — Codebase Audit 2026-03-09

## C1 — Lua sandbox incomplete: `debug` and `load`/`loadstring` not removed

- **File:** `src/scripting/lua/mod.rs:74,86`
- **Status:** [x] DONE
- **Issue:** `Lua::new()` loads `ALL_SAFE` which includes `debug`. The `sandbox()` function only nils out `os`, `io`, `loadfile`, `dofile`, `package`. Scripts can use `debug.getinfo`, `debug.sethook`, `debug.setupvalue` to inspect closure upvalues, install hooks, and traverse the registry. `load`/`loadstring` allow dynamic compilation of arbitrary Lua code.
- **Fix:** Add `"debug"`, `"load"`, `"loadstring"` to the nil-out list in `sandbox()`. Update the sandbox test to assert these are also nil.

```rust
fn sandbox(lua: &Lua) -> Result<()> {
    let globals = lua.globals();
    for name in &["os", "io", "loadfile", "dofile", "package", "debug", "load", "loadstring"] {
        globals.raw_set(*name, LuaNil)?;
    }
    Ok(())
}
```

---

## C2 — Script isolation broken: `rawset(_G, ...)` leaks to shared globals

- **File:** `src/scripting/lua/mod.rs:94-99`
- **Status:** [x] DONE
- **Issue:** `create_script_env` sets `__index = lua.globals()` for read fallthrough, but does not set `__newindex`. A script calling `rawset(_G, "foo", ...)` writes to the shared global table visible to all scripts. After removing `debug` (C1), `rawset` is the remaining concern.
- **Fix:** Set `__newindex` on the env metatable to always write to the env table:

```rust
fn create_script_env(lua: &Lua) -> LuaResult<LuaTable> {
    let env = lua.create_table()?;
    let mt = lua.create_table()?;
    mt.set("__index", lua.globals())?;
    mt.set("__newindex", env.clone())?;
    env.set_metatable(Some(mt));
    Ok(env)
}
```

Also nil out `rawset` in the script environment if full isolation is desired.

---

## C3 — Panic on non-ASCII nicks: byte-length truncation

- **File:** `src/ui/message_line.rs:89-94`
- **Status:** [x] DONE
- **Issue:** `String::len()` returns byte count, `String::truncate()` takes a byte offset. A nick like `Ñóçk` (4 chars, 8 bytes) with `max_len=4` passes the guard but `truncate(4)` splits a multi-byte char and panics. The `total_len` padding calculation also uses byte lengths, misaligning columns for non-ASCII nicks.
- **Fix:** Use char-based counting and truncation:

```rust
let char_len = display_nick.chars().count();
if config.display.nick_truncation && char_len > max_len {
    let byte_idx = display_nick.char_indices().nth(max_len).map_or(display_nick.len(), |(i, _)| i);
    display_nick.truncate(byte_idx);
}
let total_len = nick_mode.chars().count() + display_nick.chars().count();
```

---

## C4 — Per-frame theme parsing in `calculate_wrap_indent`

- **File:** `src/ui/chat_view.rs:79-96`
- **Status:** [x] DONE
- **Issue:** Called on every render frame (~60fps). Allocates a formatted timestamp string, looks up a HashMap abstract, calls `resolve_abstractions`, calls `parse_format_string`, and iterates the result — all to produce a constant integer. The indent width only changes when `timestamp_format` or `nick_column_width` config changes.
- **Fix:** Cache the indent width as a field on `App` or compute it lazily. Recompute only when config changes (after `/set` commands or theme reload).

---

## C5 — Netsplit nick lookup uses wrong case

- **File:** `src/irc/batch.rs:173`
- **Status:** [x] DONE
- **Issue:** `buf.users.contains_key(&nick)` uses the nick as-is from IRC prefix, but nick HashMap keys are always lowercase (per prior audit fix). Netsplit QUIT batch never removes users from channel buffers because the key never matches.
- **Fix:**

```rust
// Before:
&& buf.users.contains_key(&nick)
// After:
&& buf.users.contains_key(&nick.to_lowercase())
```

Also check `remove_nick` at line 181 — confirm it handles case internally or lowercase the argument.

---

## C6 — SASL loops can hang indefinitely

- **File:** `src/irc/mod.rs:618-625, 672-679, 785-793`
- **Status:** [x] DONE
- **Issue:** The "wait for `AUTHENTICATE +`" loops in `run_sasl_plain`, `run_sasl_scram`, and `run_sasl_external` spin indefinitely if the server sends anything other than `AUTHENTICATE +` (e.g., `902 ERR_NICKLOCKED`, a `NOTICE`, or any other registration-phase message). No timeout, no error break.
- **Fix:** Add a break-on-error path for SASL failure numerics (`ERR_SASLFAIL`, `ERR_SASLABORT`, `ERR_SASLTOOLONG`) and/or a tokio timeout around the entire SASL exchange.

---

## C7 — `expect()` in production: `autoload_scripts`

- **File:** `src/app.rs:2909`
- **Status:** [x] DONE
- **Issue:** `self.script_api.as_ref().expect("script_api must be set")` — the invariant that `script_api` is `Some` is not enforced by the type system. If any code path calls `script_api.take()`, this panics in a live IRC session.
- **Fix:** Use the same guard pattern used elsewhere:

```rust
let Some(api) = self.script_api.as_ref() else { return; };
```

---

## C8 — Transaction retry causes duplicate inserts

- **File:** `src/storage/writer.rs:156-168`
- **Status:** [x] DONE
- **Issue:** After partial insert failures, `COMMIT` is attempted. If `COMMIT` fails, `ROLLBACK` is called but the full `queue` is returned for re-enqueue. Successfully-committed rows from a partial success scenario will be re-inserted, causing duplicates.
- **Fix:** After `ROLLBACK`, return `Vec::new()` (discard) rather than `queue`, consistent with the skip-and-log strategy:

```rust
if let Err(e) = conn.execute_batch("COMMIT") {
    tracing::error!("failed to commit transaction: {e}");
    let _ = conn.execute_batch("ROLLBACK");
    return Vec::new(); // Don't retry — prevents duplicate inserts
}
```

# Code Audit Issues — 2026-03-06

Full codebase audit across all 57 source files. Reviewed with rust-engineer, rust-best-practices, and ratatui-tui skills.

## CRITICAL

### C1. storage/writer.rs — Transaction not rolled back on INSERT failure
- **Lines:** 96–142
- **Problem:** If a row INSERT fails, `continue` skips it but COMMIT still runs. Failed rows are silently lost. If COMMIT fails, no ROLLBACK — next flush gets "cannot start transaction within transaction".
- **Fix:** On INSERT failure, ROLLBACK and return without clearing queue (retry). On COMMIT failure, ROLLBACK.

### C2. storage/writer.rs — Blocking sync I/O in async context
- **Lines:** 85–94, called from async `writer_loop` at 63–78
- **Problem:** `flush()` takes `std::sync::Mutex` lock and does synchronous SQLite I/O inside async fn. Blocks tokio thread.
- **Fix:** Use `tokio::task::spawn_blocking` to offload flush.

### C3. scripting/lua/mod.rs — unwrap() on Mutex lock in 9+ production closures
- **Lines:** 166, 192, 216, 548, 572, 762, 778, 849, 873
- **Problem:** Every `Arc<Mutex<HandlerState>>` access uses `.unwrap()`. Poisoned mutex panics the app.
- **Fix:** Map to appropriate error type (LuaError or eyre).

### C4. irc/netsplit.rs:191 — nick_index stale after groups.retain() compaction
- **Lines:** 184–191
- **Problem:** After `groups.retain()` removes entries, indices in `nick_index` point to wrong groups. Silent corruption.
- **Fix:** Rebuild `nick_index` from scratch after retain.

## IMPORTANT

### I1. state/events.rs:121 — format!("{:?}") to derive config key
- **Fix:** Add `MessageType::as_str() -> &'static str`, use `eq_ignore_ascii_case`.

### I2. state/events.rs:209 — active_buffer_mut clones active_buffer_id
- **Fix:** Use `as_deref()` to get `&str` without cloning.

### I3. state/sorting.rs:12 — label_fn called O(N log N) times
- **Fix:** Pre-compute labels before sorting.

### I4. state/events.rs:58 — Redundant previous_buffer_id.clone()
- **Fix:** Use `&self.previous_buffer_id` with NLL distinct field borrowing.

### I5. config/mod.rs:149 — PanelConfig::default() constructs full SidepanelConfig
- **Fix:** Return `Self { width: 20, visible: true }` directly.

### I6. config/env.rs:36 — format! key allocation repeated 3× per server
- **Fix:** Use closure `let lookup = |suffix| env.get(&format!("{prefix}_{suffix}")).cloned()`.

### I7. irc/netsplit.rs:80,146 — unwrap() in production code
- **Fix:** Use let-else / expect with justification or safe alternatives.

### I8. irc/flood.rs:126,187 — checked_sub().unwrap() panic on clock anomaly
- **Fix:** Use `saturating_sub()`.

### I9. irc/ignore.rs:72 — Regex recompiled on every call in hot path
- **Fix:** Pre-compile regex on IgnoreEntry creation, store compiled form.

### I10. irc/formatting.rs:14 — Vec<char> allocated per call in hot path
- **Fix:** Use byte-level scanning since all control chars are ASCII.

### I11. irc/batch.rs:215 — process_netjoin_batch generates individual JOINs + summary
- **Fix:** Call lower-level state mutation (add_nick) directly, skip message handler.

### I12. irc/events.rs:1799 — Dual ISUPPORT storage (raw HashMap + structured Isupport)
- **Fix:** Remove legacy HashMap, redirect all reads to isupport_parsed.

### I13. irc/mod.rs:313 — is_some_and with mutation side effect
- **Fix:** Use if-let instead.

### I14. ui/buffer_list+nick_list — visible_len uses byte .len() not char count
- **Fix:** Use `.chars().count()`.

### I15. ui/buffer_list+nick_list — truncate_with_plus fast-path byte .len() vs char max_len
- **Fix:** Use `s.chars().count() <= max_len`.

### I16. ui/chat_view.rs:19 — All messages rendered per-frame
- **Fix:** Compute skip first, only render visible slice.

### I17. ui/status_line.rs — Hardcoded Color::Rgb values bypass theme
- **Fix:** Add theme color keys or resolve from ThemeColors.

### I18. ui/topic_bar.rs:27 — Duplicates styled_spans_to_line logic inline
- **Fix:** Extend styled_spans_to_line with optional default_fg param.

### I19. ui/buffer_list+nick_list — Duplicated truncate_with_plus/visible_len
- **Fix:** Move to ui/mod.rs as pub(crate) functions.

### I20. commands/registry.rs:20 — get_commands() allocates Vec on every call
- **Fix:** Use LazyLock<Vec<...>> to compute once.

### I21. commands/helpers.rs:6 — active_buffer_id.clone() on every event line
- **Fix:** Use as_deref() to get &str.

### I22. commands/handlers_irc.rs:825 — /me echoes locally even when not connected
- **Fix:** Guard with let-else on irc_handles.get(), show "Not connected".

### I23. commands/docs.rs:33 — Docs unavailable in release builds
- **Fix:** Use include_dir! to embed docs at compile time.

### I24. theme/loader.rs:37 — Silent fallback on theme parse error
- **Fix:** Log tracing::warn on deserialization failure.

### I25. scripting/event_bus.rs:110 — emit(&self) never removes once handlers
- **Fix:** Change emit to take &mut self, or handle once cleanup differently.

### I26. scripting/lua/mod.rs:592 — Timer callbacks silently discarded
- **Fix:** Return Lua error "timer() not yet implemented" until plumbing complete.

### I27. storage/db.rs:87 — purge_old_messages silently swallows FTS errors
- **Fix:** Propagate or log errors, wrap in transaction.

## MINOR

### M1. state/sorting.rs:52 — unwrap() in production (guarded)
- **Fix:** Use let-else.

### M2. state/events.rs:217 — intermediate Vec<&Buffer> alloc
- **Fix:** Inline collect.

### M3. irc/netsplit.rs:149 — to_string() for Vec::contains
- **Fix:** Use `.iter().any(|n| n == nick)`.

### M4. irc/batch.rs:158 — O(n²) dedup with Vec::contains
- **Fix:** Use HashSet for dedup.

### M5. irc/events.rs:493 — nick_prefix allocates String for single char
- **Fix:** Return Option<char>, convert at call site.

### M6. ui/input.rs:343 — per-frame .to_string() on &str slices
- **Fix:** Unavoidable due to lifetime requirements, skip.

### M7. ui/buffer_list.rs:29 — String where &str would suffice
- **Fix:** Use `&str` reference.

### M8. commands/handlers_admin.rs:140 — to_uppercase() allocates in loop
- **Fix:** Use eq_ignore_ascii_case.

### M9. commands/handlers_irc.rs:227 — /join key guard breaks with 3+ args
- **Fix:** Remove args.len() == 2 constraint.

### M10. commands/settings.rs:315 — 38 String allocs per tab-complete
- **Fix:** Use Cow<'static, str>.

### M11. scripting/engine.rs:353 — ~/ paths not expanded
- **Fix:** Expand with dirs::home_dir().

### M12. app.rs — active_conn_id() clones unnecessarily
- **Fix:** Return Option<&str> with lifetime tied to &self.

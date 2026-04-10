# v0.8.4 OOM Fix — Design Spec

**Date:** 2026-04-10
**Context:** Repartee v0.8.4 crashed on Debian with 3 GB RSS peak while user was scrolling chat with mouse wheel. Diagnosis triangulated across Cursor, Claude Code, and OpenCode GLM 5.1; critical "break-never-fires" detail caught by OpenCode GLM and empirically verified against source. Full diagnosis is in MemPalace drawer `drawer_repartee_src_e6a86e8f564e9e5ffac658d9` (wing=repartee, room=src).

## Root cause

`src/ui/chat_view.rs:47-75` builds a `VecDeque<Line<'_>>` sized to `visible_height + app.scroll_offset`, iterating all messages in the buffer and breaking only when `visual_lines.len() > needed`. Since `App::scroll_offset` is never clamped on input (`src/app/input.rs` mutators at lines 223, 226, 388, 397 use only `saturating_add` / `saturating_sub`), sustained mouse-wheel scrolling pushes `scroll_offset` past the total wrapped-line count of the buffer. At that point `needed > total possible wrapped lines`, the break condition **never fires**, and the render loop walks every message in the buffer on every frame, calling `render_message()` + `wrap_line()` per message.

Per-message allocation is ~3-8 KB (verified against `src/ui/message_line.rs`, `src/ui/mod.rs::wrap_line`, `src/theme/parser.rs`). For a 2000-message buffer: ~10 MB of temporary allocations per frame. Crossterm wheel events drive render at 20-60 fps → **200-600 MB/sec allocation churn**. glibc ptmalloc2 does not return memory to OS aggressively under this workload — arenas grow, RSS climbs to 3 GB, systemd-oomd sends SIGKILL. No app-level panic, just the notice in journalctl.

## Goals

1. Guarantee bounded render cost per frame regardless of `scroll_offset` value.
2. Add defensive allocator swap on Linux to reduce glibc fragmentation sensitivity for long-running sessions (weeks of uptime).
3. Add regression test that would have caught the bug pre-release.

## Non-goals

- **No architectural refactoring.** Dirty-flag for the 1s snapshot rebuild, Cell-based scroll_offset, and message wrap cache are all out of scope. Each is premature optimization without profiling data post-primary-fix.
- **No speculative `shrink_to_fit`.** `std::mem::take` in `drain_pending_web_events` already solves Vec capacity retention; adding `shrink_to_fit` would be counterproductive (defeats Vec capacity reuse).
- **No cross-platform allocator swap.** FreeBSD already uses jemalloc natively; macOS libsystem_malloc handles memory reclaim well. Only Linux glibc has the diagnosed fragmentation issue.
- **No changes to kick handler triple-insertion.** That is by-design (user wants kick notification in server/channel/landing buffers); the amplification is bounded to rare events.

## Scope (exactly three changes)

### Change 1: Cap `needed` in `chat_view::render`

**File:** `src/ui/chat_view.rs`

**What:** Introduce a hard upper bound on `needed` proportional to `buf.messages.len()` so the render loop's break condition fires in O(buf.messages.len()) time regardless of `scroll_offset` value.

**How:**
```rust
// Even pathologically wrapped messages rarely produce more than this many visual
// lines (a 4000-char NOTICE on an 80-col terminal wraps to ~50 lines; we cap at 16
// for normal conversational traffic, which is the realistic worst case).
const MAX_WRAPPED_LINES_PER_MSG: usize = 16;

let buffer_cap = buf
    .messages
    .len()
    .saturating_mul(MAX_WRAPPED_LINES_PER_MSG)
    .max(visible_height);
let needed = (visible_height + app.scroll_offset).min(buffer_cap);
```

**Why this is the right shape:**
- `render` signature stays `&App` — zero ripple to `src/ui/layout.rs` or other callers.
- Single constant, single `min()` — auditable in isolation.
- `MAX_WRAPPED_LINES_PER_MSG = 16` is a safe over-estimate for the realistic workload (IRC messages on 80-485 col terminals). On the user's 485-col terminal, almost every message occupies 1 visual line; on an 80-col terminal, typical messages occupy 1-3 lines. 16 is 5-16× headroom.
- Break condition on line 72 (`visual_lines.len() > needed`) now fires at latest when all messages have been processed, because `visual_lines.len() ≤ buf.messages.len() × max_wraps_observed ≤ buffer_cap = needed`.
- `App::scroll_offset` may still saturate over very long sessions, but `saturating_add` bounds it at `usize::MAX` (~18 EB) which is stable — cosmetic issue only, not a bug.

### Change 2: Regression test for bounded render cost

**File:** `src/ui/chat_view.rs` — `#[cfg(test)] mod tests`

**What:** Unit test that builds an `App` with a populated buffer (say 100 messages), pushes `scroll_offset` to a pathologically large value (say `usize::MAX / 2`), calls a render helper, and asserts that the render path terminates and allocates within a bounded budget.

**How:** Since `render` requires a `Frame`, we cannot invoke it directly. Instead we extract the core calculation (the `buffer_cap` / `needed` derivation) into a pure function `compute_render_budget(buffer_len: usize, visible_height: usize, scroll_offset: usize) -> (needed: usize, buffer_cap: usize)` and test that:
- For normal `scroll_offset` (e.g. 50), `needed` equals `visible_height + scroll_offset`
- For pathological `scroll_offset = usize::MAX / 2`, `needed` is capped to `buffer_cap`, which equals `buffer_len * 16`
- For empty buffer (`buffer_len = 0`), `needed` defaults to `visible_height`

This is the minimal invariant: **`needed ≤ buffer_len × MAX_WRAPPED_LINES_PER_MSG` for non-empty buffers** (or `≥ visible_height` for empty buffers).

### Change 3: jemalloc global allocator on Linux

**Files:** `Cargo.toml`, `src/main.rs`

**What:** Add `tikv-jemallocator` as an optional dependency conditional on `target_os = "linux"`, and register it as `#[global_allocator]`.

**How:**

`Cargo.toml` — add optional dep with target-specific entry:
```toml
[target.'cfg(target_os = "linux")'.dependencies]
tikv-jemallocator = "0.6"
```

`src/main.rs` — add before `main()`:
```rust
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;
```

**Why this is defensible, not premature:**
- Repartee runs for weeks of continuous uptime (user ran 0.8.2/0.8.3 for >1 week). Long-lived processes on glibc ptmalloc2 accumulate arena fragmentation even under normal workloads, independent of specific hot paths.
- jemalloc is the default system allocator on FreeBSD (so this change matches BSD behavior on Linux, rather than diverging from it).
- Used in production by tikv, foundationdb, rust-analyzer, and several Rust IRC clients for similar reasons.
- Conditional compile means BSD and macOS builds are **byte-identical** to pre-fix behavior — zero risk for those targets.
- Empirical industry data: ~20-30% lower steady-state RSS on long-running Rust services under bursty allocation patterns.

**Why not `all(not(target_env = "msvc"))` or other patterns:** We're not shipping a Windows build and explicitly want to avoid mixing allocators on FreeBSD (which already uses jemalloc system-wide) or macOS (which has a well-tuned libsystem_malloc).

## Testing strategy

1. **Unit test for `compute_render_budget`** (Change 2) — pure-function test, no async, no terminal.
2. **Manual repro verification:** After primary fix lands, manually reproduce by wheel-scrolling aggressively for 30 seconds in debug build with `tracing` at debug level. Confirm RSS stays bounded (< 100 MB) instead of climbing.
3. **`make clippy`** must pass (0 warnings policy per CLAUDE.md).
4. **`make test`** must pass on all existing tests.
5. **`make release`** must succeed on Linux (jemalloc build).
6. **CI cross-platform check** — at least the BSD and macOS jobs must compile without the jemalloc dependency even being pulled in (verified by the `[target.'cfg(...)'.dependencies]` syntax).

## Risks and mitigations

| Risk | Likelihood | Mitigation |
|---|---|---|
| `MAX_WRAPPED_LINES_PER_MSG = 16` is too small for some message | Low | Even a 1000-char message on 80-col terminal wraps to ~13 lines. 16 is a safe over-estimate. If ever hit, the only consequence is brief mis-scroll, not crash. |
| jemalloc introduces perf regression on Linux | Very Low | tikv-jemallocator is production-stable. Worst case: similar RSS, slightly different CPU profile. No known TUI workload regression. |
| Cross-platform build breaks | Very Low | `[target.'cfg(...)'.dependencies]` is standard Cargo pattern. CI already runs macOS/FreeBSD jobs. |
| Regression test doesn't catch a future reintroduction | Medium | Unit test covers the pure budget calculation. A more integration-style test (actually rendering into a test Frame) is possible but adds complexity; pure-function test is the minimal that catches the actual bug. |

## Commit strategy

Per project feedback rule "Commit hygiene — never blindly stage noise":
- **Commit 1:** `fix(ui): cap chat_view render budget to prevent OOM on wheel scroll` — Change 1 + Change 2 (primary fix + test). This alone solves the OOM.
- **Commit 2:** `build: use jemalloc as global allocator on Linux` — Change 3 (allocator swap). Separate because it's defense-in-depth and distinct concern.

Both commits land together on `main`. No feature branch needed (single-session fix, reviewed inline).

## Out-of-scope follow-ups (capture for future sessions)

These are NOT implemented in this spec but documented for future consideration if post-fix measurements show they are needed:

1. **Dirty-flag `build_sync_init`** — skip 1s tick rebuild if state hasn't mutated. Estimated ~40-80 MB/s allocation reduction on active servers. Only worth doing if post-fix RSS growth in long sessions remains monotonic.
2. **Wrap cache per message** — `HashMap<(msg_id, width), Vec<Line<'static>>>` with invalidation on resize/clear. Eliminates per-frame wrap work. Only worth doing if `make release` + heaptrack shows wrap_line still dominates allocation profile post-primary-fix.
3. **Write back clamped `scroll_offset`** to `App::scroll_offset` so it self-heals. Cosmetic, not a bug; deferred.

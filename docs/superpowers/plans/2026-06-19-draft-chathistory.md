# draft/chathistory + draft/event-playback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Pull IRCv3 `draft/chathistory` (with `draft/event-playback`) into repartee's SQLite-backed backlog, triggered by scroll-up exhaustion and reconnect gap-fill (active buffer).

**Architecture:** SQLite is the single source of truth. chathistory is a background filler: history batches are quietly ingested into SQLite (dedup via existing `msg_id` UNIQUE index), then the existing pagination feeds the UI. No web-ui changes. Sending via `Command::Raw` (receiving works via existing batch `CUSTOM` handling).

**Tech Stack:** Rust 2024, tokio, rusqlite (SQLite), `irc-repartee` crate, ratatui (TUI), Leptos (web-ui, untouched).

## Global Constraints

- Build only via `make` (CLAUDE.md). Gates: `make test`, `make clippy`.
- Clippy 0-warnings policy: pedantic=warn, nursery=warn, perf=deny, redundant_clone=deny.
- Error handling: `color-eyre`. Logging: `tracing` (never `println!`/`log`).
- `state/` is UI-agnostic — no ratatui imports there.
- Send via `handle.sender.send(::irc::proto::Command::Raw(line, vec![]))`.
- Cap check: `conn.enabled_caps.contains("draft/chathistory")`.
- Reference type: `msgid` if `MSGREFTYPES` includes it, else `timestamp`. Clamp count to `CHATHISTORY=<max>`.
- Historical events are store-only: never mutate live `users`/topic; never highlight/notify.

---

### Task 1: ISUPPORT — CHATHISTORY + MSGREFTYPES accessors

**Files:**
- Modify: `src/irc/isupport.rs` (impl `Isupport`, plus `#[cfg(test)]`)

**Interfaces:**
- Produces: `Isupport::chathistory_max(&self) -> Option<usize>` (`CHATHISTORY=0` → `None` meaning "no server cap"); `Isupport::msgreftypes(&self) -> Vec<String>` (lowercased, split on `,`; empty if absent).

- [ ] **Step 1: Write failing tests** — parse `CHATHISTORY=50` → `Some(50)`, `CHATHISTORY=0` → `None`, absent → `None`; `MSGREFTYPES=timestamp,msgid` → `["timestamp","msgid"]`, absent → `[]`.
- [ ] **Step 2:** `make test` (or targeted) → FAIL (methods missing).
- [ ] **Step 3:** Implement both accessors reading from `self.tokens` (pattern of existing accessors).
- [ ] **Step 4:** `make test` → PASS; `make clippy` → clean.
- [ ] **Step 5:** Commit `feat(irc): parse CHATHISTORY and MSGREFTYPES isupport tokens`.

### Task 2: CAP — request draft/chathistory + draft/event-playback

**Files:**
- Modify: `src/irc/cap.rs:4-19` (`DESIRED_CAPS`); test in same file.

**Interfaces:**
- Consumes: existing `caps.negotiate(DESIRED_CAPS)`.
- Produces: both cap names present in `DESIRED_CAPS`.

- [ ] **Step 1:** Add a test asserting `DESIRED_CAPS` contains `"draft/chathistory"` and `"draft/event-playback"` (and `"batch"`).
- [ ] **Step 2:** `make test` → FAIL.
- [ ] **Step 3:** Add both literals to `DESIRED_CAPS`.
- [ ] **Step 4:** `make test` → PASS; `make clippy` → clean.
- [ ] **Step 5:** Commit `feat(irc): request draft/chathistory + event-playback caps`.

### Task 3: Storage — newest-row cursor query (AFTER anchor)

**Files:**
- Modify: `src/storage/query.rs` (near `get_messages_paginated`), `#[cfg(test)]`.

**Interfaces:**
- Produces: `newest_anchor(db, network, buffer) -> Option<(String /*msg_id, may be ""*/, i64 /*timestamp*/, i64 /*id*/)>` — newest row by `(timestamp, id)`; `None` if buffer empty. (Symmetric to the oldest cursor used for scroll-up.)

- [ ] **Step 1:** Test against an in-memory DB seeded with rows; assert newest returns the max `(timestamp,id)` row's `(msg_id, timestamp, id)`; empty buffer → `None`.
- [ ] **Step 2:** `make test` → FAIL.
- [ ] **Step 3:** Implement `SELECT msg_id, timestamp, id ... ORDER BY timestamp DESC, id DESC LIMIT 1`.
- [ ] **Step 4:** `make test` → PASS; `make clippy` → clean.
- [ ] **Step 5:** Commit `feat(storage): newest-row anchor query for chathistory gap-fill`.

### Task 4: chathistory request builder (pure)

**Files:**
- Create: `src/irc/chathistory.rs`; register `pub mod chathistory;` in `src/irc/mod.rs`.

**Interfaces:**
- Produces:
  - `enum HistoryRef { MsgId(String), Timestamp(String), Latest }`
  - `fn pick_ref_type(msgreftypes: &[String]) -> RefKind` (`MsgId` if `"msgid"` present else `Timestamp`).
  - `fn clamp_limit(want: usize, server_max: Option<usize>) -> usize` (min with server_max; default want).
  - `fn build_command(subcommand: &str, target: &str, anchor: &HistoryRef, limit: usize) -> String` → e.g. `CHATHISTORY BEFORE #c msgid=abc 100`, `CHATHISTORY LATEST #c * 100`.

- [ ] **Step 1:** Tests for `pick_ref_type`, `clamp_limit` (with/without server max), and `build_command` for BEFORE(msgid), AFTER(timestamp), LATEST(*).
- [ ] **Step 2:** `make test` → FAIL.
- [ ] **Step 3:** Implement the pure functions.
- [ ] **Step 4:** `make test` → PASS; `make clippy` → clean.
- [ ] **Step 5:** Commit `feat(irc): chathistory request builder`.

### Task 5: chathistory per-connection state + gating

**Files:**
- Modify: `src/irc/chathistory.rs` (add `HistoryState`), tests.

**Interfaces:**
- Produces:
  - `enum Direction { Before, After, Latest }`
  - `struct HistoryState { in_flight: HashSet<(String,Direction)>, server_exhausted: HashSet<String /*target, for BEFORE*/> }`
  - `fn should_request(&self, target, dir, cap_enabled) -> bool` (false if cap off, in-flight, or (dir==Before && server_exhausted)).
  - `fn mark_in_flight / clear_in_flight / mark_exhausted`.
  - `fn note_batch_size(&mut self, target, dir, rows: usize, limit: usize)` → sets exhausted for Before when `rows < limit`.

- [ ] **Step 1:** Tests: gating off when cap disabled / in-flight / exhausted; `note_batch_size` sets exhausted on short BEFORE batch, not on full batch.
- [ ] **Step 2:** `make test` → FAIL.
- [ ] **Step 3:** Implement `HistoryState`. Store one per connection (add field to connection state; default in constructor).
- [ ] **Step 4:** `make test` → PASS; `make clippy` → clean.
- [ ] **Step 5:** Commit `feat(irc): chathistory request state + anti-loop gating`.

### Task 6: Batch quiet-ingest for chathistory (+ event-playback store-only)

**Files:**
- Modify: `src/irc/batch.rs` (detect `CUSTOM("chathistory")`, route to ingest), and the message-handling entry it calls.
- Modify: ingest must persist via the storage writer with `msg_id`/`@time`, and NOT call live-display/highlight/notify/nicklist/topic paths.

**Interfaces:**
- Consumes: `BatchInfo` (type + messages), `message_timestamp()`, storage writer (`log_tx`).
- Produces: `fn ingest_chathistory_batch(&mut self, conn_id, batch)` → persists conversational rows and `type='event'` rows; emits a "backlog grew" signal (reuse existing buffer-refresh/notify-web mechanism) for the target buffer; calls `HistoryState::note_batch_size`.

- [ ] **Step 1:** Test (unit/integration with in-memory state+DB): feeding a chathistory batch of N PRIVMSG + a JOIN event results in N+1 stored rows with correct timestamps/msg_ids; `users` map for the buffer is unchanged; no highlight flags set; dedup on repeated msg_id.
- [ ] **Step 2:** `make test` → FAIL.
- [ ] **Step 3:** Implement detection + ingest path. Conversational lines → `type='message'/'action'/'notice'`; JOIN/PART/QUIT/NICK/TOPIC/MODE → `type='event'` rows only. Guard: do not mutate `Buffer::users`/topic. Emit refresh signal.
- [ ] **Step 4:** `make test` → PASS; `make clippy` → clean.
- [ ] **Step 5:** Commit `feat(irc): quiet-ingest chathistory batches (store-only events)`.

### Task 7: Scroll-up trigger → CHATHISTORY BEFORE

**Files:**
- Modify: `src/app/backlog.rs` (`load_older_chat_backlog`), and wherever batch-end refresh re-triggers pagination.

**Interfaces:**
- Consumes: `history_exhausted`, `HistoryState::should_request`, `build_command`, oldest cursor, `enabled_caps`.
- Produces: when local SQLite page is empty AND cap enabled AND not server-exhausted → send `CHATHISTORY BEFORE <target> <ref> <limit>`; on batch-end refresh → re-run pagination.

- [ ] **Step 1:** Test: with cap enabled and SQLite exhausted, the scroll-up path enqueues a BEFORE request with the oldest-row ref; with cap disabled it does not.
- [ ] **Step 2:** `make test` → FAIL.
- [ ] **Step 3:** Wire it in. Use oldest cursor → `HistoryRef`. Mark in-flight; clear on batch-end.
- [ ] **Step 4:** `make test` → PASS; `make clippy` → clean.
- [ ] **Step 5:** Commit `feat(app): fetch server history on scroll-up exhaustion`.

### Task 8: Reconnect gap-fill (active buffer only) + lazy on focus

**Files:**
- Modify: reconnect/registration path (`src/app/irc.rs` or session reattach) + buffer-focus path.

**Interfaces:**
- Consumes: newest anchor (Task 3), `build_command` AFTER/LATEST, active-buffer id, `enabled_caps`.
- Produces: on (re)connect, for the active buffer → `CHATHISTORY AFTER <newest>` (or `LATEST * <limit>` if no anchor); on buffer focus/open → same for that buffer (once).

- [ ] **Step 1:** Test: on connect-complete with cap enabled, exactly one AFTER/LATEST request is issued for the active buffer and none for inactive buffers.
- [ ] **Step 2:** `make test` → FAIL.
- [ ] **Step 3:** Wire reconnect + focus triggers; dedupe via in-flight + a per-buffer "gap-fill done this session" guard.
- [ ] **Step 4:** `make test` → PASS; `make clippy` → clean.
- [ ] **Step 5:** Commit `feat(app): reconnect gap-fill for active buffer (lazy elsewhere)`.

---

## Final phase

- [ ] `make test` + `make clippy` full suite green, 0 warnings.
- [ ] `/code-review` on the full branch diff; apply fixes.
- [ ] Update spec "Status" to Implemented; note any deviations.
- [ ] Morning summary.

## Self-Review (against spec)

- Spec §Components 1 (ISUPPORT) → Task 1. ✅
- §2 (CAP) → Task 2. ✅
- §3 (orchestration: builders, state, anchors) → Tasks 3,4,5. ✅
- §4 (batch sink, store-only events, signal) → Task 6. ✅
- §5 (scroll-up trigger) → Task 7; (reconnect active-only + lazy) → Task 8. ✅
- §Error handling (clamp, ref type, anti-loop, store-only, dedup) → Tasks 1,4,5,6. ✅
- §Testing items → distributed across task Step-1 tests. ✅
- Placeholders: none. Types consistent across tasks (`HistoryRef`, `Direction`, `HistoryState`, `build_command`, `newest_anchor`). `FAIL CHATHISTORY` handling is best-effort logging — folded into Task 5/6 exhaustion (documented in spec as v1 best-effort).

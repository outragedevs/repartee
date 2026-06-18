# Design: `draft/chathistory` + `draft/event-playback` (v1)

Date: 2026-06-19
Status: Approved
Branch: `feat/draft-chathistory`

## Goal

Add IRCv3 `draft/chathistory` (with `draft/event-playback`) to repartee so that
message history can be pulled from the server/bouncer (soju, ergo) and merged
into the existing SQLite-backed backlog, covering two triggers:

1. **Scroll-up backlog** — when the user scrolls to the top and the *local*
   SQLite history is exhausted, fetch older messages from the server
   (`CHATHISTORY BEFORE`) and continue feeding the existing viewport-fill.
2. **Reconnect gap-fill** — after (re)connecting, fill the gap for the
   **active buffer only** (`CHATHISTORY AFTER`/`LATEST`); other buffers are
   filled lazily when focused/opened.

Content scope: conversational messages **and** `draft/event-playback` events
(join/part/quit, nick, topic, mode) replayed from history.

## Guiding principle

**SQLite is the single source of truth.** All history (live, local scroll-up,
chathistory) lands in SQLite; the UI (TUI + web-ui) reads *only* from SQLite via
the existing `get_messages_paginated` keyset pagination. chathistory is a
background *filler* of SQLite, never a direct UI feed. This keeps web-ui and the
TUI rendering path unchanged, and reuses dedup + ordering for free.

## Key existing facts this builds on

- `messages` table has `msg_id TEXT` with a **UNIQUE** index
  (`idx_messages_msg_id ... WHERE msg_id IS NOT NULL`) → dedup is automatic on
  insert. (`src/storage/db.rs:3-30`)
- `get_messages_paginated()` keyset cursor `(timestamp, id)`, oldest→newest.
  (`src/storage/query.rs:146-195`)
- Unknown BATCH types are already replayed message-by-message through the normal
  handler. (`src/irc/batch.rs:150-161`) — chathistory batch parses as
  `BatchSubCommand::CUSTOM("CHATHISTORY")` in the fork; **no proto change needed
  to receive.**
- Buffer has `history_exhausted` and `pin_backlog`; older history is prepended
  via `push_front`. (`src/state/buffer.rs:139-194`)
- Scroll-up path exists: `app/backlog.rs::load_older_chat_backlog()` pulls a
  page from SQLite when the user nears the top.
- web-ui already paginates from SQLite (`FetchMessages` / `has_more` /
  viewport-fill) — **no web-ui changes required.**
- `enabled_caps: HashSet<String>` per connection; check via
  `conn.enabled_caps.contains("…")`. `DESIRED_CAPS` in `src/irc/cap.rs:4-19`.
- Send path: `handle.sender.send(::irc::proto::Command::…)`.

## Decisions

- **Sending uses `Command::Raw`** in v1. The typed `Command::CHATHISTORY` would
  live in the `irc-proto-repartee` fork (`~/dev/irc`), which is published
  separately (CLAUDE.md:58) — modifying it would require a crates.io publish to
  reach repartee. To keep v1 self-contained, build-safe, and reviewable in one
  repo, we send raw `CHATHISTORY …` lines. Receiving needs no proto change.
  *Follow-up (out of scope): add typed `CHATHISTORY` to the fork and switch.*
- Reference type: prefer `msgid` when `MSGREFTYPES` advertises it; else
  `timestamp`. Clamp request count to ISUPPORT `CHATHISTORY=<max>`.
- Historical events are **store-only**: they must NOT mutate live state
  (nicklist `users`, topic) and must NOT produce highlights or notifications.

## Components

### 1. ISUPPORT (`src/irc/isupport.rs`)
- `chathistory_max() -> Option<usize>` from `CHATHISTORY=<n>` (`n==0` → no limit).
- `msgreftypes() -> Option<&str>` from `MSGREFTYPES` (e.g. `timestamp,msgid`).

### 2. CAP negotiation (`src/irc/cap.rs`)
- Add `draft/chathistory` and `draft/event-playback` to `DESIRED_CAPS`
  (`batch` already present and is required).

### 3. chathistory orchestration (`src/irc/chathistory.rs`, new)
- Per-connection state: in-flight set keyed by `(target, direction)`;
  per-buffer `server_history_exhausted` flag; simple FIFO throttle queue.
- Request builders:
  - `BEFORE <target> <ref> <limit>` — scroll-up (anchor = oldest local row).
  - `AFTER <target> <ref> <limit>` / `LATEST <target> * <limit>` — gap-fill
    (anchor = newest local row; `LATEST` when no anchor exists).
- Anchor queries (SQLite): reuse oldest cursor; add a newest-cursor query.
- Reference rendering: `msgid=<id>` or `timestamp=<rfc3339>` per MSGREFTYPES.
- Guards: skip if cap disabled, request already in-flight, or
  `server_history_exhausted` set. Mark exhausted when a batch returns
  `< limit` rows or a `FAIL CHATHISTORY` arrives.

### 4. Batch sink for history (`src/irc/batch.rs`)
- Detect `BatchSubCommand::CUSTOM` with type `chathistory`.
- Route its messages to a **quiet ingest path**:
  - Persist to SQLite with server `@time` and `@msgid` (existing
    `message_timestamp()` already honors `@time`).
  - Suppress live display, highlight, notifications, and unread bumps.
  - For event-playback lines (JOIN/PART/QUIT/NICK/TOPIC/MODE inside the batch):
    store as `type='event'` rows, but **do not** mutate `users`/topic.
- On batch end: emit a "backlog grew" signal for the target buffer so the
  active viewport (TUI + web) re-paginates from SQLite and shows the new rows
  without manual scrolling.

### 5. Trigger wiring
- **Scroll-up** (`src/app/backlog.rs`): in `load_older_chat_backlog`, when
  `history_exhausted` (SQLite empty for that page) AND cap enabled AND
  `!server_history_exhausted` → issue `CHATHISTORY BEFORE`. On batch end →
  re-run pagination → UI shows older rows. Set `server_history_exhausted` when
  the server returns `< limit`.
- **Reconnect (active buffer only)**: after (re)connect, for the active buffer
  compute the newest cursor and issue `CHATHISTORY AFTER` (or `LATEST` if no
  anchor). Other buffers: trigger lazily on focus/open.

## Data flow

```
scroll-up:   UI scroll → load_older_chat_backlog → SQLite page
             ├ rows found      → feed viewport (unchanged)
             └ exhausted+cap   → CHATHISTORY BEFORE → batch → quiet ingest → SQLite
                                 → "backlog grew" → re-paginate → older rows shown

reconnect:   (re)connect → active buffer newest cursor
             → CHATHISTORY AFTER/LATEST → batch → quiet ingest → SQLite
             → "backlog grew" → re-paginate → gap filled
```

## Error handling / edge cases

- `CHATHISTORY=<max>` → clamp count; `MSGREFTYPES` → choose ref type.
- `FAIL CHATHISTORY …` (standard-replies): end request, set exhausted, log via
  `tracing` (v1 may read it from a `Raw`/`FAIL` line; no numeric needed).
- Anti-loop: in-flight guard + `server_history_exhausted`.
- Historical events never touch live `users`/topic; never highlight/notify.
- Timestamps: server `@time` (RFC3339) via existing `message_timestamp()`.
- Dedup: rely on `msg_id` UNIQUE; rows without `@msgid` fall back to insert
  (may duplicate across direction boundaries — mitigated by timestamp+content,
  acceptable for v1; documented).

## Testing

- **proto/raw**: `CHATHISTORY BEFORE/AFTER/LATEST` line construction (ref type,
  clamp) round-trips to the expected wire string.
- **isupport**: `CHATHISTORY`/`MSGREFTYPES` parse + clamp.
- **chathistory module**: anchor selection (msgid vs timestamp), request gating
  (cap off / in-flight / exhausted), exhaustion detection on short batch.
- **batch ingest**: messages persisted with correct `@time`/`@msgid`; dedup;
  **no** live nicklist mutation; **no** highlight/notification; events stored as
  `type='event'`.
- **integration**: scroll-up issues `BEFORE` only when SQLite exhausted;
  reconnect issues `AFTER` for the active buffer only (others lazy).

## v1 scope refinement (implementation note, 2026-06-19)

During implementation the event-playback ingest was scoped down. The live
event handlers (JOIN/PART/QUIT/NICK/TOPIC/MODE) mutate live state through ~15+
direct sites (`buf.users`, topic, modes, away, prefixes) and render their
display text *inline* per handler (no shared formatter), and the messages
table has **no `event_params` column** (event rows must store fully-rendered
text). Re-using the live handlers under a "history mode" flag would require
gating every one of those mutation sites (high risk of corrupting live state);
a parallel renderer would duplicate six inline formatters.

Therefore **v1 ingests conversational lines only** (PRIVMSG / NOTICE, including
CTCP ACTION), store-only, via `events::ingest_chathistory_batch` →
`AppState::ingest_history_message`. `draft/event-playback` lines are counted
and skipped (the cap stays negotiated). This delivers the core backlog value
safely; rendering historical events into stored `type='event'` rows is a
documented follow-up.

Known v1 limitations:
- E2E-encrypted (`+RPE2E01…`) history messages are stored as received (not
  decrypted) — niche; revisit with event-playback.
- Historical events (joins/parts/etc.) are not shown in backlog yet.
- Reconnect gap-fill covers the **active** buffer only; lazy gap-fill for other
  buffers on focus/open is a follow-up (the focus path is in `AppState`, which
  has no IRC handle; wiring it needs an App-level focus hook).

## Out of scope (v1)

- Typed `Command::CHATHISTORY` in the fork (documented follow-up).
- `CHATHISTORY TARGETS`, `BETWEEN`, `AROUND`.
- Event-playback rendering into stored event rows (follow-up; see above).
- Eager gap-fill for all channels on reconnect (config toggle is a later item).
- `soju.im/bouncer-networks` integration (separate roadmap item).

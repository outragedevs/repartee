# Log Browser Design

## Overview

A new `repartee l` (alias `repartee logs`) subcommand that opens a **read-only TUI log browser** over the SQLite history. Reuses the entire existing layout — topic bar, left sidebar, scrollback, status line, input bar — so users navigate logs with the same muscle memory as live chat.

The log browser is a separate process, completely isolated from any running daemon. SQLite WAL mode permits concurrent reads while the daemon writes.

Goals:

- See full per-channel / per-query history, browseable like normal IRC buffers
- Lazy-loaded paginated read (don't slurp 100k messages on open)
- Zero new UI layers — sidebar groups by network, buffers per channel
- Full encryption support (AES-256-GCM logs are decrypted in this process the same way the daemon decrypts them for backlog)
- Standalone — works even when the daemon is offline

Non-goals (explicitly out of scope, candidates for later):

- Cross-network global search (separate feature; FTS already exists for that)
- Editing / deleting log entries
- Export to file
- Web UI mirror (architecture allows it later, but not in this iteration)
- Bookmarks / favorites / tagging

## CLI

```
repartee l        # log browser, alias of `logs`
repartee logs
```

- No fork. Parent process owns the terminal directly (mirrors `repartee a`).
- No tokio runtime needed for IRC, but tokio still used for crossterm `event-stream` and the periodic tick — same `App::run` loop, gated by a flag.
- No socket listener, no IRC connections, no autoconnect, no autosendcmd, no web server, no scripting engine load. The log browser is intentionally minimal.
- Pre-fork config validation (current `validate_startup_files`) still runs so a typo in `config.toml` surfaces here too.

## Architecture

A single new flag on `App` plus a small handful of new state and rendering branches:

```rust
pub struct App {
    // ...existing fields...
    pub log_browser_mode: bool,
}
```

When `true`:

- `App::run` skips `start_socket_listener`, `autoload_scripts`, `start_web_server`, autoconnect.
- IRC event dispatch is dead code (no irc_handles ever populated).
- Sidebar source is built from the SQLite catalog instead of `config.servers` + IRC state.
- Slash commands are filtered to a log-mode allowlist (anything else → status hint).

The log browser is an alternative entry point, not a runtime mode the user toggles. Once started in log mode, you stay in log mode until exit — keeps the mental model clean.

## Storage Access

A new helper opens the database read-only via SQLite URI:

```rust
// storage::open_readonly(path) → Connection opened with
// "file:<path>?mode=ro&immutable=1" so the daemon can keep writing.
```

`immutable=1` tells SQLite that the file will not change during this connection; in practice the daemon **does** still write, so we use `mode=ro` only — same effect, allows WAL reads.

Encryption: when `[storage] encrypt = true` in the config, the same `[e2e]` settings load the same `Aes256Gcm` key as the daemon does in `App::new`. Without the key encrypted rows are unreadable; we surface a clear error and exit if the key cannot be derived.

## Sidebar Catalog

At startup, after opening the read-only DB, we run:

```sql
SELECT DISTINCT network, buffer FROM messages ORDER BY network, buffer;
```

For each row we synthesise:

- One pseudo-`Connection` per distinct `network` — `should_reconnect = false`, `status = Connected`, dummy `ServerConfig`. The connection's `label` defaults to the `network` ID; if `config.servers.<id>.label` exists in the user's config file, that friendlier label is used.
- One `Buffer` per `(network, buffer)` of new type **`BufferType::Log`** — `messages: VecDeque::new()` (filled lazily), `connection_id = network`, `name = buffer`, `topic = None`.

The sidebar renders these via the existing `buffer_list::render` with no changes — networks are headers, buffers under them, sorting and grouping reuse the existing pipeline.

There is no Status pseudo-buffer per network in log mode (a Status buffer in chat is the IRC server console; in log mode it has no analog). Each pseudo-network in the sidebar contains only its log buffers.

## BufferType::Log

A new variant:

```rust
pub enum BufferType {
    Mentions,
    Server,
    Channel,
    Query,
    DccChat,
    Special,
    Shell,
    Log,        // ← new
}
```

Sort group: `2` (same as `Channel`). Log buffers always sit under a pseudo-network whose `connection_id` differs from any real network, so they never mix with live channels in the sidebar; the shared sort group simply means a network's log buffers list above its DCC chats / specials, the same ordering users already know. Keeping `Log` as a *distinct* enum variant from `Channel` is what suppresses the right-side nick list — `show_nicklist` already gates on `BufferType::Channel`, so Log buffers automatically render without it.

## Lazy Loading

```text
INITIAL_LIMIT = 200
PAGE_LIMIT = 200
```

When a log buffer becomes active for the first time and `messages.is_empty()`:

```rust
storage::query::get_messages(&db, network, buffer, /* before */ None, INITIAL_LIMIT, ...)
```

`get_messages` already orders DESC + reverses to ASC chronological. The 200 returned messages are inserted in order.

When the user scrolls up (`scroll_offset` reaches the loaded top), we fire:

```rust
let oldest_ts = buf.messages.front().map(|m| m.timestamp.timestamp());
storage::query::get_messages(&db, network, buffer, oldest_ts, PAGE_LIMIT, ...)
```

Returned rows are prepended. If `< PAGE_LIMIT` were returned, we mark the buffer as `history_exhausted = true` (a new field on `Buffer`, used **only** by log mode — defaults `false` and is written nowhere in chat mode).

Memory cap: not enforced in V1. With 200/page even an aggressive scroll-up session loads at most a few thousand messages before reaching the start of the log; the buffer will not balloon under realistic use. If telemetry shows otherwise we can add bounded eviction.

## Topic Bar

Existing topic bar widget gets a new branch:

```text
📜 Log: #polska @ libera  •  12,847 lines  •  2024-08-12 → 2026-05-09
```

Computed once per buffer activation:

```sql
SELECT COUNT(*), MIN(timestamp), MAX(timestamp)
FROM messages
WHERE network = ? AND buffer = ?
```

Cached on the `Buffer` itself (new optional fields `log_total_lines`, `log_oldest_ts`, `log_newest_ts`) so the topic bar render doesn't requery on every frame.

## Status Line

```text
log mode • libera/#polska • showing 200/12,847 from 2026-04-15 14:23  •  ↑/↓ scroll • / search • Q quit
```

The "from {timestamp}" reflects the oldest currently-loaded message — i.e. how far back the user has paged. As they scroll up and pages prepend, this value moves backward.

## Input Bar (slash commands only)

In log mode the input bar accepts only `/`-prefixed input. Anything else flashes a status hint and is ignored.

**V1 — required:**

| Command | Behaviour |
|---------|-----------|
| `/search <text>` | `storage::search_messages(db, query, Some(network), Some(buffer), 1000)` (FTS5 if plain DB; LIKE fallback if encrypted). Highlights matches in the visible buffer; `n` / `N` jump to next/previous match. |
| `/quit` | Exit log browser process. Also bound to `q` / `Q` outside input. |
| `/help` | List of available commands in the active buffer. |

**V1.1 — follow-up:**

| Command | Behaviour |
|---------|-----------|
| `/jump <YYYY-MM-DD[ HH:MM[:SS]]>` | Loads ±100 messages bracketing the closest timestamp; replaces visible window. |
| `/grep <regex>` | In-memory filter on the visible buffer. `/grep` with no argument clears. |

## Layout — Untouched

```
┌─ topic bar ─────────────────────────────────────────────────────────┐
│ 📜 Log: #polska @ libera • 12,847 lines • 2024-08 → 2026-05         │
├──────────┬──────────────────────────────────────────────────────────┤
│ libera   │ <messages>                                                │
│  #rust   │                                                           │
│ #polska* │  ← active                                                 │
│ ircnet   │                                                           │
│  #pl     │                                                           │
│ oftc     │                                                           │
│  #debian │                                                           │
├──────────┴──────────────────────────────────────────────────────────┤
│ status line                                                          │
│ /_                                                                   │
└──────────────────────────────────────────────────────────────────────┘
```

`buffer_list`, `chat_view`, `topic_bar`, `status_line`, `input` widgets are reused without forking the renderer. New behaviour goes into the **content** they receive, not into new widgets.

## Module Layout

```
src/app/log_browser.rs    NEW  — App methods used only in log_browser_mode
                                  (build_catalog, load_initial, load_older,
                                  cmd_search/jump/grep/help, exit handling).
src/commands/handlers_logs.rs  NEW — log-mode command handlers; small file.
src/state/buffer.rs       MOD  — add BufferType::Log, history_exhausted,
                                  log_total_lines, log_oldest_ts, log_newest_ts.
src/storage/db.rs         MOD  — add open_readonly(path) helper.
src/storage/query.rs      MOD  — add list_networks, list_buffers,
                                  buffer_stats (count + min/max ts).
src/main.rs               MOD  — `repartee l` / `logs` subcommand branch.
src/app/mod.rs            MOD  — log_browser_mode field, gating in `run`.
src/ui/topic_bar.rs       MOD  — log-mode header.
src/ui/status_line.rs     MOD  — log-mode line.
```

## Configuration

No new TOML options for V1. Behaviour is keyboard-driven and the storage path / encryption settings are reused from the existing `[storage]` and `[e2e]` sections.

## Testing Strategy

| Layer | Tests |
|-------|-------|
| `storage::open_readonly` | opens with daemon-style `Connection` and refuses writes. |
| `query::list_networks` / `list_buffers` | seeded DB → expected sorted lists. |
| `query::buffer_stats` | count + min/max timestamp on synthetic rows. |
| `App::build_log_catalog` | given a seeded DB, produces correct pseudo-connections + log buffers. |
| `App::load_initial_messages` / `load_older_messages` | seeded buffer → chronological order, prepend semantics, `history_exhausted` flag flip. |
| Integration smoke | `repartee l` against an empty DB shows empty sidebar and exits cleanly on Q. |

Web-UI parity tests are deferred until the web mirror is built.

## Out of Scope for V1

- Right sidebar showing "top 20 nicks in this log" — defer.
- Bookmark / favorite logs — defer.
- Cross-buffer search.
- Web UI mirror.
- Real-time tail of currently-active channel (i.e. open log of a channel I'm currently connected to and have it auto-scroll). Possible follow-up but introduces multi-process state sync.

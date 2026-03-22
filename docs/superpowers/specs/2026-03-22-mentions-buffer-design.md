# Mentions Buffer Design

## Overview

Replace the current `/mentions` dump-and-mark-read command with a persistent **Mentions buffer** — a special buffer always pinned at the top of the buffer list that displays highlight mentions in a chat-like scrollable view.

## Requirements

- Mentions buffer always at top of buffer list (no number, sort group 0)
- Togglable via `/set display.mentions_buffer true|false` (default: true)
- Renders like a chat window — new mentions at bottom, older scroll up
- Message format: `NETWORK #channel nick❯ text` with timestamp from original message
- Switching to buffer clears activity/highlight state (standard buffer behavior)
- Scrollback: 7 days max, 1000 messages max (whichever is smaller)
- Automatic DB purge: delete mentions older than 7 days (on startup + every 1h tick)
- `/clear` on mentions buffer: wipes ALL messages from buffer AND truncates entire mentions DB table
- When `display.mentions_buffer = false`: buffer hidden, no rendering, mentions still stored in DB
- When `display.mentions_buffer = true`: buffer appears, loads history from DB

## Buffer Identity

- **Buffer ID:** `_mentions` (literal string — NOT via `make_buffer_id`, which would produce `"/mentions"`)
- **BufferType:** New variant `BufferType::Mentions`
- **Sort group:** 0 (before Server=1, Channel=2, Query=3, DccChat=4, Special=5, Shell=6)
- **Display name:** `Mentions` (no `#` prefix, no network prefix)
- **Connection ID:** empty string (internal — not tied to any IRC connection)

## Message Format

Each mention is stored as a `Message` in the buffer:

```
nick:      "{network}"
text:      "#{channel} {original_nick}❯ {original_text}"
timestamp: original message timestamp (not insertion time)
highlight: true (always — renders with pubmsg_mention theme format / accent color)
```

The existing themed rendering pipeline handles layout:
- Timestamp column shows the original time (DATE comes from day-change separators if `show_timestamps` is enabled)
- Nick column shows the network name (colored by nick coloring, may be truncated to `nick_max_length` — acceptable)
- Text column shows `#channel nick❯ message`

For query/PM mentions: buffer name starts with a letter (not `#`), so format becomes:
```
text:      "{query_nick}❯ {original_text}"
```

Detection: if `channel` (from MentionRow) starts with `#` / `&` / `!` / `+` → channel mention, otherwise query/PM.

## Data Flow

### New mention arrives

1. `handle_privmsg` detects `is_mention = true` (existing logic)
2. `add_message_with_activity` adds message to the channel buffer (existing)
3. `MentionAlert` web event pushed to `pending_web_events` (existing)
4. **NEW:** Build a mention `Message` and insert it into the `_mentions` buffer using a **dedicated helper** (`add_mention_to_buffer`) that:
   - Does NOT call `maybe_log` (avoids duplicate DB logging — the mention is already stored via `insert_mention`)
   - Does NOT push a second `MentionAlert` (avoids double-counting the web badge)
   - DOES set `ActivityLevel::Mention` on the buffer and increment `unread_count`
   - DOES push `WebEvent::NewMessage` for the mentions buffer (so web clients see it)
5. Enforce scrollback: cap at 1000 messages in the mentions buffer

### Startup

1. If `display.mentions_buffer = true`: create the `_mentions` buffer
2. Query `SELECT ... FROM mentions WHERE timestamp > (now - 7 days) ORDER BY timestamp ASC LIMIT 1000`
3. Convert rows to `Message` structs using the format above
4. Insert into the buffer's `messages` VecDeque (no logging, no web events — this is history)

### `/set display.mentions_buffer true`

1. Create the `_mentions` buffer
2. Load history from DB (same as startup)
3. Buffer appears in sidebar

### `/set display.mentions_buffer false`

1. Remove the `_mentions` buffer from `state.buffers`
2. If it was the active buffer, switch to the previous buffer
3. Mentions continue to be written to DB (just not displayed)

### `/clear` on mentions buffer

1. `buf.messages.clear()` + `buf.messages.shrink_to(0)` (standard `/clear` behavior)
2. **Additionally:** `DELETE FROM mentions` (truncate the entire table)
3. Reset `buf.activity` and `buf.unread_count`

### `/close` on mentions buffer

Equivalent to `/set display.mentions_buffer false`:
1. Remove buffer from `state.buffers`
2. Switch to previous buffer if active
3. Set `config.display.mentions_buffer = false` and save config

New match arm needed in `cmd_close` (`handlers_ui.rs`).

### `/mentions` command

Repurposed to simply switch to the mentions buffer:
```rust
fn cmd_mentions(app: &mut App, _args: &[String]) {
    if !app.config.display.mentions_buffer {
        add_local_event(app, "Mentions buffer is disabled — /set display.mentions_buffer true");
        return;
    }
    app.state.set_active_buffer("_mentions");
    app.scroll_offset = 0;
}
```

### Periodic purge (every 1h tick)

1. `DELETE FROM mentions WHERE timestamp < (now - 7 days)`
2. Also prune the in-memory buffer: remove messages with `timestamp < (now - 7 days)`
3. Enforce 1000-message cap after pruning

## Sorting

Add to `sort_group()` on `BufferType` in `state/buffer.rs` (NOT sorting.rs — that's where `sort_buffers` lives):

```rust
BufferType::Mentions => 0,
```

The `sort_buffers` function sorts by `connection_label → sort_group → name`. The empty `connection_id` on the mentions buffer sorts lexicographically before any real connection label, AND sort_group 0 ensures it's first even if ordering changes. For robustness, add an explicit check in `sort_buffers`: buffers with `BufferType::Mentions` sort before everything else regardless of connection label.

Buffer list rendering:
- **Skip number prefix** for `BufferType::Mentions` (show just the name)
- **Skip connection header** for empty `connection_id` (prevents blank header line above Mentions)

## Activity & Highlighting

- New mention → `ActivityLevel::Mention` on the `_mentions` buffer (set by the dedicated `add_mention_to_buffer` helper)
- Switching to the buffer clears activity (existing `set_active_buffer` behavior)
- The sidebar entry uses `item_activity_4` theme format (same as any mention-highlighted buffer)
- Web frontend: `MentionAlert` broadcast continues as-is (from the channel buffer, NOT duplicated from mentions buffer)

## Web Frontend

- The `_mentions` buffer appears in the web buffer list like any other buffer
- `BufferMeta.buffer_type = "mentions"` — web can style it differently (pin to top via CSS)
- `FetchMessages` for `_mentions` works via the standard buffer message store
- The existing `MentionAlert` / `mention_count` system continues for the badge
- `mention_count` in `SyncInit` snapshot: query `get_unread_mention_count` from DB instead of hardcoded 0

## Config

New field in `DisplayConfig`:

```rust
/// Show the Mentions buffer at the top of the buffer list.
pub mentions_buffer: bool,  // default: true
```

Added to `/set` tab completion and `get/set` handlers.

## Database Changes

Add a `timestamp` index for efficient purge and load queries:

```sql
CREATE INDEX IF NOT EXISTS idx_mentions_timestamp ON mentions (timestamp);
```

New queries:

```sql
-- Purge mentions older than 7 days
DELETE FROM mentions WHERE timestamp < ?1;

-- Load recent mentions for buffer (7 days, max 1000)
SELECT * FROM mentions
WHERE timestamp > ?1
ORDER BY timestamp ASC
LIMIT 1000;

-- Truncate all mentions (for /clear)
DELETE FROM mentions;
```

The `read_at` column becomes unused by the new design. Kept for backward compatibility.

## Files to Modify

| File | Change |
|------|--------|
| `src/state/buffer.rs` | Add `BufferType::Mentions` variant, sort_group 0 |
| `src/state/sorting.rs` | Special-case Mentions to always sort first |
| `src/state/events.rs` | Add `add_mention_to_buffer` helper (no logging, no duplicate MentionAlert) |
| `src/irc/events.rs` | Push mention message to `_mentions` buffer after highlight detection |
| `src/app.rs` | Create mentions buffer on startup, load history, periodic purge |
| `src/commands/handlers_admin.rs` | Repurpose `cmd_mentions` to switch-to-buffer |
| `src/commands/handlers_ui.rs` | Handle `/clear` on mentions buffer (truncate DB), `/close` on mentions buffer |
| `src/commands/settings.rs` | Wire `display.mentions_buffer` get/set with buffer create/remove |
| `src/config/mod.rs` | Add `mentions_buffer: bool` to `DisplayConfig` |
| `src/storage/db.rs` | Add `idx_mentions_timestamp` index |
| `src/storage/query.rs` | Add `purge_old_mentions`, `load_recent_mentions`, `truncate_mentions` queries |
| `src/ui/buffer_list.rs` | Skip number and connection header for `BufferType::Mentions` |
| `src/web/snapshot.rs` | Map `BufferType::Mentions` to `"mentions"` string, fix `mention_count` |
| `web-ui/src/components/buffer_list.rs` | Pin mentions buffer to top in web sidebar |

## Edge Cases

- **No storage:** If `logging.enabled = false`, mentions buffer still works for live session (in-memory only, no history on restart, no purge)
- **Buffer switch during load:** History load is synchronous from SQLite (fast for 1000 rows), no race condition
- **Multiple networks:** Mentions from all networks go into the single `_mentions` buffer — the network name in the nick column disambiguates
- **DCC/query mentions:** Format omits `#channel` prefix for non-channel buffers (detected by first char of channel name)
- **`/close` on mentions buffer:** Sets `display.mentions_buffer = false`, removes buffer, saves config
- **INVITE highlight:** Currently bypasses `add_message_with_activity`. Not included in mentions buffer (INVITE is informational, not a chat mention)
- **Duplicate prevention:** The dedicated `add_mention_to_buffer` helper avoids both duplicate DB logging and duplicate `MentionAlert` web events

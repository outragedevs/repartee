# Logging & Search

repartee includes a built-in logging system backed by SQLite with optional encryption and full-text search.

## Configuration

```toml
[logging]
enabled = true
encrypt = false
retention_days = 0              # 0 = keep forever
event_retention_hours = 72      # auto-prune events after 72h (0 = keep forever)
exclude_types = []              # e.g. ["join", "part", "quit"]
```

## Storage

Logs are stored in `~/.repartee/logs/messages.db` using SQLite with WAL (Write-Ahead Logging) mode for concurrent read/write performance.

### Database schema

Each message is stored with:

| Column | Type | Description |
|---|---|---|
| `msg_id` | TEXT | Unique UUID |
| `network` | TEXT | Connection/network ID |
| `buffer` | TEXT | Channel or query name |
| `timestamp` | INTEGER | Unix timestamp |
| `msg_type` | TEXT | Message type (privmsg, join, etc.) |
| `nick` | TEXT | Sender nick |
| `text` | TEXT | Message content |
| `highlight` | INTEGER | 1 if message is a highlight |
| `ref_id` | TEXT | Reference to primary row (for fan-out dedup) |

### Fan-out deduplication

Events like QUIT and NICK affect multiple channels. repartee stores a single full row for the first channel and reference rows (with empty text and a `ref_id` pointing to the primary) for subsequent channels. This saves storage while preserving per-channel history.

## Encryption

When `encrypt = true`, message text is encrypted with AES-256-GCM before storage. The encryption key is derived from a passphrase stored in `~/.repartee/.env`:

```bash
# ~/.repartee/.env
REPARTEE_LOG_KEY=your-secret-passphrase
```

Encrypted logs can only be searched/read with the correct key.

## Full-text search

repartee uses SQLite FTS5 for fast full-text search across all logs:

```
/log search <query>
```

Search supports standard FTS5 syntax including phrase matching (`"exact phrase"`), prefix matching (`prefix*`), and boolean operators (`AND`, `OR`, `NOT`).

## Commands

### `/log status`

Show logging status, database size, and message count.

### `/log search <query>`

Search across all logged messages.

## Chat history backlog

When you open a channel, query, or DCC chat buffer, repartee automatically loads the most recent messages from the log database — so you immediately see recent context without scrolling.

```
/set display.backlog_lines 20    # default — load last 20 messages
/set display.backlog_lines 50    # load more history
/set display.backlog_lines 0     # disable backlog
```

Backlog messages appear at the top of the buffer, followed by a separator:

```
─── End of backlog (20 lines) ───
```

Backlog messages do not trigger highlights or notifications, and are not re-logged to the database (they already exist). This works for autoconnect channels, manual `/join`, queries opened via incoming messages, and DCC chat reconnections.

## Event retention

Event messages (join, part, quit, nick, kick, mode changes) are high-volume noise that accumulates over time. The `event_retention_hours` setting automatically prunes old event messages while keeping actual chat history intact.

```
/set logging.event_retention_hours 72    # default — keep 3 days of events
/set logging.event_retention_hours 24    # aggressive — only 1 day
/set logging.event_retention_hours 0     # disable — keep events forever
```

Pruning runs on startup and every hour in the background. It only deletes rows with `type = 'event'` — chat messages, actions, notices, and CTCPs are never touched regardless of their age.

This complements `retention_days` which controls the maximum age for **all** message types. When both are set, event messages are pruned at whichever threshold is reached first.

## Batched writes

Messages are written to the database in batches (50 rows or every 1 second) using an async writer task connected via a tokio mpsc channel. This minimizes SQLite lock contention and ensures the UI never blocks on disk I/O.

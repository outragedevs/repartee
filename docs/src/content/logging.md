# Logging & Search

rustirc includes a built-in logging system backed by SQLite with optional encryption and full-text search.

## Configuration

```toml
[logging]
enabled = true
encrypt = false
retention_days = 0       # 0 = keep forever
exclude_types = []       # e.g. ["join", "part", "quit"]
```

## Storage

Logs are stored in `~/.rustirc/logs/messages.db` using SQLite with WAL (Write-Ahead Logging) mode for concurrent read/write performance.

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

Events like QUIT and NICK affect multiple channels. rustirc stores a single full row for the first channel and reference rows (with empty text and a `ref_id` pointing to the primary) for subsequent channels. This saves storage while preserving per-channel history.

## Encryption

When `encrypt = true`, message text is encrypted with AES-256-GCM before storage. The encryption key is derived from a passphrase stored in `~/.rustirc/.env`:

```bash
# ~/.rustirc/.env
RUSTIRC_LOG_KEY=your-secret-passphrase
```

Encrypted logs can only be searched/read with the correct key.

## Full-text search

rustirc uses SQLite FTS5 for fast full-text search across all logs:

```
/log search <query>
```

Search supports standard FTS5 syntax including phrase matching (`"exact phrase"`), prefix matching (`prefix*`), and boolean operators (`AND`, `OR`, `NOT`).

## Commands

### `/log status`

Show logging status, database size, and message count.

### `/log search <query>`

Search across all logged messages.

## Batched writes

Messages are written to the database in batches (50 rows or every 1 second) using an async writer task connected via a tokio mpsc channel. This minimizes SQLite lock contention and ensures the UI never blocks on disk I/O.

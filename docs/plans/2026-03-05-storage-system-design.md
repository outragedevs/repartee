# Storage System Design

## Goal

Persistent message logging with SQLite for backlog feed (web frontend), full-text search, read markers, and optional AES-256-GCM encryption. 1:1 port of kokoirc's `src/core/storage/`.

## Architecture

A `src/storage/` module — SQLite (WAL mode), batched async writer via `tokio::sync::mpsc`, FTS5 full-text search (plain mode only), AES-256-GCM encryption (optional), read markers for web client sync.

### Components

| File | Purpose |
|------|---------|
| `types.rs` | `LogRow`, `StoredMessage`, `ReadMarker`, `LoggingConfig` |
| `db.rs` | Open/close SQLite, schema creation, migrations, purge |
| `writer.rs` | Batched writer — mpsc receiver, flushes every 1s or 50 rows, optional encryption |
| `query.rs` | `get_messages()`, `search_messages()`, `get_buffers()`, `get_stats()`, read marker CRUD |
| `crypto.rs` | AES-256-GCM encrypt/decrypt, key load/generate from `.env` |
| `mod.rs` | Public API: `init_storage()`, `log_message()`, `shutdown_storage()`, re-exports |

### Data Flow

```
Message added to buffer
  -> storage::log_message(network, buffer, msg_id, type, text, nick, highlight, timestamp)
    -> mpsc::Sender<LogRow> -> Writer task
      -> Batch collects (50 rows or 1s timer)
        -> Transaction: INSERT INTO messages (encrypt text if configured)
        -> FTS5 sync (plain mode only)
```

### Schema

```sql
CREATE TABLE messages (
  id        INTEGER PRIMARY KEY AUTOINCREMENT,
  msg_id    TEXT,
  network   TEXT NOT NULL,
  buffer    TEXT NOT NULL,
  timestamp INTEGER NOT NULL,
  type      TEXT NOT NULL,
  nick      TEXT,
  text      TEXT NOT NULL,
  highlight INTEGER DEFAULT 0,
  iv        BLOB
);
CREATE INDEX idx_messages_lookup ON messages(network, buffer, timestamp);
CREATE INDEX idx_messages_time ON messages(timestamp);
CREATE INDEX idx_messages_msg_id ON messages(msg_id);

CREATE TABLE read_markers (
  network   TEXT NOT NULL,
  buffer    TEXT NOT NULL,
  client    TEXT NOT NULL,
  last_read INTEGER NOT NULL,
  PRIMARY KEY (network, buffer, client)
);

-- FTS5 (plain mode only)
CREATE VIRTUAL TABLE messages_fts USING fts5(nick, text, content=messages, content_rowid=id);
```

### Config

New `[logging]` section in config.toml:

```toml
[logging]
enabled = true
encrypt = false
retention_days = 365
exclude_types = []
```

### Encryption

- AES-256-GCM via `aes-gcm` 0.10.3
- 256-bit key as hex in `.env` (`RUSTIRC_LOG_KEY=...`)
- Auto-generated on first use, `.env` chmod 0o600
- Each row gets a random 12-byte IV stored in `iv` column
- FTS5 disabled when `encrypt = true`

### Crate Versions

- `rusqlite` 0.38.0 (features: `bundled-full`)
- `aes-gcm` 0.10.3
- `uuid` 1.21.0 (features: `v4`)
- `rand` 0.10.0
- `base64` 0.22.1

### Integration Points

- `app.rs`: `init_storage()` at startup, `shutdown_storage()` on quit
- After each `add_message()`: call `log_message()`
- Writer runs as spawned tokio task
- Database file at `~/.config/repartee/logs.db`

### Testing

- Crypto: encrypt/decrypt roundtrip
- Schema: creation, insert, query, pagination
- FTS5: search matching
- Read markers: CRUD, unread counts
- Purge: retention enforcement
- Writer: batching (send N messages, flush, verify count)

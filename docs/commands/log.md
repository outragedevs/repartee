---
category: Logging
description: Chat log management
---

# /log

## Syntax

    /log [status|search] [query]

## Description

Manage persistent chat logs. Messages are stored in a local SQLite database
at `~/.repartee/logs/messages.db` with WAL mode for concurrent access.

Only messages that reach the UI are logged — messages filtered by `/ignore`,
antiflood, or script `stop()` propagation are never stored.

## Subcommands

### status

Show logging status including message count, database size, and encryption mode.

    /log status

This is the default when no subcommand is given.

### search

Full-text search across logged messages (plain text mode only).

    /log search <query>

Searches within the current buffer's network and channel context. Results
show the 20 most recent matches with timestamps.

Search is not available in encrypted mode since ciphertext cannot be indexed.

## Configuration

```toml
[logging]
enabled = true                 # enable/disable logging
encrypt = false                # AES-256-GCM encryption (disables search)
retention_days = 0             # 0 = keep forever
event_retention_hours = 72     # auto-prune events after 72h (0 = keep forever)
exclude_types = []             # filter: "message", "action", "notice", "ctcp", "event"
```

### Encryption

When `encrypt = true`, message text is encrypted with AES-256-GCM using a
256-bit key auto-generated in `~/.repartee/.env`. No password is required —
same trust model as irssi logs or SSH keys.

Only the `text` column is encrypted. Network, buffer, nick, timestamp, and
type remain queryable for the future web frontend.

### Retention

Set `retention_days` to automatically purge old messages on startup.
`0` means keep forever.

### Event retention

`event_retention_hours` controls how long event messages (join/part/quit/nick/kick/mode)
are kept. Defaults to 72 hours. Pruning runs on startup and every hour in the background.
Only event-type rows are affected — chat messages are never touched.

```
/set logging.event_retention_hours 72    # default
/set logging.event_retention_hours 0     # keep forever
```

### Excluding Types

Filter specific message types from logging:

```toml
[logging]
exclude_types = ["event"]  # skip join/part/quit/nick events
```

This is especially important if you're on many channels. A single QUIT or NICK
change fans out to every shared channel — on 70 channels, one quit becomes 35+
log rows. Adding `"event"` to `exclude_types` prevents this bloat.

## Examples

    /log
    /log status
    /log search ssl certificate

## See Also

/set, /ignore

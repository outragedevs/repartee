---
category: Info
description: Show and clear unread mentions
---

# /mentions

Show all unread highlight mentions across all buffers, then mark them as read.

## Usage

```
/mentions
```

## Output

Each mention shows: `[timestamp] #channel <nick> message text`

After displaying, all mentions are marked as read and the counter resets to 0.

## Requirements

Logging must be enabled (`logging.enabled = true` in config) — mentions are stored in the SQLite database alongside message logs.

## Web Integration

On the web frontend, the unread mention count appears as a badge in the top bar. Viewing mentions on either the terminal or web clears them on both — state is shared.

## See also

`/set logging.enabled`, `/log`

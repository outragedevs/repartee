---
category: Info
description: Switch to the mentions buffer
---

# /mentions

Switch to the Mentions buffer — a persistent buffer pinned at the top of the sidebar that aggregates all highlight mentions from all networks.

## Usage

```
/mentions
```

## Description

The Mentions buffer collects every message where your nick is mentioned across all connected networks. Messages are displayed in a scrollable chat view with the format:

```
[timestamp] network  #channel nick❯ message text
```

The network name appears in the nick column, and the channel + sender are shown in the message text.

## Buffer Behavior

- **Always first** in the buffer list (no number, pinned to top)
- **Scrollback**: 7 days or 1000 messages, whichever is smaller
- **Activity**: highlights in the sidebar when new mentions arrive
- **Switching** to the buffer clears the highlight (standard buffer behavior)

## Configuration

Toggle the mentions buffer on or off:

```
/set display.mentions_buffer true    — show the buffer (default)
/set display.mentions_buffer false   — hide the buffer (mentions still stored in DB)
```

## Commands

- `/mentions` — switch to the mentions buffer
- `/clear` — when used in the mentions buffer, clears all messages AND deletes all mentions from the database
- `/close` — hides the mentions buffer and sets `display.mentions_buffer false`

## Storage

Mentions are stored in the SQLite database. When the buffer is re-enabled after being disabled, the last 7 days of mentions (up to 1000) are loaded from the database. Old mentions (>7 days) are automatically purged every hour.

## See also

`/set display.mentions_buffer`, `/clear`

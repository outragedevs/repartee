---
category: Connection
description: Detach from the terminal, keeping repartee running in the background
---

# /detach

## Syntax

```
/detach
```

## Description

Detaches from the current terminal session. The repartee backend continues running in the background — IRC connections stay alive, messages are logged, and scripts keep executing.

After detaching, your terminal is restored and your shell prompt returns.

## Reattaching

Use `repartee a` (or `repartee attach`) from any terminal to reconnect:

```bash
repartee a           # attach to the only running session
repartee a <pid>     # attach to a specific session by PID
```

## Keyboard shortcuts

You can also detach with keyboard chords instead of typing the command:

- **Ctrl+\\** (Ctrl + backslash)
- **Ctrl+Z**

## Examples

```
/detach              # detach from terminal
```

From your shell after detaching:

```bash
repartee a           # reattach to the session
```

## Aliases

- `/dt`

## See Also

- [Sessions & Detach](../sessions.html) — full documentation on session management

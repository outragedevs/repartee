---
category: Other
description: Open an embedded terminal
---

# /shell

## Syntax

    /shell [open|cmd|close|list] [command]

## Aliases

    /sh

## Description

Open an embedded PTY-backed shell terminal inside Repartee. You get a
real terminal experience (zsh, bash, vim, htop) without leaving the IRC
client.

Shell buffers appear under a "Shell" group in the sidebar. You can open
multiple shells and switch between them with Alt+N.

### Input Mode

When a shell buffer is active, all keyboard input is forwarded to the
shell process. To return to IRC command input, press **Ctrl+]** (the
telnet escape convention).

**Alt+digit** and **Alt+Left/Right** still work for buffer switching
even while in shell input mode. Clicking a buffer in the sidebar also
switches away from the shell.

## Subcommands

- `/shell` or `/shell open` — Open a new shell using `$SHELL`
- `/shell cmd <command>` — Open a shell running a specific command (e.g. `/shell cmd htop`)
- `/shell close` — Close the currently active shell buffer (kills the process)
- `/shell list` — List all active shell sessions

If the subcommand is not recognized, it is treated as a command to run
(e.g. `/shell htop` is equivalent to `/shell cmd htop`).

## Examples

    /shell
    /sh cmd htop
    /shell cmd vim /etc/hosts
    /shell close
    /shell list

## Notes

- When you type `exit` in a shell, the buffer is automatically closed.
- Shell buffers are not logged to the message database.
- The shell PTY is resized automatically when the terminal window changes size.

## See Also

/close

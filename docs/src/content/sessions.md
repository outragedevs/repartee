# Sessions & Detach

repartee can run in the background while you close your terminal and reattach later — like tmux or screen, but built into the client.

## How it works

When you launch `repartee`, it forks into two processes:

- **Backend** — headless daemon that manages IRC connections, state, and the Unix socket
- **Shim** — lightweight terminal bridge that renders the UI and forwards your input

When you detach, the shim exits and your shell prompt returns. The backend keeps running — IRC connections stay alive, messages continue to be logged, and scripts keep executing. When you reattach, a new shim connects to the backend's socket and you pick up right where you left off.

## Detaching

Three ways to detach from a running session:

| Method | Description |
|--------|-------------|
| `/detach` | Type the command in the input bar |
| `Ctrl+\` | Keyboard chord (backslash) |
| `Ctrl+Z` | Keyboard chord |

After detaching, your terminal is restored and the shell prompt returns. The repartee process continues running in the background.

## Reattaching

```bash
repartee a           # attach to the only running session
repartee a 12345     # attach to a specific session by PID
repartee attach      # long form
```

If multiple sessions are running, `repartee a` lists them so you can pick one:

```
Active repartee sessions:
  PID 12345 — ~/.repartee/sessions/12345.sock
  PID 67890 — ~/.repartee/sessions/67890.sock
Specify a PID: repartee a <pid>
```

## Starting headless

You can start repartee without a terminal at all:

```bash
repartee -d          # start detached (headless)
repartee --detach    # long form
```

This starts the backend directly — no terminal is opened, no splash screen is shown. IRC connections are established, scripts are loaded, and the socket is created immediately. Attach when you're ready with `repartee a`.

Useful for servers, startup scripts, or running repartee in a `systemd` service.

## Session files

Sessions are tracked via Unix sockets in `~/.repartee/sessions/`:

```
~/.repartee/sessions/
  12345.sock    # socket for PID 12345
  67890.sock    # socket for PID 67890
```

Stale sockets from crashed or killed processes are cleaned up automatically on startup and when listing sessions.

## Multiple sessions

You can run multiple independent repartee instances, each with its own PID and socket. This is useful for connecting to separate networks with different identities.

Each session runs its own event loop, IRC connections, and scripts. Use `repartee a <pid>` to attach to a specific one.

## What survives detach

Everything:

- **IRC connections** — stay connected, continue receiving messages
- **Chat history** — scrollback buffers are preserved in memory
- **Log storage** — messages continue to be written to SQLite
- **Scripts** — Lua scripts keep running (timers, event handlers)
- **Channel state** — nick lists, topics, modes, ban lists
- **Input history** — your command history is preserved

## Terminal switching

When you reattach from a different terminal (or a different terminal emulator), repartee automatically:

- Detects the new terminal's image protocol capabilities (Kitty, iTerm2, Sixel)
- Updates font size measurements for correct image scaling
- Resizes the UI to fit the new terminal dimensions

This means you can detach from iTerm2 on your laptop and reattach from kitty on your desktop — image previews will use the correct protocol for each terminal.

## SIGHUP handling

If your terminal is closed unexpectedly (window closed, SSH disconnection, etc.), repartee catches `SIGHUP` and auto-detaches instead of crashing. The session remains running and can be reattached.

## Tips

- **Check running sessions**: `repartee a` with no arguments lists all live sessions
- **Clean shutdown**: use `/quit` from an attached session to disconnect from IRC and exit the backend
- **Force kill**: `kill <pid>` sends SIGTERM — repartee sends QUIT to IRC servers and shuts down cleanly
- **Startup script**: add `repartee -d` to your shell profile or system startup to always have a session ready

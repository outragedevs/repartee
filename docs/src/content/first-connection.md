# First Connection

## Quick start

After installing repartee, launch it:

```bash
repartee
```

You'll see the main UI with a status buffer. Let's connect to an IRC network.

## Add a server

Edit `~/.repartee/config.toml` (created on first run) and add a server:

```toml
[servers.libera]
label = "Libera"
address = "irc.libera.chat"
port = 6697
tls = true
autoconnect = true
channels = ["#repartee"]
```

Or use the `/server` command at runtime:

```
/server add libera irc.libera.chat
/server connect libera
```

## Join channels

Once connected, join channels with:

```
/join #channel
/join #secret mykey
```

Channels listed in your config's `channels` array are joined automatically on connect.

## Navigation

- **Esc + 1–9** — switch between buffers (windows)
- **Ctrl+N / Ctrl+P** — next / previous buffer
- **Click** on buffer list or nick list entries
- **Mouse wheel** — scroll chat history
- **Tab** — nick completion
- **Up/Down** — input history

## SASL authentication

For networks that support SASL (Libera Chat, OFTC, etc.), add credentials to `~/.repartee/.env`:

```bash
# ~/.repartee/.env
LIBERA_SASL_USER=mynick
LIBERA_SASL_PASS=hunter2
```

Then in your config:

```toml
[servers.libera]
address = "irc.libera.chat"
port = 6697
tls = true
sasl_user = "mynick"
# sasl_pass loaded from .env
```

## Next steps

- [Configuration](configuration.html) — full config reference
- [Commands](commands.html) — all available commands
- [Theming](theming.html) — customize colors and layout

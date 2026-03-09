# Configuration

## Config location

repartee stores its configuration in `~/.repartee/config.toml`. This file is created automatically on first run with sensible defaults.

The full directory layout:

```
~/.repartee/
  config.toml          # main configuration
  .env                 # credentials (passwords, SASL)
  themes/              # custom themes
  scripts/             # user scripts (Lua)
  logs/messages.db     # chat logs (SQLite)
```

## Full annotated example

```toml
[general]
nick = "mynick"
username = "mynick"
realname = "repartee user"
theme = "default"
timestamp_format = "%H:%M:%S"
flood_protection = true
ctcp_version = "repartee"

[display]
nick_column_width = 8
nick_max_length = 8
nick_alignment = "right"       # "left", "right", or "center"
nick_truncation = true
show_timestamps = true
scrollback_lines = 2000

[sidepanel.left]
width = 20
visible = true

[sidepanel.right]
width = 18
visible = true

[statusbar]
items = ["active_windows", "nick_info", "channel_info", "lag", "time"]

[servers.libera]
label = "Libera"
address = "irc.libera.chat"
port = 6697
tls = true
tls_verify = true
autoconnect = true
channels = ["#repartee", "#secret mykey"]
autosendcmd = "MSG NickServ identify pass; WAIT 2000; MODE $N +i"
# nick = "othernick"           # per-server nick override
# sasl_user = "mynick"
# sasl_pass = "hunter2"
# sasl_mechanism = "SCRAM-SHA-256"  # PLAIN (default), EXTERNAL, SCRAM-SHA-256
# bind_ip = "192.168.1.100"   # bind to specific local IP (vhost)
# auto_reconnect = true
# reconnect_delay = 30
# reconnect_max_retries = 10

[image_preview]
enabled = true
protocol = "auto"              # "auto", "kitty", "iterm2", "sixel", "symbols"
max_width = 0                  # 0 = auto
max_height = 0                 # 0 = auto
cache_max_mb = 100
cache_max_days = 7
fetch_timeout = 30             # seconds
max_file_size = 10485760       # bytes (10 MB)
kitty_format = "rgba"

[logging]
enabled = true
encrypt = false
retention_days = 0             # 0 = keep forever
exclude_types = []             # e.g. ["join", "part", "quit"]

[aliases]
wc = "/close"
j = "/join"

[scripts]
autoload = ["slap"]
# debug = true

[[ignores]]
mask = "*!*@spammer.host"
levels = ["ALL"]
```

## Sections explained

### `[general]`

Global identity and behavior. The `nick`, `username`, and `realname` are used as defaults for all servers unless overridden per-server. Set `theme` to the name of a theme file in `~/.repartee/themes/` (without the `.theme` extension).

### `[display]`

Controls how messages are rendered. `nick_column_width` sets the fixed-width column for nicks in chat view. `scrollback_lines` is the number of messages kept in memory per buffer.

### `[sidepanel]`

Left panel shows buffer list, right panel shows nick list. Set `visible = false` to hide a panel. Widths are in terminal columns.

### `[statusbar]`

Configure which items appear in the status line. Available items: `active_windows`, `nick_info`, `channel_info`, `lag`, `time`.

### `[servers.*]`

Each server gets a unique identifier (the key after `servers.`). The `channels` array lists channels to auto-join on connect. Channels with keys use the format `"#channel key"`.

Set `sasl_mechanism` to override automatic mechanism selection. Available: `PLAIN` (default), `EXTERNAL` (client TLS certificate), `SCRAM-SHA-256` (secure challenge-response).

Set `bind_ip` to bind to a specific local IP address when connecting. Useful for multi-IP hosts (vhosts/bouncers). Supports both IPv4 and IPv6 — DNS resolution automatically filters to match the address family. Can also be set per-connection with `/connect -bind=<ip>` or `/server add -bind=<ip>`.

### `[logging]`

Chat logging to SQLite. When `encrypt = true`, messages are encrypted with AES-256-GCM. `retention_days = 0` keeps logs forever.

### `[aliases]`

Custom command shortcuts. The key is the alias name, the value is the command it expands to.

### `[scripts]`

The `autoload` array lists script names to load on startup. Scripts live in `~/.repartee/scripts/` as `.lua` files.

### `[[ignores]]`

Ignore patterns for filtering unwanted messages. Uses wildcard matching (`*!*@host`). Levels: `MSGS`, `PUBLIC`, `NOTICES`, `ACTIONS`, `JOINS`, `PARTS`, `QUITS`, `NICKS`, `KICKS`, `CTCPS`, `ALL`.

## Credentials

Passwords and SASL credentials should **not** go in `config.toml` — store them in `~/.repartee/.env` instead.

```bash
# ~/.repartee/.env
LIBERA_SASL_USER=mynick
LIBERA_SASL_PASS=hunter2
LIBERA_PASSWORD=serverpassword
```

The naming convention uses the server identifier uppercased.

## Runtime changes

- **`/set section.field value`** — change a config value at runtime. Changes are saved immediately.
- **`/reload`** — reload theme and config from disk.

# Configuration

## Config location

rustirc stores its configuration in `~/.rustirc/config.toml`. This file is created automatically on first run with sensible defaults.

The full directory layout:

```
~/.rustirc/
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
realname = "rustirc user"
theme = "default"
timestamp_format = "%H:%M:%S"
flood_protection = true
ctcp_version = "rustirc"

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
channels = ["#rustirc", "#secret mykey"]
autosendcmd = "MSG NickServ identify pass; WAIT 2000; MODE $N +i"
# nick = "othernick"           # per-server nick override
# sasl_user = "mynick"
# sasl_pass = "hunter2"
# auto_reconnect = true
# reconnect_delay = 30
# reconnect_max_retries = 10

[image_preview]
enabled = true
protocol = "auto"              # "auto", "kitty", "iterm2", "sixel", "symbols"
max_width = 0                  # 0 = auto
max_height = 0                 # 0 = auto

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
pattern = "*!*@spammer.host"
levels = ["ALL"]
```

## Sections explained

### `[general]`

Global identity and behavior. The `nick`, `username`, and `realname` are used as defaults for all servers unless overridden per-server. Set `theme` to the name of a theme file in `~/.rustirc/themes/` (without the `.theme` extension).

### `[display]`

Controls how messages are rendered. `nick_column_width` sets the fixed-width column for nicks in chat view. `scrollback_lines` is the number of messages kept in memory per buffer.

### `[sidepanel]`

Left panel shows buffer list, right panel shows nick list. Set `visible = false` to hide a panel. Widths are in terminal columns.

### `[statusbar]`

Configure which items appear in the status line. Available items: `active_windows`, `nick_info`, `channel_info`, `lag`, `time`.

### `[servers.*]`

Each server gets a unique identifier (the key after `servers.`). The `channels` array lists channels to auto-join on connect. Channels with keys use the format `"#channel key"`.

### `[logging]`

Chat logging to SQLite. When `encrypt = true`, messages are encrypted with AES-256-GCM. `retention_days = 0` keeps logs forever.

### `[aliases]`

Custom command shortcuts. The key is the alias name, the value is the command it expands to.

### `[scripts]`

The `autoload` array lists script names to load on startup. Scripts live in `~/.rustirc/scripts/` as `.lua` files.

### `[[ignores]]`

Ignore patterns for filtering unwanted messages. Uses wildcard matching (`*!*@host`). Levels: `MSGS`, `PUBLIC`, `NOTICES`, `ACTIONS`, `JOINS`, `PARTS`, `QUITS`, `NICKS`, `KICKS`, `CTCPS`, `ALL`.

## Credentials

Passwords and SASL credentials should **not** go in `config.toml` — store them in `~/.rustirc/.env` instead.

```bash
# ~/.rustirc/.env
LIBERA_SASL_USER=mynick
LIBERA_SASL_PASS=hunter2
LIBERA_PASSWORD=serverpassword
```

The naming convention uses the server identifier uppercased.

## Runtime changes

- **`/set section.field value`** — change a config value at runtime. Changes are saved immediately.
- **`/reload`** — reload theme and config from disk.

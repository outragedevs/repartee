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
  dicts/               # Hunspell dictionaries (.dic/.aff)
  sessions/            # Unix sockets for detached sessions
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
backlog_lines = 20             # history lines loaded when buffer opens (0 = off)

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

[dcc]
timeout = 300                  # seconds before pending requests expire
own_ip = ""                    # override IP in DCC offers (empty = auto-detect)
port_range = "0"               # "0" = OS-assigned, "1025 65535" = range
autoaccept_lowports = false    # allow auto-accept from ports < 1024
# autochat_masks = ["*!*@trusted.host"]  # hostmask patterns for auto-accept
max_connections = 10

[spellcheck]
enabled = true
languages = ["en_US"]              # Hunspell language codes
dictionary_dir = ""                # default: ~/.repartee/dicts

[[ignores]]
mask = "*!*@spammer.host"
levels = ["ALL"]
```

## Sections explained

### `[general]`

Global identity and behavior. The `nick`, `username`, and `realname` are used as defaults for all servers unless overridden per-server. Set `theme` to the name of a theme file in `~/.repartee/themes/` (without the `.theme` extension).

### `[display]`

Controls how messages are rendered. `nick_column_width` sets the fixed-width column for nicks in chat view. `scrollback_lines` is the number of messages kept in memory per buffer. `backlog_lines` sets how many historical messages to load from the log database when a channel, query, or DCC buffer is first opened (0 to disable).

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

### `[dcc]`

DCC (Direct Client-to-Client) chat settings. DCC CHAT establishes peer-to-peer TCP connections that bypass the IRC server.

`own_ip` overrides the IP address advertised in DCC offers. When empty, Repartee auto-detects from the IRC socket's local address (like irssi's `getsockname`). Set this to your public IP if behind NAT.

`port_range` controls the TCP port for DCC listeners. `"0"` lets the OS assign a free port. Use `"1025 65535"` or `"5000-5100"` to restrict to a range (useful for firewall rules).

`autochat_masks` is a list of `nick!ident@host` wildcard patterns. Incoming DCC CHAT offers matching any pattern are auto-accepted without prompting.

### `[spellcheck]`

Inline spell checking. When `enabled = true`, misspelled words are underlined in red while typing. Press Tab to cycle suggestions, Space to accept, Escape to revert. `languages` is a list of Hunspell language codes (e.g., `en_US`, `pl_PL`, `de_DE`) — a word is correct if **any** active dictionary accepts it. Place `.dic`/`.aff` files in `~/.repartee/dicts/` (or set `dictionary_dir` to a custom path).

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

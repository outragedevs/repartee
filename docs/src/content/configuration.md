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
  certs/               # TLS certificates for web frontend
```

## Annotated example

This example covers every top-level configuration section and the settings
most commonly changed by hand. Repartee fills omitted fields with their current
defaults. Provider-specific AI model entries are documented under
[`/translate`](commands.html#translate).

```toml
config_version = 1

[general]
nick = "mynick"
username = "mynick"
realname = "repartee Client"
theme = "default"
timestamp_format = "%H:%M:%S"
flood_protection = true
flood_exemptions = []  # nick or nick!user@host wildcard masks exempt from PRIVMSG flood checks
ctcp_version = "repartee"
# default_bind_ip = "192.0.2.10"

[display]
nick_column_width = 8
nick_max_length = 8
nick_alignment = "right"       # "left", "right", or "center"
nick_truncation = true
nick_colors = true             # deterministic per-nick coloring (WeeChat-style)
nick_colors_in_nicklist = true # also color nicks in the sidebar nick list
nick_color_saturation = 0.65   # HSL saturation (0.0–1.0), truecolor only
nick_color_lightness = 0.65    # HSL lightness (0.0–1.0), tune per theme
show_timestamps = true
scrollback_lines = 2000
backlog_lines = 20             # history lines loaded when buffer opens (0 = off)
mentions_buffer = true         # show Mentions buffer at top of sidebar

[sidepanel.left]
width = 20
visible = true

[sidepanel.right]
width = 18
visible = true

[statusbar]
enabled = true
items = ["time", "nick_info", "channel_info", "typing", "lag", "active_windows"]
separator = " | "
prompt = "[$server❱ "

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
# sasl_mechanism = "SCRAM-SHA-512"  # omit to auto-detect the strongest offered
# client_cert_path = "libera-cert.pem"  # TLS client cert, for EXTERNAL/CertFP
# sasl_key_path = "libera-key.pem"      # P-256 key, for ECDSA-NIST256P-CHALLENGE
# bind_ip = "192.168.1.100"   # bind to specific local IP (vhost)
# auto_reconnect = true
# reconnect_delay = 30
# reconnect_max_retries = 10

[image_preview]
enabled = true
inline = false                # automatically show direct-image links below chat messages
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
event_retention_hours = 72     # auto-prune join/part/quit/nick/kick/mode (0 = keep forever)
exclude_types = []             # e.g. ["join", "part", "quit"]

[aliases]
ns = "/msg NickServ $*"
cs = "/msg ChanServ $*"
wc = "/close"
j = "/join $0; /msg $0 hello everyone"

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
enabled = false
computing = true
mode = "replace"                  # "replace" or "highlight"
languages = ["en_US"]              # Hunspell language codes
dictionary_dir = ""                # default: ~/.repartee/dicts

[e2e]
enabled = true
default_mode = "normal"             # "auto-accept", "normal", or "quiet"
ts_tolerance_secs = 300

[shrink]
enabled = false
api_url = "https://shr.al"
outgoing_enabled = true
incoming_enabled = true
min_url_length = 50
outgoing_timeout_ms = 2000
incoming_timeout_ms = 2000
cache_max_entries = 500

[emotes]
enabled = true
render = "graphical"                # "graphical", "text", or "off"
lang = "en"                         # "en" or "pl"
max_cols = 8
max_rows = 3

[typing]
show = true                        # receive and display others' typing indicators
send_channels = true               # send +typing while typing in a channel
send_queries = true                # send +typing while typing in a private query

[translate]
enabled = false
backend = "none"                   # "none", "ai", or test-only "stub"
my_lang = "en"
show_original_in = true
show_original_out = true
timeout_ms = 15000
max_in_flight = 4
max_queue = 200

[translate.buffers."libera/#german"]
incoming = true
outgoing = true
lang = "de"

[web]
enabled = false                    # enable embedded web frontend
bind_address = "127.0.0.1"        # listen address (0.0.0.0 for LAN)
port = 8443                        # HTTPS port
tls_cert = ""                      # custom cert (empty = auto self-signed)
tls_key = ""                       # custom key
timestamp_format = "%H:%M"        # web UI timestamp format
line_height = 1.35                 # CSS line-height for chat messages
nick_column_width = 12
nick_max_length = 9
theme = "nightfall"                # web theme (nightfall, catppuccin-mocha, etc.)
session_days = 90
username = "repartee"
image_previews = false
image_previews_max_per_msg = 4
thumbnail_cache_mb = 200
cloudflare_tunnel_name = ""

[[ignores]]
mask = "*!*@spammer.host"
levels = ["ALL"]
```

## Sections explained

### `[general]`

Global identity and behavior. The `nick`, `username`, and `realname` are used as defaults for all servers unless overridden per-server. Set `theme` to the name of a theme file in `~/.repartee/themes/` (without the `.theme` extension). `flood_exemptions` accepts bare nick patterns or full `nick!user@host` wildcard masks that bypass local incoming `PRIVMSG` flood checks.

### `[display]`

Controls how messages are rendered. `nick_column_width` sets the fixed-width column for nicks in chat view. `scrollback_lines` is the number of messages kept in memory per buffer. `backlog_lines` sets how many historical messages to load from the log database when a channel, query, or DCC buffer is first opened (0 to disable).

**Mentions buffer:** `mentions_buffer = true` (default) shows a persistent Mentions buffer pinned at the top of the sidebar. It aggregates all highlight mentions from all networks in a scrollable chat view. Scrollback is capped at 7 days or 1000 messages. Use `/clear` in the mentions buffer to wipe all stored mentions. Set to `false` to hide the buffer (mentions are still stored in the database and will reappear when re-enabled).

**Nick coloring:** `nick_colors = true` enables deterministic per-nick coloring (WeeChat-style). Each nick gets a consistent color based on a hash of its name. Truecolor terminals use an HSL hue wheel (~360 distinct colors); 256-color terminals fall back to a curated 68-color palette; 16-color terminals use 12 safe ANSI colors. Terminal capability is auto-detected (and re-detected on `repartee a` reattach). Set `nick_colors_in_nicklist = false` to keep the sidebar nick list using theme colors while chat messages stay colored. Tune `nick_color_saturation` and `nick_color_lightness` (0.0–1.0) per theme — dark themes work well with ~0.65, light themes around ~0.40.

### `[sidepanel]`

Left panel shows buffer list, right panel shows nick list. Set `visible = false` to hide a panel. Widths are in terminal columns.

### `[statusbar]`

Configure which items appear in the status line. Available items: `active_windows`, `nick_info`, `channel_info`, `typing`, `lag`, `time`. The `typing` item shows who is currently typing in the active buffer (see `[typing]` below).

### `[servers.*]`

Each server gets a unique identifier (the key after `servers.`). The `channels` array lists channels to auto-join on connect. Channels with keys use the format `"#channel key"`.

#### SASL

Leave `sasl_mechanism` unset and repartee picks the strongest mechanism the server offers that it holds a credential for, in this order:

| Mechanism | Needs | Notes |
|---|---|---|
| `EXTERNAL` | `client_cert_path` | CertFP — the TLS client certificate proves who you are. Nothing is sent. |
| `ECDSA-NIST256P-CHALLENGE` | `sasl_key_path` + `sasl_user` | Signs a server challenge with a NIST P-256 key. Nothing is sent. |
| `SCRAM-SHA-512` | `sasl_user` + `sasl_pass` | Challenge-response; the password never crosses the wire. |
| `SCRAM-SHA-256` | `sasl_user` + `sasl_pass` | As above. |
| `SCRAM-SHA-1` | `sasl_user` + `sasl_pass` | As above. Still beats `PLAIN`. |
| `PLAIN` | `sasl_user` + `sasl_pass` | Sends the password. Last resort — always over TLS. |

Set `sasl_mechanism` to one of those names to pin it. A pinned mechanism the server does not offer means **no SASL at all**, never a quiet downgrade to a weaker one. The `-PLUS` (channel-binding) variants are not implemented and are never selected.

For an OAuth-enabled Soju bouncer, explicitly select `sasl_mechanism = "OAUTHBEARER"`.
Set `sasl_user` to the bouncer account name and store the access token in the
server's `SERVERNAME_SASL_PASS` environment secret, using the same secret storage
as SASL passwords. In either server wizard, choose OAUTHBEARER and enter the token
in the SASL password/token field. Keep `tls = true` and `tls_verify = true`;
connections without verified TLS are rejected before sending credentials.
OAUTHBEARER is never selected automatically: existing passwords must not be
interpreted as tokens. Obtain and renew the access token through your bouncer
administrator's OAuth provider; automatic token acquisition and refresh are not
implemented. This authenticates the bouncer account, not the upstream IRC account.
Lurker's IRC endpoint advertises PLAIN instead, so do not select OAUTHBEARER there.

`client_cert_path` and `sasl_key_path` are separate keys with separate jobs: the first is presented during the TLS handshake, the second is only ever used to sign a challenge. Relative paths resolve against `~/.repartee/certs`.

To use `EXTERNAL` / CertFP, set `tls = true` and point `client_cert_path` to one **PEM file containing both the certificate chain (leaf first) and its unencrypted private key**. PKCS#8 (`PRIVATE KEY`), PKCS#1 (`RSA PRIVATE KEY`), and SEC1 (`EC PRIVATE KEY`) keys are supported. PKCS#12 (`.p12` / `.pfx`) and encrypted private keys are not supported. The IRC connection uses rustls for this PEM identity.

```bash
mkdir -p ~/.repartee/certs
chmod 700 ~/.repartee/certs
(umask 077; openssl req -x509 -newkey rsa:3072 -sha256 -days 365 -nodes \
  -subj "/CN=IRC client" -keyout ~/.repartee/certs/libera-key.pem \
  -out ~/.repartee/certs/libera-cert.pem)
cat ~/.repartee/certs/libera-cert.pem >> ~/.repartee/certs/libera-key.pem
```

Set `client_cert_path = "libera-key.pem"` in the server block, then reconnect and register the certificate with your network's NickServ according to its CertFP instructions. Absolute paths and `~/` paths are also accepted. Relative paths always resolve against the certificates directory, regardless of the working directory. Missing files, missing certificate/key blocks, and mismatched keys produce an error before connecting; `sasl_key_path` is not used for EXTERNAL.

To use `ECDSA-NIST256P-CHALLENGE`, generate a key and register its public half:

```bash
openssl ecparam -genkey -name prime256v1 -noout -out ~/.repartee/certs/libera-key.pem
chmod 600 ~/.repartee/certs/libera-key.pem
# the compressed public key, base64 — what NickServ wants
openssl ec -in ~/.repartee/certs/libera-key.pem -pubout -conv_form compressed -outform DER \
  | tail -c 33 | base64
```

Then `/msg NickServ SET PUBKEY <that base64>` and set `sasl_key_path = "libera-key.pem"`. Both PEM encodings load — `ecdsatool`'s SEC1 (`BEGIN EC PRIVATE KEY`) and OpenSSL 3's PKCS#8 (`BEGIN PRIVATE KEY`).

Set `bind_ip` to bind to a specific local IP address when connecting. Useful for multi-IP hosts (vhosts/bouncers). Supports both IPv4 and IPv6 — DNS resolution automatically filters to match the address family. Can also be set per-connection with `/connect -bind=<ip>` or `/server add -bind=<ip>`.

### `[logging]`

Chat logging to SQLite. When `encrypt = true`, messages are encrypted with AES-256-GCM. `retention_days = 0` keeps logs forever. `event_retention_hours` controls how long event messages (join/part/quit/nick/kick/mode) are kept before automatic pruning — defaults to 72 hours. Set to `0` to keep event messages forever. Event pruning runs hourly in the background and is independent of `retention_days`.

### `[aliases]`

Custom command shortcuts. The key is the alias name, the value is the command template.

Templates support positional args (`$0`-`$9`), range args (`$1-`), all args (`$*`), context variables (`$C` channel, `$N` nick, `$S` server, `$T` buffer), and command chaining with `;`. If no `$` appears in the template, `$*` is appended automatically.

```toml
[aliases]
ns = "/msg NickServ $*"
cs = "/msg ChanServ $*"
wc = "/close"
j = "/join $0; /msg $0 hello everyone"
w = "/who $C"
```

Manage at runtime with `/alias` and `/unalias`.

### `[scripts]`

The `autoload` array lists script names to load on startup. Scripts live in `~/.repartee/scripts/` as `.lua` files.

### `[dcc]`

DCC (Direct Client-to-Client) chat settings. DCC CHAT establishes peer-to-peer TCP connections that bypass the IRC server.

`own_ip` overrides the IP address advertised in DCC offers. When empty, Repartee auto-detects from the IRC socket's local address (like irssi's `getsockname`). Set this to your public IP if behind NAT.

`port_range` controls the TCP port for DCC listeners. `"0"` lets the OS assign a free port. Use `"1025 65535"` or `"5000-5100"` to restrict to a range (useful for firewall rules).

`autochat_masks` is a list of `nick!ident@host` wildcard patterns. Incoming DCC CHAT offers matching any pattern are auto-accepted without prompting.

### `[spellcheck]`

Inline spell checking. `mode = "replace"` replaces a misspelling with the first
suggestion and lets Tab cycle alternatives; `mode = "highlight"` keeps the
typed word and marks it instead. `languages` is a list of Hunspell language
codes such as `en_US`, `pl_PL`, or `de_DE`. The bundled computing dictionary is
controlled independently with `computing`.

### `[e2e]`

RPE2E end-to-end encryption defaults. `enabled` controls whether the encryption
manager is available, `default_mode` is applied by `/e2e on`, and
`ts_tolerance_secs` bounds accepted clock skew. Channel trust and peer state are
managed with `/e2e`, not by editing this table.

### `[shrink]`

URL shortening through the configured shrink-compatible endpoint. The master
switch and incoming/outgoing switches are independent. `SHRINK_API_KEY` belongs
in `~/.repartee/.env`; it is never serialized to this table. See `/help shrink`
for runtime/restart behavior.

### `[emotes]`

Built-in `:name:` emotes. `render` accepts `graphical`, `text`, or `off`; `lang`
selects English or Polish picker names. `max_cols` and `max_rows` bound inline
image dimensions in terminal cells.

### `[translate]` and `[translate.ai]`

Per-buffer incoming and outgoing translation. `enabled` is the master switch,
`backend` selects `none`, `ai`, or the test-only `stub`, and `my_lang` is the
language you read and write. Manage buffer mappings with `/translate addin`,
`addout`, `delin`, and `delout`; use `/help translate` for AI providers, keys,
language support, fallback routing, and restart behavior.

### `[typing]`

IRCv3 `+typing` client tag support (typing indicators). `show = true` (default) receives and displays other people's typing status in the TUI status line and the web UI; set to `false` to stop showing it, both live and after reload. `send_channels` and `send_queries` independently control whether Repartee sends its own `+typing` while you type in a channel or a private query, respectively — turn either off if you'd rather not announce your own typing in that context. All three are booleans, so use `/set typing.show false` (not `off`) to disable one at runtime.

Turning a send switch off at runtime stops all further notifications immediately, but it does **not** retract an indicator that is already on the wire: an indicator you have just sent lapses at the peer's own timeout — 6 seconds after your last `active`, or 30 seconds after a `paused` — rather than being taken down with an explicit `done`.

### `[web]`

Embedded web frontend. When `enabled = true` and `WEB_PASSWORD` is set in `.env`, the app starts an HTTPS server alongside the terminal interface. Both share the same state — read a message on web, it's marked read on terminal, and vice versa.

Set `bind_address = "0.0.0.0"` to allow LAN access. TLS is always on — if no custom cert/key is provided, a self-signed certificate is auto-generated in `~/.repartee/certs/`.

The `theme` setting controls the web UI appearance. Available: `nightfall` (default dark), `catppuccin-mocha`, `tokyo-storm`, `gruvbox-light`, `catppuccin-latte`.

### `[[ignores]]`

Ignore patterns for filtering unwanted messages. Uses wildcard matching (`*!*@host`). Levels: `MSGS`, `PUBLIC`, `NOTICES`, `ACTIONS`, `JOINS`, `PARTS`, `QUITS`, `NICKS`, `KICKS`, `CTCPS`, `ALL`.

## Credentials

Passwords should **not** go in `config.toml` — store them in
`~/.repartee/.env` instead. SASL usernames may be set in `config.toml`; the
environment form remains supported for existing configurations.

```bash
# ~/.repartee/.env
LIBERA_SASL_USER=mynick
LIBERA_SASL_PASS=hunter2
LIBERA_PASSWORD=serverpassword
WEB_PASSWORD=mysecretpassword
SHRINK_API_KEY=your-shr-al-key
OPENROUTER_API=your-openrouter-key
OLLAMA_API=your-ollama-key
GEMINI_API=your-gemini-key
GROQ_API_KEY=your-groq-key
```

Server credentials use the server identifier uppercased. `WEB_PASSWORD` is
required for the web frontend. Translation needs only one usable model key;
models whose keys are absent are skipped.

## Runtime changes

- **`/set section.field value`** — change a config value at runtime. Changes are saved immediately.
- **`/reload`** — reload config, `.env` credentials, and the current theme from disk.

## Inline terminal images

Set `/set image_preview.inline true` to show a thumbnail below messages containing
a direct-image URL. The option is off by default. The first direct-image link in
each message receives a fixed 48-column by 8-row slot (narrow terminals use the
available width). Images load only when their slot is visible; scrolling keeps
message positions stable while a download finishes. Partial thumbnails are clipped
to the chat viewport. Clicking a link still opens the larger popup.

Automatic downloads accept public HTTP/HTTPS image URLs, with the same DNS and
redirect address checks used by web previews. Private-network URLs and generic
web pages are not automatically fetched. Up to four downloads run concurrently,
and the in-memory thumbnail cache holds at most 32 message previews. Existing
file-size and timeout settings apply; inline downloads do not write to the disk cache. Decoding is additionally limited
to 8192 pixels per dimension and a 64 MiB allocation budget. Animated files show
a static thumbnail.

The renderer uses the detected terminal graphics protocol, with Unicode half-block
images as the fallback. Inline graphics are preserved across unchanged frames, including clock ticks,
single-line typing and emote animation. Changes to chat content or layout clear
and rebuild the graphics to avoid stale pixels after scrolling or buffer changes.


## Explicit bouncer network binding

For a known Lurker or Soju network ID, set `bouncer_network_id` in that server's
configuration. It is the positive numeric ID advertised by `BOUNCER LISTNETWORKS`,
not the network's display name. Configure SASL authentication to the bouncer:

```toml
[servers.bouncer_libera]
label = "Bouncer Libera"
address = "bouncer.example.org"
port = 6697
tls = true
channels = []
sasl_user = "your-bouncer-account"
sasl_mechanism = "PLAIN"
bouncer_network_id = "42"
```

Store the password in `.env` as `BOUNCER_LIBERA_SASL_PASS`, as for other server
credentials. Alternatively set the network ID with
`/set servers.bouncer_libera.bouncer_network_id 42` and reconnect, or use
`-bouncer-network=42` with `/server add`. Set the option to an empty string through
`/set` to remove explicit binding.

The client authenticates first, sends `BOUNCER BIND` before `CAP END`, and verifies
that the server confirms the requested ID. Rejected or unconfirmed binding is a
connection error. Configured autojoin channels are suppressed for explicit
binding: the bouncer restores its joined channels. PASS-only logins can continue
using the bouncer's username/network selector without this option; explicit ID
binding currently requires SASL.

## Bouncer control connection

Set `bouncer_control = true` on a dedicated server entry to discover its networks.
Authenticate using the account login without a network selector. Do not set
`bouncer_network_id` on that entry. The connection negotiates the network-list
capabilities, suppresses configured autojoin and receives network changes when
notifications are supported. Use `/bouncer list` to see IDs, names, states and
errors, or `/bouncer refresh` to request a fresh list.

`/server add -bouncer-control` and `/set servers.<id>.bouncer_control true`
configure this mode. Reconnect after changing the setting. Both terminal and web
command input support `/bouncer`.

Control connections automatically open their discovered networks using separate,
SASL-authenticated bound connections. Names and removals follow bouncer updates;
manual child disconnects remain off until `/bouncer connect ID`. Disconnecting the
control connection suspends all children until a new valid network list arrives.
Explicit binding and control mode use server-owned history for channels and
private IRC conversations. Their messages and mentions remain in memory; they
are not written to the local chat database or text logs. Peer-to-peer DCC chats
keep local history because the bouncer cannot retrieve them.
Terminal and web scrollback request older pages from the bouncer. A bouncer that
does not advertise CHATHISTORY provides only live traffic and its automatic
replay; the connection displays that limitation. Existing local logs are not
deleted and remain available in the explicit log browser.

## Bouncer network identity

For explicit network binding and control connections, local data uses a stable
scope separate from the display label. It combines the configured server ID,
endpoint, login selectors and network ID. Two configured accounts using the same
network name and numeric ID therefore remain isolated. Changing a display label
or rotating a password preserves the scope; changing the account entry ID,
endpoint or login selector creates a different scope. Keep the account entry ID
stable when renaming its display label. The username selector includes the global
`general.username` fallback when the server has no explicit `username`. Invalid
network IDs are rejected while loading the configuration. Automatic reconnects
retain the username captured for the existing connection; a fresh connection
uses the updated configuration. The SASL mechanism selector also participates
in the scope. If multiple authentication methods are configured for a bouncer,
set an explicit `sasl_mechanism` to avoid switching accounts through automatic
mechanism negotiation.

History lookup, E2E contexts and peer-handle caches use this scope in both terminal
and web flows. Existing rows stored under old display labels remain untouched;
these ambiguous rows are not automatically adopted into a bouncer scope. If E2E was enabled under the old label-based scope, sending is refused until
you verify this network and explicitly use `/e2e on` or `/e2e off` in its new
scope. The refusal also prevents translation and URL-shortening from seeing
the content. For DMs whose peer has not spoken since reconnecting, the explicit
decision may use a unique previous cached handle; ambiguous handles require a
fresh message from the peer. No previous encryption keys are copied. Direct IRC connections retain
their existing label-based storage and migration behavior.

History pages use the stable network scope for request isolation. Reconnect
anchors come from messages still in memory, never from old local chat rows.
Read markers, TARGETS discovery and server-side search are separate stages;
these features are not implied by basic history pagination support.

# Scripting — API Reference

Complete reference for the `api` object passed to every script's `setup` function.

```lua
function setup(api)
    -- api.on, api.irc, api.log, etc.
end
```

---

## Events

### `api.on(event, handler, priority?)`

Register an event handler. Returns a handler ID for removal.

```lua
local id = api.on("irc.privmsg", function(event)
    -- handle message
end)
```

**Parameters:**

| Param | Type | Description |
|---|---|---|
| `event` | string | Event name (see event list below) |
| `handler` | function | Handler function receiving event table |
| `priority` | number | Optional. Default: 50 (normal) |

Handlers run in descending priority order. Return `true` from a handler to suppress the event (prevent lower-priority handlers and built-in handling from running).

**Priority constants:**

| Constant | Value | Description |
|---|---|---|
| `api.PRIORITY_HIGHEST` | 100 | Run before all other handlers |
| `api.PRIORITY_HIGH` | 75 | Run early |
| `api.PRIORITY_NORMAL` | 50 | Default priority |
| `api.PRIORITY_LOW` | 25 | Run late |
| `api.PRIORITY_LOWEST` | 0 | Run after all other handlers |

```lua
api.on("irc.privmsg", function(event)
    -- this handler runs before normal-priority handlers
end, api.PRIORITY_HIGH)
```

### `api.once(event, handler, priority?)`

Same as `api.on()` but the handler fires only once, then removes itself.

### `api.off(id)`

Remove a previously registered handler by its ID.

```lua
local id = api.on("irc.privmsg", handler)
api.off(id)  -- remove the handler
```

---

## IRC Events

These events fire when the IRC server sends data.

### `irc.privmsg`

A channel or private message.

```lua
api.on("irc.privmsg", function(event)
    -- event.connection_id  (string)
    -- event.nick           (string)
    -- event.ident          (string)
    -- event.hostname       (string)
    -- event.target         (string) channel or your nick
    -- event.channel        (string) same as target
    -- event.message        (string)
    -- event.is_channel     (string) "true" or "false"
end)
```

### `irc.action`

A CTCP ACTION (`/me` message). Same fields as `irc.privmsg`.

### `irc.notice`

| Field | Type | Description |
|---|---|---|
| `connection_id` | string | |
| `nick` | string | Sender (nil for server notices) |
| `target` | string | |
| `message` | string | |
| `from_server` | boolean | |

### `irc.join`

| Field | Type | Description |
|---|---|---|
| `connection_id` | string | |
| `nick` | string | |
| `ident` | string | |
| `hostname` | string | |
| `channel` | string | |

### `irc.part`

Same as `irc.join` plus `message` (part reason).

### `irc.quit`

| Field | Type | Description |
|---|---|---|
| `connection_id` | string | |
| `nick` | string | |
| `ident` | string | |
| `hostname` | string | |
| `message` | string | Quit reason |

### `irc.kick`

| Field | Type | Description |
|---|---|---|
| `connection_id` | string | |
| `nick` | string | Who kicked |
| `ident` | string | |
| `hostname` | string | |
| `channel` | string | |
| `kicked` | string | Who was kicked |
| `message` | string | Kick reason |

### `irc.nick`

| Field | Type | Description |
|---|---|---|
| `connection_id` | string | |
| `nick` | string | Old nick |
| `new_nick` | string | New nick |
| `ident` | string | |
| `hostname` | string | |

### `irc.topic`

| Field | Type | Description |
|---|---|---|
| `connection_id` | string | |
| `nick` | string | Who changed it |
| `channel` | string | |
| `topic` | string | |

### `irc.mode`

| Field | Type | Description |
|---|---|---|
| `connection_id` | string | |
| `nick` | string | Who set the mode |
| `target` | string | Channel or nick |
| `modes` | string | Mode string (e.g. "+o nick") |

### `irc.invite`

| Field | Type | Description |
|---|---|---|
| `connection_id` | string | |
| `nick` | string | Who invited |
| `channel` | string | |
| `invited` | string | Who was invited |

### `irc.ctcp_request` / `irc.ctcp_response`

| Field | Type | Description |
|---|---|---|
| `connection_id` | string | |
| `nick` | string | |
| `ctcp_type` | string | e.g. "PING", "TIME" |
| `message` | string | |

### `irc.wallops`

| Field | Type | Description |
|---|---|---|
| `connection_id` | string | |
| `nick` | string | |
| `message` | string | |
| `from_server` | boolean | |

---

## DCC Events

These events fire for DCC CHAT activity. DCC connections are direct peer-to-peer TCP connections that bypass the IRC server.

| Event | Params | Suppressible | Description |
|---|---|---|---|
| `dcc.chat.request` | connection_id, nick, ip, port | Yes | Incoming DCC CHAT offer |
| `dcc.chat.connected` | connection_id, nick | No | DCC CHAT TCP connection established |
| `dcc.chat.message` | connection_id, nick, text | Yes | Message received over DCC CHAT |
| `dcc.chat.closed` | connection_id, nick, reason | No | DCC CHAT connection closed |

### `dcc.chat.request`

Fires when a remote user sends a DCC CHAT offer. Return `true` to suppress (auto-reject).

```lua
api.on("dcc.chat.request", function(event)
    -- event.connection_id  (string) IRC connection the offer arrived on
    -- event.nick           (string) offering nick
    -- event.ip             (string) IP address to connect to
    -- event.port           (number) TCP port
    api.ui.print("DCC CHAT offer from " .. event.nick .. " (" .. event.ip .. ":" .. event.port .. ")")
end)
```

### `dcc.chat.connected`

Fires when a DCC CHAT TCP connection is fully established (either direction).

```lua
api.on("dcc.chat.connected", function(event)
    -- event.connection_id  (string)
    -- event.nick           (string)
end)
```

### `dcc.chat.message`

Fires when a message arrives over an established DCC CHAT connection. Return `true` to suppress display.

```lua
api.on("dcc.chat.message", function(event)
    -- event.connection_id  (string)
    -- event.nick           (string)
    -- event.text           (string) raw message text
end)
```

### `dcc.chat.closed`

Fires when a DCC CHAT connection closes (either side).

```lua
api.on("dcc.chat.closed", function(event)
    -- event.connection_id  (string)
    -- event.nick           (string)
    -- event.reason         (string) e.g. "remote closed", "timeout", "error"
end)
```

---

## Lifecycle Events

### `connected`

| Field | Type |
|---|---|
| `connection_id` | string |
| `nick` | string |

### `disconnected`

| Field | Type |
|---|---|
| `connection_id` | string |

### `command_input`

Fired before a command executes. Return `true` to suppress.

| Field | Type |
|---|---|
| `command` | string |
| `args` | table |
| `connection_id` | string |

---

## Commands

### `api.command(name, def)`

Register a custom slash command.

```lua
api.command("greet", {
    handler = function(args, connection_id)
        local target = args[1] or "world"
        api.irc.say(target, "Hello, " .. target .. "!")
    end,
    description = "Send a greeting",
    usage = "/greet <nick>",
})
```

---

## IRC Methods

All IRC methods take an optional `connection_id` as the last parameter. If omitted, the active buffer's connection is used.

### `api.irc.say(target, message, connection_id?)`

Send a PRIVMSG to a channel or nick.

### `api.irc.action(target, message, connection_id?)`

Send a CTCP ACTION (`/me`).

### `api.irc.notice(target, message, connection_id?)`

Send a NOTICE.

### `api.irc.join(channel, key?, connection_id?)`

Join a channel, optionally with a key.

### `api.irc.part(channel, message?, connection_id?)`

Leave a channel with an optional part message.

### `api.irc.raw(line, connection_id?)`

Send a raw IRC protocol line.

### `api.irc.nick(new_nick, connection_id?)`

Change your nickname.

### `api.irc.whois(nick, connection_id?)`

Send a WHOIS query.

### `api.irc.mode(target, mode_string, connection_id?)`

Set a channel or user mode.

### `api.irc.kick(channel, nick, reason?, connection_id?)`

Kick a user from a channel.

### `api.irc.ctcp(target, type, message?, connection_id?)`

Send a CTCP request.

---

## UI Methods

### `api.ui.print(text)`

Display a local event message in the active buffer.

### `api.ui.print_to(buffer_id, text)`

Display a local event message in a specific buffer.

### `api.ui.switch_buffer(buffer_id)`

Switch to a buffer.

### `api.ui.execute(command_line)`

Execute a client command (e.g. `api.ui.execute("/set theme default")`).

---

## State Access

### `api.store.active_buffer()`

Returns the active buffer ID, or nil.

### `api.store.our_nick(connection_id?)`

Returns your current nick, or nil if not connected. If no `connection_id` is given, uses the active buffer's connection.

### `api.store.connections()`

Returns a table of all connections. Each entry has: `id`, `label`, `nick`, `connected`, `user_modes`.

### `api.store.connection_info(connection_id)`

Returns info for a specific connection, or nil.

### `api.store.buffers()`

Returns a table of all buffers. Each entry has: `id`, `connection_id`, `name`, `buffer_type`, `topic`, `unread_count`.

### `api.store.buffer_info(buffer_id)`

Returns info for a specific buffer, or nil.

### `api.store.nicks(buffer_id)`

Returns a table of nicks in a buffer. Each entry has: `nick`, `prefix`, `modes`, `away`.

---

## Config

### `api.config.get(key)`

Get a per-script config value.

### `api.config.set(key, value)`

Set a per-script config value at runtime.

### `api.config.app_get(key_path)`

Read an app-level config value using dot-separated path. Returns the value as a string, or nil.

```lua
local theme = api.config.app_get("general.theme")
local nick_width = api.config.app_get("display.nick_width")
```

---

## Timers

### `api.timer(ms, handler)`

Start a repeating timer. Returns a timer ID.

### `api.timeout(ms, handler)`

Start a one-shot timeout. Returns a timer ID.

### `api.cancel_timer(id)`

Cancel a timer.

---

## Logging

### `api.log(message)`

Log a debug message. Only outputs when `scripts.debug = true` in config.

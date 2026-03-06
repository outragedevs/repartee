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
    -- event.message        (string)
    -- event.is_channel     (boolean)
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
| `channel` | string | |
| `kicked` | string | Who was kicked |
| `message` | string | Kick reason |

### `irc.nick`

| Field | Type | Description |
|---|---|---|
| `connection_id` | string | |
| `nick` | string | Old nick |
| `new_nick` | string | New nick |

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

## App Events

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

### `buffer_switch`

| Field | Type |
|---|---|
| `from_buffer_id` | string |
| `to_buffer_id` | string |

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

### `api.remove_command(name)`

Remove a command registered by this script.

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

### `api.print(text)`

Display a local event message in the active buffer.

### `api.print_to(buffer_id, text)`

Display a local event message in a specific buffer.

### `api.switch_buffer(buffer_id)`

Switch to a buffer.

### `api.execute(command_line)`

Execute a client command (e.g. `api.execute("/set theme default")`).

---

## State Access

### `api.active_buffer()`

Returns the active buffer ID, or nil.

### `api.our_nick(connection_id?)`

Returns your current nick, or nil if not connected.

### `api.connections()`

Returns a table of all connections.

### `api.buffers()`

Returns a table of all buffers.

### `api.nicks(buffer_id)`

Returns a table of nicks in a buffer.

---

## Config

### `api.config.get(key)`

Get a per-script config value.

### `api.config.set(key, value)`

Set a per-script config value at runtime.

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

---
category: Scripts
description: Manage user scripts
---

# /script

## Syntax

    /script [list|load|unload|reload|autoload|template] [name]

## Description

Manage user scripts. Scripts are Lua 5.4 files in `~/.repartee/scripts/`
that extend Repartee with custom commands, event hooks, filters, and automation.

Scripts run in a sandboxed Lua environment — `os`, `io`, `loadfile`, `dofile`,
and `package` are removed. Each script gets its own isolated environment.

## Subcommands

### list

Show currently loaded scripts with version and description.

    /script list

This is the default when no subcommand is given.

### load

Load a script by name.

    /script load <name>

Looks for `~/.repartee/scripts/<name>.lua`.

### unload

Unload a script. All event handlers, commands, and timers registered by
the script are automatically cleaned up. If the script returned a cleanup
function from `setup()`, it is called.

    /script unload <name>

### reload

Unload and reload a script.

    /script reload <name>

### autoload

Show or manage scripts that load automatically on startup.

    /script autoload

### template

Create a starter script file with the standard boilerplate.

    /script template

## Autoloading

All `.lua` files in `~/.repartee/scripts/` are loaded automatically on startup.

You can also explicitly list scripts in `config.toml`:

```toml
[scripts]
autoload = ["auto-away", "spam-filter"]
debug = false
```

## Writing Scripts

Scripts are `.lua` files with a `meta` table and a `setup` function:

```lua
meta = {
    name = "my-script",
    version = "1.0",
    description = "What it does",
}

function setup(api)
    -- Register event handlers
    api.on("irc.privmsg", function(ev)
        -- handle message
    end)

    -- Register custom commands
    api.command("mycommand", {
        handler = function(args, conn_id) --[[ ... ]] end,
        description = "Does something",
        usage = "/mycommand <arg>",
    })

    -- Optional: return cleanup function
    return function()
        api.log("my-script unloaded")
    end
end
```

### Available Events

**IRC events:** `irc.privmsg`, `irc.action`, `irc.notice`, `irc.join`, `irc.part`,
`irc.quit`, `irc.kick`, `irc.nick`, `irc.topic`, `irc.mode`, `irc.invite`,
`irc.ctcp_request`, `irc.ctcp_response`, `irc.wallops`

**Lifecycle events:** `command_input`, `connected`, `disconnected`

### Event Priority

Handlers run in descending priority order. Use priority constants:

- `api.PRIORITY_HIGHEST` (100)
- `api.PRIORITY_HIGH` (75)
- `api.PRIORITY_NORMAL` (50) — default
- `api.PRIORITY_LOW` (25)
- `api.PRIORITY_LOWEST` (0)

Return `true` from a handler to suppress the event.

### Per-Script Config

Access per-script config values at runtime:

```lua
local val = api.config.get("timeout", 300)
api.config.set("timeout", 600)
```

Read app-level config with dot-path notation:

```lua
local theme = api.config.app_get("general.theme")
```

## Examples

    /script list
    /script load auto-away
    /script reload auto-away
    /script unload auto-away
    /script template

## See Also

/set

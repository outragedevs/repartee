# Scripting — Getting Started

rustirc supports Lua 5.4 scripting with a rich API modeled after WeeChat, irssi, and kokoirc.

## Script location

Scripts live in `~/.rustirc/scripts/` as `.lua` files.

## Script format

Every script has two parts: a `meta` table and a `setup` function:

```lua
meta = {
    name = "hello",
    version = "1.0",
    description = "A simple greeting script"
}

function setup(api)
    api.on("irc.join", function(event)
        if event.nick ~= api.our_nick() then
            api.irc.say(event.channel, "Welcome, " .. event.nick .. "!")
        end
    end)

    api.log("Hello script loaded!")

    -- Return a cleanup function (optional)
    return function()
        api.log("Hello script unloaded")
    end
end
```

### `meta` table

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | Yes | Script name (used for loading/unloading) |
| `version` | string | No | Version string |
| `description` | string | No | Short description |

### `setup(api)` function

Called when the script is loaded. Receives the `api` object with all scripting capabilities. Can optionally return a cleanup function that runs on unload.

## Loading scripts

```
/script load hello
/script unload hello
/script reload hello
/script list
```

## Autoloading

Add script names to your config to load them automatically:

```toml
[scripts]
autoload = ["hello", "urllogger"]
```

## Sandboxing

Scripts run in a sandboxed Lua environment. The following globals are **removed** for security:

- `os` — no filesystem/process access
- `io` — no file I/O
- `loadfile`, `dofile` — no arbitrary file execution
- `package` — no module loading

Scripts are isolated from each other — each gets its own Lua environment.

## Next steps

- [API Reference](scripting-api.html) — full API documentation
- [Examples](scripting-examples.html) — practical script examples

---
category: Scripts
description: Manage user scripts
---

# /script

## Syntax

    /script [list|available|load|unload|reload] [name]

## Description

Manage user scripts. Scripts are TypeScript files in `~/.kokoirc/scripts/`
that extend kokoIRC with custom commands, event hooks, filters, and automation.

Scripts have full access to the IRC client, store, and UI — same trust model
as irssi Perl scripts.

## Subcommands

### list

Show currently loaded scripts with version and description.

    /script list

This is the default when no subcommand is given.

### available

Show all script files found in `~/.kokoirc/scripts/`.

    /script available

Loaded scripts are marked with a filled circle (●).

### load

Load a script by name or absolute path.

    /script load <name>
    /script load /path/to/script.ts

When given a name without a path, looks for `~/.kokoirc/scripts/<name>.ts`.

### unload

Unload a script. All event handlers, commands, and timers registered by
the script are automatically cleaned up.

    /script unload <name>

### reload

Unload and reload a script (cache-busted import).

    /script reload <name>

## Autoloading

Add script names to `config.toml` to load them on startup:

```toml
[scripts]
autoload = ["auto-away", "spam-filter"]
debug = false
```

## Writing Scripts

Scripts are `.ts` files that export a default init function:

```ts
import type { KokoAPI, IrcMessageEvent } from "@/core/scripts/types"

export const meta = { name: "my-script", version: "1.0.0", description: "..." }
export const config = { timeout: 300 }  // defaults for [scripts.my-script]

export default function init(api: KokoAPI) {
  // Use api.EventPriority for priority constants (HIGHEST, HIGH, NORMAL, LOW, LOWEST)
  api.on("irc.privmsg", (event: IrcMessageEvent, ctx) => {
    // ctx.stop() prevents lower-priority handlers + built-in store update
  }, api.EventPriority.LOW)

  api.command("mycommand", { handler(args, connId) { /* ... */ }, description: "..." })

  return () => { /* cleanup on unload */ }
}
```

**Import rules:** Scripts live outside the project, so `@/` path aliases only work
with `import type` (stripped at runtime). For values like `EventPriority`, use
`api.EventPriority` instead of importing.

### Available Events

**IRC events:** `irc.privmsg`, `irc.action`, `irc.notice`, `irc.join`, `irc.part`,
`irc.quit`, `irc.kick`, `irc.nick`, `irc.topic`, `irc.mode`, `irc.invite`,
`irc.ctcp_request`, `irc.ctcp_response`, `irc.wallops`

**App events:** `command_input`, `connected`, `disconnected`

### Per-Script Config

Declare defaults with `export const config = {...}`. Users override in TOML:

```toml
[scripts.auto-away]
timeout = 300
message = "AFK"
```

Access via `api.config.get("timeout", 300)`.

## Examples

    /script available
    /script load auto-away
    /script list
    /script reload auto-away
    /script unload auto-away

## See Also

/set, /alias

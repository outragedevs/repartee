---
category: Configuration
description: Define, list, or remove user aliases
---

# /alias

## Syntax

    /alias                    List all aliases
    /alias <name>             Show a specific alias
    /alias <name> <body>      Define or replace an alias
    /alias -<name>            Remove an alias

## Description

Define custom command aliases. Aliases expand before execution and support
template variables, context variables, and command chaining with semicolons.

### Template Variables

- `$0`-`$9` — positional arguments
- `$0-` through `$9-` — all arguments from position N onward
- `$*` — all arguments joined by space

If the alias body contains no `$` references, `$*` is appended automatically.
This means `/alias ns /msg NickServ` works the same as `/alias ns /msg NickServ $*`.

### Context Variables

- `$C` or `${C}` — current channel/buffer name
- `$N` or `${N}` — current nick
- `$S` or `${S}` — current server label
- `$T` or `${T}` — current buffer name (same as `$C`)

### Command Chaining

Use `;` to chain multiple commands in one alias:

    /alias j /join $0; /msg $0 hello everyone

### Recursion Guard

Aliases can reference other aliases. Recursion is capped at 10 levels to
prevent infinite loops.

Cannot override built-in commands.

## Examples

    /alias
    /alias ns
    /alias ns /msg NickServ $*
    /alias cs /msg ChanServ $*
    /alias j /join $0; /msg $0 hello everyone
    /alias w /who $C
    /alias -ns

## Config

Aliases are stored in the `[aliases]` section of `config.toml`:

```toml
[aliases]
ns = "/msg NickServ $*"
cs = "/msg ChanServ $*"
wc = "/close"
j = "/join $0; /msg $0 hello everyone"
```

## See Also

/unalias

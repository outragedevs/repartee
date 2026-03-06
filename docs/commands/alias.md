---
category: Configuration
description: Define, list, or remove user aliases
---

# /alias

## Syntax

    /alias [[-]name] [body]

## Description

Define custom command aliases. Aliases expand before execution and support
template variables and command chaining with semicolons.

With no arguments, lists all defined aliases.
With just a name, shows that alias's body.
With `-name`, removes the alias.
With name and body, defines or replaces the alias.

Template variables:
- `$0`-`$9` — positional arguments
- `$*` — all arguments
- `$C` — current channel name
- `$N` — current nick
- `$S` — current server label
- `$T` — current buffer name

Cannot override built-in commands.

## Examples

    /alias
    /alias ns /msg NickServ $*
    /alias cs /msg ChanServ $*
    /alias j /join $0; /msg $0 hello everyone
    /alias -ns

## See Also

/unalias

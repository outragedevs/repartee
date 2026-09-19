---
category: Configuration
description: View or change configuration
---

# /set

## Syntax

    /set [section.field] [value]

## Description

View or change runtime configuration. Settings use dot-notation paths
like `general.nick` or `servers.libera.port`. Changes are saved to
`~/.repartee/config.toml` immediately. Server and SASL passwords are stored in
`.env` instead; SASL usernames remain ordinary TOML configuration.

With no arguments, lists all settings grouped by section.
With just a path, shows the current value.
With a path and value, sets the value and saves.

Wrap a value in matching single or double quotes to preserve leading or trailing
spaces. The outer quotes are removed; `""` or `''` sets an empty string.
Inside a quoted value, escape the matching quote or a backslash with a backslash.
Other backslashes remain literal. Unclosed quotes or text after a closing quote
are rejected without changing the setting. Values must be a single line without
NUL characters. Unquoted values keep their existing
behavior, including trimming trailing whitespace.

Boolean values accept `true` or `false`.
Array values use comma-separated format: `#chan1,#chan2`. An empty quoted value
clears a list or an optional server override (such as the per-server nick or SASL mechanism).
Required values such as the global nick, server address, port or TLS toggle
reject empty input.

## Examples

    /set
    /set general.nick
    /set general.nick newnick
    /set statusbar.prompt "❯ "
    /set statusbar.prompt ""
    /set general.theme tokyo-night
    /set servers.libera.tls true
    /set servers.libera.channels #linux,#irc
    /set display.nick_colors true
    /set display.nick_colors_in_nicklist false
    /set display.nick_color_lightness 0.40

## See Also

/reload, /server, /items

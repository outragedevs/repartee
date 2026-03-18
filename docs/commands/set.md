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
`config/config.toml` immediately. Credentials (passwords, SASL) are
stored in `.env` instead.

With no arguments, lists all settings grouped by section.
With just a path, shows the current value.
With a path and value, sets the value and saves.

Boolean values accept: `true`/`false`, `on`/`off`, `yes`/`no`.
Array values use comma-separated format: `#chan1,#chan2`.

## Examples

    /set
    /set general.nick
    /set general.nick newnick
    /set general.theme tokyo-night
    /set servers.libera.tls true
    /set servers.libera.channels #linux,#irc
    /set display.nick_colors true
    /set display.nick_colors_in_nicklist false
    /set display.nick_color_lightness 0.40

## See Also

/reload, /server, /items

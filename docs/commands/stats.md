---
category: Info
description: Request server statistics
---

# /stats

## Syntax

    /stats [type] [server]

## Description

Request statistics from the IRC server. Common stat types include:

- `u` — server uptime
- `m` — command usage counts
- `o` — configured operators
- `l` — connection information

If no type is given, the server may return a summary or help text.

## Examples

    /stats
    /stats u
    /stats o irc.libera.chat

## See Also

/quote

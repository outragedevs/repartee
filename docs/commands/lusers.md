---
category: Info
description: Request server user statistics
---

# /lusers

## Syntax

    /lusers [mask] [server]

## Description

Request user statistics from the IRC server, including total users, invisible users, servers, operators, and channels. Optionally provide a mask to filter results, and a server name to query a specific server.

## Examples

    /lusers
    /lusers *.fi
    /lusers * irc.libera.chat

## See Also

/stats, /info, /who

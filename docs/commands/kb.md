---
category: Moderation
description: Kickban a user (kick then ban *!*ident@host)
---

# /kb

## Syntax

    /kb <nick> [reason]

## Description

Kick and ban a user from the current channel. Looks up the user's ident
and host from cached WHOX data to create a proper `*!*ident@host` ban
mask, then kicks with the given reason.

Falls back to `nick!*@*` if the user's ident and host are not available
(e.g. the server does not support WHOX or userhost-in-names).

The reason defaults to the nick if not provided.

## Examples

    /kb troll
    /kb spammer Enough is enough

## See Also

/kick, /ban, /unban, /mode

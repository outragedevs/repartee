---
category: Moderation
description: Kickban a user (kick then ban *!*ident@host)
---

# /kb

## Syntax

    /kb <nick> [reason]

## Description

Kick and ban a user from the current channel. Looks up the user's
ident and host via USERHOST to create a proper `*!*ident@host` ban mask,
then kicks with the given reason. Falls back to `nick!*@*` if the
lookup times out after 5 seconds.

## Examples

    /kb troll
    /kb spammer Enough is enough

## See Also

/kick, /ban, /unban, /mode

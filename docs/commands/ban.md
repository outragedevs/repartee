---
category: Moderation
description: Ban a user or hostmask
---

# /ban

## Syntax

    /ban <nick|mask>

## Description

Ban a user or hostmask from the current channel. If a plain nick is
given, it's converted to `nick!*@*`. If the argument contains `!` or `@`,
it's used as a literal hostmask.

## Examples

    /ban troll
    /ban *!*@bad.host.com
    /ban *!*ident@*.isp.net

## See Also

/unban, /kick, /kb, /mode

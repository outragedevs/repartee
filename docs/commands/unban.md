---
category: Moderation
description: Remove a ban
---

# /unban

## Syntax

    /unban <nick|mask>

## Description

Remove a ban from the current channel. If a plain nick is given, it's
converted to `nick!*@*`. If the argument contains `!` or `@`, it's used
as a literal hostmask.

## Examples

    /unban friend
    /unban *!*@good.host.com

## See Also

/ban, /kick, /kb, /mode

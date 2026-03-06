---
category: Moderation
description: Ban a user or hostmask
---

# /ban

## Syntax

    /ban [mask]
    /ban -a <account>

## Description

Ban a user or hostmask from the current channel.

With no arguments, requests the ban list from the server and displays it
as a numbered list. These numbers can be used with `/unban` to remove
entries by index.

With a mask argument, sets mode `+b` on the channel. If the argument
contains `!` or `@`, it is used as a literal hostmask. Otherwise it is
sent as-is (the server typically treats a plain nick as `nick!*@*`).

The `-a` flag creates an account extban (`$a:account`) if the server
supports EXTBAN with the account type.

## Examples

    /ban                        Show numbered ban list
    /ban *!*@bad.host.com       Ban a hostmask
    /ban *!*ident@*.isp.net     Ban by ident and host pattern
    /ban -a troll               Account extban ($a:troll)

## See Also

/unban, /kick, /kb, /mode

---
category: Moderation
description: Manage flood protection
---

# /flood

## Syntax

    /flood
    /flood on
    /flood off
    /flood add <nick|mask>
    /flood remove <number|mask>

## Description

Manage local flood protection and `PRIVMSG` exemptions. Without
arguments, lists the current status and exemption rules.

Exemptions use the same wildcard matching as `/ignore`. A bare nick
pattern matches the nick only. A pattern containing `!` matches the
full `nick!user@host` mask.

Exemptions only bypass local incoming `PRIVMSG` flood checks. They do
not affect `/ignore`, nick-change flood suppression, or the IRC
crate's outgoing send throttle.

## Examples

    /flood
    /flood add trustednick
    /flood add *!*@trusted.host
    /flood remove 1
    /flood off

## See Also

/ignore, /set

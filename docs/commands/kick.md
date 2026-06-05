---
category: Moderation
description: Kick a user from the channel
---

# /kick

## Syntax

    /kick [#channel] <nick>[,nick2,...,nick6] [reason]

## Aliases

    /k

## Description

Kick one or more users from the current channel (or `#channel` if
given). Separate multiple nicks with commas — up to six per
invocation. Everything after the nick list is the reason; no colon
is needed. If no reason is given, the user's nick is used as the
reason. You must be a channel operator to use this command.

## Examples

    /kick troll
    /kick spammer Stop spamming
    /kick alice,bob,carol go away
    /kick #other troll Get out
    /k troll Get out

## See Also

/ban, /kb, /mode

---
category: Channel
description: Add invite exception or show list
---

# /invex

## Syntax

    /invex [mask]

## Description

With no arguments, requests the invite exception list (+I) from the
server. With a mask, sets +I on the current channel. Invite exceptions
allow users matching the mask to join invite-only channels.

## Examples

    /invex
    /invex *!*@friends.com

## See Also

/uninvex, /invite

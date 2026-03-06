---
category: Channel
description: Add exception or show exception list
---

# /except

## Syntax

    /except [mask]

## Description

With no arguments, requests the exception list (+e) from the server.
With a mask, sets +e on the current channel. Ban exceptions allow
users matching the mask to join even if banned.

## Examples

    /except
    /except *!*@trusted.com

## See Also

/unexcept, /ban, /unban

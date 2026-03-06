---
category: Channel
description: Add reop entry or show list
---

# /reop

## Syntax

    /reop [mask]

## Description

With no arguments, requests the reop list (+R) from the server.
With a mask, sets +R on the current channel. Reop entries
automatically re-op users matching the mask when they rejoin.

## Examples

    /reop
    /reop *!*@ops.net

## See Also

/unreop, /op

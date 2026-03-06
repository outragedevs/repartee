---
category: Info
description: Set or query channel/user modes
---

# /mode

## Syntax

    /mode [target] [+/-modes] [params]

## Description

Set or query channel or user modes. With no arguments, queries your
own user modes. If the first argument starts with `+` or `-`, applies
the mode change to the current channel. Otherwise, the first argument
is treated as the target.

## Examples

    /mode
    /mode +i
    /mode #linux +o friend
    /mode +nt
    /mode #linux +b *!*@bad.host

## See Also

/op, /deop, /voice, /ban

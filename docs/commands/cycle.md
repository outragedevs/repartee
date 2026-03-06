---
category: Channel
description: Part and rejoin a channel
---

# /cycle

## Syntax

    /cycle [channel] [message]

## Description

Leave and immediately rejoin a channel. Useful for refreshing your user list,
re-triggering auto-op or auto-voice, or clearing stale state. If the channel
has a key set, it is preserved for the rejoin.

With no arguments, cycles the current channel. An optional part message can
be provided.

## Examples

    /cycle
    /cycle #linux
    /cycle #linux Refreshing...

## See Also

/part, /join

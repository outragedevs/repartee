---
category: Other
description: Close windows by number, range, or the active one
---

# /close

## Syntax

    /close [reason]
    /close <number> [reason]
    /close <from>-<to> [reason]
    /close -YES ...

## Description

Close the active window, a window by its sidebar number, or a whole range
of windows at once. Channels are parted with an optional reason; queries,
DCC chats and log windows just close.

The numbers are the ones printed next to each name in the buffer list, so
`/wc 22-35` closes exactly what you see as 22 through 35, inclusive. A
range that runs past the last window closes what exists rather than
failing, which makes `/wc 20-999` a convenient "everything from 20 on".

Mentions is a protected window: closing it needs `-YES`. It only goes away
for the current session — `display.mentions_buffer` stays on, so `/mentions`
or the next restart brings it straight back.

Server windows cannot be closed while connected; `/disconnect` first.
Closing a disconnected server window drops the whole network along with all
its windows, so a *range* refuses to touch server windows at all, and `-YES`
does not override that. Close them one at a time instead.

Anything that is not a number or a range is treated as the part reason, so
the old `/wc going to bed` still works. A malformed range such as `22-` or
`35-22` is reported as an error rather than quietly becoming a reason.

## Examples

    /close
    /close going to bed
    /wc 22
    /wc 22 see you tomorrow
    /wc 22-35
    /wc 3-40 spring cleaning
    /wc -YES
    /wc 1 -YES

## See Also

/part, /mentions, /disconnect, /quit

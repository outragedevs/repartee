---
category: Moderation
description: Remove a ban
---

# /unban

## Syntax

    /unban <number|mask> [number2|mask2 ...]

## Description

Remove one or more bans from the current channel. Accepts both numeric
references (from the numbered list shown by `/ban`) and literal masks.

Use `/ban` with no arguments first to display the numbered ban list,
then `/unban 1 3 5` to remove entries by their index.

Multiple arguments can be given to remove several bans at once.

## Examples

    /unban 1                     Remove first entry from ban list
    /unban 2 4 7                 Remove multiple entries by index
    /unban *!*@good.host.com     Remove by literal mask
    /unban 1 *!*@other.net       Mix numeric and literal

## See Also

/ban, /kick, /kb, /mode

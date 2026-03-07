---
category: Moderation
description: Remove a ban
---

# /unban

## Syntax

    /unban <number|mask|wildcard> [...]

## Description

Remove one or more bans from the current channel. Accepts numeric
references (from the numbered list shown by `/ban`), literal masks,
and wildcard patterns that match against stored bans.

Use `/ban` with no arguments first to display the numbered ban list,
then `/unban 1 3 5` to remove entries by their index.

Wildcard patterns (`*` and `?`) are matched against the stored ban list.
Use `/unban *` to remove all bans from the channel.

Multiple arguments can be given to remove several bans at once.

## Examples

    /unban *                     Remove all bans
    /unban 1                     Remove first entry from ban list
    /unban 2 4 7                 Remove multiple entries by index
    /unban *!*@*.spam.host       Remove all bans matching pattern
    /unban *!*@good.host.com     Remove by literal mask
    /unban 1 *!*@other.net       Mix numeric and wildcard

## See Also

/ban, /kick, /kb, /mode

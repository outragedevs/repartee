---
category: Moderation
description: Add an ignore rule
---

# /ignore

## Syntax

    /ignore [mask] [levels...] [-channels #a,#b]

## Description

Add an ignore rule to suppress messages and events from matching
users. Without arguments, lists current ignore rules.

A bare nick pattern (e.g., `troll`) matches the nick only.
A pattern containing `!` (e.g., `*!*@bad.host`) matches the
full `nick!user@host` mask with wildcard support (`*`, `?`).

Use `-channels` to restrict the ignore to specific channels
(comma-separated). Without it, the ignore applies everywhere.

## Levels

    MSGS      Private messages
    PUBLIC    Channel messages
    NOTICES   Notices
    ACTIONS   CTCP ACTIONs (/me)
    JOINS     Join events
    PARTS     Part events
    QUITS     Quit events
    NICKS     Nick change events
    KICKS     Kick events
    CTCPS     CTCP requests and responses
    ALL       All of the above

If no levels are specified, ALL is assumed.

## Examples

    /ignore
    /ignore troll
    /ignore *!*@bad.host.com
    /ignore spammer PUBLIC NOTICES
    /ignore troll ALL -channels #chat,#help

## See Also

/unignore

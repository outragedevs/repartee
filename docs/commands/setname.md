---
category: Connection
description: Change your real name on the current connection
---

# /setname

## Syntax

    /setname <real name>

## Description

Request a new IRC real name on the active connection. Spaces are preserved inside
the name. The connection must have negotiated the `setname` capability; otherwise
the command reports that the server does not support it.

Repartee updates the confirmed real name only after the server sends SETNAME.
Shared channel nick entries and web nick-list tooltips follow incoming changes;
changes and server rejections are shown as events. This does not change your
nickname or rewrite the saved local server configuration.

Soju can store the requested name for an upstream network. If that upstream does
not support live SETNAME, Soju may reconnect it to apply the change. On a Soju
control connection this changes the account default. The pinned Lurker version
does not advertise SETNAME; use its own configuration interface instead.

## Examples

    /setname Example User

## See Also

/nick, /bouncer, /server

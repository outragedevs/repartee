---
category: Connection
description: Watch IRC nick availability and identity updates
---

# /monitor

## Syntax

    /monitor add <nick[,nick...] ...>
    /monitor remove <nick[,nick...] ...>
    /monitor clear
    /monitor list
    /monitor status
    /monitor show

## Description

Watch nicknames on the active IRC network, including users outside shared channels.
`+` and `-` are aliases for add and remove; `C`, `L` and `S` mean clear, list and
status. Commands work from terminal and web input. Use a bound network connection,
not a bouncer control connection.

The server must advertise MONITOR in ISUPPORT. Changes are queued while the server
is unavailable, and a list read after each change verifies accepted subscriptions.
List-full replies remove rejected targets. Targets are scoped to their connection
and bouncer network and retained in memory across transport reconnects, not saved
as message history. Closing the application discards this in-memory list.

`list` requests a fresh server list; `status` requests current online/offline
replies. `show` displays cached information, including account, away, host and real
name when those notifications are available. Unknown information is displayed as
unknown. Both extended-monitor capability spellings are supported; their detailed
notifications also depend on the upstream's account/away/host/name capabilities.

A list timeout retains cached information and stops further changes until the
reply finishes or the connection is re-established, avoiding ambiguous resends.

## Examples

    /monitor add alice bob
    /monitor status
    /monitor show
    /monitor remove alice

## See Also

/whois, /setname, /bouncer

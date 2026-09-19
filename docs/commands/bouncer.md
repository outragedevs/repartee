---
category: Connection
description: List and refresh the networks on a bouncer control connection
---

# /bouncer

## Syntax

    /bouncer [list|refresh]

## Description

Show the networks belonging to the active bouncer account, including their
stable IDs, names, connection states and reported errors. `refresh` requests
a new list. Complete list responses replace the cached list atomically;
incomplete or malformed responses retain the previous list.

Enable `bouncer_control = true` in the server configuration or use
`/server add -bouncer-control`. Use the account login without a network
selector. Control mode and `bouncer_network_id` are mutually exclusive.
The server must support `soju.im/bouncer-networks` and `batch`.

Soju and Lurker notify-capable connections receive subsequent network changes
automatically. Otherwise, use `refresh` to request an updated list.
This command currently lists networks; it does not automatically open their
connections. Use a separate server entry with `bouncer_network_id` to bind one.

## Examples

    /bouncer
    /bouncer refresh

## See Also

/server, /connect, /set

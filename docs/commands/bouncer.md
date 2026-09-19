---
category: Connection
description: Discover and connect to networks belonging to a bouncer account
---

# /bouncer

## Syntax

    /bouncer [list|refresh|connect ID]

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
Each discovered network opens a separate connection automatically, including
networks whose upstream IRC connection is offline. Background discovery preserves
the active conversation and draft. Labels include the account entry and network ID
to distinguish equal names and avoid collisions with channel or query buffers.
Their names and removal follow
bouncer notifications. Disconnecting the control connection suspends its children;
a new valid list after reconnect resumes them. Closing the disconnected control
window removes its generated connections and buffers. Removing a network closes its
buffers without deleting stored logs. A manually disconnected child remains off;
use `/bouncer connect ID` on the control connection or one of its children to
resume it. The account must support SASL authentication for explicit network binding.

## Examples

    /bouncer
    /bouncer refresh
    /bouncer connect 42

## See Also

/server, /connect, /set

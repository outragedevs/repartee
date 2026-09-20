---
category: Connection
description: Discover, connect to and manage bouncer networks
---

# /bouncer

## Syntax

    /bouncer [list|refresh|connect ID]
    /bouncer add host=HOST [ATTRIBUTE=VALUE ...]
    /bouncer change ID ATTRIBUTE=VALUE [...]
    /bouncer delete ID

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
resume it. Generated connections use explicit network-ID binding with SASL, or
provider-compatible USER/network selectors with PASS. Repartee confirms the
returned network ID in both cases; Soju requires SASL for the explicit BIND command.

## Network management

Soju supports adding, changing and deleting networks. Lurker rejects these
operations and manages networks through its own web UI. These commands work in
both terminal and web input, on the control connection or one of its generated
children. They always target that account's control connection.

Attributes are `host`, `port`, `tls` (`1` or `0`), `name`, `nickname`, `username`
and `realname`. Quote values containing spaces; single quotes preserve literal
backslashes. Repartee encodes semicolons, spaces and backslashes for the bouncer.
An empty value clears the attribute. A new network requires `host`.

For the upstream server password, put the value in the credentials file `.env`
and use `pass-env=KEY`. The command history contains the key, not the resolved
password. Use `pass=` to clear an existing password. Literal nonempty `pass=`
values are rejected. These are upstream network credentials, separate from the
login used to authenticate to the bouncer itself. Raw IRC client trace frames are
excluded from diagnostic logs even when `RUST_LOG` enables trace, to keep resolved
credentials out of persistent logs.

`delete` removes the upstream network from the bouncer account, affecting other
clients too. To close only a local child connection, use `/disconnect` instead.

A submitted operation is pending until the bouncer acknowledges or rejects it.
Only one operation may be pending per account. Successful replies trigger a
fresh network list; notifications update the generated connections and buffers.
If a reply times out or the connection breaks, the result is unknown and Repartee
does not retry automatically. Inspect `/bouncer list` or `/bouncer refresh` before
reconnecting and deciding whether to repeat the operation.

## Examples

    /bouncer
    /bouncer refresh
    /bouncer connect 42
    /bouncer add host=irc.example.org name="Work Network" tls=1
    /bouncer change 42 realname="Example User" pass-env=WORK_IRC_PASSWORD
    /bouncer delete 42

## See Also

/server, /connect, /set

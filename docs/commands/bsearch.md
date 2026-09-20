---
category: Connection
description: Search history stored by a bouncer
---

# /bsearch

## Syntax

    /bsearch <target> [-from nick] [-after timestamp] [-before timestamp] [-limit 1..100] -- <text>
    /bsearch context <row>
    /bsearch cancel

## Description

Search the active bouncer network's server-owned history. Soju supports this when
its message store provides search; Lurker does not advertise this IRC extension.
The connection must be bound to one network. The target is a channel or peer nick.

`-after` and `-before` accept RFC3339 timestamps. Boundary handling is defined
by the server store; the pinned Soju SQLite store uses exclusive bounds. `-from`
restricts the sender. The default limit is 100; the maximum is 100. Use `--` before
search text containing spaces or leading hyphens. Omit `-- <text>` to search only
by the other selectors. Text matching is defined by the server's message store. Server-side text search
cannot match encrypted plaintext. Readable encrypted results use the existing
history decryption path; undecryptable ciphertext and own encrypted echoes are
skipped, with a notice.

Results appear in the read-only `*search*` buffer for that network in both the
terminal and web client. They preserve server timestamps, senders and message IDs,
without changing the original conversation's unread count or writing local logs.
Each new request replaces the previous results.
`context <row>` counts result rows from 1 and replaces the view with up to 50
messages around that result, fetched using CHATHISTORY AROUND on the original
network and target. It uses the retained message ID when the server supports it, otherwise the
server timestamp. Context retrieval uses request labels when available. Without labels, an earlier
history timeout requires reconnecting first so delayed replies cannot be confused
with the new context. Results from a different account/network cannot be reused. This context remains separate from the live conversation. Closing the view discards late
results; ordinary messages cannot be sent from this view.

Only one unresolved search is allowed per connection. `cancel` stops displaying
its results; it cannot cancel work already running on the bouncer. A timeout also
discards late results. Wait for the terminal response or reconnect before sending
a replacement request. A failed request is reported separately from zero matches.

## Examples

    /bsearch #repartee -from Alice -limit 25 -- release notes
    /bsearch Alice -after 2026-01-01T00:00:00Z -- meeting
    /bsearch #repartee -before 2026-01-01T00:00:00Z
    /bsearch cancel

## See Also

/bouncer, /log

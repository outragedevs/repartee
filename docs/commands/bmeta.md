---
category: Connection
description: Manage bouncer conversation metadata
---

# /bmeta

## Syntax

    /bmeta <target>
    /bmeta <target> list
    /bmeta <target> pin|mute|block on|off|unset
    /bmeta <target> clear
    /bmeta sync
    /bmeta subscriptions

## Description

Read or change Soju's metadata for a channel or peer on the active bound bouncer
network. This requires acknowledged `draft/metadata-2` and `batch` capabilities.
Lurker does not advertise this IRC extension.

Pin, mute and block are independent server-owned flags. `unset` removes a flag's
value; Soju reports the resulting value as off. `clear` resets all three flags.
The server determines whether the target exists and whether a change succeeds.
Sending a request does not optimistically change the local flags.

With no action, request all three supported values. `list` requests the target's
metadata listing. `subscriptions` lists the keys subscribed on this connection.
`sync` renews the subscription and requests the provider's current state. The
command waits for subscription acknowledgements before sending target operations.
Values and changes are scoped to the bouncer account and network.

Only one operation runs at a time. Completion waits for the bouncer to process
an ordered round-trip check after the operation. A failed mutation triggers a
readback, never an automatic repeat of the change. A timeout leaves the result
unknown: wait for the outstanding response or reconnect before issuing another
operation or `sync`. Readback failure is reported without retrying indefinitely.

## Examples

    /bmeta #repartee pin on
    /bmeta #repartee mute off
    /bmeta Alice block on
    /bmeta Alice clear
    /bmeta #repartee

## See Also

/bouncer, /bsearch

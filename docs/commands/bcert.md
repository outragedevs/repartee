---
category: Connection
description: Manage bouncer TLS client certificates
---

# /bcert

## Syntax

    /bcert [list]
    /bcert create <name>
    /bcert delete [fingerprint]

## Description

Manage TLS client certificates pinned to the active Soju account. Available on
both control and bound network connections over verified TLS after successful SASL authentication,
with `soju.im/client-cert` and `batch` negotiated. Lurker does not advertise this
extension. The command works from native and web command input.

`list` displays certificate SHA-512 fingerprints, names, last-use times and IPs.
An empty list is reported only after a complete server response. `create` pins
the TLS certificate already presented on the current connection, using the given
name (1–64 UTF-8 bytes). It does not generate a certificate. `delete` removes the
specified 128-character hexadecimal fingerprint; without an argument it removes
the certificate presented on the current connection.

To enroll a certificate, configure `client_cert_path` to a PEM file containing
its certificate chain and unencrypted private key, enable TLS with certificate verification and explicitly
select `sasl_mechanism = "PLAIN"` with your existing bouncer credentials. Reconnect,
then use `/bcert create My device`. After confirmation, you can select
`sasl_mechanism = "EXTERNAL"` and reconnect. The command never rewrites these
settings automatically. Another client can revoke the certificate at any time;
a successful enrollment does not guarantee future authentication.

Only one request may be pending per connection. After a timeout, the result is
unknown; wait for the outstanding response or reconnect before retrying. No
mutation is automatically retried. Certificate management applies to the whole
bouncer account, not just the currently selected upstream network.

## Examples

`/bcert list`
`/bcert create Desktop terminal`
`/bcert delete`

## Related

/bouncer, /connect, /disconnect

# Bouncer TLS validation

The actual-provider fixture exercises the normal asynchronous App connection
attempt against the pinned Soju and Lurker TLS listeners. Certificates, private
keys, CA roots, accounts and databases are disposable. TLS verification stays
`true` in every case; there is no insecure retry.

## Reproduce

Use the pinned revisions in [binding acceptance](bouncer-binding.md) and run
`make clippy` first. For each provider:

```sh
python3 scripts/test_bouncer_binding.py soju /path/to/pinned/soju --tls-case valid
python3 scripts/test_bouncer_binding.py soju /path/to/pinned/soju --tls-case untrusted
python3 scripts/test_bouncer_binding.py soju /path/to/pinned/soju --tls-case wrong-host
python3 scripts/test_bouncer_binding.py lurker /path/to/pinned/lurker --tls-case valid
python3 scripts/test_bouncer_binding.py lurker /path/to/pinned/lurker --tls-case untrusted
python3 scripts/test_bouncer_binding.py lurker /path/to/pinned/lurker --tls-case wrong-host
```

The valid case trusts the signing CA and uses a certificate whose IP SAN matches
127.0.0.1. It must complete SASL and confirm the requested network ID, publish a
connected event to web consumers, and provide an authenticated IRC handle.

The untrusted case keeps the matching SAN but supplies an unrelated CA to the
client. It must report `UnknownIssuer`. The wrong-host case trusts the signing CA
but supplies an IP SAN of 192.0.2.1 while still connecting to 127.0.0.1. It must
report `certificate not valid for name`. The latter address is only certificate text: no network
connection to it is made. Checking these causes excludes unrelated connection,
authentication or provider-startup failures from being counted as TLS rejection.

For both negative cases, every processed App event must leave the connection
unconnected and without an IRC handle. No successful web connection event may be
published. The diagnostic must be present in the native server buffer and its
web message event, and must not contain the synthetic password. The existing
native-test-only CA injection is used; system trust stores are not modified.

## Diagnostic fix

The initial untrusted-certificate run correctly rejected TLS but only surfaced
`an io error occurred`. Connection attempts previously discarded the error's
source chain when creating the disconnect event. They now preserve that chain
with the error report's alternate Display, so certificate cause information
reaches connection state and both message consumers.

These are real TLS/provider and App/event tests. They do not claim browser or
terminal pixel rendering, packet inspection, certificate renewal, or the separate
selector/account-lifecycle acceptance gates. Both providers' upstream IRC peers
remain the existing disabled/simulated binding fixtures.

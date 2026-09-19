# Explicit bouncer network binding

The `pinned_bouncer_registration` test uses the actual Repartee connection path
against each pinned bouncer, authenticates, binds the configured network ID,
checks the emitted connection events and ISUPPORT, then reconnects. It is ignored
in the normal suite because it needs disposable external processes.

Prepare the pinned checkouts from `BOUNCER_SUPPORT.md`. In Soju run `make soju`.
In Lurker install its root npm dependencies and build the `better-sqlite3` native
module (`npm ci`). No running service, production account or existing database is
used. The scripts create temporary databases and synthetic credentials, listen
on loopback, and terminate the fixture processes afterward.

Run from the Repartee repository, with the platform SDK environment used for
normal native builds:

```sh
make clippy
make test
python3 scripts/test_bouncer_binding.py soju /path/to/soju
python3 scripts/test_bouncer_binding.py lurker /path/to/lurker
```

The wrapper verifies each upstream commit against the audited revision. Lurker
uses its own bouncer test harness: the listener, authentication, binding and
registration are real; its upstream IRC connection is simulated. Soju uses a
real daemon with a disabled upstream network. These tests prove downstream
binding and reconnect, not complete bouncer compatibility or live upstream IRC.

The normal TCP regression suite additionally exercises CAP rejection, failed
SASL, rejected BIND, wrong or missing BOUNCER_NETID, premature disconnect,
canonical IDs and injection rejection. It verifies that direct IRC still
autojoins and explicit bouncer binding does not autojoin configured channels.
The application-level reconnect test also prevents rejoining stale channel
buffers. Native and web server editors preserve the binding setting.

Network discovery, control-mode lifecycle, PASS-specific explicit binding,
server-owned history and the remaining feature matrix are separate pending work.

# Bouncer binding and network discovery

The `pinned_bouncer_registration` test uses the actual Repartee connection path
against each pinned bouncer, authenticates, binds the configured network ID,
checks the emitted connection events and ISUPPORT, then reconnects. It is ignored
in the normal suite because it needs disposable external processes.
The `pinned_bouncer_discovery` test opens a control connection to the same real
bouncer and verifies that its initial batched network list reaches the registry
with the expected stable ID and name.

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
binding, reconnect, initial discovery and automatically generated child connections
through the App event loop, not complete bouncer compatibility or
live upstream IRC.

The normal TCP regression suite additionally exercises CAP rejection, failed
SASL, rejected BIND, wrong or missing BOUNCER_NETID, premature disconnect,
canonical IDs and injection rejection. It verifies that direct IRC still
autojoins and explicit bouncer binding does not autojoin configured channels.
The application-level reconnect test also prevents rejoining stale channel
buffers. Native and web server editors preserve the binding setting.

Scripted tests cover control registration without BIND or autojoin, rejection of
a login that selects a network, atomic list replacement, partial updates, both
attribute-removal encodings, deletion, malformed batches and abandoned batches.

App regressions cover child creation from committed snapshots, identity isolation,
rename without reconnect, deletion with stale events, parent suspension/reconnect,
manual disconnect and reopening closed children. Native/web focus remains stable
for background discovery, replayed joins and newly arriving DMs; explicit joins
still activate their target. Web rename/resync tests preserve valid buffer IDs.
PASS-specific explicit binding and the remaining feature matrix are separate
pending work. Server-owned history regressions cover memory-only BEFORE ingestion,
reloading after buffer trimming, web page completion and timeout replies, equal
timestamp cursors, retained local rows that are not implicitly loaded, and
volatile account-scoped mentions, including seven-day expiration. Unsolicited
private-message replay creates its missing query in the background without
unread activity or local logging. DCC regressions verify local message writes,
backlog loading and persistent mentions. Web page completion follows all queued
inserts so its final exhaustion flag remains authoritative. Legacy timestamp-only
web cursors retain complete equal-timestamp groups. A BEFORE page arriving after
the user leaves its buffer cannot re-pin it or prevent later reloading. Automatic
channel history waits for NAMES completion, including background channels. An
incomplete BEFORE batch leaves the original retry anchor unchanged. Reconnect
gap-fills all retained bouncer queries without changing focus. Recreating the
Mentions buffer restores same-session volatile entries even without SQLite. The real upstream
fixtures seed 300 messages in each bouncer's store. An automatically discovered
child fetches the latest 200, retrieves the remaining 100 with BEFORE, verifies
ordering and exhaustion, and confirms that no message entered the local log
queue. Soju runs with its upstream disabled, exercising stored history while the
IRC network is offline.

Cancellation regression tests drop registration during CAP negotiation and
network confirmation and verify peer EOF. A separate synthetic 16 MiB PASS write
fills the local socket while the peer stops reading; after cancellation, the
client must abort the pending write instead of finishing it in the background.
Disabling the abort makes this regression fail. The oversized synthetic payload
exists only to produce backpressure deterministically, not as a supported login
format or a real credential.

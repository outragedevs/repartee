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
fixtures seed 300 messages for a query and 300 for a channel in each bouncer's store. The child discovers both targets through TARGETS without manually creating
a buffer. It loads 200 messages for each, retrieves the remaining 100 query messages
with BEFORE, verifies ordering and exhaustion, and confirms that neither target
enters the local log queue. The test also waits until TARGETS pagination finishes. Soju runs with its upstream disabled, exercising stored history while the
IRC network is offline.

Cancellation regression tests drop registration during CAP negotiation and
network confirmation and verify peer EOF. A separate synthetic 16 MiB PASS write
fills the local socket while the peer stops reading; after cancellation, the
client must abort the pending write instead of finishing it in the background.
Disabling the abort makes this regression fail. The oversized synthetic payload
exists only to produce backpressure deterministically, not as a supported login
format or a real credential.

History discovery regressions cover malformed target rows, truncated batches,
request windows, overlapping pages and target deduplication. Synthetic scheduler
tests distinguish a timeout from a successful empty history page, retry failed
hydration, and enforce two concurrent requests when discovering targets or
reconnecting retained queries. TARGETS retries use the original window and stop
after three failed attempts.

Enumeration completeness for timestamp ties larger than a server page and
server-side filtering after LIMIT is not established by these fixtures. Lurker
always orders TARGETS descending and groups messages inside the requested window;
Soju selects the direction and tests the latest message per target. Both filter
closed/detached targets after their store query. These cases remain explicit
acceptance gaps for the full bouncer-support contract.

## Read-marker validation

The generated-child fixture also negotiates `draft/read-marker`, advances the
query marker to the exact server timestamp of history row 149, and verifies the
server acknowledgment. A second independently authenticated client on the same
network must receive that update. A fresh query must return the stored marker.
Both checks use disposable users and messages; the normal no-local-log assertions
remain enabled.

Native regressions cover focus loss/return, headless rendering, delayed messages,
partial reads, precise millisecond timestamps, legacy `READ`, retransmission,
and network-scope isolation. Browser read requests carry a displayed message ID;
the server derives the IRC timestamp from the retained message's `time` tag.
Focus, modal visibility and following the conversation tail gate browser requests.

This stage is still under review. These tests do not establish completion of
aggregate presence or the full bouncer feature matrix.

The read-marker fixture additionally asserts 300 unread hydrated query messages
before marking, then 150 after the server confirms the middle message. Unit
regressions cover marker-before-history and history-before-marker ordering,
reconnect gap rows, older-page insertion, deduplication and own-message exclusion.
History contributes unread counts but emits no live-message or mention alerts.

A web-dispatch regression sends `FetchMessages` through `handle_web_command` with
an empty bouncer buffer and seeded stale SQLite rows. The response remains empty
and the stored rows remain untouched: this route uses `fetch_server_history_page`,
not the direct-IRC SQLite fallback in `web_fetch_messages`.

Pending read updates retain their original IRC target even if a query buffer is
renamed locally. Retry scheduling includes pending targets without a current
buffer; the renamed target is queried separately. This follows the pinned
bouncers' target-keyed storage (`GetReadReceipt(networkID, target)` in Soju and
`newestIdAtOrBefore(networkId, target, ...)` in Lurker), rather than applying an
old target's timestamp to a different server-side history.

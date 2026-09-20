# MONITOR validation plan

Status: core implementation merged in https://github.com/outragedevs/repartee/pull/74
after a clean third Sol medium review. Dynamic real-upstream acceptance passed on the follow-up branch; its review is pending.

## Scope

Provide native/web command access to MONITOR add/remove/clear/list/status, track
per-connection subscribed nicknames and online/offline replies, and preserve
account/network isolation. Consume 730/731/732/733/734 with correct list boundaries,
limits and failures. Reconnect and CAP/ISUPPORT changes must not leave stale online
state or silently subscribe to another account's list.

Negotiate extended-monitor only once its account/away/chghost/setname consumers
work for monitored users outside shared channels. Audit both the standard and
`draft/extended-monitor` aliases against each pinned server; accepting a capability
without consuming the notifications is not completion.

## Initial source pointers

Soju `downstream.go` implements MONITOR around line 2900 and advertises both
extended-monitor aliases around lines 282–287. Availability is gated by upstream
MONITOR ISUPPORT. Lurker `server/services/bouncer.ts` maintains a per-client monitor
list and maps extended-monitor aliases through the upstream capability set;
handlers begin around line 2483. Both must be exercised through actual App clients
and disposable upstreams, including multiple clients, unsupported upstreams,
list-full replies, reconnect and dynamic capability changes.

The branch adds `/monitor` command routing, a connection-scoped target manager,
server-list read barriers after mutations, and a separate cache for monitored
peers without channel membership. This cache consumes account, away, host and
real-name changes independently of channel nicklists.

## Local regression evidence

Nine state-machine tests cover initial list adoption without deleting existing
subscriptions; pending versus confirmed display; mutation/list ordering; failed
list sends without repeated adds; expired and invalid snapshots; partial
list-full rejection; reconnect and MONITOR support loss; extended notifications
without shared channels; both extended-monitor aliases; nickname case mapping;
and bounded queued input.

Two App-level tests additionally exercise web command routing to its selected
network, metadata without channel membership, connection isolation, and discarding
old targets when the SASL account scope changes.

Validation run `/tmp/repartee-monitor-check17` passed 2483 native tests (7 ignored)
and 142 web host tests. `make clippy` passed for both crates without project
warnings. The dependency-only `proc-macro-error2` future-compatibility notice
remains unchanged.

## Pinned bouncer integration evidence

`python3 scripts/test_bouncer_presence.py PROVIDER SOURCE --monitor` runs two
actual App clients against each pinned bouncer and a disposable upstream.
Both providers passed: `/tmp/repartee-monitor-soju7.log` and
`/tmp/repartee-monitor-lurker7.log`. One client uses web command routing and the
other the native command handler. The scenario verifies separate target lists,
online/offline delivery, list snapshots, upstream list-full rejection without
repeated adds, transport reconnect with restored
targets and invalidated stale availability, clearing one client's list while the
other remains subscribed, and account/away/host notifications outside shared
channels. Both negotiate an extended-monitor alias. Soju additionally delivers
the monitored user's real-name notification. Lurker's SETNAME limitation remains
as documented in the SETNAME validation.

## Dynamic follow-up acceptance

`fix/monitor-capability-transitions` fixes missing status refresh when MONITOR is
restored but the bouncer retains its per-client list. The client now requests
MONITOR S after synchronization on initial support, restored support and transport
reconnect. Without that query, the real Soju fixture left accepted targets at
unknown availability indefinitely. The state regression reproduces retained
server targets and proves the status query is issued.

Both providers passed dynamic account-notify withdrawal/re-advertisement; stale
account data is cleared and the capability is negotiated again. Soju additionally
passed MONITOR withdrawal and restoration with refreshed online/offline states.
Results: `/tmp/repartee-monitor-dynamic-soju5.log` and
`/tmp/repartee-monitor-dynamic-lurker5.log`.

The pinned Lurker does not expose dynamic MONITOR withdrawal: `bouncer.ts`
RELAY_DROP includes numeric 005 (lines 212–222, applied at line 471), and
`ircConnection.ts` lines 1829–1831 never clear useMonitor after initial support.
The fixture sends an ordered NOTICE after its ISUPPORT update, waits for both
clients to receive that marker, and confirms MONITOR remains advertised. This is
an upstream limitation, not a client claim that hidden withdrawal was handled.
The fixture restores upstream support before proceeding. Repartee cannot infer an
upstream state change that the bouncer does not report.

Both pinned bouncers also passed `--monitor-unavailable`: no MONITOR advertised
on connection, add remains queued without optimistic peer availability, and a raw
list probe gets the bouncer's unsupported-command response visibly handled.
Results: `/tmp/repartee-monitor-unavailable-soju1.log` and
`/tmp/repartee-monitor-unavailable-lurker1.log`.

`make clippy` then `make test` passed in
`/tmp/repartee-monitor-dynamic-check6`: zero project warnings, 2484 native tests,
142 web host tests and 8 ignored fixture tests. Follow-up review is pending.

## Review round 1

The pinned Sol medium review found two actionable issues: incomplete authentication
scope and endless retries for targets omitted from the server's list. The scope
now includes username (including the configured default), SASL mechanism, client
certificate path and SASL key path as well as the server/network and SASL user.
The App test changes every identity selector independently and proves old targets
are discarded. Successful mutations are recorded until the list barrier completes:
unaccepted additions are removed with a local explanation, and unaccepted removals
stop synchronization instead of repeating indefinitely. A regression test covers
both cases. Round 2 identified the support-transition issues described below.

## Review round 2

Support withdrawal previously retained an unfinished list transaction and could
accept delayed availability replies. Support transitions now discard the old
snapshot, submitted mutation and send retry, while preserving desired targets.
Unavailable MONITOR numerics and extended notifications cannot repopulate the
cache. A regression withdraws support during a partially received list, supplies
late status/list/identity messages, restores support and verifies reconciliation.
The real-bouncer test now explicitly waits for matching desired/confirmed lists
and no in-flight operation; absence of a next command alone is insufficient.
Round 3 completed with no actionable findings.

## Compiled web client

`/tmp/repartee-browser-qa/monitor.cjs` passed against the committed WASM in WebKit.
It verifies command emission for the selected server buffer and rendered event
rows for subscribed/pending states and rejection, with literal percent and markup
characters. `/tmp/repartee-monitor-browser1.log` records the result; the screenshot
`/tmp/repartee-monitor-web.png` was inspected. Controlled WebSocket messages supply
these UI payloads; actual App/bouncer behavior is tested separately above.

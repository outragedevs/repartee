# MONITOR validation plan

Status: implementation in progress on `feat/irc-monitor`; not reviewed or merged.

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

## Outstanding acceptance evidence

Extend the real-bouncer scenarios to unsupported upstreams and dynamic capability changes. Verify rendered web output.
Review/fix rounds and PR merge are pending. The passing integration scenario
covers the behavior listed above; it does not prove these remaining cases.

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
Round 3 is pending.

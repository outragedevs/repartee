# Explicit bouncer NAMES validation

Status: implemented on `feat/explicit-bouncer-names`; the full tracked and
untracked diff passed a clean GPT-5.6 Sol medium review.

Soju at `82e8b7adfb2ab64ec3b88807d29b8b6940236008` advertises
`no-implicit-names`, `draft/no-implicit-names` and `soju.im/no-implicit-names`.
Its downstream state burst omits NAMES when any alias is negotiated
(`downstream.go`, around line 4156). Lurker at
`be42a04e73d6f337e76734684deb457cb5dcdb5f` advertises none of these aliases.

Repartee now requests the advertised aliases and sends NAMES after a live own
JOIN, before the new channel's own-nick entry can mask an empty list. Other
users' JOINs, duplicate own JOINs and CHATHISTORY playback do not send requests.
Replies use the existing nicklist, WHO/WHOX and web snapshot paths. Connections
without an enabled alias retain implicit NAMES. A failed transport send produces
a visible diagnostic; a transport reconnect rebuilds channel state.

## Evidence

`/tmp/repartee-names-check4` passed `make clippy` then `make test`: zero project
warnings, 2489 native tests, 142 web host tests and 10 ignored fixture tests.
The additional capability-transition regression passed in the same run.
Unit coverage includes all three aliases, duplicate/other-user JOIN suppression,
history exclusion, capability ACK/DEL and connection isolation.

Both real bouncer fixtures passed with a disposable upstream:

- `python3 scripts/test_bouncer_presence.py soju /tmp/repartee-bouncer-audit.U3fkHg/soju --names`
- `python3 scripts/test_bouncer_presence.py lurker /tmp/repartee-bouncer-audit.U3fkHg/lurker --names`

Logs: `/tmp/repartee-names-soju1.log` and `/tmp/repartee-names-lurker1.log`.
The test joins two channels, verifies Alice's operator prefix and Bob's membership,
disconnects the actual client, verifies stale users were cleared, reconnects and
checks both restored native state and web nicklist snapshots. Soju enables all
three aliases; Lurker retains its implicit lists. This does not claim a rendered
browser test or server-to-server netsplit coverage.

Review log: `/tmp/repartee-names-review1.log`. The reviewer reported no actionable
correctness issues.

## Remaining integration audit

Generic/nested live batches currently use a separate state-dispatch path. Audit
own JOIN and NAMES completion there with labeled-response support; these cases
are not proven by the unbatched reconnect fixture. All remaining acceptance rows
in `BOUNCER_SUPPORT.md` remain required, including dynamic ISUPPORT, redaction and
upstream account operations.

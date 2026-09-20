# Bouncer history acceptance audit

Audited on 2026-09-20 against Repartee `3b8f0f0672d1169725ad7b368a1f4966fd81bd39`
and the upstream revisions pinned in `BOUNCER_SUPPORT.md`. This is an open-gap
record, not a completion claim. The full bouncer objective remains unchanged.

## Timestamp ties in conversation discovery: reproduced gap

Reproduce with the pinned checkout and its built `soju`, `sojudb` and `sojuctl`
binaries (the same prerequisites as `scripts/test_bouncer_binding.py`):

```sh
python3 scripts/audit_bouncer_history_targets.py /path/to/pinned/soju
```

The script creates and deletes its own database, configuration and local socket
listener, seeds all rows, checks negotiated capabilities/identity, and asserts
page counts `[1000, 1000, 0]` with exactly one missing target. Exit zero means the
known provider limitation was reproduced; it does not mean client acceptance
passed. Its JSON output contains the complete page-count transcript below.

A disposable, unmodified pinned Soju instance used SQLite message storage, one
disabled upstream network and 1001 private targets. Each target had one PRIVMSG
at `2024-01-01T00:00:00.000Z`. No production account or data was used.

After PASS authentication with a network selector and negotiation of `batch`,
`server-time` and `draft/chathistory`, Soju advertised `CHATHISTORY=1000` and
`MSGREFTYPES=timestamp`. The following requests reproduce the cursor sequence in
`App::receive_history_targets` (all lower bounds are the Unix epoch):

| Upper bound | Requested limit | Returned targets | Distinct targets seen |
| --- | --- | --- | --- |
| `2025-01-01T00:00:00.000Z` | 1000 | 1000 | 1000 |
| `2024-01-01T00:00:00.001Z` | 1000 | 1000 | 1000 |
| `2024-01-01T00:00:00.000Z` | 1000 | 0 | 1000 |

Wire request form:

```text
CHATHISTORY TARGETS timestamp=2025-01-01T00:00:00.000Z timestamp=1970-01-01T00:00:00.000Z 1000
```

The response is a `draft/chathistory-targets` batch. One known stored target
remains undiscovered. This probe exercised the actual Soju socket endpoint;
it reproduced the client's cursor sequence explicitly rather than running the
App itself. The App regression
`full_target_pages_advance_and_deduplicate_overlapping_rows` separately confirms
that an overlapping page is followed by an exclusive boundary and that an empty
page sets `finished`. It does not establish complete discovery.

Authoritative source locations:

- Repartee: `src/app/history_discovery.rs`, `receive_history_targets`.
- Soju: `database/sqlite.go`, `ListMessageLastPerTarget`, orders by latest time
  and applies LIMIT without a secondary cursor. `downstream.go` rejects requests
  above its history limit.
- Lurker: `server/db/messages.ts`, `listActiveTargetsInWindow`, uses exclusive
  time bounds and `ORDER BY lastMessageAt DESC LIMIT ?`. This is source evidence
  for the same risk; a Lurker runtime reproduction is still required.

Both providers additionally filter hidden/closed targets after the limited
store query (`downstream.go` TARGETS and `bouncer.ts` handleChatHistoryTargets).
A filtered-empty page can therefore hide older visible conversations. This
second scenario has source evidence only; a runtime regression remains required.

Required follow-up: reproduce both boundary cases through the App against both
providers; determine which results can be recovered using their supported IRC
interfaces; distinguish incomplete discovery from confirmed exhaustion in native
and web presentation. Do not claim enumeration beyond a provider's observable
protocol merely because retries finish. Do not increase limits beyond advertised
bounds or silently enable local logging as a workaround.

## Persistent history exclusion: evidence is narrower than the gate

The real-provider `pinned_bouncer_generated_children` fixture in
`src/app/bouncer_children.rs` currently proves initial history hydration,
300-message LATEST/BEFORE retrieval, TARGETS discovery for its small fixture and
read-marker synchronization. It installs a channel as `state.log_tx` and checks
that the channel receives no rows for the two history targets.

This proves that those replay paths did not enqueue those messages. It does not
run the persistent writer or reopen a database after daemon restart, and it does
not exercise live outgoing messages or web pagination in that real-provider
fixture. Treat those distinctions as outstanding verification requirements,
not as evidence of a known disk leak.

Focused tests in `src/app/server_history.rs`, `src/irc/batch.rs` and state modules
cover memory pages, stale local rows, logging gates and error handling. They
remain useful component evidence; their scope does not replace a process/storage
acceptance test.

Required follow-up:

- Run both real providers with the normal SQLite writer and diagnostic text logging in a
  disposable data directory. Verify live incoming/outgoing, replay and server
  search do not add message records or text logs.
- Reopen the same local database and recreate the client after shutdown; prove
  bouncer buffers hydrate from the provider and ignore preserved legacy rows.
- Exercise browser pagination, timeout/disconnect and disabled Soju history
  storage with the same persistence checks.
- Include a direct-IRC positive control that writes messages normally, and
  preserve existing user archives without automatic deletion.

These are still required by the existing integration gates in
`BOUNCER_SUPPORT.md`. No acceptance row is closed by this audit.

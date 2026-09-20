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

## Disk-backed client reconstruction regression

`src/app/history_storage_fixture.rs::pinned_bouncer_persistent_history` uses the
normal schema and asynchronous SQLite writer with an explicit temporary database
path. The test-only storage constructor avoids changing HOME or opening the
operator's data directory. Run it against each pinned provider:

```sh
python3 scripts/test_bouncer_binding.py soju /path/to/soju --test-filter pinned_bouncer_persistent_history
python3 scripts/test_bouncer_binding.py lurker /path/to/lurker --test-filter pinned_bouncer_persistent_history
```

The fixture seeds a legacy row under the bound account/network scope before
connecting. Actual provider TARGETS/LATEST responses hydrate 200 messages, and a
web `FetchMessages` command obtains the remaining 100 through BEFORE. Assertions
inspect both the application buffer and the session-targeted web page. The old
local row must never appear in either.

Before shutdown, a live incoming message and own echo are injected through the
normal IRC event handler on that same bouncer connection. The web send command
exercises local outgoing display with echo-message temporarily removed from the
fixture's local capability state. Each row must actually appear in memory before
checking that it remains absent on disk. These live rows are controlled inputs,
not evidence of actual upstream delivery; the binding fixture has no live
upstream. A similarly injected direct-IRC positive control must persist its
message and connection event.

The writer is drained, the App is dropped, and the database is reopened read-only.
Its complete message list must contain only the seeded legacy row and two direct
control rows. A fresh App then opens that same database, reconnects to the real
bouncer, repeats hydration and web pagination, and drains its writer. A second
read-only inspection must find exactly the same three rows.

Both pinned-provider runs passed on 2026-09-20. The full default suite passed
2684 native and 146 web tests (24 provider-dependent native tests ignored), with
no project Clippy warnings. This closes the narrower disk-writer/reconstructed-App
regression. It does not claim an operating-system daemon restart,
actual browser rendering, diagnostic-file inspection, network-delivered live
messages, server-search persistence, error-path persistence, or Soju without
history storage. Those remaining full-lifecycle acceptance cases stay open.

The separate real-daemon/browser follow-up is documented in
`bouncer-daemon-history.md`. It adds actual process restart and rendered browser
pagination, and fixes diagnostic-file content leaks found by that broader test.
Its remaining limits are stated separately; this does not retroactively enlarge
the scope of the in-process fixture above.

## Timestamp-tie warning and named recovery

The client now distinguishes a potentially incomplete discovery run internally
and emits one server-buffer message when an overlapping boundary page is still
full at a single timestamp. Native state and the web event stream carry the same
warning. Pagination continues toward older timestamps; the warning does not
prevent other targets from being discovered or histories from loading. A short
overlap page or explicit `draft/chathistory-end` does not trigger this warning.

Run the actual App fixture against either pinned endpoint:

```sh
python3 scripts/test_bouncer_binding.py soju /path/to/soju --target-tie --test-filter pinned_bouncer_discovery_limit
python3 scripts/test_bouncer_binding.py lurker /path/to/lurker --target-tie --test-filter pinned_bouncer_discovery_limit
```

Both runs passed on 2026-09-20. The fixture creates 1001 targets sharing a timestamp,
observes exactly 1000 discovered queries, checks one native/web warning, determines
the missing name from the known fixture set, opens it with `/query`, and retrieves
its history through the web FetchMessages path. This supplies the previously
missing Lurker runtime reproduction as well as App-level evidence for Soju.
The Lurker rows use the corresponding target nick as sender so private-history
routing models real conversations correctly.

This is detection and named recovery, not automatic enumeration of the missing
name. Neither pinned TARGETS endpoint exposes a secondary cursor within one
timestamp. The warning deliberately says conversations *may* be missing: exactly
one full timestamp group can also be complete. Filtered-empty pages and complete
automatic discovery remain unresolved; they are not marked complete by this PR.

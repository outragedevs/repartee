# Soju server-side search

Status: isolation prerequisite merged in PR #88; commands and result/context
views merged in PR #89. The sections below distinguish the original development
stages from subsequent runtime and persistence acceptance.

## Pinned provider behavior

The Soju revision in BOUNCER_SUPPORT.md implements SEARCH in
`downstream.go:3381-3471`; the protocol is documented in `doc/ext/search.md`.
The Lurker revision does not advertise this capability.

SEARCH takes one IRC-tag-encoded attribute parameter. The pinned implementation
requires `in` and a bound network. Cross-network search is not implemented by this
provider. Supported attributes are `in`, `from`, `text`, `before`, `after`, and
`limit`; dates use the server-time format, limit defaults to 100 and is capped at
100. Matching is provider-defined. Text and target attributes need IRC tag escaping.

The actual successful response is a `soju.im/search` batch, including an empty
batch when nothing matches. One paragraph of the upstream document uses `search`
instead; use the actual wire implementation as the integration reference.
FAIL SEARCH INVALID_PARAMS and INTERNAL_ERROR indicate errors; missing search
storage returns numeric 421. Do not turn these into an empty successful result.

## Current client boundary

Before the isolation prerequisite, `src/irc/batch.rs` replayed search as an
unknown batch type through the ordinary live-message
handler. Requesting the search capability before a dedicated consumer exists
would therefore risk live unread/activity updates and routing search results into
normal conversation buffers. `src/app/irc.rs` collects batch bodies and invokes
`receive_completed_batch`; the search consumer must intercept before that fallback.

The existing `/search` in `handlers_logs.rs` belongs to the standalone local log
browser. It does not supply the required native/web server-search interface.
Server-search state must remain UI-independent and serializable for web snapshots,
with results separate from persistent message history and live conversation state.

## Implementation and acceptance gates

- Negotiate `soju.im/search` only with its implemented consumer; handle runtime
  capability changes and refuse unsupported or unbound connections.
- Expose selectors and a dedicated native/web result view. Preserve original
  target, nick, timestamp, msgid and stable ordering, including timestamp ties.
- Scope requests to the connection generation, account and network. Support
  pending, empty, failure, timeout and cancellation states without letting late
  replies complete a different request.
- Quarantine search batches from live unread/activity, history ingestion and
  persistent logging. Keep displayed results bounded and ephemeral.
- Provide navigation to original conversation context via server history without
  copying search results into unrelated live buffers.
- Validate real Soju matches, zero matches, escaped selectors, invalid criteria,
  reconnect/removal, concurrent network requests and native/web presentation.
  Confirm Lurker refusal and unchanged direct IRC/local log behavior.
- Run clippy, tests, browser validation where UI changes, and full Sol medium
  review/fix rounds before merge. Broader acceptance rows remain in scope.

## Isolation prerequisite

A regression reproduced unsolicited search batch rows entering the conversation
through the live dispatch fallback (message count increased from 1 to 2). Search
batches are now classified as non-live and consumed at both App and state batch
dispatch boundaries. This prevents nested wrappers from folding search rows into
a live parent and prevents expired batches from replaying partial results.

The regression matrix covers direct IRC and bound bouncer connections, clean and
expired batches, standalone and nested labeled responses, and a nested generic
wrapper inside the search. Assertions cover message count/activity, unexpected
channel creation, local log writes and web live-message delivery. This change
does not negotiate search support or expose a search command. The full feature
and every acceptance gate above remain pending.

A second regression reproduced a late row referencing an already closed/expired
batch entering live chat. The App now refuses orphan batch-tagged messages instead
of treating them as unbatched live traffic. The batch protocol forbids references
outside their batch lifetime (https://ircv3.net/specs/extensions/batch); delayed
rows after a local timeout likewise retain their non-live classification. The
same matrix verifies that subsequent untagged live messages still display and
that direct IRC still logs while bound bouncer connections do not. First review
was clean before this additional late-reply fix; full Sol medium review round 2
is also clean. Clippy passed with no project warnings, and 2586 native plus 144
web tests passed (15 provider-dependent native fixtures remain ignored in the
default suite). No search capability is negotiated by this prerequisite.

## Command and result-view increment

`/bsearch` supports the pinned provider selectors with tag escaping, bounded
limits and UTC millisecond timestamps. The connection-local `*search*` special
buffer is shared by the native and web render paths, read-only, replaceable and
independently closable. Results retain timestamp, sender, original target and
message ID, preserve provider order, decode ACTIONs and propagate redactions.
No rows enter live conversation activity or persistent history.

`/bsearch context <row>` uses a retained result target/timestamp to request
CHATHISTORY AROUND, replacing the isolated view with surrounding messages. It
serializes against other history requests for that target. Timeout and local
cancellation discard late results and block new requests until terminal completion
or transport reset. Context failure is not an empty result.

Pinned Soju tests have passed ordinary searches, sender filtering and empty
results. Unit tests cover network isolation, identical timestamps, tag encoding,
invalid results, timeouts, cancellation, capability absence, closed views and
redaction. Review round 1 found missing independent close behavior, ACTION
decoding and context navigation; these have been implemented with regressions.
A real WebKit test passed searching and replacing results, then showed that a
reload selects the daemon's active buffer. Its follow-up explicitly reopens the
retained search view and also checks context navigation and independent closing.
The final provider/browser runs and review results are recorded below.

Review rounds 2–4 corrected message-ID context anchors, application-derived tag
names, view-ID collisions with server buffers, stable row numbering after
redaction, historical REDACT handling, E2E decryption and unrelated error routing.
Pinned Soju plus real WebKit passed filtering, context, replacement, reopening
after reload and independent close; incoming and outgoing private messages remain
searchable after the local nick changes. Pinned Lurker rejects the absent feature.

Review round 5 identified ambiguous context correlation after an ordinary history
request expires locally. When labeled-response is acknowledged, context sends a unique request label
and matches both the target and label. Nested batch
openers inherit the enclosing label. Unrelated errors and delayed ordinary history
cannot consume the context request; late already-completed context batches cannot
enter the live conversation. A regression initially failed for nested response
wrappers and now covers the real labeled-response/CHATHISTORY nesting.
Follow-up validation and review results are recorded below.

Real-provider follow-up exposed a registration race: Soju ACKs labeled-response
before network binding, then withdraws it when the upstream lacks it. The 001
Connected event previously restored the stale negotiation snapshot. Registration
now applies CAP DEL/ACK to that snapshot before publishing it. Context labels are
optional: unlabelled context remains available after unambiguous history requests;
a timed-out older history request blocks unlabelled context until reconnect.
This prevents treating its delayed reply as AROUND without disabling normal
context retrieval on Soju networks that lack upstream labeled-response support.
The real provider fixture asserts that the withdrawn cap stays withdrawn.

Validation after the registration fix:

- `make clippy`: no project warnings.
- `make test`: 2603 native and 144 web tests passed; 16 provider fixtures are
  excluded from the default native run.
- Pinned Soju + real WebKit: passed both without upstream labels and with labels
  enabled (`--server-search`, and `--server-search --names`). Both runs cover
  native and web commands, sender filters, empty results, AROUND context, direct
  messages after nick changes, reload/reopen and independent view close.
- Pinned Lurker: unsupported search rejected without starting a pending request.
- Request-label regressions cover delayed prior history, unrelated errors,
  nested response wrappers, duplicate late batches and numeric failures.
- Full Sol medium review round 7 required rechecking batch, time and tag
  capabilities before context requests. This is fixed with CAP DEL regressions;
  running requests also discard results when a required capability disappears.
  Round 8 found that errors inside labeled-response wrappers bypassed the
  early error handler. Search errors now run through dispatch after batch label
  inheritance, with nested FAIL and numeric regressions. Round 9 identified a race between capability withdrawal and the maintenance
  tick. Completed responses now recheck expiration/capabilities immediately;
  search and context regressions finish their batch before any tick.
  Full Sol medium review round 10 is clean. All actionable findings from
  prior rounds were fixed before this final review.

## Daemon persistence and browser acceptance

The real-daemon `--live-history` scenario in
`scripts/test_bouncer_presence.py` now also exercises search after live traffic
and browser pagination. Run it with the pinned Soju checkout and a production
daemon image as described in
[bouncer-daemon-history.md](bouncer-daemon-history.md).

For each of two daemon processes using the same isolated runtime directory, the
compiled Chromium UI submits a sender-filtered search against the real Soju
store. Incoming and outgoing texts both match the query, so requiring only
Alice's rows verifies sender filtering. It checks the result count, requests surrounding
history with `/bsearch context 1`, replaces the view with an empty search,
reloads that empty view, and closes it. The ordinary conversation's message rows
must remain identical, and no result may be pushed as a live or inserted message
into that conversation. Normal presence events caused by browser detach/reattach
are allowed and are not search results.

After each shutdown, the existing daemon runner inspects SQLite and the TRACE
diagnostic file. Only local startup events may persist; live, replayed, searched
and context message bodies must be absent. Thus the display checks cannot pass
merely by discarding search results before they reach the UI.

The same browser command on Lurker and Soju's memory-only store must produce the
unsupported-search explanation without creating a result view. The direct CAP
probe verifies that search is advertised only in the supported Soju database
scenario. Both unsupported paths are also covered by the disk/log inspection.

This adds actual process/browser/storage evidence for successful, empty and
unsupported search. It does not replace the existing request-correlation unit
regressions or claim process-level coverage of every timeout, cancellation,
capability-withdrawal and transport-error path.

The offline PING barrier allows 30 seconds: the pinned Soju upstream writer
queues PONG behind normal commands and rate-limits them at two seconds per
message (`conn.go`, `server.go`). Browser reloads add real AWAY transitions;
a ten-second barrier can expire despite healthy delivery. The runner still
requires a received PONG before starting the next daemon.

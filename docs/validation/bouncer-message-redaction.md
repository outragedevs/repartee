# Bouncer message redaction implementation audit

## Current acceptance status

| Requirement | Current evidence / remaining boundary |
| --- | --- |
| Selective negotiation | Registration and CAP NEW request redaction only for bound non-control bouncer networks. Direct IRC does not request it. Unit scope tests and automatic negotiation through actual Soju pass. |
| Native and web commands | `/redact target msgid [reason]` validates opaque IDs and IRC framing; server confirmation drives replacement. Native and WebCommand routing tests pass. |
| Provider behavior | Actual pinned Soju forwards requests and confirmations; server CHATHISTORY replay works after clearing local rows and deletion state. Pinned Lurker rejects unsupported use. |
| Pending work and memory | Shared identities protect batches, multiline, history rows, mention aggregates and deferred transforms. Detailed notices use a 4096-entry cache; scope-owned SHA-256 identity fingerprints preserve older deletion knowledge without message bodies. Older fingerprints spill to an automatically removed temporary SQLite database with a 256 KiB page cache and 64 MiB page limit. It stores only hashed identities, never bodies; I/O failure hides unverified content until scope reset. |
| Mention privacy | Volatile mention history owns scoped identities and drops deleted entries. Reloaded aggregation rows retain identity. Reopen-after-deletion regression passes. |
| Read-marker activity | Replaced unread rows become Events; ActivityChanged is emitted and the row is not falsely marked read. Regression passes. |
| Browser | Built WASM replaces rendered rows using actual Soju event payloads replayed over a controlled WebSocket; no page errors. This is not a live browser-to-App connection test. |
| Local persistence | Actual provider fixture confirms no log queue writes during live deletion or server history replay. Existing direct-IRC/DCC non-regressions remain in the full suite. |
| Review | Rounds 1–5 returned findings, all addressed. Full round 6 explicitly found no actionable correctness issues, including untracked source files. |

Latest full suite: 2546 native and 144 web tests pass, 11 native fixture tests
ignored; Clippy has no project warnings. WASM rebuilt with `NO_COLOR=true` because
Trunk rejects the inherited `NO_COLOR=1`. An upstream dependency still emits its
existing future-incompatibility notice. No release build or merge is claimed.

The remainder is chronological audit evidence. Earlier pending statements are
historical and are superseded by this table and later entries.

## Protocol and provider evidence

Specification: https://ircv3.net/specs/extensions/message-redaction

Use `draft/message-redaction` with `message-tags`; request `echo-message` so own
messages have server IDs. The wire request is `REDACT target msgid [reason]`.
Act on the server response, not the outbound request. Preserve server failures
and omit a reason when the user supplies none. Reject cross-target identity
matches. History may omit deleted messages or send a REDACT after the original;
REDACT does not count against the requested history message limit. Keep visible
redaction accountability without retaining the original body by default.

Pinned sources already listed in BOUNCER_SUPPORT.md:

- Soju `downstream.go:288` offers the capability, `:593` gates delivery,
  `:2633` forwards requests; `upstream.go:51` requests upstream support and
  `:703` accepts the incoming command.
- Lurker `server/services/bouncer.ts:123` and `:165` omit the capability from
  both static and passthrough sets. `bouncerClientFilter.ts:196` blocks REDACT
  without negotiation. Its filter comment explicitly anticipates commands it
  does not request today. This is not evidence that Lurker offers redaction.
  Test unsupported behavior instead of manufacturing a successful negotiation.

## Baseline Repartee gaps

- `src/irc/cap.rs` does not request redaction.
- No REDACT handler or sending command exists in the IRC/app paths inspected.
- `src/irc/events.rs::ingest_chathistory_batch` accepts only PRIVMSG/NOTICE,
  skips REDACT, and can retain the original body from the same history batch.
- Before this prerequisite, `src/irc/batch.rs` passed raw `batch.messages.len()`
  into history completion;
  redaction metadata must not produce a false full-page continuation decision.
- `WebEvent::DeleteMessages` already removes in-memory IDs, but has no server
  msgid matching for an older message retained only by a browser. Reusing it
  alone cannot prove web parity after native buffer eviction.
- `src/storage/writer.rs` queues LogRow inserts asynchronously. A direct SQLite
  delete alone can race a pending insert. Therefore globally enabling this cap
  before storage behavior is implemented would be incorrect for direct IRC.
- Translation/shrink and own-echo queues can release messages after a live
  redaction. Filtering only the visible buffer leaves resurrection paths.

## Implementation gates

1. Represent a deletion using connection/account-scoped opaque server msgid and
   conversation identity, applying IRC CASEMAPPING to targets, never msgids.
   Resolve channel, incoming DM and own-echo DM targets. Keep the deletion
   identity across reconnect to the same account; never share it with another
   account or network. Handle duplicate/unknown IDs and CAP loss deliberately.
2. Apply deletion consistently to native messages, cached history, pending
   transformations and web state, including a browser retaining messages that
   the native buffer evicted. Clear derived previews and references that carry
   original content. Bound detailed retained state and keep compact deletion
   knowledge so known deleted content cannot reappear after cache eviction.
3. Process history redactions before exposing original bodies, including nested
   and expired batches. Exclude redaction metadata from message-limit decisions,
   while preserving valid pagination anchors. Test deletion before/after the
   original and across overlapping history/live delivery.
4. Expose a shared native/web command with optional reason and validated wire
   arguments. Negotiate only when the implemented history ownership policy can
   honour it; unsupported Lurker connections must explain unavailability.
5. Test the real pinned Soju forwarding path, actual web rendering and browser
   retention case, reconnect/history replay, server FAIL and dynamic capability
   removal. Verify zero bouncer history writes and unchanged direct IRC/DCC.
6. Run make clippy before make test, rebuild WASM for frontend changes, and full
   explicitly pinned Sol medium review/fix until clean before PR and merge.

All BOUNCER_SUPPORT.md acceptance rows remain required. This document records
implementation requirements, not a claim of completed message-redaction support.

## Pagination prerequisite

Two new regression tests fail on the original implementation: both select the
deleting event's msgid/time as the next history anchor. Evidence:
`/tmp/repartee-redaction-pagination-regression-test.log`.

The prerequisite excludes REDACT metadata from page size and both oldest/newest
anchors. Original messages still count towards the limit even if later deleted.
This does not yet change visible message deletion, capability negotiation, or
request commands. Those gates above remain required.

The pagination prerequisite passes make clippy followed by make test with zero
project warnings: 2512 native tests, 142 web host tests and 10 ignored fixtures
(`/tmp/repartee-redaction-pagination-check2`). Coverage includes short BEFORE
and AFTER pages, full AFTER continuation, redaction-only pages, lowercase verbs
and timeout preservation. The original conversational identity remains the
continuation anchor. This is unit evidence for pagination, not end-to-end
redaction support. Full explicitly pinned Sol medium review completed without findings:
`/tmp/repartee-redaction-pagination-review1.log`.

## Delivery work in progress

Pagination merged in https://github.com/outragedevs/repartee/pull/81.
`feat/bouncer-redaction-delivery` now implements initial bouncer-scoped receive
handling, in-memory body replacement, delayed-delivery filtering, a history
redaction prepass, opaque IRC msgid transport and a web redaction event. The web
render key changes when message content/type changes so existing DOM rows update.
No redaction capability is requested yet and no send command exists.

Validation so far:

- `/tmp/repartee-redaction-delivery-check5`: make clippy then make test; zero
  project warnings, 2516 native and 143 web host tests, 10 ignored fixtures.
- `/tmp/repartee-redaction-delivery-wasm1.log`: successful make wasm; generated
  assets use hash `c686e202da1cde77`.
- `/tmp/repartee-redaction-delivery-browser1.log`: built WASM in WebKit replaces
  an already-rendered message using only server msgid, preserves a differently
  cased msgid and removes original visible text. Controlled WebSocket fixture;
  this is not a real Soju integration test. Screenshot:
  `/tmp/repartee-redaction-delivery-web.png`.

Before enabling the capability or merging delivery, finish tombstone retention
bounds/lifecycle, mention copies and unread state, E2E placeholders, deferred
fan-out, DMs/custom CASEMAPPING, capability loss, unsupported/direct connection
behavior and command/FAIL handling. Add real Soju forwarding/history validation
and full Sol medium review. The current state is explicitly incomplete.

The delivery build also corrected two `redundant_pub_crate` warnings introduced
by the pagination helper; the earlier prerequisite zero-warning description was
inaccurate. Current check5 output was inspected and has no project warnings.

## Mention and encrypted-placeholder follow-up

The current branch additionally qualifies aggregate mention correlation IDs by
network/account scope, target and opaque server msgid. Deleting a channel message
updates its aggregate copy, including via a msgid-only web event when the browser
retains that copy. Deferred translation fan-out consults the deletion registry
before creating a new mention. Cross-network msgid collisions are tested.

`Message.redaction_msgid` preserves the deletion identity of transient E2E
placeholders independently from tags and storage/history deduplication identity.
It defaults to None at existing construction sites. An encrypted placeholder
remains tagless until redacted; deletion can replace it natively and on the web.
A later decrypted replay does not create a duplicate redaction marker. Existing
placeholder non-persistence/deduplication regressions still pass. The larger
message payload required boxing the incoming shrink-delivery enum variant.

`/tmp/repartee-redaction-delivery-check10` passed make clippy then make test:
zero project warnings, 2518 native tests and 143 web host tests, 10 ignored
fixtures. The frontend was unchanged in this follow-up; the prior built-WASM
render test remains the relevant frontend evidence.

Outstanding before merge/negotiation: bounded deletion retention and lifecycle,
unread/notification semantics, full DM/custom CASEMAPPING and capability-loss
coverage, command/FAIL handling, real Soju integration and full Sol medium review.
No PR or merge has been created for the delivery branch yet.

## Command and rejection follow-up

`/redact <target> <msgid> [reason]` now sends a request on the active bound
bouncer connection. It requires a connected state plus message-tags and redaction
capabilities, preserves opaque msgid case, omits an unspecified reason and rejects
control characters, invalid middle parameters, multiple targets and oversized
frames. Sending never changes local message state. The shared web RunCommand
path is covered, including native selection on another network. Labeled server
FAIL replies return to the originating buffer and leave the original visible.

`docs/commands/redact.md` supplies embedded help and the generated command page.
`/tmp/repartee-redaction-command-check3` passes make clippy then make test with
zero project warnings: 2521 native tests, 143 web host tests, 10 ignored fixtures.
`/tmp/repartee-redaction-command-docs1.log` records a successful make docs run.
Deletion notices and rejection text escape percent formatting.

Capability negotiation remains disabled. Outstanding before review/merge:
bounded deletion retention/lifecycle, unread semantics, complete DM/custom
CASEMAPPING/capability-loss cases, real Soju forwarding and history fixtures,
unsupported Lurker validation and a full explicitly pinned Sol medium review.

## Capability-loss and scope lifecycle follow-up

A new production-App regression reproduced original-body exposure when REDACT
arrived inside a history batch before CAP DEL and the batch closed afterwards
(`/tmp/repartee-redaction-caploss-regression-test.log`). Redactions admitted to
an open batch are now processed when received, while negotiation is still valid;
the later history prepass/delivery retains that confirmed deletion after CAP loss.

Tests also cover incoming and own-echo private-message routing, rejecting a
third-party DM target, custom CHANTYPES, ASCII/strict-RFC1459/RFC1459 target
matching, and account-scope lifecycle. Adding/replacing/removing connections
prunes tombstones for scopes no longer represented by a bouncer connection;
reconnecting the same account/network preserves them.

`/tmp/repartee-redaction-lifecycle-check3` passes make clippy then make test with
zero project warnings: 2525 native tests, 143 web host tests, 10 ignored fixtures.
No process remains running from these checks.

The current exact-ID registry still grows with deletions in a long-lived active
scope. Resolve this retention policy without evicting evidence required by
pending deliveries; do not silently restore deleted content to meet a fixed
cache limit. Unread/notification semantics and real Soju/Lurker fixture coverage
also remain required. Default negotiation is still disabled and delivery has
not been reviewed or merged.

Shared redaction lifetime follow-up: aggregate mention rows now retain their
source identity after the source buffer is trimmed. Mention fan-out and replay
lookup consult the shared registry as well as the temporary compatibility map.
Surfaced CHATHISTORY rows acquire the same reference before insertion. Two
regressions remove the compatibility-map entries after 10,000 unrelated
deletions and verify that retained mentions/history still prevent original-body
replay and duplicate mention fan-out. `make clippy` passed without project
warnings, followed by 2531 native and 143 web tests passing (10 native tests
ignored). The toolchain still reports the existing future-incompatibility notice
for the external `proc-macro-error2` dependency. Logs:
`/tmp/repartee-redaction-history-retention2-clippy.log` and
`/tmp/repartee-redaction-history-retention2-test.log`.

Open raw batches still need reference retention before the unbounded
compatibility map can be removed. This increment is not yet reviewed or merged;
capability negotiation remains disabled.

Open-batch retention follow-up: queued PRIVMSG/NOTICE and REDACT entries now
retain scoped identities, including incoming and outgoing private-message
routing. Child batches transfer retained references to their parent on normal
completion and timeout folding. A native App regression closes a nested child,
withdraws the redaction capability, adds 10,000 unrelated deletion entries,
clears the compatibility map, and verifies that parent history completion
renders the deletion notice rather than the original body. Channel case folding
and account isolation are covered by a separate identity test.

Validation: `make clippy` passed without project warnings; `make test` passed
2533 native and 143 web tests, with 10 native tests ignored. Logs are
`/tmp/repartee-redaction-open-batch2-clippy.log` and
`/tmp/repartee-redaction-open-batch2-test.log`. The external dependency's existing
future-incompatibility notice remains.

This is not the final retention policy: multiline opener msgids and exact target
retention during bounded nested folding still need verification before removing
the compatibility map. Negotiation, real-provider acceptance, and clean review
remain outstanding.

Multiline and exact-target follow-up: draft/multiline openers now retain their
msgid before any fragments arrive. A native App regression deletes that identity,
exceeds the recent-deletion cache, then delivers two untagged fragments, closes
the child and parent history batch, and verifies that only the deletion notice
is displayed. Folded and admitted batch references are rebuilt from the actual
retained messages using their account/target identity; matching only a raw msgid
could otherwise retain discarded targets indefinitely. A regression covers the
same opaque ID on retained and discarded channels.

Validation passed: 2535 native tests, 143 web tests, 10 native tests ignored;
Clippy has no project warnings. Logs:
`/tmp/repartee-redaction-exact-retention2-clippy.log` and
`/tmp/repartee-redaction-exact-retention2-test.log`. The pre-existing external
future-incompatibility notice persists. Negotiation remains disabled while the
compatibility-map removal, remaining lifecycle audit, real-provider fixtures,
and review are outstanding.

The unbounded compatibility map has now been removed. Production deletion
lookup uses only shared identities plus the 4096-entry recent-deletion cache;
references owned by displayed rows and pending batch/translation work survive
cache eviction. Scope-isolation and pressure regressions run without manually
clearing any alternate storage. Validation passed with 2535 native and 143 web
tests, 10 native tests ignored, and no project Clippy warnings:
`/tmp/repartee-redaction-bounded-registry2-clippy.log` and
`/tmp/repartee-redaction-bounded-registry2-test.log`.

Full Sol medium review was launched with all three required model overrides;
its output is `/tmp/repartee-redaction-delivery-review1.log`. No clean result is
claimed while that review is running. Negotiation and real-provider acceptance
remain outstanding.

First full Sol medium review completed with two findings: missing automatic
negotiation (P1) and loss of unreferenced deletion knowledge after recent-cache
eviction (P2). Negotiation remains an explicit outstanding gate. For P2, expired
identities now leave an in-memory SHA-256 fingerprint of their length-delimited
target and opaque msgid, grouped by network scope. Later replay is replaced with
`Message deleted` even after detailed attribution has expired. Scope removal
clears these fingerprints. This archive grows with the number of deletions in
active scopes; only the detailed recent cache is bounded. It stores no message
bodies and creates no disk history. A regression covers 10,000 later deletions,
subsequent replay, target isolation and scope cleanup.

The new `--redaction` pinned-provider scenario passed on both Soju and Lurker.
Soju uses an explicit fixture CAP request while production negotiation remains
disabled. The App sends `/redact`, the disposable upstream records its target
and msgid, and the returned event replaces the native row, emits the matching web
redaction event, and produces no local log write. Lurker exercises the unsupported
command path. These checks do not yet establish provider history-redaction
playback or browser rendering against a real provider. Logs:
`/tmp/repartee-redaction-soju1.log` and `/tmp/repartee-redaction-lurker1.log`.

Latest validation after the archive fix: 2536 native tests and 143 web tests
passed, 11 native fixture tests ignored; Clippy has no project warnings. Logs:
`/tmp/repartee-redaction-archived-ids2-clippy.log` and
`/tmp/repartee-redaction-archived-ids2-test.log`. Earlier map-removal checks had six
manual-assert warnings; these were corrected before this run. The external
future-incompatibility notice is unchanged. Full review must be repeated after
negotiation and remaining acceptance gates are completed.


Automatic negotiation now selects redaction during registration and CAP NEW only
for bound, non-control bouncer networks. The real-provider fixture no longer
requests the capability manually. Soju passes live command/response, native and
web event delivery, and CHATHISTORY replay after clearing the client's displayed
rows and deletion registry. No original body or local log write appears during
replay. Lurker still passes its unsupported-operation test. Logs:
`/tmp/repartee-redaction-soju2.log`, `/tmp/repartee-redaction-lurker2.log`.
Native/web suites pass 2537/143 tests with 11 ignored native fixtures; project
Clippy is clean (`/tmp/repartee-redaction-history-provider-check-*`). Full review
round 2 is running in `/tmp/repartee-redaction-delivery-review2.log`.

The Soju fixture can export its actual WebBroadcaster events through
`REPARTEE_REDACTION_WEB_EVENTS`. The latest run saved
`/tmp/repartee-soju-redaction-events.json`; built WASM was exercised in WebKit
using the exact captured NewMessage and RedactMessage payloads. The already
rendered original disappeared, the server-confirmed deletion notice appeared,
and no page error occurred. Screenshot: `/tmp/repartee-redaction-soju-web.png`.
Runner: `/tmp/repartee-browser-qa/redaction-soju.cjs`. This validates browser
rendering of real-provider payloads with replayed transport, not a live
browser-to-App WebSocket session. The temporary HTTP server was stopped.

The export hook and fixture passed Clippy and the 2537-native/143-web suites
(11 ignored native fixtures), logged under
`/tmp/repartee-redaction-browser-events-check-*`. Actual provider replay run:
`/tmp/repartee-redaction-soju3.log`. Full review round 2 is still running.

Full review round 2 found two actionable issues: volatile mention history still
held original text, and read-marker unread state retained Mention activity.
Volatile mention records now own the original scoped identity; confirmed
redaction removes the matching records during App event dispatch, broadcasts a
badge adjustment, and preserves identity on aggregate rows recreated from
mention history. A regression opens the aggregate, deletes the source, and
reopens the aggregate to verify that no original body returns. Unread source
rows are reclassified as Events and ActivityChanged is emitted without falsely
marking the row read. A regression covers that state transition.

After both fixes, Clippy passed without project warnings and the suites passed
2539 native / 143 web tests (11 native fixtures ignored):
`/tmp/repartee-redaction-review2-fixes2-*`. The frontend protocol now includes
MentionsRedacted for badge adjustment and requires a refreshed WASM build.

Actual Soju rejection coverage now sends a deliberately denied deletion through
the App command path. The disposable upstream responds with FAIL REDACT;
Repartee displays that rejection and keeps the original row. A subsequent
accepted request removes the row and CHATHISTORY replay preserves the deletion.
Provider run: `/tmp/repartee-redaction-soju-failure.log`. The unchanged full-suite
counts (2539 native / 143 web, 11 ignored) and clean project Clippy are recorded in
`/tmp/repartee-redaction-provider-failure-check-*`. Round 3 remains running.


Round 3 found two additional cases. Web badge adjustments now carry original
message IDs, and each session records its own mention-read boundary plus alerts
received since that boundary. An older viewed deletion cannot erase a newer
unread badge; a delayed alert with an earlier allocated ID is still tracked.
The regression uses two independent web states and a delayed alert. MentionsList
carries the server message-counter boundary. Already-redacted queued alerts are
suppressed before broadcast and volatile-history insertion.

REDACT wire length is also validated after the actual response label is attached;
an overlong request removes its unused label registration and never reaches the
sender. A regression uses an untagged request within 512 bytes that exceeds the
limit after labeling. These changes still require the next clean review.


Validation after round-3 fixes passed with 2541 native and 144 web tests, 11
native fixtures ignored, and no project Clippy warnings. Logs:
`/tmp/repartee-redaction-review3-fixes3-clippy.log` and
`/tmp/repartee-redaction-review3-fixes3-test.log`. WASM rebuild output is
`/tmp/repartee-redaction-session-badge-wasm.log`.

## Fourth review: retained identity after query rename

The fourth full Sol medium review reported two actionable findings: a retained
confirmed deletion could be bypassed after a query rename, and archived deletion
fingerprints grew without a bound in active scopes.

Retained message identities now remain authoritative across target renames when
the network scope and opaque message ID still match. Registry lookups for messages
without a retained identity remain target-scoped. A regression receives a DM,
retains a deferred copy, receives REDACT, processes the peer's NICK, then delivers
the deferred copy into the renamed query. It verifies the deletion notice and
also verifies that a different account cannot inherit that notice.

Validation: `make clippy` has no project warnings; `make test` passed 2542 native
and 144 web tests (11 native fixture tests ignored). Logs:
`/tmp/repartee-redaction-review4-rename-clippy.log` and
`/tmp/repartee-redaction-review4-rename-test.log`.
The archived-fingerprint memory finding remains open. No merge or clean-review
claim is made for this revision.

## Fourth review: bounded deletion archive

Replaced the unbounded in-memory fingerprint sets with an SQLite temporary
main database (empty filename; removed when closed). The page cache is 256 KiB,
memory mapping is disabled, spilling is enabled, and the database page limit is
64 MiB. Only SHA-256 hashes of scope and length-delimited target/message ID are
stored; message text, actor, reason, and raw identities are never stored there.
The 4096 recent detailed notices and identities retained by live messages remain
in memory. Scope removal deletes its archived fingerprints; removing all scopes
closes the database. This is transient deletion bookkeeping, not message history.

Read/write failures are logged once and make unverified messages unavailable
until all scopes reset; a failure cannot silently restore deleted content.
Tests exercise an archive larger than its page cache, exact old-ID retrieval,
absence of unrelated IDs, fixed-size hashed rows, cleanup, and injected read and
write failures. Weak identity cleanup also runs for archived lookups.

Validation after the temporary archive change: 2545 native and 144 web tests
passed, 11 native fixture tests ignored. Clippy has no project warnings.
Logs: `/tmp/repartee-redaction-archive-sqlite3-clippy.log` and
`/tmp/repartee-redaction-archive-sqlite3-test.log`. Full Sol medium review round 5
is the next acceptance gate.

## Fifth review: native inline preview disposal

Round 5 found retained native inline-image data after confirmed redaction. App
now purges the small native inline-image cache on each RedactMessage event,
including cached protocol encodings and direct placements, and invalidates the
frame so terminal graphics are cleared on redraw. The purge covers all cache
entries because a query may have changed nick since the preview was created.
Unaffected visible previews can be fetched again on the next render.

In-flight request IDs remain counted against the concurrency limit until their
results arrive; without an entry, those results are dropped instead of rebuilding
the deleted preview. A regression exercises App event draining without a web
client, a ready image, an in-flight image, a renamed query, frame invalidation,
and late-result disposal. This avoids losing concurrency accounting while
removing the data-bearing cache entries.

After the native preview fix, make clippy passed with no project warnings and
make test passed 2546 native plus 144 web tests (11 native fixture tests ignored).
Logs: `/tmp/repartee-redaction-native-preview-clippy.log` and
`/tmp/repartee-redaction-native-preview-test.log`. Full review round 6 follows.

## Clean review gate

Full CLI review round 6 completed successfully and reported no actionable
correctness issues in the full diff, including untracked source files. Model
`gpt-5.6-sol`, review model `gpt-5.6-sol`, reasoning effort `medium` were pinned.
Review log: `/tmp/repartee-redaction-delivery-review6.log`.

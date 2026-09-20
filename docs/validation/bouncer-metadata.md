# Bouncer metadata implementation and validation

## Current acceptance result

The metadata increment passes full Sol medium review round 8 with no actionable
findings (`/tmp/repartee-metadata-review8.log`). Final local checks pass with no
project clippy warnings, 2630 native tests and 145 web tests; 17 provider/browser
fixtures are opt-in (`/tmp/repartee-metadata-reviewfix7-{clippy,test}.log`). The
existing proc-macro-error2 future-compatibility warning is dependency-only.
Pinned Soju plus real WebKit passed after automatic negotiation was enabled
(`/tmp/repartee-metadata-browser-final6.log`); pinned Lurker rejects the unsupported
feature. Both providers' invitation fixtures passed again after unread fixes
(`/tmp/repartee-metadata-invites-{soju,lurker}-final7.log`).

This covers metadata subscription/query/update/reconciliation, native/web flags
and ordering, mute/block delivery filtering, visible search counts, and related
activity corrections. Reconnecting a second Soju client proves stored metadata
replay; a bouncer process restart was not exercised by this fixture. Certificates,
WebPush and the full remaining acceptance matrix remain required for the overall
goal. The chronological evidence below records intermediate states superseded by
this result.


## Pinned sources

- Soju: `82e8b7adfb2ab64ec3b88807d29b8b6940236008`,
  `doc/ext/metadata.md`, `downstream.go` metadata command/subscription handlers,
  `upstream.go` source blocking and push handling, `xirc/xirc.go` numerics.
- Lurker: `be42a04e73d6f337e76734684deb457cb5dcdb5f`; this IRC extension is absent.

## Required behavior

The three vendor keys are `soju.im/pinned`, `soju.im/muted`, and
`soju.im/blocked`; values are `0` and `1`. The capability is `draft/metadata-2`.
Soju advertises `before-connect,max-keys=0,max-value-bytes=1`; zero custom keys does
not prohibit these provider-managed keys. LIST/GET use metadata batches and 761;
SUB/UNSUB/SUBS use 770/771/772. SUB can replay updates before its acknowledgement.
SET/CLEAR can produce both a numeric reply and an idempotent METADATA broadcast.
Clearing a flag produces `0`. The SQLite provider returns a default MessageTarget
for a previously unknown target; SET creates its stored metadata. KEY_INVALID
for a nil target in the downstream handler must not be interpreted as proof that
unknown channel names are rejected.

Subscriptions must be scoped by bouncer account/network and re-established after
reconnect or capability restoration. Missing acknowledgements and failures must
not masquerade as success. Changes originating in another client must reach both
native and web state, including a fresh web snapshot. No local message history is
created by metadata operations.

Pinning changes conversation prominence/order consistently in terminal and web.
Muting reduces prominence and suppresses notification/mention fan-out without
losing messages. Blocking hides messages from the source across live messages,
history, search and invitations, including rows deferred for translation. State
updates still run where required; source-blocking must not corrupt membership.
The provider already suppresses blocked messages for clients that do not
subscribe to the blocked key. Once subscribed, it deliberately forwards them so
the client must enforce this behavior itself. Do not enable the subscription
before these delivery paths are covered.

## Work in progress

The protocol parser/request builder and runtime subscription handler are drafted
with tests for all three keys, subscription numerics, delete semantics, malformed
values, source target preservation and command injection. Shared buffer flags,
web snapshots/events, native/web ordering and `/bmeta` commands are connected.
The capability is deliberately not negotiated yet: filtering and notification
semantics must be implemented before the subscription can transfer responsibility
for blocked messages to the client. This is an implementation checkpoint, not
completed support or a merge-ready increment.

Remaining implementation and acceptance gates:

1. Connection/network-scoped cache, subscription lifecycle, command controls,
   idempotent remote updates, failure reporting and reconciliation.
2. Shared native/web flags, snapshots, updates and ordering; suppress muted
   alerts; block source delivery in every path identified above.
3. Real Soju persistence/reconnect and second-client updates, unsupported Lurker,
   actual WebKit presentation and notification behavior; no local history.
4. Clippy, tests, WASM build and full pinned Sol medium review/fix until clean;
   PR and merge. All other `BOUNCER_SUPPORT.md` rows remain in scope.

Protocol regression found during implementation: `irc-proto-repartee` decodes a
three-parameter METADATA deletion as its typed METADATA variant, while ordinary
four-parameter vendor-key updates are Raw commands. Both forms must be consumed;
serializing the typed deletion back to a string loses its trailing arguments.
The parser handles both directly and its deletion regression exercises the real
wire parser.

Checkpoint validation: `make clippy` passed without project warnings and
`make test` passed 2606 native plus 144 web tests (16 provider fixtures ignored).
The runtime/UI work and real-provider metadata acceptance matrix above remain
unfinished; this branch has not been submitted for review or merged.

The shared-state checkpoint covers updates arriving before SUB acknowledgements,
network isolation, cache seeding for a newly opened query, flags in SyncInit,
command gating until all subscriptions are acknowledged, CAP DEL/ACK subscription
recovery, and native/web ordering. Bare server prefixes are supported even though
the IRC library parses server names without a dot as nicknames; userhost-prefixed
metadata updates are rejected. Non-conversation buffers do not acquire metadata
flags when a peer happens to share their display name.

Still required before activation/review: source and buffer filtering across live,
historical, search and deferred delivery; mute notification and mention handling;
cache reconciliation for changed network scope/casemapping; operation reply/error
correlation; real-provider and browser validation; final WASM build.

Latest checkpoint validation: clippy has no project warnings; 2609 native and 145
web tests pass. No review or merge has been performed for this incomplete feature.

Filtering checkpoint: shared delivery gates now suppress blocked message rows and
remove mention highlighting for muted sources/targets, including deferred row
delivery and history surfacing. History ingestion checks blocking after updating
page cursors and before decryption. INVITE and typing ingestion explicitly apply
the metadata policy. A block update removes existing matching rows, queued web
row events and typing state, and removes corresponding read-marker activity IDs.

Two application-level wire regressions cover PRIVMSG, NOTICE, ACTION, INVITE,
typing, removal of existing rows, unrelated senders, unblock, and muted mentions
and invitations. Clippy has no project warnings and tests pass: 2611 native,
145 web, 16 provider fixtures ignored. Logs:
`/tmp/repartee-metadata-filter2-clippy.log` and
`/tmp/repartee-metadata-filter2-test.log`.

This does not prove complete filtering: historical mention reconstruction,
volatile mention drawer cleanup, non-marker unread reconciliation and dedicated
history/deferred-delivery regression scenarios remain outstanding. Metadata
negotiation remains disabled until the full consumer contract is verified.

Mention reconstruction checkpoint: metadata blocking now removes matching volatile
mentions and sends MentionsRedacted to existing web drawers, with a shared
snapshot refresh. Reconstructed native mention rows retain network/target/source
identity so a subsequent block removes them as well. The wire regression checks
two senders, selective removal, the web notification and reopening the native
mentions buffer without resurrecting the blocked sender. Clippy has no project
warnings; 2612 native and 145 web tests pass (16 provider fixtures ignored).
Evidence: `/tmp/repartee-metadata-mentions2-clippy.log` and
`/tmp/repartee-metadata-mentions2-test.log`. Full metadata acceptance remains open.

Late-delivery checkpoint: the activity-bearing shrink/translation delivery path
now applies metadata before attempting local logging. Regressions cover a history
page obtained before blocking but surfaced afterwards, fully blocked ingestion
with preserved oldest/newest pagination cursors, and all three translation queue
delivery kinds (logged, transient, local). Muted translation output stays visible
without highlight or mention fan-out. Clippy has no project warnings; 2614 native
and 145 web tests pass, with 16 ignored provider fixtures. Logs:
`/tmp/repartee-metadata-late1-clippy.log` and
`/tmp/repartee-metadata-late1-test.log`.

Subscription reconciliation checkpoint: each SUB key tracks the replayed targets
until its 770 acknowledgement. A successful acknowledgement clears stale values
for targets omitted from Soju's complete replay; an unsuccessful subscription
keeps the old values rather than treating an incomplete replay as authoritative.
Changing the network scope refreshes existing buffer flags from the matching
cache only. Cached CASEMAPPING is retained separately from transport sessions;
a change invalidates incompatible keys even across reconnect, and resubscribes.
Regressions exercise missing targets, failed replay, scope replacement and
rfc1459-to-ascii nick distinctions across a recreated session.

Clippy has no project warnings; 2616 native and 145 web tests pass (16 provider
fixtures ignored). Evidence: `/tmp/repartee-metadata-sync1-clippy.log` and
`/tmp/repartee-metadata-sync1-test.log`. Operation completion/error correlation,
provider/browser acceptance and review remain outstanding; negotiation is still
disabled.

Operation checkpoint: `/bmeta` serializes operations per connection and follows
each command with a unique PING. The pinned Soju user event loop handles each
downstream command synchronously (`user.go`, eventDownstreamMessage); its PONG
handler responds locally (`downstream.go`, handleMessageRegistered). Consequently
the matching PONG follows SET/CLEAR database persistence and any resulting FAIL,
not merely the earlier 761 and METADATA broadcasts. Mutation completion requires
the expected numeric keys; failure triggers one GET readback and never repeats
the mutation. A failed readback terminates without an automatic retry loop.
Empty listings complete, successful reads clear absent flags, and unrelated PONGs
cannot release the operation. A timeout retains the pending operation and rejects
new operations or sync until its own completion or reconnect.

Application wire regressions cover storage failure after values were broadcast,
readback to the old state, one mutation only, empty batches, unrelated PONG,
concurrent-command rejection and late completion following timeout. Clippy has no
project warnings; 2619 native and 145 web tests pass, with 16 ignored provider
fixtures. Evidence: `/tmp/repartee-metadata-operations4-clippy.log` and
`/tmp/repartee-metadata-operations4-test.log`. Command documentation was rebuilt.
This ordering still requires real-provider acceptance before activation/review.

Pinned-provider checkpoint: `scripts/test_bouncer_presence.py soju SOURCE
--metadata` passes against the audited Soju revision with disposable SQLite data
and a real upstream fixture. It exercises manual capability negotiation,
subscription replay, channel pin/mute/read/clear operations, second-client replay
from database state, a second-client flag update reaching the first client,
metadata for a previously unknown target, and no queued local history writes.
The corresponding Lurker fixture verifies unsupported command handling.
Logs: `/tmp/repartee-metadata-soju5.log` and
`/tmp/repartee-metadata-lurker1.log`.

The initial fixture timeout came from its raw text METADATA command path;
using the same explicit request builder as the production command resolved it.
A later assertion exposed an incorrect source-audit assumption: SQLite
GetMessageTarget returns a default object on sql.ErrNoRows, so a previously
unknown channel can acquire metadata. The audit statement above is corrected.

Full checks: clippy has no project warnings; 2619 native and 145 web tests pass
with 17 opt-in fixtures ignored. Evidence:
`/tmp/repartee-metadata-fixture2-clippy.log` and
`/tmp/repartee-metadata-fixture2-test.log`. Actual browser presentation, complete
filtering/provider scenarios, WASM, automatic negotiation and clean review/merge
are still required. The broader goal matrix remains unchanged.

Activation/browser checkpoint: bound bouncer connections now negotiate
`draft/metadata-2` automatically. The real-provider fixture waits for the actual
subscription instead of enabling the cap itself. A rebuilt WASM frontend passes
real WebKit checks for independent pin/mute/block flags, pinned-query ordering,
removal of blocked existing rows, exclusion of new blocked-source messages while
another source remains visible, state after reload, and clearing flags.
`/tmp/repartee-metadata-browser-enabled1.log` records this automatic-negotiation
run against pinned Soju. No HTTP or WebSocket mocks were used.

Non-marker mention counters now subtract only removed unread rows; a regression
preserves another sender's unread mention and clears activity only after the
last unread mention is blocked. Latest full checks pass 2620 native and 145 web
tests, with 17 opt-in fixtures ignored and no project clippy warnings:
`/tmp/repartee-metadata-enabled1-clippy.log` and
`/tmp/repartee-metadata-enabled1-test.log`. WASM build evidence:
`/tmp/repartee-metadata-wasm1.log`. Full review/fix and merge remain required.

Full Sol medium review round 1 identified two actionable issues: blocking ran
after E2E/flood side effects, and CAP NEW omitted metadata negotiation. Both are
fixed: PRIVMSG/NOTICE apply blocking before message-specific protocol/flood
handling while clearing typing, and runtime capability negotiation includes
metadata only on bound bouncer connections. Regressions cover blocked CTCP flood
isolation and CAP DEL/NEW/ACK recovery with control-connection exclusion.
Clippy has no project warnings; 2622 native and 145 web tests pass (17 opt-in
fixtures ignored). Evidence: `/tmp/repartee-metadata-reviewfix1-clippy.log`,
`/tmp/repartee-metadata-reviewfix1-test.log`, and
`/tmp/repartee-metadata-review1.log`. A subsequent full review is required.

Full Sol medium review round 2 found that invitations already on screen were not
purged when their inviter/target became blocked. Invitation events now carry a
local event kind and scoped source/target origin, so both own invitations and
third-party invite notifications participate in later blocking and deferred
policy checks. A regression verifies selective inviter removal followed by
channel blocking. The literal-percent invitation regression now exercises the
actual native renderer and web wire formatting path because typed events use
literal text instead of pre-escaped theme text.

Clippy has no project warnings; 2623 native and 145 web tests pass (17 fixtures
ignored). Evidence: `/tmp/repartee-metadata-review2.log`,
`/tmp/repartee-metadata-reviewfix2b-clippy.log`, and
`/tmp/repartee-metadata-reviewfix2b-test.log`. Review round 3 follows.

Full Sol medium review round 3 found that server-search summaries counted rows
which metadata later suppressed. Search now filters blocked targets/sources before
decryption and summary/count generation. Regression cases cover mixed visible and
blocked results, plus an entirely blocked response reporting zero visible rows.
Clippy has no project warnings; 2624 native and 145 web tests pass (17 opt-in
fixtures ignored). Evidence: `/tmp/repartee-metadata-review3.log`,
`/tmp/repartee-metadata-reviewfix3-clippy.log`, and
`/tmp/repartee-metadata-reviewfix3-test.log`. Both pinned invitation fixtures pass
after round 2 fixes (`/tmp/repartee-metadata-invites-soju.log` and
`/tmp/repartee-metadata-invites-lurker.log`). Full review round 4 follows.

Full Sol medium review round 4 found that query renames temporarily retained the
old target's flags. Query renaming now refreshes flags immediately, emits the web
metadata event, and removes rows blocked by the destination target's policy.
The refresh is limited to actual query renames. A regression verifies pinned
Alice becoming muted Alicia and then unflagged Carol without a maintenance tick.
The first validation passes 2625 native and 145 web tests with no project clippy
warnings (`/tmp/repartee-metadata-reviewfix4-{clippy,test}.log`); the final guarded
rename path is checked again before round 5. The increment is still unmerged.

Full Sol medium review round 5 found two runtime edges. Blocked own echoes now
consume and abandon matching translation reservations, preventing an ordering
barrier from surviving its suppressed reflection. Metadata subscription pushes
remain active when only BATCH is withdrawn; target operations requiring batches
are rejected, and in-flight operations cannot report a successful incomplete
read after capability loss. Regression tests cover blocked/unblocked remote
pushes without BATCH and blocked own-echo reservation cleanup.

Clippy has no project warnings; 2627 native and 145 web tests pass (17 opt-in
fixtures ignored). Evidence: `/tmp/repartee-metadata-review5.log`,
`/tmp/repartee-metadata-reviewfix5-clippy.log`, and
`/tmp/repartee-metadata-reviewfix5-test.log`. Full review round 6 follows.

Full Sol medium review round 6 found that explicit local log searches could
reveal archived rows from blocked targets/sources, and that removing a highlighted
invitation from a server buffer could leave stale activity above the remaining
muted invitation. Local search now filters results against matching network
metadata before formatting/counting without changing the stored archive. Bouncer
buffers outside server-owned chat history now track delivered unread row levels,
so blocking recomputes activity from surviving rows and sends the corrected web
event. Activating these local buffers still clears their unread state locally.
Regression tests exercise both real command dispatch with SQLite and successive
IRC invitations/metadata updates with the web broadcast receiver. The first test
run exposed a test observing the already-drained event queue; it now observes the
actual broadcast. Final validation and review round 7 follow.

Round 6 fixes pass clippy without project warnings and 2629 native/145 web tests
(`/tmp/repartee-metadata-reviewfix6b-{clippy,test}.log`). Both pinned invitation
fixtures pass again (`/tmp/repartee-metadata-invites-{soju,lurker}-final7.log`).
Round 7 found that the local-search limit was applied before filtering. Storage
search now streams candidates in deterministic newest-first order, accepts only
visible matches until the requested limit, then returns chronological results.
A regression places 23 blocked matches ahead of 22 visible matches and verifies
that the command returns 20 visible matches. The archive remains unchanged.

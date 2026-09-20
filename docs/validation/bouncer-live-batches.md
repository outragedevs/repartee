# Live bouncer batch dispatch validation

Status: merged in https://github.com/outragedevs/repartee/pull/78 after fixing
the round 3 ordering finding and a clean full GPT-5.6 Sol medium review round 4.

## Defect and change

The App handled unbatched IRC messages through its full update path, but generic
batches and reassembled multiline messages bypassed it and called the state
handler directly. A new regression failed on the prior implementation because a
batched own JOIN never requested NAMES (`/tmp/repartee-live-batch-check1-test.log`).
The same bypass skipped WHO/MODE scheduling after a batched 366 and App-level web
broadcast draining.

Extract the existing live update body into `src/app/irc_dispatch.rs` and call it
from both unbatched processing and completed live batches. The expiration path
uses the same dispatcher. CHATHISTORY remains on the history ingestion path;
netsplit/netjoin summaries remain intact. Nested live wrappers and multiline
messages fold into their parent before dispatch. Timeout folds proceed from
children to parents independently of map iteration order. Each captured message
retains a connection-local receive sequence, and folded payloads merge by that
sequence before dispatch. Orphan/cyclic live
batches are discarded. Folding retains the 4096-message limit and dropped count.

## Evidence

`make clippy` then `make test` passed in `/tmp/repartee-live-batch-check14` with
zero project warnings, 2499 native tests, 142 web host tests and 10 ignored
fixtures. Tests cover batched own JOIN/NAMES completion, incomplete live batch
dispatch, generic/multiline single delivery to native/web, no bouncer log writes,
and nested multiline history without live NewMessage notifications.

The pinned Soju fixture now explicitly negotiates labeled-response in test code,
asks for a labeled NAMES reply, observes the labeled batch on the actual socket
and requires both restored nicknames and App WHO scheduling. Production request
labeling is not enabled by this prerequisite. The disposable upstream advertises
the capabilities needed for Soju's local NAMES handling; this is not evidence for
arbitrary upstream labeled-response behavior. Lurker retains its unlabeled fixture.

The actual Soju labeled NAMES test passed in
`/tmp/repartee-live-batch-soju7.log`. The fixture must advertise echo-message with
labeled-response: Soju's `upstream.go:updateCaps` requests both together, and a
mock server that rejects the absent echo-message rejects the entire CAP request.
This fixture correction does not change production capability negotiation.

Lurker fallback passed in `/tmp/repartee-live-batch-lurker4.log`.

The first two full reviews (`/tmp/repartee-live-batch-review1.log` and
`/tmp/repartee-live-batch-review2.log`) found no actionable correctness issues.
A separate parent-agent regression then exposed a nested generic history wrapper
running live JOIN hooks (`/tmp/repartee-live-batch-check9-test.log`). It now passes,
alongside nested expiration in different collection orders, orphan handling and
bounded folding. Review round 3 (`/tmp/repartee-live-batch-review3.log`) found that appending child
payloads after parent traffic could reorder JOIN/NICK/PART and leave a stale nick.
The added regression failed in `/tmp/repartee-live-batch-check12-test.log`.
Receive-order tracking now preserves this sequence on both normal completion and
expiration. Existing manually constructed test batches receive empty ordering
metadata; runtime batches record ordering as each message is accepted. The final
diff passed full review round 4 (`/tmp/repartee-live-batch-review4.log`), which
reported no actionable correctness defects.

## Remaining acceptance

See `bouncer-labeled-responses.md`: general nested batches, request/reply context,
ACK, timeouts, reconnect and capability loss remain required. This prerequisite
must not be reported as full labeled-response support. The entire
`BOUNCER_SUPPORT.md` matrix remains in scope.

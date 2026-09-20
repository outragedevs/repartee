# Bouncer labeled responses and batch integration

Status: implementation and integration verification on
`feat/bouncer-labeled-responses`, following PR 78. Full GPT-5.6 Sol medium review clean.

## Source evidence

Pinned Soju `82e8b7adfb2ab64ec3b88807d29b8b6940236008` exposes
`labeled-response` through its upstream-dependent capability list.
`downstream.go:738` creates a label context only when negotiated. `FlushBatch`
at line 513 emits a labeled single response, a labeled-response batch for several
messages, or an empty labeled ACK. `SendMessage` around line 608 wraps replies,
while preserving existing inner batch tags. Therefore nested batches are a
required case, not an optional generalization.

Pinned Lurker `be42a04e73d6f337e76734684deb457cb5dcdb5f` explicitly lists
labeled-response as not offered in `server/services/bouncer.ts:164`. Keep its
existing unlabelled behavior and do not synthesize server support.

PR 78 repaired App dispatch for generic/multiline batches and nested live wrappers,
including receive-order preservation, expiration and history-context retention.
This increment negotiates labeled-response and correlates manual WHOIS, WHO,
WHOWAS, NAMES and LIST replies with their originating buffer. Labels are random
32-byte ASCII identifiers stored per connection. Pending plus completed context
is bounded to 512 entries and one minute; completed context survives briefly for
payloads buffered inside a parent batch. Missing/evicted/closed-buffer contexts
fall back to the originating server buffer. Transport disconnect and capability
loss clear correlation. Send failures remove their reservation.

Replies temporarily carry a dedicated routing context; they do not select another
active buffer. Server/status and WHOIS reply routing uses this context. Chat
message targets retain their normal protocol routing. Labels on outer batch
openers are inherited by payloads through nested parents. ACK completes the request
without a display row. Labeled bouncer replies are not written to local logs,
including when their origin channel closed and they fall back to a server buffer.

## Validation evidence

`/tmp/repartee-labels-check5`: make clippy then make test passed with zero project
warnings, 2506 native tests, 142 web host tests and 10 ignored fixtures. The web-command
origin regression passed in the same run.
Tests cover nested WHOIS after switching networks, web broadcast destination,
ACK completion, expiration, closed/foreign contexts, CAP loss, reconnect cleanup,
failed send cleanup, bounded context and native selection preservation.

The real pinned Soju fixture passed in `/tmp/repartee-labels-soju1.log`:
production capability negotiation, labeled NAMES, and a forwarded WHOIS whose
upstream response is a labeled batch. The App switches to another network before
the response and requires the original buffer, web event, completed request and
empty local log queue. `/tmp/repartee-labels-lurker1.log` passed implicit NAMES and
reconnect without enabling labels. The disposable upstream implements labeled
WHOIS responses for this test; this does not prove every upstream command variant.

Full review: `/tmp/repartee-labels-review1.log` reported no actionable correctness
defects in the tracked or untracked diff.

## Acceptance and remaining audit

- Route ordinary live batch payloads through the same App update path as live
  unbatched events, retaining special netsplit/netjoin summaries.
- Preserve parent/child ordering and history boundaries, including generic wrappers
  inside history and history/multiline inside labeled-response. Timeouts, malformed
  batches and dropped-message limits must not turn history into live notifications.
- Negotiate labeled-response when offered; bind requests and responses to their
  originating connection and request context, including single responses, ACK,
  multiple replies, errors, timeout, capability loss and reconnect.
- Cover own JOIN/NAMES completion and native/web updates through live batches.
- Test actual pinned Soju label replies with the disposable upstream and retain
  Lurker fallback coverage. Run make clippy before make test, then full pinned
  GPT-5.6 Sol medium review/fix rounds before PR/merge.

All remaining `BOUNCER_SUPPORT.md` rows remain acceptance requirements. This
stage does not complete message-redaction, search, metadata, account operations,
FILEHOST, certificates, WebPush or the final full-support audit.

Protocol references for the next stage: [IRCv3 batch](https://ircv3.net/specs/extensions/batch)
and [labeled responses](https://ircv3.net/specs/extensions/labeled-response).
The label is an opaque value of at most 64 bytes; a response is one logical
message (single reply, batch or ACK). A missing/delayed response after a netsplit
must retain the ordinary unlabeled fallback rather than wedging request state.

Before marking this surface complete, audit reply context through all specialized
batch types, auto-generated requests, overlapping commands and reconnect/capability
transitions against real peers. Unlabeled WHOIS currently retains the older active
buffer fallback; cross-network fallback and its local-log boundary need a separate
verification, especially for Lurker. These gaps remain part of the full goal.

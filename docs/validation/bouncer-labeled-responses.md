# Bouncer labeled responses and batch integration

Status: source audit after PR 77. The prerequisite App dispatch repair is on
`fix/bouncer-live-batch-dispatch`; request labeling and general nested-batch
integration remain incomplete. No new capability is negotiated in production yet.

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

Repartee currently routes unknown completed batches directly through
`irc::events::handle_irc_message` (`src/irc/batch.rs:370`). This bypasses App hooks
for explicit NAMES, WHO/MODE scheduling, MONITOR, presence and read markers.
Only nested multiline batches are folded into their parent in `src/app/irc.rs`.

## Required implementation and evidence

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

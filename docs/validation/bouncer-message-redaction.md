# Bouncer message redaction implementation audit

Status: source audit complete; full implementation and validation pending. The first
prerequisite is on `fix/bouncer-redaction-pagination`. No capability has been
enabled yet.

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

## Confirmed Repartee gaps

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
   original content. Bound retained state without allowing known deleted content
   to reappear silently after eviction.
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

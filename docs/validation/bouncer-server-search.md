# Soju server-side search

Status: source audit completed; isolation prerequisite implemented on
`fix/bouncer-search-batch-isolation`. Search commands and results UI are pending.

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

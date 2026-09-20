# Channel context in live messages and server history

Scope: native event routing and its web event bridge, for finalized
`+channel-context` and legacy `+draft/channel-context`. The finalized tag takes
precedence, including when its value is invalid. This builds on the separately
validated WebPush routing.

Specification: https://ircv3.net/specs/client-tags/channel-context.html

## Routing boundaries

Context is accepted only with negotiated `message-tags`, a user source, a
single private recipient and a valid channel name. Live messages require an
existing channel on the same connection. Public/status-targeted messages,
broadcasts and malformed targets retain their original routing. Server
CASEMAPPING determines channel matching. Historical context must match the
CHATHISTORY batch target; it cannot move a query-history row into a channel.

Context selects display, metadata policy, mentions and web event buffers. The
original IRC target remains authoritative for CTCP dispatch and private-message
E2E decryption. Private-message ignores still apply. The client does not require
current sender membership: services and historical senders may be absent from
current NAMES. Membership filtering is optional in the specification.

## Provider evidence

Pinned Soju: `82e8b7adfb2ab64ec3b88807d29b8b6940236008`.
Pinned Lurker: `be42a04e73d6f337e76734684deb457cb5dcdb5f`.

The disposable upstream advertises message-tags, joins a channel with Alice,
and emits a private NOTICE carrying the legacy context tag. Each real provider
relays it to Repartee and serves it back through `CHATHISTORY LATEST #context`.
The fixture verifies live channel display, web event routing, history recovery,
absence of an Alice query, and absence of local log writes for both stages.
The Soju run also retrieves the NOTICE through channel SEARCH. A separate
regression rejects search rows whose context names another channel.

Soju recognizes the legacy tag for storage when the sender belongs to the
channel. Lurker recognizes it for NOTICE storage in joined channels and may
serialize playback with a channel wire target. The fixture exercises those
actual provider differences. Lurker also has a body-prefix NOTICE heuristic;
that provider-side policy is not treated as a negotiated client tag. Its
upstream outgoing PRIVMSG/NOTICE path does not currently preserve arbitrary
client tags; this change does not claim to alter provider behavior.

Commands (with the audited checkouts):

```sh
python3 scripts/test_bouncer_presence.py soju /tmp/repartee-bouncer-audit.U3fkHg/soju --channel-context
python3 scripts/test_bouncer_presence.py lurker /tmp/repartee-bouncer-audit.U3fkHg/lurker --channel-context
```

Seven focused tests cover both tag names, PRIVMSG/NOTICE, source-prefix forms,
invalid context, public-message isolation, case mapping, history batch scope,
ignores, metadata blocking, network isolation, capability negotiation, local
history ownership and actual private-recipient E2E decryption in live/history
paths. A search regression additionally checks matching and foreign contexts.
Clippy has no project warnings; 2677 native and 145 web tests pass, with 22
provider-dependent native tests ignored in the default run. Review round one
identified the generic binding runner selecting the new dedicated fixture;
the generic runner now excludes it and the dedicated scenario owns its setup.

SAFERATE publication, ICON and the full acceptance matrix remain separate goal
requirements. This increment does not establish complete bouncer coverage.

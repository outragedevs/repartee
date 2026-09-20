# Lurker and Soju support plan

## Completion contract

Support every client-facing feature exposed by the IRC bouncer interfaces of
both pinned implementations. A capability name in CAP REQ is not evidence of
support: its behavior must be connected to the native and web clients and tested.
Unknown extensions must remain forward-compatible. Each implementation PR gets
`gpt-5.6-sol`, `medium` review/fix rounds until clean, then merges to main before
the next PR. Run `make clippy` before `make test`; rebuild WASM for web changes.

This is an implementation backlog, not a statement of completed support.
All acceptance rows below remain pending until linked to tests and merged PRs.

## Reproducible upstream evidence

Audited checkouts under `/tmp/repartee-bouncer-audit.U3fkHg/`:

- Lurker: https://github.com/amiantos/lurker at
  `be42a04e73d6f337e76734684deb457cb5dcdb5f`.
- Soju: https://codeberg.org/emersion/soju at
  `82e8b7adfb2ab64ec3b88807d29b8b6940236008`.
- Repartee baseline: `c612bb390b9d67bba457fc2fe53a67961a7bc844`.

Lurker's `docs/CLIENT_PROTOCOL.md` describes its separate JSON/REST frontend.
The requested IRC bouncer endpoint is implemented in
`server/services/bouncer.ts`, with protocol regression tests beside it.
Soju's authoritative endpoint is `downstream.go`; extension descriptions live in
`doc/ext/`. Audit their implementations as well as the descriptions: for example,
Soju SEARCH emits `soju.im/search` despite an inconsistent unprefixed batch name
in part of its extension document.

## Source-derived differences

| Surface | Lurker | Soju | Required behavior / evidence still needed |
| --- | --- | --- | --- |
| Authentication | PASS and SASL PLAIN; dedicated bouncer credential parsing in `bouncerLogin.ts` | PASS, SASL PLAIN or configured OAUTHBEARER, optionally EXTERNAL; connection/client/network selectors | Credential isolation, selectors, failure handling, TLS and reconnect tests against both |
| Network discovery | `soju.im/bouncer-networks`, notify; batched or unbatched NETWORK | Same extensions | Control connection, initial list, partial attribute updates, attribute removal, deletion, unknown attributes |
| Binding | BIND before registration; requires successful SASL or supplied PASS | BIND before registration requires successful SASL; PASS is verified only at registration | Use SASL success then BIND then CAP END on both. Lurker additionally permits supplied PASS before BIND; Soju PASS uses login/network selectors or an unbound control session. Independent child connections per account/netid |
| Network editing | ADDNETWORK/CHANGENETWORK/DELNETWORK explicitly rejected; web UI manages networks | All three implemented | Soju commands and replies; display Lurker's rejection without claiming a change succeeded |
| Network lifecycle | State/nick/error notifications; retained upstream session on downstream QUIT | State/error notifications and retained upstream sessions | Separate transport and upstream status; no accidental PART/rejoin, duplicated autojoin, or reconnect storms |
| History | CHATHISTORY LATEST/BEFORE/AFTER/AROUND/BETWEEN/TARGETS, event playback, limit 1000 | Same query family; availability depends on message store | Initial channels and DMs, scrollback, reconnect gaps, timestamp-only references on both servers, msgid deduplication, limits, failures, cancellation, offline upstream |
| Automatic replay | Suppressed when CHATHISTORY negotiated | Suppressed when CHATHISTORY negotiated | Explicit initial hydration and target discovery; never rely on automatic replay after requesting the cap |
| Read state | `draft/read-marker`, shared with Lurker web/iOS | `draft/read-marker` and legacy `soju.im/read` | Query/update markers, monotonic timestamps, remote read updates, no playback notifications, native/web parity |
| Presence | `draft/pre-away`, aggregate account away, background `AWAY *` | `draft/pre-away` and aggregate presence | Explicit attached/away behavior; daemon attachment must not imply active user; test multiple clients |
| Message delivery | server-time, tags, echo-message, znc.in/self-message, batch | server-time, echo-message, batch; tags depend on upstream | Own messages from other clients, deduplication, correct private-message routing, event playback, no double echo |
| Dynamic upstream caps | away/account notifications, account-tag, chghost, extended-join, multi-prefix, userhost-in-names, extended-monitor aliases | Same core passthrough except userhost-in-names; adds labeled-response and message-redaction | CAP NEW/DEL correctness, state updates, MONITOR numerics, label routing, redaction behavior |
| Invitations | `invite-notify` always available | `invite-notify` always available | Third-party INVITE routing, channel context and native/web presentation; distinguish invitations addressed to our own nick |
| Account operations | Post-registration AUTHENTICATE rejected locally | Upstream SASL and account-registration conditionally available | Keep bouncer auth separate from upstream auth; supported registration/verification operations and failures |
| Identity and ISUPPORT | Bound network ISUPPORT plus BOUNCER_NETID and FILEHOST | Extended ISUPPORT batches, SETNAME, BOUNCER_NETID, account-required, SAFERATE, ICON | Parse updates/removals and batches, preserve identity, enforce auth requirements, honor server rate semantics |
| Name population | Implicit names supported | no-implicit-names plus two aliases | If negotiated, explicitly request needed names; no empty nicklists; aliases handled consistently |
| Server search | Not implemented in bouncer | `soju.im/search` when store supports history | Structured query and result view, isolation from live unread/history, empty/error/timeout handling |
| Buffer metadata | Not advertised | `draft/metadata-2`: pinned, muted, blocked | Subscribe/query/update and reflect remote changes in native/web ordering, notifications, filtering |
| File upload | `soju.im/FILEHOST` points to `/api/filehost` | FILEHOST points to configured ingress `/uploads` | Unauthenticated OPTIONS then authenticated POST, MIME/size/errors, 201 Location; TLS downgrade and credential redirect protection |
| Client certificate management | Not advertised | `soju.im/client-cert` conditionally available | CREATE/LIST/DELETE, batches and failures; SASL EXTERNAL reconnect after pinning |
| Push notifications | Not exposed over IRC bouncer | `soju.im/webpush`, VAPID, REGISTER/UNREGISTER | Full subscription lifecycle and web delivery; reconnect persistence and failure behavior; permission by user action |
| Bouncer service commands | Normal forwarding where supported | BouncerServ management in `service.go` | Ensure service conversations/commands and replies remain usable without unsafe rewriting |

Capability inventories: Lurker `SUPPORTED_CAPS` and `PASSTHROUGH_CAPS` at
`server/services/bouncer.ts:123`; Soju `permanentDownstreamCaps` and
`passthroughDownstreamCaps` at `downstream.go:250`, conditional history/search/
certificate caps at `downstream.go:430`, dynamic account/event caps at
`downstream.go:1280`. Expand this table if deeper command, ISUPPORT, or integration
audits expose another client-facing feature. Do not drop rows to obtain a clean
completion report.

## History ownership

For an identified bouncer connection with server history, the bouncer is the
authoritative history source. Keep displayed pages and deduplication state in
memory; do not write its messages to Repartee SQLite or text logs. Do not restore
stale local rows as though they came from the bouncer. Keep non-message metadata
needed to reconnect separately, scoped by bouncer account and stable network ID.
Do not delete pre-existing user logs automatically.

Soju does not always advertise CHATHISTORY: its filesystem/database stores do,
whereas other configurations may only replay a limited backlog. Connection to a
bouncer alone therefore does not prove arbitrary history retrieval is available.
Expose that limitation clearly; do not silently enable duplicate local logging or
pretend older messages are retrievable. Ordinary direct IRC retains its existing
history policy. An explicit local-history override, if provided, must be visible
and opt-in.

Explicit bouncer connections use memory-backed history ingestion and pagination
(`src/irc/batch.rs`, `src/app/backlog.rs`, `src/app/server_history.rs`). Direct IRC
keeps SQLite-backed history. Native and web requests avoid implicit local reads;
existing logs remain available through explicit log browsing. Server-side search
and full history discovery remain separate acceptance items.

## Ordered PRs

Each numbered item is a bounded PR target. Split further if its diff becomes too
large to review, retaining the acceptance rows and dependencies.

1. **Protocol state and configuration:** account/netid identity, bouncer mode,
   history ownership policy, network attribute encoding/decoding, isolated unit
   tests. Do not request new caps before their consumers exist.
2. **Registration and explicit binding:** SASL/PASS selector handling, implementation-specific PASS binding rules, BIND
   ordering after SASL success, control-mode registration, reconnect identity, socket tests.
3. **Discovery and connection lifecycle:** network batches/notifications, child
   connection creation and teardown, offline states, duplicate names, two
   accounts with equal IDs, native/web snapshots.
4. **Server-owned history storage boundary:** all write/read gates, memory page
   ingestion and pagination, direct IRC non-regression, no implicit data deletion.
5. **History discovery and hydration:** TARGETS, initial LATEST, bounded lazy
   loading, disconnected upstreams, reconnect gaps and server limits.
6. **Read markers and presence:** native/web read updates, remote marker sync,
   aggregate away/background behavior, multi-client tests.
7. **Soju network management:** user commands/UI, tag escaping, acknowledgments,
   failures, Lurker unsupported-operation handling and documentation.
8. **Remaining IRC passthrough behavior:** dynamic caps, MONITOR aliases, SETNAME,
   labeled replies, redaction, invite-notify, extended ISUPPORT, names and upstream account
   operations. Split by extension if needed.
9. **Soju server-side search:** request correlation, dedicated result handling,
   navigation/history context, native/web parity.
10. **Soju metadata:** pin/mute/block synchronization and presentation.
11. **Bouncer file hosting:** upload discovery, authenticated transport, response
    handling, native/web entry points, actual uploads against both servers.
12. **Soju certificate management:** pin/list/delete and reconnect verification.
13. **Soju Web Push:** subscription lifecycle, web receiver, settings and tests.
14. **Integration and user documentation:** reproducible isolated server fixtures,
    feature-by-feature acceptance evidence, configuration examples for both,
    full native/web gates, clean final review and coverage audit.

## Integration and completion gates

- Run pinned real Lurker and Soju instances using disposable data and credentials
  plus a local IRC test server; never use the user's live account as a fixture.
- Exercise TLS/auth failure, initial discovery, two networks and two bouncer
  accounts, offline upstream, remove/rename/recreate network, reconnect, restart,
  concurrent clients, backlog larger than a page, and identical timestamps.
- Prove bouncer messages do not enter local persistent history, including replay,
  sent messages, daemon restart, web pagination and error paths; direct IRC still
  persists according to its existing configuration.
- Test Soju without history storage as well as with it. Test capability additions
  and removals and unsupported Lurker commands without corrupting UI state.
- Link every table row to its exact regression/integration evidence and merged
  PR. Record limitations as unfinished work, not as successful coverage.
- A clean review or passing unit suite alone does not establish 100% coverage.
  Inspect actual server transcripts, UI behavior and persistence for the feature
  being claimed. Finish only when no required row is pending or unverified.


## Implementation progress

- Explicit SASL-authenticated network binding: `bouncer_network_id` configuration,
  `/server add -bouncer-network=ID`, `/set`, reconnect-preserved configuration,
  suppressed configured autojoin, and verified BOUNCER_NETID before connection
  success. Socket regressions cover authentication failure, missing/NAK capability,
  invalid binding, mismatched/missing network confirmation, disconnect, and direct
  IRC autojoin. Merged in https://github.com/outragedevs/repartee/pull/62 .
- Control connections and discovery: opt-in `bouncer_control`, `/bouncer list`
  and `refresh`, atomic batched snapshots, partial notifications, attribute
  removal and network deletion. Initial discovery and explicit binding pass
  against both pinned upstream fixtures. Scripted tests cover abandoned and
  malformed snapshots and control sessions accidentally bound by their login.
  These complete part of stages 1–2; automatic child-connection lifecycle,
  PASS-specific binding, history ownership and the remaining matrix are pending.

- Registration cancellation now aborts the outgoing task when the connection
  future is dropped, including a pending socket write. A synthetic backpressure
  regression fails without the abort and passes with it; ordinary cancellation
  tests cover capability negotiation and network confirmation. This is a
  prerequisite for deleting a discovered network while it is still connecting,
  not yet the automatic child lifecycle itself.

- Bouncer network identity is separate from display names. Bound/control scopes
  isolate configured account entries, endpoints, login selectors and numeric
  network IDs. Storage reads/writes, native/web backlog, search and E2E paths
  use the scope. Renaming a display label preserves it; legacy E2E rows are not
  automatically borrowed by bouncer scopes. Regression tests cover equal names,
  equal network IDs on two accounts, rename, password rotation and legacy-key
  isolation. Automatic child lifecycle and server-owned history remain pending.

- Connection attempts now carry a generation through registration and all
  forwarded events. Cancelling or replacing an attempt discards its queued
  events; dropping a handle aborts both reader and writer tasks. Disconnecting
  during registration cancels the pending attempt. Tests cover stale handles,
  stale connection events, pending-task cancellation and an idle registered
  socket closing without further server traffic. Automatic discovery-driven
  child creation and teardown still need to be wired to this infrastructure.

- Control discovery now creates a bound child connection per account/network ID.
  Committed snapshots and partial updates reconcile creation, rename and removal;
  parent disconnect suspends children and a fresh list resumes them. Manually
  disconnected children require `/bouncer connect ID`. Data scopes remain based on
  the parent account and netid. Tests cover two accounts with identical IDs,
  rename without reconnect, deletion with stale events, parent reconnect and
  manual disconnect; web tests cover renamed buffer caches and connection
  removal. The pinned Soju and Lurker fixtures both establish an automatically
  generated child through the real App event loop. Server-owned history and the
  remaining extension matrix are still pending.

- Explicit bouncer channel/query buffers now skip the chat log writer. DCC
  conversations retain local storage because their history is peer-to-peer. BEFORE pages surface
  in memory; collapsing a backlog resets its server pagination watermark before
  future trimming can discard rows. Native seeding and reconnect anchors avoid
  SQLite. Web requests use memory cursors and wait for server pages; mention text
  remains volatile with seven-day retention. Both pinned upstream fixtures fetch
  300 ordered messages through an automatically generated child in LATEST/BEFORE
  pages without queuing local log rows. Synthetic tests also cover web completion,
  timeouts, equal timestamps and existing local rows remaining intact but unused.
  TARGETS discovery, bounded hydration and server search remain pending.

- History discovery now requests TARGETS after MOTD/ISUPPORT and creates query and
  channel buffers in the background. Initial and reconnect hydration share a
  queue, with per-connection and aggregate request limits and bounded retries.
  Both pinned fixtures discover an unknown query and channel and load their
  stored history; the Soju upstream is disabled. This does not yet establish
  complete enumeration across large timestamp ties or targets filtered after
  LIMIT; these remain acceptance gaps, alongside the remaining extension matrix.

Read markers merged in https://github.com/outragedevs/repartee/pull/70 after
a clean gpt-5.6-sol medium review: native and web read actions resolve displayed
message IDs to server timestamps; background terminal focus and history scrolling
do not advance markers. Pending updates retry and are isolated by network scope.
The pinned integration fixture now checks server persistence and notification of
a second client for `draft/read-marker`. Legacy Soju `READ` has isolated protocol
regressions. Presence implementation followed in PR 71.


Presence implementation merged in https://github.com/outragedevs/repartee/pull/71
following a clean second gpt-5.6-sol medium review. Actual App/two-client upstream
fixtures pass for Lurker and Soju with AutoAway enabled and disabled. Validation
and the remaining browser focus-delivery gap are recorded in
`docs/validation/bouncer-presence.md`; this does not close the full matrix.

Soju network management merged in https://github.com/outragedevs/repartee/pull/72
after a clean second Sol medium review. Add/change/delete operations, Lurker
rejection, escaped attributes, credential references, timeout handling and web
snapshot updates have unit and pinned-server evidence in
`docs/validation/bouncer-network-management.md`.

Stage 8 SETNAME merged in https://github.com/outragedevs/repartee/pull/73 after
a clean second Sol medium review. Own/peer real-name updates, initial WHO/WHOX,
incremental JOIN and web tooltips are covered in `docs/validation/bouncer-setname.md`.

MONITOR and extended-monitor aliases merged in
https://github.com/outragedevs/repartee/pull/74 after a clean third Sol medium
review. Per-client isolation, notifications, rejection and reconnect have pinned
bouncer evidence in `docs/validation/bouncer-monitor.md`. Compiled WASM command
routing and event rendering were checked separately with controlled WebSocket data.

The MONITOR capability-transition follow-up merged in
https://github.com/outragedevs/repartee/pull/75 after a clean second Sol medium
review. Real-server evidence covers restored availability, independent metadata
notifications after CAP ACK, and initially unsupported upstreams. Lurker's hidden
dynamic ISUPPORT withdrawal remains explicitly documented.

Invitations merged in https://github.com/outragedevs/repartee/pull/76 after two
clean Sol medium reviews. Connection-scoped routing, local-history exclusion,
case mapping, native/web notification payloads and rendered real-server events
are documented in `docs/validation/bouncer-invitations.md`.

The `feat/explicit-bouncer-names` increment implements Soju's three
no-implicit-names aliases and explicitly populates nicklists when negotiated,
while retaining Lurker's implicit NAMES behavior. Unit tests, two-channel reconnect
fixtures against both pinned bouncers, and a clean Sol medium review passed; see
`docs/validation/bouncer-names.md`.

Explicit NAMES merged in https://github.com/outragedevs/repartee/pull/77.
Live batch dispatch merged in https://github.com/outragedevs/repartee/pull/78,
including nested history protection, receive-order preservation, bounded folding
and expiration. See `docs/validation/bouncer-live-batches.md`.

Labeled responses merged in https://github.com/outragedevs/repartee/pull/79.
Manual WHOIS/WHO/WHOWAS/NAMES/LIST replies correlate with their originating buffer.
Native/web routing, ACK, cleanup and a real forwarded Soju WHOIS passed, followed
by a clean full Sol medium review. See `docs/validation/bouncer-labeled-responses.md`.

The `fix/bouncer-unlabeled-reply-routing` increment confines unlabeled replies to
the originating connection and prevents automatic local writes for bouncer server
buffers. Direct IRC and DCC retain local logging. Both pinned bouncers passed a
forwarded WHOIS after switching networks; native/web destinations, unit tests and
a clean full Sol medium review are recorded in
`docs/validation/bouncer-unlabeled-replies.md`.

Next, implement message redaction and audit remaining specialized-batch/request
variants and older server-buffer backlog reads. Remaining increments must still
cover other dynamic capability behavior, ISUPPORT and upstream account operations,
as well as every other acceptance row above; no remaining row is dropped.

Unlabeled routing and logging merged in
https://github.com/outragedevs/repartee/pull/80. Source audit for the next redaction
increment is in `docs/validation/bouncer-message-redaction.md`; implementation,
real-server validation and review remain pending.

The `fix/bouncer-redaction-pagination` prerequisite excludes deletion metadata
from CHATHISTORY page counts and anchors. Regression tests reproduced the old
anchor error; make clippy/test and a clean full Sol medium review passed. Full
redaction delivery, commands and native/web behavior remain unimplemented.

Pagination prerequisite merged in https://github.com/outragedevs/repartee/pull/81.
Redaction delivery is in progress on `feat/bouncer-redaction-delivery`.
Selective bouncer capability negotiation, native/web command and event paths,
derived-message retention, and actual Soju live deletion/history replay now pass
validation. Pinned Lurker correctly rejects the unsupported feature. Full review
rounds 1–5 returned findings, all fixed; round 6 is clean and local validation passes; see the redaction audit for the remaining acceptance gates.

Confirmed message redaction merged in
https://github.com/outragedevs/repartee/pull/82 after clean full Sol medium review
round 6. FILEHOST is now implemented on `feat/bouncer-filehost`: native command,
authenticated browser upload route, browser chooser, and actual pinned-provider
upload/download tests pass. See `docs/validation/bouncer-filehost.md` for evidence
and the remaining review/integration boundaries. All other requirements remain
in scope.

FILEHOST merged in https://github.com/outragedevs/repartee/pull/83 after clean
Sol medium review round 3 and complete browser-to-provider tests for both pinned
bouncers. Follow-up `fix/redaction-live-isupport` corrects redaction's legacy
ISUPPORT reads, with a reproduced failure via actual 005 and passing regressions.
See `docs/validation/redaction-live-isupport.md`; review remains required.

Explicit OAUTHBEARER is implemented on `feat/bouncer-oauthbearer` with verified
TLS, native/web configuration, and a pinned Soju OAuth-provider fixture covering
network binding, reconnect and credential rejection. Full Sol medium reviews 1
and 2 are clean; 2567 native and 144 web tests passed, and WASM was rebuilt.
See `docs/validation/bouncer-oauthbearer.md`. Remaining acceptance rows, including
upstream account operations, remain in scope.

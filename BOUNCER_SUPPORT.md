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
| Authentication | PASS and SASL PLAIN; dedicated bouncer credential parsing in `bouncerLogin.ts` | PLAIN or configured OAUTHBEARER, optionally EXTERNAL; connection/client/network selectors | Credential isolation, selectors, failure handling, TLS and reconnect tests against both |
| Network discovery | `soju.im/bouncer-networks`, notify; batched or unbatched NETWORK | Same extensions | Control connection, initial list, partial attribute updates, attribute removal, deletion, unknown attributes |
| Binding | BIND before registration; requires successful SASL or supplied PASS | BIND before registration, authenticated account | Bind after SASL success, or after supplying PASS (verified at CAP END), always before CAP END; independent child connections per account/netid |
| Network editing | ADDNETWORK/CHANGENETWORK/DELNETWORK explicitly rejected; web UI manages networks | All three implemented | Soju commands and replies; display Lurker's rejection without claiming a change succeeded |
| Network lifecycle | State/nick/error notifications; retained upstream session on downstream QUIT | State/error notifications and retained upstream sessions | Separate transport and upstream status; no accidental PART/rejoin, duplicated autojoin, or reconnect storms |
| History | CHATHISTORY LATEST/BEFORE/AFTER/AROUND/BETWEEN/TARGETS, event playback, limit 1000 | Same query family; availability depends on message store | Initial channels and DMs, scrollback, reconnect gaps, timestamp-only references on both servers, msgid deduplication, limits, failures, cancellation, offline upstream |
| Automatic replay | Suppressed when CHATHISTORY negotiated | Suppressed when CHATHISTORY negotiated | Explicit initial hydration and target discovery; never rely on automatic replay after requesting the cap |
| Read state | `draft/read-marker`, shared with Lurker web/iOS | `draft/read-marker` and legacy `soju.im/read` | Query/update markers, monotonic timestamps, remote read updates, no playback notifications, native/web parity |
| Presence | `draft/pre-away`, aggregate account away, background `AWAY *` | `draft/pre-away` and aggregate presence | Explicit attached/away behavior; daemon attachment must not imply active user; test multiple clients |
| Message delivery | server-time, tags, echo-message, znc.in/self-message, batch | server-time, echo-message, batch; tags depend on upstream | Own messages from other clients, deduplication, correct private-message routing, event playback, no double echo |
| Dynamic upstream caps | away/account notifications, account-tag, chghost, extended-join, multi-prefix, userhost-in-names, extended-monitor aliases | Same core passthrough except userhost-in-names; adds labeled-response and message-redaction | CAP NEW/DEL correctness, state updates, MONITOR numerics, label routing, redaction behavior |
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

Current Repartee history ingestion/pagination is tied to SQLite
(`src/irc/batch.rs`, `src/app/backlog.rs`); removing persistence without replacing
that path would break scrollback. Native, web, reconnect, search, exports, session
restore, and logging paths all need review for the ownership rule.

## Ordered PRs

Each numbered item is a bounded PR target. Split further if its diff becomes too
large to review, retaining the acceptance rows and dependencies.

1. **Protocol state and configuration:** account/netid identity, bouncer mode,
   history ownership policy, network attribute encoding/decoding, isolated unit
   tests. Do not request new caps before their consumers exist.
2. **Registration and explicit binding:** SASL/PASS selector handling (PASS need only be supplied before BIND), BIND
   ordering, control-mode registration, reconnect identity, socket tests.
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
   labeled replies, redaction, extended ISUPPORT, names and upstream account
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

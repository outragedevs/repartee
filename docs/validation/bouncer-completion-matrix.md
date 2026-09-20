# Bouncer completion matrix

Audit date: 2026-09-20. Source baseline:
`e89418d9ba39285a791b40b6e1fef88a7bda8c57` (merged PR 124).
Provider revisions remain the two exact commits in
[BOUNCER_SUPPORT.md](../../BOUNCER_SUPPORT.md#reproducible-upstream-evidence).

This matrix preserves every original surface and adds channel-context behavior
found during implementation. It is a source and evidence audit, not a claim that
the complete goal has passed. A merged PR or a requested capability does not close
a row. The evidence column identifies implemented behavior and its reproducible
checks; the final column names remaining cross-cutting gates where applicable.
Earlier validation documents contain chronological development notes: their
intermediate “pending” statements do not override their later results or this
matrix. Historical provider/browser runs are recorded evidence, not fresh reruns
performed by this documentation audit.

## Capability and source reconciliation

The audited inventories are Lurker's `SUPPORTED_CAPS` and `PASSTHROUGH_CAPS` in
`server/services/bouncer.ts`, and Soju's `permanentDownstreamCaps`,
`passthroughDownstreamCaps`, conditional history/search/certificate capabilities,
and dynamic account-registration/event-playback handling in `downstream.go`.
The client negotiation lives in `src/irc/cap.rs` and `src/irc/mod.rs`; consumers
are listed below.

Lurker's `znc.in/self-message` is not a missing implementation merely because it
is absent from DESIRED_CAPS: `wantsSelfMessages()` accepts either that capability
or `echo-message`, which Repartee requests. Cross-client delivery still needs the
combined real-client scenario in G5. Conversely, Soju's
`soju.im/account-required` is informational and deliberately must not be requested.
Tests in `src/irc/account_required_tests.rs` enforce that distinction.

ISUPPORT retains unknown tokens in `src/irc/isupport.rs`; recognized consumers
include case mapping, prefixes/channel modes, target limits, CLIENTTAGDENY,
network identity and ICON. Retaining a token does not prove its behavioral
contract. The advertised `soju.im/SAFERATE` now controls transport flood behavior
through the published `irc-repartee` 1.5.2 API; G1 records the release verification.

## All feature surfaces

Paths below are relative to the repository root. Fixture names are exact Rust
test filters unless a script is named. Linked validation records contain their
commands, provider differences and detailed limits.

| Surface | Implementation and reproducible evidence | Merged changes | Remaining gate |
| --- | --- | --- | --- |
| Authentication | `src/irc/mod.rs`; `src/irc/bouncer/tests.rs::pinned_bouncer_registration` verifies PLAIN binding/reconnect on both providers. `src/app/oauthbearer_fixture.rs` checks Soju token/account rejection and reconnect; `src/app/bouncer_certificates_fixture.rs` checks EXTERNAL and revoked-certificate rejection. [Binding](bouncer-binding.md), [PASS](bouncer-pass-authentication.md), [TLS](bouncer-tls-validation.md), [OAuth](bouncer-oauthbearer.md), [certificates](bouncer-client-certificates.md). | [62](https://github.com/outragedevs/repartee/pull/62), [85](https://github.com/outragedevs/repartee/pull/85), [91](https://github.com/outragedevs/repartee/pull/91), [95](https://github.com/outragedevs/repartee/pull/95) | G9 |
| Network discovery | `src/irc/bouncer/networks.rs` tests atomic snapshots, partial/removal updates and malformed/abandoned batches. `pinned_bouncer_discovery` and `pinned_bouncer_generated_children` exercise actual provider lists. [Binding/discovery](bouncer-binding.md). | [63](https://github.com/outragedevs/repartee/pull/63), [65](https://github.com/outragedevs/repartee/pull/65), [67](https://github.com/outragedevs/repartee/pull/67) | G9 |
| Binding | `src/irc/bouncer.rs`, `src/irc/mod.rs`: SASL success precedes BIND; identity confirmation precedes Connected. Socket tests reject missing/wrong IDs, premature EOF and cancellation; real tests reconnect. | [62](https://github.com/outragedevs/repartee/pull/62), [64](https://github.com/outragedevs/repartee/pull/64), [66](https://github.com/outragedevs/repartee/pull/66) | G9 |
| Network editing | `src/app/bouncer_management.rs::pinned_bouncer_network_management` drives Soju add/change/delete and web snapshots, Lurker rejection of all three. Unit tests cover scoping, failure/timeout and ambiguous late replies. [Management](bouncer-network-management.md). | [72](https://github.com/outragedevs/repartee/pull/72) | G9 |
| Network lifecycle | `src/app/bouncer_children.rs` covers rename without reconnect, manual close, parent reconnect, stale child teardown and focus. Actual daemon transport recovery now runs on both providers. [Daemon](bouncer-daemon-history.md). | [65](https://github.com/outragedevs/repartee/pull/65)–[67](https://github.com/outragedevs/repartee/pull/67), [107](https://github.com/outragedevs/repartee/pull/107) | G9 |
| History | `src/app/history_discovery.rs`, `src/app/server_history.rs`, `src/irc/chathistory.rs`, `src/irc/batch.rs`; real generated-child fixture hydrates 200+100 query rows and channels. Disk-backed App and real daemon fixtures exclude local history, preserve legacy rows, paginate and recover after client restart. [History audit](bouncer-history-acceptance.md), [daemon](bouncer-daemon-history.md). | [68](https://github.com/outragedevs/repartee/pull/68), [69](https://github.com/outragedevs/repartee/pull/69), [100](https://github.com/outragedevs/repartee/pull/100)–[107](https://github.com/outragedevs/repartee/pull/107) | G3 |
| Automatic replay | Explicit TARGETS/LATEST hydration when CHATHISTORY is negotiated. Soju memory-store scenario verifies the no-cap warning and actual missed-message replay between daemon processes. [Memory history](bouncer-memory-history.md). | [69](https://github.com/outragedevs/repartee/pull/69), [104](https://github.com/outragedevs/repartee/pull/104), [105](https://github.com/outragedevs/repartee/pull/105) | G3 |
| Read state | `src/app/read_markers.rs`; generated-child fixture checks precise stored markers and a second real client's update. Native focus/tail and browser displayed-ID regressions cover partial reads, legacy READ, scope, failure/retry and playback isolation. [Read validation](bouncer-binding.md#read-marker-validation). | [70](https://github.com/outragedevs/repartee/pull/70) | G6 |
| Presence | `src/app/presence.rs`, `src/app/presence_fixture.rs::pinned_bouncer_presence`: two actual Apps, preregistration absence, manual away, reconnect, Soju AutoAway on/off and Lurker account-wide semantics. Actual headed browser focus/visibility transitions and upstream AWAY are verified on both providers; native shim handling is covered separately below. [Presence](bouncer-presence.md). | [71](https://github.com/outragedevs/repartee/pull/71) | G6 |
| Message delivery | `src/irc/events.rs`, `src/irc/batch.rs`: own-message routing, activity and batch handling. Real upstream/daemon/browser tests check incoming/outgoing delivery and no duplicate visible rows through restart/reconnect. [Live batches](bouncer-live-batches.md), [daemon](bouncer-daemon-history.md). | [78](https://github.com/outragedevs/repartee/pull/78), [101](https://github.com/outragedevs/repartee/pull/101), [105](https://github.com/outragedevs/repartee/pull/105), [107](https://github.com/outragedevs/repartee/pull/107) | G9 |
| Dynamic upstream capabilities | CAP state in `src/irc/cap.rs`, `src/irc/events.rs`; `src/app/monitor.rs`, `src/app/names_fixture.rs`, `src/app/redaction_fixture.rs`. Real fixtures and regressions cover MONITOR aliases/removal, metadata changes, labeled/unlabeled replies and redaction delivery/pagination. [MONITOR](bouncer-monitor.md), [labels](bouncer-labeled-responses.md), [redaction](bouncer-message-redaction.md). | [74](https://github.com/outragedevs/repartee/pull/74), [75](https://github.com/outragedevs/repartee/pull/75), [79](https://github.com/outragedevs/repartee/pull/79)–[82](https://github.com/outragedevs/repartee/pull/82), [84](https://github.com/outragedevs/repartee/pull/84), [89](https://github.com/outragedevs/repartee/pull/89) | G9 |
| Invitations | `src/app/invite_fixture.rs::pinned_bouncer_invites` and event tests check own/third-party routing and isolation. [Invitations](bouncer-invitations.md). | [76](https://github.com/outragedevs/repartee/pull/76) | G9 |
| Account operations | `src/app/upstream_auth_fixture.rs`, `src/app/account_registration_fixture.rs`: actual upstream SASL, saved credentials, failures, registration/verification, and unsupported Lurker behavior. [Upstream SASL](bouncer-upstream-sasl.md), [registration](bouncer-account-registration.md). | [86](https://github.com/outragedevs/repartee/pull/86), [87](https://github.com/outragedevs/repartee/pull/87) | G9 |
| Identity and ISUPPORT | `src/app/isupport_tests.rs`, `src/irc/account_required_tests.rs`, `src/app/network_icon_fixture.rs`; real SETNAME, extended-ISUPPORT and ICON fixtures plus browser icon proxy tests. [ISUPPORT](bouncer-extended-isupport.md), [account-required](bouncer-account-required.md), [ICON](bouncer-network-icon.md). SAFERATE uses the published runtime transport API; see [verification](bouncer-saferate.md). | [73](https://github.com/outragedevs/repartee/pull/73), [94](https://github.com/outragedevs/repartee/pull/94), [95](https://github.com/outragedevs/repartee/pull/95), [98](https://github.com/outragedevs/repartee/pull/98), [99](https://github.com/outragedevs/repartee/pull/99) | G9 |
| Name population | `src/irc/names.rs`, `src/app/names_fixture.rs::pinned_bouncer_names`: explicit NAMES when required, normal implicit names otherwise, labeled and unlabeled refresh. [NAMES](bouncer-names.md). | [77](https://github.com/outragedevs/repartee/pull/77), [79](https://github.com/outragedevs/repartee/pull/79), [80](https://github.com/outragedevs/repartee/pull/80) | G9 |
| Server search | `src/app/server_search.rs`, `src/app/server_search_fixture.rs`, shared daemon browser: selectors, separate result/context view, empty/reload/close, no live-copy or disk/diagnostic writes. Lurker and Soju memory store reject unsupported search. [Search](bouncer-server-search.md). | [88](https://github.com/outragedevs/repartee/pull/88), [89](https://github.com/outragedevs/repartee/pull/89), [106](https://github.com/outragedevs/repartee/pull/106) | G9 |
| Buffer metadata | `src/app/bouncer_metadata*.rs`: query/subscribe/update, real second-client changes, native/web pin/mute/block behavior, Lurker refusal. [Metadata](bouncer-metadata.md). | [90](https://github.com/outragedevs/repartee/pull/90) | G9 |
| File upload | `src/app/filehost_fixture.rs::pinned_bouncer_filehost`, backend/client upload checks and actual browser picker/provider runs. OPTIONS/POST, returned URL and transport protections are recorded in [FILEHOST](bouncer-filehost.md). | [83](https://github.com/outragedevs/repartee/pull/83) | G9 |
| Client certificates | `src/app/bouncer_certificates_fixture.rs`: real create/list/delete, EXTERNAL reconnect, revoked-cert rejection and missing-cert failure; Lurker unsupported path. Browser command test is recorded. [Certificates](bouncer-client-certificates.md). | [91](https://github.com/outragedevs/repartee/pull/91) | G9 |
| Push notifications | `src/app/bouncer_webpush*.rs`, `scripts/test_bouncer_webpush.py`, browser/service-worker regressions: real subscription, encrypted delivery, route/read dismissal, reload/disable, retry and cleanup. [Protocol](bouncer-webpush.md), [browser final evidence](bouncer-webpush-browser.md#final-review). | [92](https://github.com/outragedevs/repartee/pull/92), [93](https://github.com/outragedevs/repartee/pull/93), [96](https://github.com/outragedevs/repartee/pull/96) | G9 |
| Bouncer service commands | `pinned_bouncer_service` exercises actual Soju mutations/errors/empty history and Lurker forwarding/rejection through native commands and the compiled web UI, on control and bound connections. [Service acceptance](bouncer-service-commands.md). | Dedicated service acceptance fixture | G9 |
| Channel context (additional discovered surface) | `src/irc/channel_context.rs`, `src/app/channel_context_fixture.rs`: current and legacy tags, private/live/history/search routing; push scope and read routing. [Channel context](bouncer-channel-context.md). | [96](https://github.com/outragedevs/repartee/pull/96), [97](https://github.com/outragedevs/repartee/pull/97) | G9 |

## Finite remaining gates

Open rows are required work, not waived limitations. G1, G2, G4, G5, G7 and G8 have
recorded acceptance evidence; G3, G6 and G9 remain open. “No evidence found” means
completion is unproven, not that a production bug has been established.

| Gate | Current evidence and next closure condition |
| --- | --- |
| G1 — SAFERATE and distributable dependency | SAFERATE uses the published `irc-repartee` 1.5.2 registry API. Real Soju and Lurker runs verify provider-dependent flood behavior; socket checks cover removal and connection isolation. Cargo package verification and installation of the normalized package source into a disposable prefix succeeded without Git/path patches or vendor code. See [SAFERATE evidence](bouncer-saferate.md). |
| G2 — authentication paths and TLS rejection | Explicit PASS-before-BIND now works with Lurker, with exact registration confirmation and disk-backed history exclusion. Actual-provider fixtures verify PASS control mode on both, Soju BIND refusal, missing/invalid credentials and invalid PLAIN on both. See [PASS evidence](bouncer-pass-authentication.md). Repartee detects legacy USER/PASS network selectors on the actual connection, confirms runtime identity, suppresses autojoin and excludes conversation history from SQLite; both pinned providers pass the persistent-history/reconnect fixture. See [implicit registration](bouncer-implicit-registration.md). The published registry dependency is integrated. PASS control children use USER selectors with strict network ID confirmation. WebPush accepts confirmed PASS-authenticated bound sessions; CLIENTCERT correctly requires SASL under the provider specification. See [WebPush authentication](bouncer-webpush.md#pass-authentication). Confirmed Soju scopes exclude colon-bearing PASS secrets; Lurker combined PASS credentials are resolved for FILEHOST using its registration identity. See [provider-specific credentials](bouncer-pass-authentication.md#provider-specific-pass-credentials). Actual TLS cases on both endpoints now verify a successful trusted connection, UnknownIssuer rejection and wrong-host rejection, with no successful App/web connection state on failure; [TLS acceptance](bouncer-tls-validation.md). |
| G3 — complete target discovery | Actual tests prove a 1001-way timestamp tie omits one name, show a warning, and recover by a known name. They do not discover that unknown name. The [filtered-empty runtime probe](bouncer-history-acceptance.md#filtered-empty-targets-pages-verified-on-both-providers) also reproduces empty first pages hiding an older visible conversation on both providers. No continuation cursor is returned: providers apply LIMIT before visibility filtering. Upstream filtering-before-limit and a timestamp tie-break cursor are required for complete enumeration; the decision whether to extend this client task to upstream changes remains pending. A warning alone is not full enumeration. |
| G4 — full history query family | `/bsearch between` now provides tracked two-bound requests through native and web command handlers, with clamped limits and shared cancellation/quarantine. The [bounded history acceptance](bouncer-history-ranges.md) verifies both providers' wire semantics and actual App results with SQLite exclusion. Actual WebKit covers both directions and reload. The [partial history fixture](bouncer-partial-history.md) covers disconnect after the first actual range row and clean retry. Stalled range timeout and explicit cancellation now release and discard actual late bytes before a clean retry. Real 60/90-second expiry variants verify late orphan discard and reconnect-required recovery. The [account lifecycle scenario](bouncer-account-lifecycle.md) verifies concurrent BEFORE/BETWEEN requests across four networks and two accounts with exact result isolation. |
| G5 — combined account/network/client lifecycle | The [account lifecycle scenario](bouncer-account-lifecycle.md) verifies two accounts and four networks with repeated labels/targets, concurrent history isolation, an independent read-marker observer, one-account transport reconnect, provider-side rename/delete/recreate and an actual provider process restart. It also exposed and fixed logging of status rows in special search views. Actual IDs are globally distinct on these providers; overlap is covered separately by scope tests. The extended scenario now activates four real TCP upstreams, verifies manual away scope and Soju metadata isolation, stops/restarts an upstream, and verifies fresh isolated traffic after provider restart. The upstream wire log contains no unsolicited JOIN. Native state and the compiled browser preserve four identical query names and their isolated contents after provider/browser reload. The browser check exposed and fixed own-echo query display names. Both pinned providers pass; OS terminal focus emission remains in G6. |
| G6 — actual focus/visibility transitions | The [real browser acceptance](bouncer-presence.md#real-browser-focus-and-visibility-acceptance) now verifies actual headed Chromium foreground/background/hidden/closed-tab transitions, two independent browser clients, Presence frames and upstream AWAY through both providers. Playwright focus emulation is disabled by attaching with no default overrides. The [native shim scenario](bouncer-presence.md#native-shim-acceptance) exercises actual PTY attach/detach and injected CSI focus reports, and exposed the corrected legacy detach chord. It does not prove OS-generated terminal focus events; GUI access to Ghostty was rejected by the computer-use tool. |
| G7 — service conversation contract | Covered by `pinned_bouncer_service` and actual WebKit runs on both pinned providers: quoted/percent/backslash payloads, Soju database-verified mutations and failures, bound/current versus control routing, successful empty service history preserving live rows, browser reload and local-log exclusion. Lurker forwards exactly the two bound messages and rejects both control sends. See [reproducible evidence](bouncer-service-commands.md); native coverage is command/state, not terminal pixel rendering. |
| G8 — interrupted history/search batches | Existing unit tests cover timeouts, cancellation, late/orphan rows and scoping. PR 107 adds real TCP loss after completed operations. The [partial history App fixture](bouncer-partial-history.md) now proves an actual BETWEEN prefix, disconnect, discard, clean retry and SQLite exclusion on both providers. The [actual daemon partial-batch scenario](bouncer-daemon-history.md#interrupted-batches-in-actual-daemon-processes) now forwards one real BETWEEN row before cutting both providers and separately interrupts SEARCH on Soju. Exact UI results after automatic reconnect/retry and reload, two daemon lifecycles, SQLite exclusion and TRACE exclusion pass. |
| G9 — final reconciliation and release gate | After G1–G8, reconcile every row against the resulting code and authoritative outcomes, update user configuration/command documentation, and run the required final native/web checks and applicable real-provider/browser scenarios on the final tree. Confirm exact reviewed-head merges, clean main matching origin/main and no remaining required artifact or release dependency. Only then can the full goal be marked complete. |

## Current validation anchor

The latest unchanged implementation passed `make clippy` before `make test`:
2702 native tests and 146 web tests passed, with 33 native provider fixtures
explicitly ignored by the default suite. The provider fixtures are separate
runs; their exclusion must not be reported as 100% integration coverage.

PR 107's actual daemon runs passed Lurker, Soju database history and Soju memory
history, two daemon processes each. They include live traffic, browser reload,
server search or unsupported refusal, offline replay, TCP interruption, rejected
offline send, successful automatic recovery, SQLite inspection and TRACE
content exclusion. The implementation also preserves direct-IRC logging and
legacy rows in the separate disk-backed App regression from PR 101.

The goal remains open. This matrix deliberately provides no percentage: these
rows and gates have unequal scope, and several contain missing behavior rather
than merely an unchecked test box.

The native macOS release build of the PR 124 production tree also passed
`make release` on 2026-09-20; the resulting binary reports `repartee 2.0.0` with
`--version`. This is a local build, not a published application release.

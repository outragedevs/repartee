# Bouncer WebPush validation

## Scope

The backend increment adds conditional `soju.im/webpush` negotiation on bouncer
bound network sessions and a session-targeted structured WebSocket API. Browser
permission UI, service worker registration, notification display/click handling
and browser subscription renewal are a separate required increment. This backend
alone does not complete the WebPush acceptance row or the overall goal.

Pinned provider sources: Soju `82e8b7adfb2ab64ec3b88807d29b8b6940236008`,
`doc/ext/webpush.md`, `downstream.go:3702-3846`, `server.go:339-383,847-881`;
Lurker `be42a04e73d6f337e76734684deb457cb5dcdb5f` does not expose this extension
through its IRC bouncer. Provider sources are unchanged by the fixture.

## Backend contract

`WebCommand::WebPush` carries a UUID request ID, connection ID and one action:

- `Get`: returns Ready with an opaque account/network scope and validated VAPID
  P-256 public key, or Unavailable.
- `Register`: includes the previously returned scope/VAPID plus a subscription
  containing endpoint, p256dh and auth. Both scope and VAPID must still match.
- `Unregister`: includes the expected scope and endpoint. Scope must still match.

Responses are delivered only to the requesting WebSocket session. No endpoint or
auth secret is returned, displayed as chat, added to history, or included by the
request's Debug implementation. Raw IRC trace output is suppressed for both legacy and fork library targets by
`diagnostics::IrcWireFilter`, even under explicit dependency trace directives.
Subscription data remains transient; the browser and bouncer own persistence.

Operations require verified TLS, successful SASL on the current transport,
negotiated capability and a valid VAPID point. Endpoint validation requires HTTPS,
no userinfo/fragment/whitespace/control characters, a valid uncompressed P-256
subscription key, a 16-byte auth secret and an IRC frame within 512 bytes.
Each physical connection serializes mutations. Matching server acknowledgments
and a PING/PONG barrier determine completion; failures never echo raw server
parameters. Timeouts retain uncertain requests, reject overlap and allow a late
reply to resolve. Reconnect/cancellation reports Unknown and clears the old
operation. CAP, account/network or VAPID changes invalidate pending success.
Unsolicited/user-prefixed protocol replies cannot become chat/history rows.

## Actual encrypted provider fixture

`scripts/test_bouncer_webpush.py` builds pinned unmodified Soju with its Makefile
inside a disposable Linux container. A dedicated Docker bridge uses an unused
benchmark subnet and publishes IRC only on host loopback. Both the bouncer and
HTTPS receiver run inside that container; synthetic push traffic stays on this
network. This avoids changing Soju's protection against private/loopback push
endpoints. A fixture CA is trusted only inside the container and by the test IRC
client, never installed into host trust. The container/network and temporary
credentials are removed after the run.

The HTTPS receiver independently verifies the VAPID ES256 signature, audience,
expiration and expected public key. It derives the RFC 8291 encryption key/nonce
and decrypts aes128gcm, checking the record delimiter before accepting the push.
The application fixture verifies REGISTER's real encrypted NOTE, delivery of a real
upstream private message through the bound network, repeat REGISTER
without duplicate storage/delivery, persistence across IRC reconnect, UNREGISTER,
idempotent unknown UNREGISTER and a 404 endpoint failure without stored state.
No production credentials or external push service are used.

Evidence: `/tmp/repartee-webpush-provider4.log` passes. Initial harness iterations
fixed nullable Docker IPAM config and port publication on an internal-only Docker
network; neither changed the provider or weakened its endpoint/TLS checks.
The final harness uses an isolated named bridge with loopback-only IRC publication.

## Remaining gates

The backend PR/merge and subsequent browser delivery increment remain required. Genuine browser push
transport must be distinguished from injected service-worker events; UI tests
alone do not prove end-to-end push delivery. All other `BOUNCER_SUPPORT.md`
acceptance rows remain in scope.

## Backend review progress

Initial full checks pass 2649 native and 145 web tests, with 19 opt-in fixtures,
and no project clippy warnings (`/tmp/repartee-webpush-check5-{clippy,test}.log`).
Sol medium review round 1 found a missing response for malformed UUID request IDs.
They now receive a correlated, session-targeted Invalid response without sending
IRC traffic. A regression checks that response and the empty outgoing queue.
Evidence: `/tmp/repartee-webpush-review1.log`.

Round 2 identified that control-session subscriptions accept REGISTER and its test
NOTE but receive no network messages: Soju broadcasts only to subscriptions for
the concrete network ID. Initial negotiation, CAP NEW and API availability now
require a bound network. Regressions cover control sessions with and without an
inconsistent network ID. The provider fixture now creates an actual upstream
network and independently decrypts its private-message push, not just the NOTE.

The expanded diagnostics regression first reproduced leaked synthetic push
credentials under `irc_repartee::client` TRACE. The filter now covers the actual
fork target and its transport children, as well as legacy `irc::client` targets.
The failure is recorded in `/tmp/repartee-webpush-trace-repro-test.log`; subsequent
checks pass in `/tmp/repartee-webpush-bound-fix2-{clippy,test}.log`.

Final post-fix checks pass 2652 native and 145 web tests, with 19 opt-in fixtures,
and no project clippy warnings (`/tmp/repartee-webpush-bound-final-{clippy,test}.log`).
The pinned provider fixture passes separately in `/tmp/repartee-webpush-provider4.log`.

Round 3 reported a case-sensitive CAP NEW comparison. Source inspection shows
`handle_cap_new` normalizes every capability with `str::to_ascii_lowercase`
before filtering, so this report does not reproduce. The bound/control matrix
now explicitly tests lowercase, uppercase and mixed-case WebPush announcements.
No production change is needed for this finding; the next full review will
include the regression evidence.

Full Sol medium review round 4 is clean (`/tmp/repartee-webpush-review4.log`).
The explicit mixed-case regression passes with all 2652 native and 145 web tests
and no project clippy warnings (`/tmp/repartee-webpush-case-check-{clippy,test}.log`).

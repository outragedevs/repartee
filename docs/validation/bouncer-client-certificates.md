# Bouncer client certificate validation

## Current acceptance result

Full `gpt-5.6-sol` medium review round 4 is clean, including untracked sources
(`/tmp/repartee-cert-review4.log`). Final local validation passes 2640 native and
145 web tests, with 18 opt-in fixtures, and no project clippy warnings
(`/tmp/repartee-cert-reviewfix3-{clippy,test}.log`). Pinned Soju proves enrollment,
EXTERNAL reconnect and revocation, with real WebKit proving command delivery and
literal rendering. Pinned Lurker refuses its unavailable extension. Details and
chronological review findings below describe the evidence; earlier pending review
statements are superseded by this result.


## Scope and source contract

This increment implements account-level `soju.im/client-cert` management from
both native and web command input: LIST, CREATE and DELETE, including optional
current-certificate deletion, batched empty lists, standard failures and pending
request lifecycle. It does not change the user's configured SASL mechanism or
generate a private key implicitly.

Sources are the pinned Soju checkout
`82e8b7adfb2ab64ec3b88807d29b8b6940236008` (`doc/ext/client-cert.md`,
`downstream.go:3466-3568`) and pinned Lurker
`be42a04e73d6f337e76734684deb457cb5dcdb5f`. Soju advertises the extension when
`client-cert-auth` is enabled, and EXTERNAL only when the TLS peer supplied a
certificate. Lurker does not advertise the extension. Soju sends certificate
attributes as one semicolon-separated tag string; the parser also accepts the
separate parameters described in its specification and ignores unknown keys.

The SASL negotiation result travels on the physical `IrcHandle`; advertising or
negotiating SASL is not treated as successful authentication. Both bouncer control
and bound network sessions negotiate client-cert initially and on CAP NEW.
Commands require a connected SASL-authenticated session over verified TLS with client-cert and batch.
One operation per connection is serialized with a bouncer PING/PONG barrier;
mutations require matching acknowledgments, lists require a completed batch.
Timeouts retain the pending operation rather than allowing ambiguous retries.
Capability loss invalidates pending success; new transport sessions, disconnect
and cancellation discard old request state. No mutations are automatically retried.

## Evidence before review

- `make clippy`: no project warnings. Existing proc-macro-error2 future-incompatibility
  warning is from a dependency. `/tmp/repartee-cert-check4-clippy.log`.
- `make test`: 2637 native and 145 web tests pass; 18 integration fixtures are
  opt-in. `/tmp/repartee-cert-check4-test.log`.
- Native command regressions cover authentication gating, malformed/empty/incomplete
  replies, unsolicited user-prefix spoofing, mismatched delete acknowledgment,
  delayed completion, timeout retry rejection, CAP withdrawal and cancellation.
- Pinned Soju with disposable verified TLS: PLAIN with certificate -> CREATE ->
  LIST -> bound-network EXTERNAL reconnect -> explicit DELETE -> failed EXTERNAL
  reconnect; then re-enroll, delete current certificate without fingerprint,
  confirm empty list and verify CREATE without a certificate returns NOCERT.
  `/tmp/repartee-cert-soju1.log`.
- Real WebKit -> actual Repartee HTTP/WebSocket -> pinned Soju: list/create/delete,
  certificate name containing percent, semicolon and spaces, and page reload.
  No HTTP/WebSocket mocks. The same fixture continues through EXTERNAL reconnect
  and revocation checks. `/tmp/repartee-cert-soju-web1.log`.
- Pinned Lurker: unsupported command refused without creating a pending request.
  `/tmp/repartee-cert-lurker1.log`.
- Command documentation and generated HTML updated with `make docs`. No frontend
  source/protocol change is needed; the web client uses existing command/event
  paths and the previously built WASM verified by the browser fixture.

Review/fix rounds must finish clean before merge. WebPush and every remaining
`BOUNCER_SUPPORT.md` acceptance row remain in scope for the overall goal.

## Review round 1

Full `gpt-5.6-sol` medium review found that protocol-permitted unsolicited CREATE
could invalidate an unrelated pending LIST or DELETE. CREATE notifications now
report the server's pinning event independently unless CREATE is the requested
operation. An interleaving regression verifies successful LIST and DELETE across
unsolicited CREATE, without spurious failure. Evidence:
`/tmp/repartee-cert-review1.log`; post-fix validation follows.

Round 1 fixes pass 2638 native and 145 web tests with no project clippy warnings
(`/tmp/repartee-cert-reviewfix1-{clippy,test}.log`). Full Sol medium review round 2
found that the draft permits bare DELETE acknowledgment for implicit current-
certificate removal, while pinned Soju returns its fingerprint. Both forms are
now accepted for implicit deletion; explicit deletion still requires the matching
fingerprint. A regression verifies bare acknowledgment is rejected for an explicit
target. Evidence: `/tmp/repartee-cert-review2.log`.

Round 2 fixes pass 2639 native and 145 web tests with no project clippy warnings
(`/tmp/repartee-cert-reviewfix2b-{clippy,test}.log`). Review round 3 requires the
same verified-TLS boundary as other account-level operations. Availability now
requires both TLS and verification, in addition to successful SASL and negotiated
extensions. Regression tests reject LIST/CREATE/DELETE for plaintext and
`tls_verify=false`, even with server-reported authentication/capabilities.
The existing actual Soju fixture already uses a disposable CA and verified TLS.
Evidence: `/tmp/repartee-cert-review3.log`.

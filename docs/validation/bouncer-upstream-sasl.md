# Upstream SASL through a bouncer

Status: implementation and validation complete on `feat/bouncer-upstream-sasl`; ready for merge.

## Contract and source evidence

The pinned Soju downstream implementation advertises `sasl=PLAIN,ANONYMOUS`
after registration when its bound upstream supports PLAIN. It forwards PLAIN
authentication, saving the supplied credentials after upstream success.
ANONYMOUS clears the saved network SASL credentials, including EXTERNAL keys;
it is not a generic logout operation. Lurker rejects post-registration SASL.
See `BOUNCER_SUPPORT.md` for exact upstream revisions and full acceptance scope.

## Implementation

`/auth login <account> <password-env-name>` loads the password from `.env` and
sends it only after the bouncer's SASL challenge. `/auth clear -YES` explicitly
requests removal of saved upstream SASL credentials. Both command paths require
a bound network, verified TLS and a runtime upstream SASL advertisement.
Initial bouncer authentication configuration remains unchanged.

A per-connection exchange prevents overlapping requests. Timeout and capability
withdrawal discard unsent secrets and block retries until a terminal reply or
reconnect. Replies are displayed in the originating connection's server buffer.
Native and web command routing share the backend command; arguments contain a
secret variable name rather than the password itself.

## Validation evidence

The real pinned Soju fixture passed login, rejected-password preservation and
credential clearing, with read-only checks of its SQLite network record. After
login, the fixture disables and enables the network through the disposable
instance's administrative socket. The next upstream connection authenticates
successfully using the stored credentials. Rejected login and clearing also
pass through the web command handler. Buffer contents contain none of the
fixture's secrets. The fixture uses separate disposable bouncer OAuth credentials,
upstream PLAIN credentials and CA-verified TLS.

The pinned Lurker fixture confirms no upstream SASL mechanism is advertised,
`/auth` starts no exchange, and a secret-free explicit AUTHENTICATE probe is
rejected with its existing "Already authenticated" response. Lurker's fixture
uses plaintext loopback; native command TLS enforcement has separate tests.

The first Soju run exposed pre-welcome `CAP NEW sasl=PLAIN,ANONYMOUS`: resetting
exchange state on Connected lost that advertisement. Reset now occurs at
HandleReady, and a regression covers preservation through Connected.

Unit regressions cover secret separation, challenge ordering, unavailable and
unverified connections, concurrent attempts, capability withdrawal, timeout,
reconnect cleanup, confirmation for ANONYMOUS, premature success, response
routing across buffer switches, and late aborts that must not unlock retries.
Final clippy/test checks passed: 2579 native tests and 144 web tests, with 14
native integration tests excluded from the default suite. Both pinned provider
fixtures passed separately. Full Sol medium review round 4 is clean.

## Remaining acceptance boundary

No browser rendering changes are required; commands use the existing native and
web send paths. These tests establish the web backend path, not a fresh automated
browser interaction. Broader browser acceptance remains part of the final matrix.
REGISTER/VERIFY remain a separate increment. The merge gate requires the clean fourth review and passing local checks.

## Review round 1

Sol medium found that consuming numeric 900 hid the upstream account identity
from the existing read-state tracker. A full App-dispatch regression reproduced
an own historical message being counted unread (1 instead of 0). Informational
900/901/908 now continue through normal routing without completing the exchange.
The disposable upstream also emits 900 before successful SASL completion, matching
the real account notification sequence. Full checks and the next review follow.

## Review round 2

Sol medium identified missing SASLprep in the new PLAIN path. A regression
reproduced sending an unprepared soft hyphen in the account and non-breaking
space in the password. Both fields now use the existing SASLprep helper; prepared
empty credentials are refused. Prepared passwords remain zeroized on drop.
The real-provider fixture now supplies these mapped characters and expects the
normalized account/password in Soju storage and on upstream reconnect.

## Review round 3

Sol medium identified that verified downstream TLS does not establish upstream
transport security. Login now checks the discovered network's `tls=1` attribute
before loading the secret. Unknown or plaintext upstreams require an explicit
`-allow-insecure-upstream` command option; the refusal explains password exposure.
The loopback IRC fixture now opts in explicitly. Downstream TLS remains mandatory.
Tests cover refusal before secret loading and explicit override for both unknown
and plaintext upstream transport. Clearing does not forward a password upstream.

The final TLS check is repeated immediately before credential delivery; a
regression verifies that a network downgrade aborts without sending the secret.
Both provider fixtures were rerun successfully after the transport changes.

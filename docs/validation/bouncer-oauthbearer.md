# Bouncer OAUTHBEARER validation

Status: implemented and validated on `feat/bouncer-oauthbearer`; ready for merge.

## Protocol and scope

Soju commit `82e8b7adfb2ab64ec3b88807d29b8b6940236008` advertises
OAUTHBEARER when its OAuth authentication backend is configured. Lurker's
pinned IRC endpoint advertises PLAIN instead. Authentication to the bouncer
must remain separate from upstream account authentication.

The exchange follows https://www.rfc-editor.org/rfc/rfc7628.html:
GS2 authorization identity, SOH-separated Bearer credentials, and a dummy
SOH response to a server error challenge. Tokens use the existing secret
credential path; explicit mechanism selection and verified TLS are required.
Automatic mechanism selection must never reinterpret a stored password as a token.

## Evidence so far

- Socket regression exercises a multi-frame token, decodes and checks the exact
  transmitted payload, and covers success plus the server rejection handshake,
  oversized challenge chunks and repeated challenges after acknowledgement.
- Selection regressions cover explicit selection, missing credentials, and
  exclusion from password-based automatic selection.
- Transport regression rejects plaintext and disabled certificate verification
  before opening a connection.
- Payload regressions cover GS2 identity escaping and invalid credentials without
  including secrets in error messages.
- `make clippy` followed by `make test` passed: 2567 native tests and 144 web tests;
  13 native integration tests remain ignored in the default suite. No project
  warnings; the existing dependency future-compatibility notice remains.

## Pinned Soju integration

`python3 scripts/test_bouncer_presence.py soju /path/to/pinned/soju --oauth`
starts the real pinned Soju binary, a loopback OAuth discovery/introspection
provider, a disposable IRC upstream and a CA-signed TLS listener. The ignored
`pinned_bouncer_oauthbearer` test passed: successful token authentication binds
network 1 on two consecutive connections, while invalid tokens and mismatched
authorization identities are rejected. The CA override exists only in test builds.
The fixture does not use a live user account or disable TLS verification.

## Review and remaining boundary

Configuration documentation and both wizard labels now distinguish passwords
from tokens; WASM was rebuilt. A socket regression verifies that numeric 907
completes authentication without transmitting the token. The first full
`gpt-5.6-sol` medium review found no actionable issues. The second full review, including the additional regressions and generated web
assets, also found no actionable issues. This increment does not implement OAuth token
acquisition or refresh; those require a separately defined provider workflow.

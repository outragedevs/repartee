# Upstream IRC account registration

Status: implemented and validated on `feat/bouncer-account-registration`.

## Scope and protocol

Support REGISTER and VERIFY through the pinned Soju bouncer when its upstream
advertises draft/account-registration. Lurker does not advertise this extension.
The source audit is downstream.go:2849-2870 and upstream.go:1011-1030 at the Soju
revision recorded in BOUNCER_SUPPORT.md. The draft specification is
https://ircv3.net/specs/extensions/account-registration.

REGISTER SUCCESS differs from REGISTER VERIFICATION_REQUIRED. VERIFY SUCCESS
finishes verification. Soju may save the supplied account/password on a REGISTER
response before verification has completed. Tests must check this intermediate
provider state, not infer it from SASL authentication success.

## Implementation

`/account register` and `/account verify` share native/web command routing.
Both use `.env` references for secrets, require a bound connection and confirmed
capability, enforce verified downstream TLS and the explicit upstream transport
policy used by `/auth`. Registration checks advertised naming/email constraints
and password byte-length limits. Passwords remain opaque at registration.

Per-connection pending operations serialize against upstream SASL, retain
unknown-outcome timeouts, and reset on transport replacement. Replies stay on the
origin server buffer, distinguish success/verification-required/failure, preserve
verification instructions and redact direct echoes of the submitted secret.

## Evidence and remaining gates

The pinned Soju fixture passed REGISTER verification-required, provider database
readback before VERIFY, invalid-code retry, successful VERIFY, upstream restart
with the saved account/password, duplicate-account refusal and immediate success.
VERIFY and subsequent REGISTER requests use the web command handler. The pinned
Lurker fixture confirms absent capability and local refusal. Both ran through
`scripts/test_bouncer_presence.py --account-registration` (Soju also `--oauth`).

First full Sol medium review found lost initial CAP LS rules and case-sensitive
reply matching. Both are corrected, with scripted socket coverage for initial
CAP values and unit coverage for session initialization, capability withdrawal,
and lowercase terminal replies. Successful registration/verification also records
account ownership without requiring an additional numeric 900; verification-required
does not. This follows the draft's authenticated-on-success semantics and has a
history unread regression. Registration passwords remain opaque; the fixture's
SASL reconnect now reports the actual authenticated account.

Final clippy passed without project warnings; 2585 native and 144 web tests
passed. Both pinned providers passed the integration fixture, and Soju passed
again after the review fixes. Full Sol medium review round 2 found no actionable
issues. The only toolchain notice is the existing proc-macro-error2 future
compatibility notice. Broader browser interactions,
capability/ISUPPORT coverage and the full BOUNCER_SUPPORT.md acceptance matrix
remain in scope; these command-handler tests do not establish browser coverage.

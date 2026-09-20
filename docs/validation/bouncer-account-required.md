# Required account authentication

Pinned Soju advertises the informational `soju.im/account-required` capability
and rejects attempts to request it. Its `doc/ext/account-required.md` permits
`FAIL * ACCOUNT_REQUIRED` even when the capability was absent. `downstream.go`
accepts PASS at registration and successful SASL before registration. Lurker
does not advertise this capability; its existing PASS/SASL behavior is preserved.

The client never requests the informational capability. When it is present,
registration requires successful SASL or a configured alternative (PASS or a
client certificate); an alternative is not treated as authenticated until the
server accepts registration. Failure reports use fixed, actionable text rather
than reflecting server-supplied details.

Global ACCOUNT_REQUIRED failures are recognized during CAP discovery, capability
acknowledgment, SASL challenge/result waits, binding confirmation and the wait for
welcome. They do not turn into silent waits or generic disconnects. After welcome,
the same global failure is displayed in that connection's server buffer, including
the web event, without forcibly dropping an established connection. Command-level
failures such as `FAIL JOIN ACCOUNT_REQUIRED` are not global registration failures.

Socket tests cover absent advertisement, missing credentials, SASL success,
SASL failure, PASS success and PASS fallback after SASL failure, and refusals at
multiple registration stages. An ignored `pinned_bouncer_account_required` test
uses the existing disposable binding fixture against both upstream revisions;
Soju additionally proves early refusal without credentials. Native/web event
routing has a regression. Clippy, full tests, integrations and full pinned Sol medium review round 3 pass.

SAFERATE, ICON, channel-context routing and the full acceptance matrix remain
required for the overall goal.

The first full Sol medium review found no actionable issues. A final extension
also maps `FAIL BOUNCER ACCOUNT_REQUIRED BIND` during binding confirmation to the
same actionable authentication error, with a socket regression. The global
post-registration handler remains scoped to `FAIL * ACCOUNT_REQUIRED`.

Positive pinned tests pass for Soju and Lurker with both PASS and SASL
(`/tmp/repartee-account-required-{soju,lurker}1.log`). Soju certificate enrollment,
EXTERNAL reconnect and browser command validation pass
(`/tmp/repartee-account-required-certificates1.log`), as does its real OAUTHBEARER
fixture (`/tmp/repartee-account-required-oauth1.log`). These use disposable data
and verified TLS for certificate/OAUTHBEARER authentication.

Final native validation after binding-error handling passes Clippy without project
warnings and 2669 native/145 web tests
(`/tmp/repartee-account-required5-{clippy,test}.log`).

Review round 2 found a post-welcome gap in bouncer confirmation, which waits for
end-of-MOTD after welcome. That phase now tracks welcome and retains subsequent
global failures for normal server-buffer display. Socket regressions cover the
same sequence on both direct and bound connections; pre-welcome refusals remain
fatal and binding-specific authentication failures remain binding errors.

After the post-welcome fix, Clippy remains free of project warnings and all
2669 native/145 web tests pass (`/tmp/repartee-account-required6-{clippy,test}.log`).

Final clean review: `/tmp/repartee-account-required-review3.log`, with all three
model/review-model/reasoning overrides pinned. Both providers passed again after
the post-welcome fix (`/tmp/repartee-account-required-{soju,lurker}2.log`).

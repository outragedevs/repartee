# PASS authentication and explicit binding

The pinned Lurker implementation (`server/services/bouncer.ts`,
`handleBouncerPreReg`, `authenticate`, `completeAttach`) permits BIND after a
nonempty PASS has been supplied. It verifies the credential at CAP END and only
then resolves the network ID. Pinned Soju (`downstream.go`, preregistration BIND)
requires the account to have authenticated already, so PASS-only BIND is rejected.
Both providers support PASS on an unbound control connection.

Repartee previously rejected every explicit binding without successful SASL.
It now permits a supplied nonempty PASS to reach the provider's BIND path, while
retaining the existing registration barrier: capability acknowledgement,
successful registration, and an exact BOUNCER_NETID confirmation are required
before a handle or Connected event is exposed. A PASS attempt does not set the
SASL-authenticated flag. No credentials are fabricated or copied into SASL fields.

PASS-only scopes additionally hash the account prefix of a combined credential;
changing `alice:secret` to `bob:secret` cannot reuse the same account scope, even
when `username` and the server entry remain unchanged. The secret after the colon
is excluded, so rotation within that form keeps the scope. Configured SASL must
succeed rather than silently falling back to PASS for a potentially different
account; successful SASL retains its previous scope independent of unused PASS.
Before provider identification, a colon in a PASS-only Soju secret is conservatively
treated as a possible account separator for scoping too. Changing that prefix
therefore starts a separate scope. Provider-aware continuity for that ambiguous
secret form remains part of G2's identity work.

## Actual-provider acceptance

Use the revisions and setup in [binding acceptance](bouncer-binding.md), then:

```sh
make clippy
python3 scripts/test_bouncer_binding.py lurker /path/to/pinned/lurker --test-filter pinned_bouncer_pass_registration
python3 scripts/test_bouncer_binding.py soju /path/to/pinned/soju --test-filter pinned_bouncer_pass_registration
python3 scripts/test_bouncer_binding.py lurker /path/to/pinned/lurker --pass-auth --test-filter pinned_bouncer_persistent_history
python3 scripts/test_bouncer_binding.py lurker /path/to/pinned/lurker --pass-auth combined --test-filter pinned_bouncer_persistent_history
```

`pinned_bouncer_pass_registration` verifies control PASS on both providers,
Lurker explicit binding and reconnect, Lurker's combined `username:secret` PASS,
Soju's explicit-binding refusal, invalid credentials, absent credentials, invalid
PLAIN authentication, and Lurker's invalid network ID rejection. Successful PASS
sessions have neither the SASL capability nor the SASL-authenticated flag. Failure
paths produce no usable connection handle, and errors exclude the synthetic
password values. Mixed SASL/PASS credentials with failed SASL are rejected for
both bound and control connections.

The existing persistent-history fixture also runs with both USER-plus-secret and combined PASS-only Lurker binding.
It loads 300 upstream rows, serves native/web history, exercises volatile live and
own-message paths, shuts down the actual SQLite writer, inspects the database,
reconstructs App and repeats. Only the deliberately seeded legacy row and the
direct-IRC positive control remain on disk; bouncer rows are not written or
recovered from a local archive. This is an App reconstruction test, not two OS
processes, and the live-message inputs in this fixture are controlled App events.

TLS uses the existing verified transport settings; these loopback acceptance
runs do not prove TLS rejection. G2 remains open for actual untrusted/wrong-host
TLS checks and legacy username/network selector behavior with correct history
ownership. This change does not claim those separate paths are complete.

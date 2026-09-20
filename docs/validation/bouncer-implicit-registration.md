# Implicit bouncer registration (in progress)

A login containing a network selector is not evidence that the server is a
bouncer. Repartee detects `soju.im/bouncer-networks` in CAP LS on the actual
connection, negotiates that capability, and disables the IRC library's autojoin
before CAP END. Registration waits for the server's confirmed `BOUNCER_NETID`
(or control mode) before exposing the connection and replaying early messages.
Explicitly configured control and BIND sessions retain their strict identity
checks.

The detected identity belongs to the live connection. It does not rewrite the
original login configuration or add BIND to a legacy PASS reconnect. History,
read markers, presence, metadata and other bouncer feature gates use this runtime
identity. A changed implicit network scope discards old conversation buffers.
The original login is retained for reconnects.

## Verification

- A TCP regression checks legacy USER/PASS registration, confirmed network ID,
  absence of BIND and absence of a configured automatic JOIN.
- An application regression checks history ownership, preserved login settings,
  scope changes, removal of old network buffers and return to direct IRC.
- The existing persistent-history fixture runs with
  `REPARTEE_BOUNCER_TEST_LEGACY=1` against both pinned providers. It uses a real
  USER `account/network` selector and PASS, with no bouncer configuration flags.
  Initial hydration, shared web pagination, native pagination, live messages,
  reconnect and SQLite exclusion passed on both providers.
- The pre-registration `Connecting to fixture...` diagnostic is retained locally;
  conversational history and live bouncer messages are not. Existing historical
  rows and direct IRC persistence remain intact.
- The full suite passed: 2695 native tests and 146 web tests. Clippy passed without
  project warnings. The existing dependency future-compatibility notice remains.

Run the actual-provider check with:

```sh
REPARTEE_BOUNCER_TEST_LEGACY=1 python3 scripts/test_bouncer_binding.py soju /path/to/pinned/soju --test-filter pinned_bouncer_persistent_history
REPARTEE_BOUNCER_TEST_LEGACY=1 python3 scripts/test_bouncer_binding.py lurker /path/to/pinned/lurker --test-filter pinned_bouncer_persistent_history
```

## Release dependency

This draft pins the reviewed library commit
`a2c50fb237434892cc6727a40d38f6a23db5b184` from
https://github.com/outragedevs/irc so the branch can be built on another machine.
It uses no local path override or vendored library. Before merging, the owner
publishes irc-repartee 1.5.2 from the `crates-io-publish` branch; then remove the
Git revision/source from Cargo.toml and update Cargo.lock to the registry release.
The final distribution must retain `cargo install repartee`.

This evidence covers legacy bound USER/PASS sessions. Provider-specific combined
PASS account scope, implicit control child connections, and privileged feature
eligibility for non-SASL authentication still require reconciliation. It does not
close the entire bouncer completion goal.

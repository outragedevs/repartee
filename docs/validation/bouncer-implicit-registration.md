# Implicit bouncer registration

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

The owner published irc-repartee 1.5.2. Cargo.toml and Cargo.lock now use the
crates.io release, with no Git dependency, local path override or vendor copy.
The package supplies the connection-local autojoin control used during CAP
negotiation. The registry package checksum is
`f1ee01f1d7f62d201ad71bd832867ebc7217540514d691e8723e49f7cfd24688`.

## PASS control connections

Generated child connections preserve the parent's PASS authentication and select
the discovered network in USER. They do not send BIND before PASS authentication;
Soju rejects that ordering. The requested numeric network ID remains mandatory
in the registration confirmation. The selector is read from the current network
registry for each connection attempt, including reconnect after a rename.
The existing `pinned_bouncer_generated_children` fixture also passed on both
pinned providers with `REPARTEE_BOUNCER_TEST_LEGACY=1`: an implicit PASS control
session creates the network child, hydrates/paginates history and excludes
conversation rows from the local log. SASL-authenticated child connections
retain BIND. Names that cannot fit a USER
parameter require SASL authentication.

Provider-specific combined PASS account scope and privileged feature eligibility
for non-SASL authentication still require reconciliation. This change does not
close the entire bouncer completion goal.

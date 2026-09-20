# Bouncer service conversation acceptance

The pinned providers have different contracts. Soju intercepts messages to
BouncerServ on both control and bound connections (`downstream.go`, `service.go`).
It parses quotes and backslash escapes, sends service replies as private messages,
and explicitly returns empty service CHATHISTORY batches because it does not
store those conversations. Lurker has no corresponding service: a bound message
is ordinary upstream traffic; a control connection rejects it with a notice.

## Reproducible acceptance

Use the unmodified revisions in [BOUNCER_SUPPORT.md](../../BOUNCER_SUPPORT.md#reproducible-upstream-evidence),
with their dependencies and binaries prepared as in the existing presence fixture.
Provide Playwright WebKit through `NODE_PATH` (or `REPARTEE_PLAYWRIGHT_MODULE`).
Run `make clippy` before the fixtures:

```sh
python3 scripts/test_bouncer_presence.py soju /path/to/pinned/soju --service
python3 scripts/test_bouncer_presence.py lurker /path/to/pinned/lurker --service
```

The runner starts disposable accounts, real provider processes, and a local IRC
upstream. `src/app/bouncer_service_fixture.rs::pinned_bouncer_service` exercises
native `App::handle_submit` and the compiled web UI through Playwright WebKit,
real HTTP and WebSocket connections. It repeats with a control connection and an
explicit network binding. Provider responses are not mocked.

Assertions cover:

- Soju creation/status/deletion of disabled temporary networks whose names contain
  spaces, semicolons and literal percent signs. A read-only query of the disposable
  Soju database verifies the parsed realname, including an escaped backslash;
  another query verifies deletion. Status identifies the current network only on
  the bound connection.
- Unknown commands and unterminated quoted arguments produce actual Soju errors
  in the service query. Native outgoing commands retain their payload and appear
  once, including with provider echo enabled.
- Browser create/status/delete and both errors work from each connection type.
  A service reply remains visible exactly once after browser reload.
- Lurker rejects control sends from both entry points. Bound sends preserve the
  complete quoted/percent/backslash payload. The upstream records exactly those
  two messages, and none of the rejected control messages. Soju sends none of
  these service commands upstream.
- A tracked LATEST request to Soju BouncerServ completes successfully with an
  empty batch on both connection types, preserving all existing message IDs and
  text. It neither erases the live conversation nor duplicates its rows.
- A live local-log channel installed before the commands receives no service query
  rows or sentinel content. The browser reload reads volatile daemon state;
  it does not enable a separate local conversation archive.

This is native command/state acceptance, not a terminal screenshot test. The
upstream peer used for Lurker echoes the received payload solely to verify ordinary
forwarding; it is not an implementation of a Lurker service. Provider restart, combined account isolation and interrupted history remain
separate completion gates.

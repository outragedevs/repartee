# Bouncer invitation validation

Status: implementation and validation complete on `fix/invite-network-isolation`;
final review and merge are pending.

## Verified defect and behavior

The existing INVITE handler distinguishes invitations to our own nick from
invite-notify for another user, but own invitations previously went to any active
buffer, including another network. The storage gate uses the destination buffer,
so cross-network routing can also cross the bouncer history boundary.

A regression with an active channel on another connection failed on the existing
handler (`/tmp/repartee-invite-check1-test.log`). The fix accepts the active buffer
only on the originating connection, otherwise selects that connection's server
buffer. Bouncer invitations use transient message delivery even in server buffers;
direct IRC retains logging. Own invitations raise mention activity, and percent
characters in invitation event data are escaped for the theme formatter.

Nickname identity now respects ascii, rfc1459 and strict-rfc1459 CASEMAPPING.
The existing MONITOR folding function moved unchanged to the shared ISUPPORT
module, so both consumers use the same rules without invitation-to-MONITOR coupling.

## Validation evidence

`make clippy` followed by `make test` passed in `/tmp/repartee-invite-check5`:
zero project warnings, 2486 native tests, 142 web host tests, 9 fixture-only tests
ignored by default. Regressions cover own/other-user routing, separate connections,
direct-IRC logging versus bouncer non-persistence, mention activity and web event
destinations, nickname case mappings and literal percent rendering.

`python3 scripts/test_bouncer_presence.py PROVIDER SOURCE --invites` passed with
both pinned implementations. The actual App joins a channel through the bouncer,
keeps another network active, receives an own INVITE and a third-party INVITE,
and verifies the destination buffers, actual web broadcast messages and storage
queue. Results: `/tmp/repartee-invite-soju2.log` and
`/tmp/repartee-invite-lurker2.log`. The first fixture attempt incorrectly inspected
an already-drained pending-event queue; the final fixture subscribes to the web
broadcaster and verifies delivered events directly.

`/tmp/repartee-browser-qa/invites.cjs` replays the captured real App/bouncer event
payloads through the committed WASM client in WebKit. Own and third-party messages
from each provider render in the correct server/channel view with literal percent
characters. `/tmp/repartee-invite-browser1.log` passed without browser errors;
`/tmp/repartee-invite-soju-self-web.png` was visually inspected. This browser replay
covers rendering of captured events, not a live browser-to-bouncer connection.

First pinned Sol medium review returned no actionable findings. A final round is
requested because the fixture's web receiver changed during the first review.

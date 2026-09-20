# Real daemon history and diagnostic-content regression

## Defects reproduced

At Repartee `0738ffb749380947f10c288b2c2b0e2591619659`, a real detached daemon
with `RUST_LOG=trace` served bouncer history to the compiled Chromium UI. SQLite
contained no bouncer messages, but the diagnostic file contained message bodies:

- `TRACE tungstenite::protocol: Sending frame: ...` included complete JSON
  `Messages` events, including the 300 fixture history texts.
- `DEBUG repartee::app: web command received cmd=RunCommand { ... }` included the
  text submitted through the web composer.

The existing IRC wire filter did not cover WebSocket frames or the application's
structured command dump. This is a real text-file history leak, independently of
the SQLite storage boundary.

`WireContentFilter` now suppresses tungstenite DEBUG/TRACE records (including
frame submodules, close payloads and client handshake traces), while retaining
WARN/ERROR and unrelated diagnostics. Existing IRC TRACE suppression is retained.
The application logs receipt of a web command without formatting its payload.
The regression covers both log-facade and native tracing events, explicit TRACE
directives, target-prefix boundaries and retained warning/debug diagnostics.

## Reproducible process/browser fixture

Prerequisites: the pinned Soju/Lurker checkouts and binaries/dependencies from
`BOUNCER_SUPPORT.md`, Docker with `host.docker.internal` access to host loopback
(tested on macOS with OrbStack), Node and Playwright Chromium. Set `NODE_PATH` or
`REPARTEE_PLAYWRIGHT_MODULE` if Playwright is outside the default module path;
`PLAYWRIGHT_CHROMIUM_EXECUTABLE` can select an installed Chromium executable.

Build from a clean archive so no personal files or credentials enter the image:

```sh
fixture_source=$(mktemp -d /tmp/bouncer-daemon-source.XXXXXX)
git archive HEAD | tar -x -C "$fixture_source"
docker build -f "$fixture_source/scripts/fixtures/daemon-history.Dockerfile" \
  -t repartee-daemon-acceptance "$fixture_source"
python3 scripts/test_bouncer_binding.py soju /path/to/pinned/soju \
  --daemon-image repartee-daemon-acceptance
python3 scripts/test_bouncer_binding.py lurker /path/to/pinned/lurker \
  --daemon-image repartee-daemon-acceptance
```

The Dockerfile pins its Rust base by digest and builds with `make build`. The
container uses its own root home with a disposable mounted data directory; the
operator's HOME and runtime directories are never changed or mounted. Disposable
credentials are written only to the fixture's `.env`.

Each provider run starts two distinct daemon containers in sequence against the
same data directory. Each cycle performs actual browser login, selects the
history query, observes all 300 expected texts over the real WebSocket, and
scrolls with wheel gestures until the oldest message is rendered. It also
submits a `/msg` command containing a diagnostic sentinel and waits for the
server-created query buffer before stopping. Each cycle uses a distinct target. HTTP, WebSocket,
server history and rendering are not mocked.

Neither upstream accepts live traffic in this scenario: Soju has a disabled network, while
Lurker's existing upstream test double is held in the non-writable `connecting`
state. Unlike `disconnected`, this state does not trigger replacement of the fake
with a real DNS/socket connection when a downstream client binds. The actual pinned
bouncer endpoints and their stores serve the history. The outgoing command
exercises the downstream/browser path, not successful delivery to an IRC peer.

The fixture stops the daemon with SIGTERM and requires exit code zero, copies the
closed database out of the stopped container, and inspects that disposable copy.
Only the local Status welcome event for each startup may exist in `messages`.
Its presence also proves the persistent writer was enabled. No bouncer message
row is allowed. The original data directory remains in place for the next daemon
process. At each stop, the diagnostic file must contain neither history text nor
the outgoing sentinel, despite TRACE being enabled.

The earlier disk-backed App fixture separately supplies direct-IRC positive
controls and preserved legacy rows. This process fixture adds actual process
restart, built-browser pagination, and diagnostic-file inspection. It does not
close TARGETS enumeration gaps, no-history-store configurations, successful live
upstream delivery, server-search persistence, or every failure-path acceptance
case. The complete bouncer goal remains open.

Both pinned-provider process/browser runs passed on 2026-09-20 after the fixes.
The default suite passed 2684 native and 146 web tests (24 provider-dependent
native tests ignored), with no project Clippy warnings. Review round one found
an unintended Lurker upstream restart and an outgoing-command observation race;
the fixture now uses a stable non-writable fake and waits for the server's
BufferCreated response. Both provider runs passed again with those corrections.

## Real upstream traffic and restart hydration

The presence fixture now provides a separate `--live-history` mode for both
pinned providers. It starts a real local TCP IRC upstream; Lurker creates its
normal upstream connection instead of retaining the binding fixture's fake.
Soju uses its database message store. Build the same daemon image as above, then
run:

```sh
python3 scripts/test_bouncer_presence.py lurker /path/to/pinned/lurker \
  --live-history --daemon-image repartee-daemon-acceptance
python3 scripts/test_bouncer_presence.py soju /path/to/pinned/soju \
  --live-history --daemon-image repartee-daemon-acceptance
```

The direct capability probe requires CHATHISTORY. Through the compiled browser,
the daemon receives an incoming private message and sends a private message
through the actual provider to the upstream. The upstream records that delivery
and sends its echo. Browser reload must retrieve both messages, without duplicate
visible rows. Older-page requests are driven through the UI until `has_more`
becomes false; assertions combine all received pages because the final older
page may be empty.

After the first daemon stops cleanly, the upstream sends an offline private
message and a PING barrier. The runner waits for the provider's PONG before
starting the second daemon. Before generating new traffic, its browser must
display the offline message and the first daemon's incoming and outgoing rows.
This checks server-backed history hydration across actual daemon processes.
Both cycles inspect the copied SQLite database and TRACE diagnostic file using
the same persistent-exclusion assertions as the earlier scenario.

The existing Soju `--memory-history` mode uses this shared browser fixture while
retaining its different contract: no CHATHISTORY, a visible limitation, and only
missed-message replay after restart. It does not require replaying messages the
previous connection already acknowledged. See
[bouncer-memory-history.md](bouncer-memory-history.md).

These scenarios close the live-upstream delivery gap of the earlier fake/disabled
upstream fixture. They do not cover provider process restart, server-search
persistence, all transport failures, or the unresolved TARGETS enumeration gaps.

## Transport loss and reconnect

The live-history and Soju memory-store daemon scenarios route the downstream IRC
connection through `scripts/bouncer_fault_proxy.py`. This disposable loopback
proxy forwards bytes unchanged and never records their contents. Its separate
loopback control endpoint is used by the test runner, not by application code.

For each daemon lifecycle, after the search checks, the browser fixture cuts an
established TCP connection and blocks replacement connections. It requires an
actual daemon `ConnectionStatus` transition to disconnected. The browser reloads
while transport is still unavailable and must display the conversation from
volatile daemon memory. A send attempt must report `Failed to send message`; its sentinel
must never appear as a chat message or reach the upstream, including after
reconnection.

The proxy then resumes forwarding. With the fixture's reconnect delay set to one
second, the browser must observe automatic reconnection, send a new message, and
receive its upstream echo without duplicating the pre-disconnect message. The
upstream event log independently proves successful post-reconnect delivery. The
normal shutdown inspection still requires only local startup records in SQLite
and no message bodies in the diagnostic file.

This covers an unexpected downstream TCP close, offline browsing, rejected send,
and automatic recovery for both providers, including Soju without CHATHISTORY.
It does not claim TLS-failure coverage, a partial history-batch timeout, or a
restart of the provider itself. Those require separate evidence.


## Interrupted batches in actual daemon processes

Set `REPARTEE_DAEMON_PARTIAL_HISTORY=1` when running the existing binding runner
with `--daemon-image`. The daemon runner installs a TCP proxy in front of the
actual pinned provider. The proxy observes a real BETWEEN request, forwards its
batch opener and exactly one message, then closes the transport before the batch
terminator. On Soju, a second proxy repeats this sequence for a real SEARCH
response. Lurker does not expose SEARCH.

The compiled Chromium UI asserts that no partial rows are shown as completed
results, waits for automatic reconnect after the proxy resumes forwarding,
retries and receives exact expected results. Browser reload retains those
transient results without duplicates. The parent runner then stops the daemon,
inspects SQLite and TRACE output, and starts a second daemon on the same data
directory for the original history acceptance scenario.

Both pinned providers pass. Each database contains only the expected local
startup events; server history, outgoing conversation bodies and search results
do not enter SQLite or TRACE. This is actual process, socket, browser and storage
evidence, unlike the older interruption after completed operations. The tested
image uses source `a0ced811e69cb99d456a3489b1f853562ce16a08`, including the special
buffer persistence fix, and the published `irc-repartee` 1.5.2 dependency.

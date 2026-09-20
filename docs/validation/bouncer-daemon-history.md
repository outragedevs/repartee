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

# Network icon discovery and terminal access

The first network-icon increment exposes the current `draft/ICON` URL through
`/server icon [connection-id]`. The active connection is the default; explicitly
selected bouncer child connections do not need a saved configuration entry.
The command is also available through the web command input. The later web
sidebar increment is documented below.

Specification: https://ircv3.net/specs/extensions/network-icon . This is still a
draft ISUPPORT token. HTTP and HTTPS image URLs are accepted, with URL userinfo
and non-web schemes rejected. `{size}` requests 128 pixels for the command;
size is a hint, not a decoder dimension guarantee. Existing URL percent escapes
are retained, and command output escapes theme formatting separately.

The URL is derived from current ISUPPORT rather than persisted independently.
Normal 005 updates and validated atomic `draft/isupport` bursts therefore share
existing update/removal and disconnect semantics. No automatic image request or
message-history write is introduced. Users can open the link, or use `/preview`
for formats supported by the existing terminal image renderer.

## Provider verification

- Soju `82e8b7adfb2ab64ec3b88807d29b8b6940236008` forwards the upstream icon
  through its passthrough ISUPPORT list (`downstream.go`). It can also advertise
  a separate control-connection icon from its server configuration.
- Lurker `be42a04e73d6f337e76734684deb457cb5dcdb5f` replays upstream 005 lines
  through `sendAttachBurst` in `server/services/bouncer.ts`; the icon survives
  even though it has no dedicated ICON-specific branch.

Both pinned-provider fixtures pass with an upstream template URL and a bound
network connection. They assert the parsed URL and `/server icon` output;
they do not fetch the image or claim browser rendering coverage.

```sh
python3 scripts/test_bouncer_presence.py soju /tmp/repartee-bouncer-audit.U3fkHg/soju --network-icon
python3 scripts/test_bouncer_presence.py lurker /tmp/repartee-bouncer-audit.U3fkHg/lurker --network-icon
```

Clippy has no project warnings; 2680 native and 145 web tests pass, with 23
provider-dependent tests ignored by the default native run. `make docs`
regenerates the published command reference. The web presentation and transport follow-up is documented below. SAFERATE
publication and the full bouncer acceptance audit remain open.


## Web sidebar and authenticated image transport

The web sidebar displays each server buffer's advertised icon. `SyncInit`
includes the icon URL; live `NetworkIcon` updates refresh it without a reload.
Removal, invalid replacements and disconnects clear the icon, and an image
load failure hides the image while retaining the network name. The icon
registry is initialized independently of the default-off message preview
setting; disabling chat previews does not disable network icons.

Browsers receive only an authenticated same-origin `/api/network-icon` URL
with an HMAC identifier. The server accepts registered URLs only, uses a
public-address DNS resolver and validates redirect destinations. Environment
HTTP proxies are disabled so they cannot bypass that resolver. Direct image
responses are limited to 1 MiB and a 10-second fetch timeout. HTML is rejected.
Raster image types and SVG are served with `nosniff` and a sandbox CSP that
blocks scripts/external subresources while retaining inline styles and data
images/fonts. This also protects direct navigation to an SVG URL.

Caching is browser-private for one hour; changing the advertised URL changes
the proxy identifier. Icons do not enter message history and no icon body is
stored in the local message database. This increment does not add a server
image-body cache. Terminal access remains the link/preview command above.

Verification combines distinct boundaries:

- Native HTTP integration: authentication, unknown identifiers, actual local
  fixture image response through a test-only DNS override, oversized response,
  HTML rejection, localhost redirect rejection and response security headers.
- State tests: atomic ISUPPORT removal, connection isolation, disconnect,
  default configuration independent of chat previews and web-server teardown.
- Pinned Soju and Lurker: their upstream icon reaches both the native parsed
  state and the web broadcaster as the matching same-origin identifier.
- Compiled WASM in Chromium: initial icon, browser cache after reload, live
  replacement/removal, failed-image fallback, connection isolation, disconnect
  and no external browser requests. Direct SVG navigation does not execute its
  fixture script. The browser fixture controls HTTP responses and WebSocket
  events; it does not claim to be a single end-to-end real-bouncer browser run.

Browser command: `node scripts/test_network_icons_browser.cjs` with Playwright
available via NODE_PATH and, when needed, PLAYWRIGHT_CHROMIUM_EXECUTABLE.
Both generated WASM distribution directories are rebuilt with `make wasm`.
Review round one found the dependency on chat previews; independent registry
initialization and a default-configuration regression address that finding.
SAFERATE publication and the full bouncer acceptance matrix remain open.


Review round two identified missing special-use address exclusions in the
shared image proxy. The filter now checks IPv4 special-purpose ranges and IPv6
global-unicast allocations, including protocol-assignment exceptions. Mapped
IPv4 and well-known NAT64 addresses are checked against the embedded IPv4
address; deprecated transition ranges and unallocated IPv6 space are denied.
The same predicate gates literal URLs, DNS results and redirects.
IPv6 RIR allocation boundaries are also checked against
https://www.iana.org/assignments/ipv6-unicast-address-assignments/ .
Source special-purpose registries checked on 2026-09-20:
https://www.iana.org/assignments/iana-ipv4-special-registry/ and
https://www.iana.org/assignments/iana-ipv6-special-registry/ .
The registry-based policy must be revisited when IANA changes allocations.

The final web increment passes Clippy without project warnings, 2684 native
and 146 web tests (23 provider-dependent native tests ignored by default), both
pinned-provider web-event fixtures and the compiled-WASM Chromium scenario.
The authentication fixture's teardown accepts either EOF or macOS connection
reset only after all expected protocol frames have been checked.
Review round three tightened IPv6 further to the currently allocated IANA
ranges rather than the entire global-unicast supernet; boundary regressions
cover the reserved gaps.

Review round four found that a failed image could remain hidden after a direct
URL replacement. Successful image loads now restore visibility; the compiled
browser regression includes failure followed immediately by a working URL.

Review round five raised ORCHID/DET overlay identifiers. Its statement that
IANA marks them non-global was not borne out by the live XML/HTML registry
(which marks both True). Nevertheless, the proxy deliberately excludes those
identifier prefixes: RFC 7343 defines ORCHIDs as non-routable at the IP layer,
and RFC 9374 defines DETs as a form of ORCHID. This is an explicit HTTP-fetch
policy restriction beyond the registry flag, supported by
https://www.rfc-editor.org/rfc/rfc7343.html and
https://www.rfc-editor.org/rfc/rfc9374.html . Regression tests reject both.

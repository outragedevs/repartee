# Network icon discovery and terminal access

The first network-icon increment exposes the current `draft/ICON` URL through
`/server icon [connection-id]`. The active connection is the default; explicitly
selected bouncer child connections do not need a saved configuration entry.
The command is also available through the web command input, but sidebar image
rendering is a separate pending increment.

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
regenerates the published command reference. Browser presentation, authenticated
image fetching/caching and their tests remain required for the complete icon
feature; SAFERATE publication and the full bouncer acceptance audit remain open.

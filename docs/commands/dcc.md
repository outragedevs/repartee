---
category: Connection
description: DCC CHAT — direct peer-to-peer chat connections
---

# /dcc

## Syntax

    /dcc <chat|close|list|reject> [args...]

## Description

Manage DCC (Direct Client-to-Client) CHAT connections. DCC CHAT establishes a
direct TCP connection between two IRC users, bypassing the IRC server entirely.
Messages are exchanged over a raw TCP socket, not through the IRC network.

DCC CHAT buffers use the `=nick` naming convention (e.g., `=Alice`). Typing in
a `=nick` buffer sends text over the DCC connection, not via IRC. The
`/msg =nick text` syntax also routes to DCC.

## Subcommands

### chat

Initiate a DCC CHAT or accept a pending request.

    /dcc chat                  Accept the most recent pending request
    /dcc chat <nick>           Initiate DCC CHAT or accept pending from nick
    /dcc chat -passive <nick>  Initiate passive DCC (for NAT/firewall)

When no nick is given, the most recent pending incoming request is accepted.
If a pending request from the specified nick exists, it is accepted instead of
initiating a new one.

Passive mode (`-passive`) sends a reverse DCC offer — the remote peer opens the
listener instead. Use this when your firewall blocks incoming connections.

### close

Close an active DCC CHAT connection.

    /dcc close chat <nick>

The `=nick` buffer remains after closing (with a disconnect message). Use
`/close` on the buffer to remove it.

### list

List all DCC connections with their current state.

    /dcc list

Shows: nick, type (CHAT), state (waiting/listening/connecting/connected),
duration, and bytes transferred.

### reject

Reject a pending DCC CHAT request and notify the sender.

    /dcc reject chat <nick>

Sends a `DCC REJECT` CTCP notice to the remote client so they know the offer
was declined. Requests that time out (5 minutes by default) are silently
discarded without sending REJECT.

## Configuration

DCC settings can be changed at runtime with `/set`:

    /set dcc.timeout 300           Seconds before pending requests expire
    /set dcc.own_ip 203.0.113.5    Override IP in DCC offers
    /set dcc.port_range 0          Port range for listeners (0 = OS-assigned)
    /set dcc.max_connections 10    Max simultaneous DCC connections
    /set dcc.autoaccept_lowports false  Allow auto-accept from ports < 1024

## IP Detection

Repartee automatically detects your IP from the IRC socket (like irssi's
`getsockname`). If you're behind NAT or connecting through a bouncer, set
`dcc.own_ip` to your public/LAN IP.

## Examples

    /dcc chat Alice
    /dcc chat -passive Bob
    /dcc list
    /dcc close chat Alice
    /dcc reject chat Eve
    /msg =Alice hello over DCC!
    /me waves (in a =nick buffer, sent over DCC)

## See Also

/msg, /query, /set

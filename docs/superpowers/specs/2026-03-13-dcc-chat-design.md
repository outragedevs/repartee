# DCC CHAT Support — Design Specification

**Date:** 2026-03-13
**Branch:** `wip/dcc-chat-implementation`
**Reference:** erssi (`~/dev/erssi/src/irc/dcc/`)

## Overview

Add DCC CHAT support to Repartee with full erssi parity: active and passive (reverse) DCC, `=nick` buffer convention, auto-accept masks, REJECT, nick change tracking, timeout, and scripting integration.

DCC (Direct Client-to-Client) enables peer-to-peer TCP connections between IRC users, bypassing the server. DCC CHAT is a line-delimited text protocol over a raw TCP socket, initiated via CTCP messages on the IRC connection.

The `irc-repartee` crate has zero DCC support — all DCC logic is application-level.

## Module Structure

```
src/dcc/
├── mod.rs          — DccManager, DccEvent enum, public API
├── chat.rs         — DCC CHAT connection logic (active + passive async tasks)
├── protocol.rs     — CTCP DCC message parsing & encoding, IP long<->IpAddr conversion
└── types.rs        — DccRecord, DccState, DccType enums
```

## Core Types

### DccType

```rust
pub enum DccType {
    Chat,
    // Send — future extension
}
```

### DccState

```rust
pub enum DccState {
    WaitingUser,   // Incoming request, waiting for user to accept
    Listening,     // Our listen socket open, waiting for peer to connect
    Connecting,    // TCP connect() in progress
    Connected,     // Active chat session
}
```

Records are immediately removed from `DccManager::records` on terminal errors (connect failure, listener timeout). There is no `Failed` state — failed DCC attempts are cleaned up instantly, matching erssi's `dcc_destroy()` behavior.

### DccRecord

```rust
pub struct DccRecord {
    pub id: String,                  // Unique: nick, or nick2/nick3 if multiple
    pub dcc_type: DccType,
    pub nick: String,                // Remote nick
    pub conn_id: String,             // Originating IRC connection ID
    pub addr: IpAddr,                // Remote IP (or fake 1.1.1.1 for passive)
    pub port: u16,                   // Remote port (0 = passive)
    pub state: DccState,
    pub passive_token: Option<u32>,  // For passive DCC matching
    pub created: Instant,            // For timeout tracking
    pub started: Option<Instant>,    // When Connected — for uptime display
    pub bytes_transferred: u64,      // Total bytes for stats
    pub mirc_ctcp: bool,             // mIRC vs irssi CTCP style (default: true, auto-detected on first received CTCP)
}
```

**ID generation:** The DCC record ID is the remote nick (case-insensitive). If a DCC record with that nick already exists, a numeric suffix is appended (`Alice`, `Alice2`, `Alice3`). IDs are compared case-insensitively.

### DccEvent

Sent from DCC async tasks to the main event loop via `dcc_rx`:

```rust
pub enum DccEvent {
    IncomingRequest {
        nick: String,
        conn_id: String,
        addr: IpAddr,
        port: u16,
        passive_token: Option<u32>,
        ident: String,
        host: String,
    },
    ChatConnected { id: String },
    ChatMessage { id: String, text: String },
    ChatAction { id: String, text: String },
    ChatClosed { id: String, reason: Option<String> },
    ChatError { id: String, error: String },
}
```

### DccManager

Lives on `App`, holds all DCC state:

```rust
pub struct DccManager {
    pub records: HashMap<String, DccRecord>,
    pub dcc_tx: mpsc::UnboundedSender<DccEvent>,
    // Per-connection send channels for writing to DCC TCP sockets
    pub chat_senders: HashMap<String, mpsc::UnboundedSender<String>>,
    // Config
    pub timeout_secs: u64,          // Default: 300
    pub port_range: (u16, u16),     // Default: (0, 0) = OS-assigned
    pub own_ip: Option<IpAddr>,     // Override IP in DCC offers
    pub autoaccept_lowports: bool,  // Allow auto-accept from ports < 1024
    pub autochat_masks: Vec<String>,// Hostmask patterns for auto-accept
    pub max_connections: usize,     // Default: 10
}
```

## Buffer Integration

### New BufferType Variant

```rust
pub enum BufferType {
    Server,
    Channel,
    Query,
    DccChat,   // NEW — sorted after Query, before Special
    Special,
}
```

### Buffer Naming Convention (erssi parity)

- Buffer name: `=nick` (e.g., `=Alice`)
- Buffer ID: `{conn_id}/=nick` (e.g., `libera/=alice` — lowercased per `make_buffer_id` convention)
- Display name preserves original nick casing (`=Alice`)
- The `=` prefix distinguishes DCC buffers from regular queries
- `/msg =nick text` sends over DCC TCP, not IRC PRIVMSG

### Message Routing

When the user sends text in a `=nick` buffer:
1. `App` checks `buffer_type == DccChat`
2. Routes to `DccManager::send_chat_line()` instead of IRC sender
3. `/me` in DCC buffer sends `\x01ACTION text\x01` over TCP

### Nick Change Tracking

When IRC NICK change is observed:
- `DccManager::update_nick(old, new)` renames the DCC record
- Buffer renamed from `=oldnick` to `=newnick`
- TCP connection unaffected (it's direct, not IRC-mediated)

## Connection Lifecycle

### Active DCC — We Initiate

1. User: `/dcc chat nick`
2. Bind TCP listener on `0.0.0.0:0` (or configured port range)
3. Send CTCP via IRC: `PRIVMSG nick :\x01DCC CHAT CHAT <our_ip_long> <port>\x01`
4. State: `Listening` — tokio task awaits incoming connection with timeout
5. Peer connects → `Connected` → create `=nick` buffer, switch to it
6. Timeout → close listener, notify user

### Active DCC — We Receive

1. Incoming CTCP `DCC CHAT CHAT <ip> <port>` parsed in `events.rs`
2. `DccEvent::IncomingRequest` sent to main loop
3. State: `WaitingUser` — notification: `"DCC CHAT request from nick [ip:port]"`
4. Auto-accept check: if nick!user@host matches `autochat_masks`, accept immediately
5. Manual accept: user types `/dcc chat nick`
6. Tokio task connects to `ip:port` → `Connected` → create `=nick` buffer
7. Timeout (300s) → discard request silently (no REJECT — prevents CTCP flood amplification)

### Passive DCC — Firewalled Initiator

1. User: `/dcc chat -passive nick`
2. Generate random token (0-63, erssi range)
3. Send: `PRIVMSG nick :\x01DCC CHAT CHAT 16843009 0 <token>\x01`
4. State: `WaitingUser` — waiting for peer's response with matching token
5. Peer responds: `DCC CHAT CHAT <their_ip> <their_port> <token>`
6. Token matched → connect to their IP:port → `Connected`

### Passive DCC — We Respond to Firewalled Peer

1. Incoming `DCC CHAT CHAT <ip> 0 <token>` — port=0 signals passive
2. User accepts → bind listener, respond: `PRIVMSG nick :\x01DCC CHAT CHAT <our_ip> <our_port> <token>\x01`
3. Peer connects → `Connected`

### Cross-Request Auto-Allow

If we have a pending outgoing DCC CHAT (`Listening` state) for a nick and receive an incoming DCC CHAT from that same nick, auto-accept the incoming request and tear down our listener. This prevents deadlock when both sides initiate DCC CHAT simultaneously. Matches erssi behavior.

### IRC Server Disconnect

When the IRC connection (`conn_id`) disconnects, existing DCC CHAT TCP sessions remain active. The `conn_id` field is retained for buffer routing but the DCC peer-to-peer connection is independent of the IRC server.

### ERR_NOSUCHNICK (401) Cleanup

On receiving numeric 401 (No such nick) for a nick with pending DCC requests (`WaitingUser` or `Listening`), automatically close those requests and notify the user.

### Chat Session Protocol

- **Send:** lines terminated with `\n` (LF)
- **Receive:** accept `\n`, `\r\n`, or `\r` as line terminators
- **ACTION:** `\x01ACTION text\x01` — displayed as `* nick text`
- **mIRC CTCP detection:** auto-detect `\x01CMD\x01` (mIRC style) vs `CTCP_MESSAGE \x01CMD\x01` / `CTCP_REPLY \x01CMD\x01` (ircII style)
- **Close:** either side closes TCP → `ChatClosed` event → buffer stays with disconnect message

## IP Address Encoding

### IPv4 — Network-order Long Integer

```
Encode: IP 192.168.1.100 → (192 << 24) | (168 << 16) | (1 << 8) | 100 = 3232235876
Decode: 3232235876 → 192.168.1.100
```

Transmitted as decimal ASCII string in CTCP message.

### IPv6

Standard colon-separated hex notation (e.g., `::1`, `2001:db8::1`).
Detection: presence of `:` in address field = IPv6.

### Own IP Detection

Priority order:
1. `dcc.own_ip` config override (if set)
2. Local address of the IRC TCP socket (from the irc-repartee connection)
3. Fallback: `127.0.0.1` with warning

## Commands

| Command | Description |
|---------|-------------|
| `/dcc chat` | Accept the most recent pending DCC CHAT request (no args) |
| `/dcc chat <nick>` | Initiate DCC CHAT or accept pending request from nick |
| `/dcc chat -passive <nick>` | Initiate passive DCC CHAT (firewalled users) |
| `/dcc close chat <nick>` | Close a DCC CHAT connection |
| `/dcc list` | List all DCC connections with state/duration/bytes |
| `/dcc reject chat <nick>` | Reject pending request and send DCC REJECT CTCP |

### DCC REJECT Wire Format

```
NOTICE nick :\x01DCC REJECT CHAT chat\x01
```

Optional — only sent on explicit `/dcc reject`, never on timeout.

**Note:** Outgoing DCC offers use uppercase `CHAT` for the argument (matching erssi). Incoming parsing is case-insensitive.

## Configuration

| Setting | Type | Default | Description |
|---------|------|---------|-------------|
| `dcc.timeout` | `u64` | `300` | Seconds before unaccepted requests expire |
| `dcc.own_ip` | `String` | `""` | Override IP in DCC offers (empty = auto-detect) |
| `dcc.port_range` | `String` | `"0"` | Port or range for listen sockets |
| `dcc.autoaccept_lowports` | `bool` | `false` | Allow auto-accept from ports < 1024 |
| `dcc.autochat_masks` | `Vec<String>` | `[]` | Hostmask patterns for auto-accept |
| `dcc.max_connections` | `usize` | `10` | Maximum simultaneous DCC connections |

## Main Loop Integration

### New tokio::select! arm

```rust
dcc_ev = self.dcc_rx.recv() => {
    if let Some(ev) = dcc_ev {
        self.handle_dcc_event(ev);
    }
}
```

### Timeout Tick

Piggyback on existing 1-second tick: `DccManager::purge_expired()` — same pattern as batch timeout purge.

### Scripting Events

| Event | Params | Suppressible |
|-------|--------|-------------|
| `dcc.chat.request` | conn_id, nick, ip, port | Yes |
| `dcc.chat.connected` | conn_id, nick | No |
| `dcc.chat.message` | conn_id, nick, text | Yes |
| `dcc.chat.closed` | conn_id, nick, reason | No |

### Storage/Logging

DCC chat messages logged to SQLite via existing `log_tx` with `buffer_id = "{conn_id}/=nick"`.

## Security

- **Port validation:** Reject port < 1 or > 65535
- **Privileged port warning:** Warn (don't auto-accept) for ports < 1024 unless `dcc.autoaccept_lowports`
- **IP exposure:** DCC inherently exposes real IPs. `dcc.own_ip` allows override.
- **Flood protection:** DCC requests are CTCP — covered by existing CTCP flood detection
- **Timeout:** Unaccepted requests cleaned up silently (no REJECT on timeout — prevents amplification)
- **Max connections:** Configurable limit (default 10) prevents resource exhaustion
- **No auto-accept by default:** `autochat_masks` is empty — all requests require manual acceptance

## Test Strategy

- **Unit tests:** IP encoding/decoding, CTCP DCC message parsing, passive token matching, state transitions
- **Integration tests:** Full lifecycle (request → accept → message exchange → close) using tokio test runtime with `TcpListener`/`TcpStream` on localhost
- **Edge cases:** IPv6, nick changes during DCC, simultaneous requests from same nick, timeout expiry, flood during DCC negotiation

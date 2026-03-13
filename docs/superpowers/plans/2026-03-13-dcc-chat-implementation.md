# DCC CHAT Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add full DCC CHAT support with erssi parity — active/passive connections, `=nick` buffers, commands, auto-accept, nick tracking, scripting events.

**Architecture:** New `src/dcc/` module with 4 files (types, protocol, chat, mod). DCC connections run as spawned tokio tasks communicating via dedicated `mpsc` channel (`dcc_tx`/`dcc_rx`). `DccManager` on `App` owns all DCC state. Separate from IRC event flow — new `tokio::select!` arm in main loop.

**Tech Stack:** tokio (TcpListener, TcpStream, BufReader), mpsc channels, existing irc-repartee for CTCP send

**Spec:** `docs/superpowers/specs/2026-03-13-dcc-chat-design.md`

---

## File Structure

| File | Action | Responsibility |
|------|--------|---------------|
| `src/dcc/types.rs` | Create | `DccType`, `DccState`, `DccRecord` enums/structs |
| `src/dcc/protocol.rs` | Create | IP encoding/decoding, CTCP DCC message parsing/building |
| `src/dcc/chat.rs` | Create | Async TCP tasks: listener, connector, line reader/writer |
| `src/dcc/mod.rs` | Create | `DccManager`, `DccEvent`, public API, timeout purge |
| `src/state/buffer.rs` | Modify | Add `DccChat` variant to `BufferType` |
| `src/config/mod.rs` | Modify | Add `DccConfig` struct to `AppConfig` |
| `src/commands/settings.rs` | Modify | Wire `dcc.*` settings for `/set` |
| `src/commands/handlers_dcc.rs` | Create | `/dcc` command handlers |
| `src/commands/registry.rs` | Modify | Register `/dcc` command |
| `src/app.rs` | Modify | Add `DccManager` + `dcc_rx` to `App`, new select arm, message routing |
| `src/irc/events.rs` | Modify | Parse incoming CTCP DCC, emit `DccEvent::IncomingRequest`, nick change hook, 401 cleanup |
| `src/scripting/api.rs` | Modify | Add DCC event constants |
| `src/commands/mod.rs` | Modify | Export `handlers_dcc` |

---

## Chunk 1: Foundation Types & Protocol

### Task 1: DCC Types

**Files:**
- Create: `src/dcc/types.rs`

- [ ] **Step 1: Create `src/dcc/types.rs` with core types**

```rust
use std::net::IpAddr;
use std::time::Instant;

/// DCC sub-protocol type. Currently only Chat; Send can be added later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DccType {
    Chat,
}

/// State machine for a DCC connection lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DccState {
    /// Incoming request received, waiting for user to accept.
    WaitingUser,
    /// Our TCP listener is open, waiting for peer to connect.
    Listening,
    /// Outgoing TCP connect() in progress.
    Connecting,
    /// TCP connected, actively exchanging chat lines.
    Connected,
}

/// A single DCC connection record.
#[derive(Debug, Clone)]
pub struct DccRecord {
    /// Unique ID: nick, or nick2/nick3 if multiple DCC to same nick.
    pub id: String,
    pub dcc_type: DccType,
    /// Remote user's current nick.
    pub nick: String,
    /// IRC connection ID this DCC was initiated from.
    pub conn_id: String,
    /// Remote IP address (fake 1.1.1.1 for outgoing passive).
    pub addr: IpAddr,
    /// Remote port (0 = passive DCC).
    pub port: u16,
    pub state: DccState,
    /// Token for passive/reverse DCC matching.
    pub passive_token: Option<u32>,
    /// When this record was created (for timeout).
    pub created: Instant,
    /// When the TCP connection was established.
    pub started: Option<Instant>,
    /// Total bytes transferred over this connection.
    pub bytes_transferred: u64,
    /// Whether remote uses mIRC CTCP style (default true, auto-detected).
    pub mirc_ctcp: bool,
    /// Remote ident (from original CTCP request).
    pub ident: String,
    /// Remote hostname (from original CTCP request).
    pub host: String,
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check 2>&1 | head -5`
Expected: No errors from `dcc/types.rs` (module not yet wired)

- [ ] **Step 3: Commit**

```bash
git add src/dcc/types.rs
git commit -m "feat(dcc): add DccType, DccState, DccRecord types"
```

---

### Task 2: DCC Protocol — IP Encoding & CTCP Parsing

**Files:**
- Create: `src/dcc/protocol.rs`

This module handles the wire format: encoding/decoding IPv4 as 32-bit integers, and parsing/building CTCP DCC messages.

- [ ] **Step 1: Write tests for IP encoding/decoding**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    // === IP encoding ===

    #[test]
    fn encode_ipv4_localhost() {
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(encode_ip(&ip), "2130706433");
    }

    #[test]
    fn encode_ipv4_192_168_1_100() {
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(encode_ip(&ip), "3232235876");
    }

    #[test]
    fn encode_ipv6() {
        let ip = IpAddr::V6(Ipv6Addr::LOCALHOST);
        assert_eq!(encode_ip(&ip), "::1");
    }

    #[test]
    fn decode_ipv4_long() {
        assert_eq!(
            decode_ip("2130706433").unwrap(),
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))
        );
    }

    #[test]
    fn decode_ipv4_long_192() {
        assert_eq!(
            decode_ip("3232235876").unwrap(),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100))
        );
    }

    #[test]
    fn decode_ipv6_colon() {
        assert_eq!(
            decode_ip("::1").unwrap(),
            IpAddr::V6(Ipv6Addr::LOCALHOST)
        );
    }

    #[test]
    fn decode_ipv6_full() {
        assert_eq!(
            decode_ip("2001:db8::1").unwrap(),
            IpAddr::V6("2001:db8::1".parse().unwrap())
        );
    }

    #[test]
    fn decode_invalid() {
        assert!(decode_ip("not_an_ip").is_err());
    }

    #[test]
    fn encode_fake_passive_ip() {
        // 1.1.1.1 = 16843009 — used as fake IP for passive DCC
        let ip = IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1));
        assert_eq!(encode_ip(&ip), "16843009");
    }
}
```

- [ ] **Step 2: Implement IP encoding/decoding**

```rust
use std::net::{IpAddr, Ipv4Addr};

/// Encode an IP address for DCC CTCP messages.
/// IPv4: 32-bit network-order integer as decimal string.
/// IPv6: standard colon-hex notation.
pub fn encode_ip(ip: &IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            let long = u32::from_be_bytes(octets);
            long.to_string()
        }
        IpAddr::V6(v6) => v6.to_string(),
    }
}

/// Decode an IP address from a DCC CTCP message.
/// If the string contains ':', parse as IPv6. Otherwise parse as u32 → IPv4.
pub fn decode_ip(s: &str) -> Result<IpAddr, String> {
    if s.contains(':') {
        s.parse::<IpAddr>().map_err(|e| e.to_string())
    } else {
        let long: u32 = s.parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
        Ok(IpAddr::V4(Ipv4Addr::from(long.to_be_bytes())))
    }
}
```

- [ ] **Step 3: Run IP tests**

Run: `cargo test -p repartee dcc::protocol::tests -- --nocapture 2>&1 | tail -20`
Expected: All IP tests pass

- [ ] **Step 4: Write tests for CTCP DCC message parsing**

```rust
    // === CTCP DCC parsing ===

    #[test]
    fn parse_active_chat() {
        let msg = parse_dcc_ctcp("DCC CHAT CHAT 3232235876 12345").unwrap();
        assert_eq!(msg.dcc_type, "CHAT");
        assert_eq!(msg.addr, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)));
        assert_eq!(msg.port, 12345);
        assert!(msg.passive_token.is_none());
    }

    #[test]
    fn parse_passive_chat() {
        let msg = parse_dcc_ctcp("DCC CHAT CHAT 16843009 0 42").unwrap();
        assert_eq!(msg.addr, IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)));
        assert_eq!(msg.port, 0);
        assert_eq!(msg.passive_token, Some(42));
    }

    #[test]
    fn parse_lowercase_chat() {
        // Incoming parsing is case-insensitive
        let msg = parse_dcc_ctcp("DCC CHAT chat 2130706433 5000").unwrap();
        assert_eq!(msg.dcc_type, "CHAT");
        assert_eq!(msg.addr, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(msg.port, 5000);
    }

    #[test]
    fn parse_ipv6_chat() {
        let msg = parse_dcc_ctcp("DCC CHAT CHAT ::1 5000").unwrap();
        assert_eq!(msg.addr, IpAddr::V6(Ipv6Addr::LOCALHOST));
        assert_eq!(msg.port, 5000);
    }

    #[test]
    fn parse_not_dcc() {
        assert!(parse_dcc_ctcp("VERSION").is_none());
    }

    #[test]
    fn parse_not_chat() {
        // DCC SEND is not handled (yet)
        assert!(parse_dcc_ctcp("DCC SEND file.txt 2130706433 5000 1024").is_none());
    }

    // === CTCP DCC building ===

    #[test]
    fn build_active_chat() {
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        let ctcp = build_dcc_chat_ctcp(&ip, 12345, None);
        assert_eq!(ctcp, "\x01DCC CHAT CHAT 3232235876 12345\x01");
    }

    #[test]
    fn build_passive_chat() {
        let ip = IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1));
        let ctcp = build_dcc_chat_ctcp(&ip, 0, Some(42));
        assert_eq!(ctcp, "\x01DCC CHAT CHAT 16843009 0 42\x01");
    }

    #[test]
    fn build_reject() {
        assert_eq!(build_dcc_reject(), "\x01DCC REJECT CHAT chat\x01");
    }
```

- [ ] **Step 5: Implement CTCP DCC parsing and building**

```rust
/// Parsed DCC CTCP message fields.
#[derive(Debug, Clone)]
pub struct DccCtcpMessage {
    /// DCC sub-type (e.g. "CHAT"). Normalized to uppercase.
    pub dcc_type: String,
    /// Remote IP address.
    pub addr: IpAddr,
    /// Remote port (0 = passive).
    pub port: u16,
    /// Passive DCC token (present when port == 0 and extra param exists).
    pub passive_token: Option<u32>,
}

/// Parse a CTCP body (without \x01 delimiters) as a DCC request.
/// Returns None if not a DCC message or not a CHAT type.
///
/// Expected formats:
///   `DCC CHAT CHAT <addr> <port>`
///   `DCC CHAT CHAT <addr> 0 <token>`  (passive)
pub fn parse_dcc_ctcp(body: &str) -> Option<DccCtcpMessage> {
    let parts: Vec<&str> = body.split_whitespace().collect();
    if parts.len() < 5 {
        return None;
    }
    if !parts[0].eq_ignore_ascii_case("DCC") {
        return None;
    }
    let dcc_type = parts[1].to_uppercase();
    if dcc_type != "CHAT" {
        return None;
    }
    // parts[2] is the argument ("CHAT" or "chat") — ignored per spec
    let addr = decode_ip(parts[3]).ok()?;
    let port: u16 = parts[4].parse().ok()?;

    let passive_token = if port == 0 && parts.len() >= 6 {
        parts[5].parse::<u32>().ok()
    } else {
        None
    };

    Some(DccCtcpMessage {
        dcc_type,
        addr,
        port,
        passive_token,
    })
}

/// Build a CTCP DCC CHAT message (with \x01 delimiters).
/// For active: `token` is None. For passive: `port` is 0, `token` is Some.
pub fn build_dcc_chat_ctcp(ip: &IpAddr, port: u16, token: Option<u32>) -> String {
    let ip_str = encode_ip(ip);
    if let Some(t) = token {
        format!("\x01DCC CHAT CHAT {ip_str} {port} {t}\x01")
    } else {
        format!("\x01DCC CHAT CHAT {ip_str} {port}\x01")
    }
}

/// Build a CTCP DCC REJECT message for CHAT (with \x01 delimiters).
pub fn build_dcc_reject() -> String {
    "\x01DCC REJECT CHAT chat\x01".to_string()
}

/// The fake IP address used for outgoing passive DCC (1.1.1.1 as u32).
pub const PASSIVE_FAKE_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1));
```

- [ ] **Step 6: Run all protocol tests**

Run: `cargo test -p repartee dcc::protocol::tests -- --nocapture 2>&1 | tail -20`
Expected: All tests pass

- [ ] **Step 7: Commit**

```bash
git add src/dcc/protocol.rs
git commit -m "feat(dcc): IP encoding/decoding and CTCP DCC message parsing"
```

---

### Task 3: DCC Module Stub & DccManager

**Files:**
- Create: `src/dcc/mod.rs`
- Modify: `src/main.rs` or `src/lib.rs` — add `mod dcc;`

- [ ] **Step 1: Create `src/dcc/mod.rs` with DccManager and DccEvent**

```rust
pub mod chat;
pub mod protocol;
pub mod types;

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Instant;

use tokio::sync::mpsc;

use types::{DccRecord, DccState, DccType};

/// Events sent from DCC async tasks to the main event loop.
#[derive(Debug)]
pub enum DccEvent {
    /// A CTCP DCC CHAT request was received from the IRC connection.
    IncomingRequest {
        nick: String,
        conn_id: String,
        addr: IpAddr,
        port: u16,
        passive_token: Option<u32>,
        ident: String,
        host: String,
    },
    /// DCC CHAT TCP connection established.
    ChatConnected { id: String },
    /// A line of text received over DCC CHAT.
    ChatMessage { id: String, text: String },
    /// A CTCP ACTION received over DCC CHAT.
    ChatAction { id: String, text: String },
    /// DCC CHAT connection closed.
    ChatClosed { id: String, reason: Option<String> },
    /// DCC CHAT connection error.
    ChatError { id: String, error: String },
}

/// Manages all DCC connections and state.
pub struct DccManager {
    /// All DCC records keyed by ID (case-insensitive nick, with numeric suffixes for duplicates).
    pub records: HashMap<String, DccRecord>,
    /// Sender cloned into spawned DCC tasks so they can send events to the main loop.
    pub dcc_tx: mpsc::UnboundedSender<DccEvent>,
    /// Per-DCC send channels: main loop writes here, the TCP writer task reads.
    pub chat_senders: HashMap<String, mpsc::UnboundedSender<String>>,
    // --- Config ---
    pub timeout_secs: u64,
    pub port_range: (u16, u16),
    pub own_ip: Option<IpAddr>,
    pub autoaccept_lowports: bool,
    pub autochat_masks: Vec<String>,
    pub max_connections: usize,
}

impl DccManager {
    /// Create a new DccManager. Returns (manager, receiver).
    pub fn new() -> (Self, mpsc::UnboundedReceiver<DccEvent>) {
        let (dcc_tx, dcc_rx) = mpsc::unbounded_channel();
        let manager = Self {
            records: HashMap::new(),
            dcc_tx,
            chat_senders: HashMap::new(),
            timeout_secs: 300,
            port_range: (0, 0),
            own_ip: None,
            autoaccept_lowports: false,
            autochat_masks: Vec::new(),
            max_connections: 10,
        };
        (manager, dcc_rx)
    }

    /// Generate a unique DCC record ID for a nick.
    /// Returns "nick" if unused, otherwise "nick2", "nick3", etc.
    pub fn generate_id(&self, nick: &str) -> String {
        let base = nick.to_lowercase();
        if !self.records.contains_key(&base) {
            return base;
        }
        let mut suffix = 2u32;
        loop {
            let candidate = format!("{base}{suffix}");
            if !self.records.contains_key(&candidate) {
                return candidate;
            }
            suffix += 1;
        }
    }

    /// Find a pending DCC record for a nick (WaitingUser or Listening state).
    pub fn find_pending(&self, nick: &str) -> Option<&DccRecord> {
        let nick_lower = nick.to_lowercase();
        self.records.values().find(|r| {
            r.nick.to_lowercase() == nick_lower
                && matches!(r.state, DccState::WaitingUser | DccState::Listening)
        })
    }

    /// Find the most recent pending incoming DCC request (for `/dcc chat` with no args).
    pub fn find_latest_pending(&self) -> Option<&DccRecord> {
        self.records
            .values()
            .filter(|r| r.state == DccState::WaitingUser)
            .max_by_key(|r| r.created)
    }

    /// Find a connected DCC CHAT record for a nick.
    pub fn find_connected(&self, nick: &str) -> Option<&DccRecord> {
        let nick_lower = nick.to_lowercase();
        self.records.values().find(|r| {
            r.nick.to_lowercase() == nick_lower && r.state == DccState::Connected
        })
    }

    /// Remove expired DCC records (created > timeout_secs ago, still in WaitingUser or Listening).
    /// Returns IDs of purged records for UI notification.
    pub fn purge_expired(&mut self) -> Vec<(String, String)> {
        let now = Instant::now();
        let timeout = std::time::Duration::from_secs(self.timeout_secs);
        let expired: Vec<(String, String)> = self
            .records
            .iter()
            .filter(|(_, r)| {
                matches!(r.state, DccState::WaitingUser | DccState::Listening)
                    && now.duration_since(r.created) > timeout
            })
            .map(|(id, r)| (id.clone(), r.nick.clone()))
            .collect();
        for (id, _) in &expired {
            self.records.remove(id);
            self.chat_senders.remove(id);
        }
        expired
    }

    /// Update nick on all DCC records when an IRC NICK change is observed.
    /// Returns list of (old_buffer_suffix, new_buffer_suffix) for buffer renaming.
    pub fn update_nick(&mut self, old_nick: &str, new_nick: &str) -> Vec<(String, String)> {
        let old_lower = old_nick.to_lowercase();
        let mut renames = Vec::new();
        // Collect IDs to modify (can't mutate while iterating)
        let ids: Vec<String> = self
            .records
            .iter()
            .filter(|(_, r)| r.nick.to_lowercase() == old_lower)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(rec) = self.records.get_mut(&id) {
                let old_buf = format!("={}", rec.nick);
                rec.nick = new_nick.to_string();
                let new_buf = format!("={new_nick}");
                renames.push((old_buf, new_buf));
            }
        }
        renames
    }

    /// Close and remove a DCC record by nick. Returns the removed record if found.
    pub fn close_by_nick(&mut self, nick: &str) -> Option<DccRecord> {
        let nick_lower = nick.to_lowercase();
        let id = self
            .records
            .iter()
            .find(|(_, r)| r.nick.to_lowercase() == nick_lower)
            .map(|(id, _)| id.clone())?;
        self.chat_senders.remove(&id);
        self.records.remove(&id)
    }

    /// Close and remove a DCC record by ID.
    pub fn close_by_id(&mut self, id: &str) -> Option<DccRecord> {
        self.chat_senders.remove(id);
        self.records.remove(id)
    }

    /// Send a line of text over an active DCC CHAT connection.
    pub fn send_chat_line(&self, id: &str, text: &str) -> Result<(), String> {
        let sender = self
            .chat_senders
            .get(id)
            .ok_or_else(|| format!("No active DCC chat: {id}"))?;
        sender
            .send(text.to_string())
            .map_err(|e| format!("DCC send failed: {e}"))
    }

    /// Check if auto-accept should apply for this ident@host.
    pub fn should_auto_accept(&self, nick: &str, ident: &str, host: &str, port: u16) -> bool {
        if port < 1024 && !self.autoaccept_lowports {
            return false;
        }
        if self.autochat_masks.is_empty() {
            return false;
        }
        let mask_target = format!("{nick}!{ident}@{host}");
        self.autochat_masks.iter().any(|mask| {
            crate::irc::ignore::wildcard_match(mask, &mask_target)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn make_record(nick: &str, state: DccState) -> DccRecord {
        DccRecord {
            id: nick.to_lowercase(),
            dcc_type: DccType::Chat,
            nick: nick.to_string(),
            conn_id: "test".to_string(),
            addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 12345,
            state,
            passive_token: None,
            created: Instant::now(),
            started: None,
            bytes_transferred: 0,
            mirc_ctcp: true,
            ident: "user".to_string(),
            host: "host.com".to_string(),
        }
    }

    #[test]
    fn generate_id_unique() {
        let (mut mgr, _rx) = DccManager::new();
        assert_eq!(mgr.generate_id("Alice"), "alice");
        mgr.records.insert("alice".to_string(), make_record("Alice", DccState::Connected));
        assert_eq!(mgr.generate_id("Alice"), "alice2");
        mgr.records.insert("alice2".to_string(), make_record("Alice", DccState::Connected));
        assert_eq!(mgr.generate_id("Alice"), "alice3");
    }

    #[test]
    fn find_pending_by_nick() {
        let (mut mgr, _rx) = DccManager::new();
        mgr.records.insert("bob".to_string(), make_record("Bob", DccState::WaitingUser));
        assert!(mgr.find_pending("Bob").is_some());
        assert!(mgr.find_pending("bob").is_some());
        assert!(mgr.find_pending("Charlie").is_none());
    }

    #[test]
    fn find_latest_pending() {
        let (mut mgr, _rx) = DccManager::new();
        let mut r1 = make_record("Alice", DccState::WaitingUser);
        r1.created = Instant::now() - std::time::Duration::from_secs(10);
        mgr.records.insert("alice".to_string(), r1);
        let r2 = make_record("Bob", DccState::WaitingUser);
        mgr.records.insert("bob".to_string(), r2);
        assert_eq!(mgr.find_latest_pending().unwrap().nick, "Bob");
    }

    #[test]
    fn update_nick_renames() {
        let (mut mgr, _rx) = DccManager::new();
        mgr.records.insert("alice".to_string(), make_record("Alice", DccState::Connected));
        let renames = mgr.update_nick("Alice", "Alice_");
        assert_eq!(renames.len(), 1);
        assert_eq!(renames[0], ("=Alice".to_string(), "=Alice_".to_string()));
        assert_eq!(mgr.records.get("alice").unwrap().nick, "Alice_");
    }

    #[test]
    fn close_by_nick_removes() {
        let (mut mgr, _rx) = DccManager::new();
        mgr.records.insert("alice".to_string(), make_record("Alice", DccState::Connected));
        assert!(mgr.close_by_nick("Alice").is_some());
        assert!(mgr.records.is_empty());
    }

    #[test]
    fn purge_expired_removes_old() {
        let (mut mgr, _rx) = DccManager::new();
        mgr.timeout_secs = 1; // 1 second timeout for test
        let mut r = make_record("Alice", DccState::WaitingUser);
        r.created = Instant::now() - std::time::Duration::from_secs(5);
        mgr.records.insert("alice".to_string(), r);
        // Connected records should NOT be purged
        mgr.records.insert("bob".to_string(), make_record("Bob", DccState::Connected));
        let expired = mgr.purge_expired();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].1, "Alice");
        assert!(mgr.records.contains_key("bob")); // Bob still there
    }
}
```

- [ ] **Step 2: Create empty `src/dcc/chat.rs` stub**

```rust
//! DCC CHAT async TCP tasks: listener, connector, and line reader/writer.

// Implementation in Task 7 (Chunk 2).
```

- [ ] **Step 3: Wire `mod dcc` into the crate**

Find where other top-level modules are declared (likely `src/main.rs` or `src/lib.rs`) and add `mod dcc;`.

Run: `grep -n "^mod " src/main.rs` to find the module declarations, then add `mod dcc;` in the appropriate place.

- [ ] **Step 4: Run tests**

Run: `cargo test -p repartee dcc:: -- --nocapture 2>&1 | tail -30`
Expected: All DCC tests pass (types, protocol, manager)

- [ ] **Step 5: Commit**

```bash
git add src/dcc/mod.rs src/dcc/chat.rs
git commit -m "feat(dcc): DccManager with record management, ID generation, timeout purge"
```

---

### Task 4: BufferType::DccChat Variant

**Files:**
- Modify: `src/state/buffer.rs:8-24` — add `DccChat` variant and sort group

- [ ] **Step 1: Add `DccChat` to `BufferType` enum**

In `src/state/buffer.rs`, add `DccChat` between `Query` and `Special`:

```rust
pub enum BufferType {
    Server,
    Channel,
    Query,
    DccChat,
    Special,
}
```

Update `sort_group()`:
```rust
pub const fn sort_group(&self) -> u8 {
    match self {
        Self::Server => 1,
        Self::Channel => 2,
        Self::Query => 3,
        Self::DccChat => 4,
        Self::Special => 5,
    }
}
```

- [ ] **Step 2: Fix all exhaustive match errors**

Run: `cargo check 2>&1 | grep "non-exhaustive"` and fix every match arm. Known locations:

- `src/commands/handlers_ui.rs:~178` — `/close` command: add `DccChat` arm that closes the DCC connection via `DccManager::close_by_nick` then removes buffer (similar to `Query`)
- `src/state/mod.rs:~62` — script snapshot `BufferType` → string mapping: add `DccChat => "dcc_chat"`
- `src/state/events.rs` — activity level logic: `DccChat` behaves like `Query`
- `src/ui/` — sidepanel, chat view rendering: `DccChat` behaves like `Query`
- `src/app.rs:~3030` — `handle_plain_message`: add `DccChat` to allowed types (Task 8 does this)

For most matches, `DccChat` should behave like `Query` (it's a 1:1 chat buffer). Add `DccChat` alongside `Query`:
```rust
BufferType::Channel | BufferType::Query | BufferType::DccChat => { ... }
```

- [ ] **Step 3: Update existing sort test**

In `src/state/buffer.rs` tests, update the sort ordering test:
```rust
assert!(BufferType::Query.sort_group() < BufferType::DccChat.sort_group());
assert!(BufferType::DccChat.sort_group() < BufferType::Special.sort_group());
```

- [ ] **Step 4: Run full test suite**

Run: `cargo test 2>&1 | tail -5`
Expected: All tests pass, 0 clippy warnings

- [ ] **Step 5: Commit**

```bash
git add src/state/buffer.rs
# Include any other files modified for exhaustive matches
git commit -m "feat(dcc): add BufferType::DccChat variant"
```

---

### Task 5: DCC Config

**Files:**
- Modify: `src/config/mod.rs` — add `DccConfig` struct
- Modify: `src/commands/settings.rs` — wire `/set dcc.*` settings

- [ ] **Step 1: Add `DccConfig` struct to `src/config/mod.rs`**

Add after `LoggingConfig`:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DccConfig {
    /// Seconds before unaccepted DCC requests expire.
    pub timeout: u64,
    /// Override IP address sent in DCC offers (empty = auto-detect from IRC socket).
    pub own_ip: String,
    /// Port or range for DCC listen sockets. "0" = OS-assigned, "1025 65535" = range.
    pub port_range: String,
    /// Allow auto-accepting DCC from privileged ports (< 1024).
    pub autoaccept_lowports: bool,
    /// Hostmask patterns for auto-accepting DCC CHAT (e.g. "*!*@trusted.host").
    pub autochat_masks: Vec<String>,
    /// Maximum simultaneous DCC connections.
    pub max_connections: usize,
}

impl Default for DccConfig {
    fn default() -> Self {
        Self {
            timeout: 300,
            own_ip: String::new(),
            port_range: "0".to_string(),
            autoaccept_lowports: false,
            autochat_masks: Vec::new(),
            max_connections: 10,
        }
    }
}
```

Add to `AppConfig`:
```rust
pub struct AppConfig {
    // ... existing fields ...
    pub dcc: DccConfig,
}
```

- [ ] **Step 2: Wire `/set dcc.*` in `src/commands/settings.rs`**

Add DCC settings to the get/set handlers and the settings list. Follow the existing pattern used for `logging.*` or `image_preview.*` settings.

Settings to wire:
- `dcc.timeout` (u64)
- `dcc.own_ip` (String)
- `dcc.port_range` (String)
- `dcc.autoaccept_lowports` (bool)
- `dcc.max_connections` (usize)

- [ ] **Step 3: Verify compilation and existing tests pass**

Run: `cargo test 2>&1 | tail -5`
Expected: All tests pass

- [ ] **Step 4: Commit**

```bash
git add src/config/mod.rs src/commands/settings.rs
git commit -m "feat(dcc): add DccConfig with /set dcc.* settings"
```

---

## Chunk 2: Async TCP & Chat Connection Tasks

### Task 6: DCC Chat Async Tasks

**Files:**
- Modify: `src/dcc/chat.rs` — implement TCP listener, connector, line reader/writer tasks

This is the core async TCP code. Each DCC CHAT connection spawns two tokio tasks:
1. **Reader task**: reads lines from the TCP socket, parses ACTIONs, sends `DccEvent` to main loop
2. **Writer task**: receives lines from `chat_sender` channel, writes to TCP socket

- [ ] **Step 1: Implement the TCP listener task (for active DCC we initiate)**

```rust
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use super::DccEvent;

/// Accept one incoming connection on an already-bound TCP listener.
/// The caller binds the listener, extracts the port for the CTCP offer,
/// then passes the listener here. On connect, spawns reader/writer tasks.
/// On timeout, sends `ChatError`.
pub async fn listen_for_chat(
    id: String,
    listener: TcpListener,
    timeout: Duration,
    event_tx: mpsc::UnboundedSender<DccEvent>,
    line_rx: mpsc::UnboundedReceiver<String>,
) {
    // Wait for incoming connection with timeout
    let accept_result = tokio::time::timeout(timeout, listener.accept()).await;
    match accept_result {
        Ok(Ok((stream, _peer_addr))) => {
            let _ = event_tx.send(DccEvent::ChatConnected { id: id.clone() });
            run_chat_session(id, stream, event_tx, line_rx).await;
        }
        Ok(Err(e)) => {
            let _ = event_tx.send(DccEvent::ChatError {
                id,
                error: format!("DCC accept failed: {e}"),
            });
        }
        Err(_) => {
            let _ = event_tx.send(DccEvent::ChatError {
                id,
                error: "DCC CHAT request timed out".to_string(),
            });
        }
    }
}

/// Spawn a TCP connector for active DCC CHAT (we accept an incoming request).
pub async fn connect_for_chat(
    id: String,
    addr: SocketAddr,
    timeout: Duration,
    event_tx: mpsc::UnboundedSender<DccEvent>,
    line_rx: mpsc::UnboundedReceiver<String>,
) {
    let connect_result = tokio::time::timeout(timeout, TcpStream::connect(addr)).await;
    match connect_result {
        Ok(Ok(stream)) => {
            let _ = event_tx.send(DccEvent::ChatConnected { id: id.clone() });
            run_chat_session(id, stream, event_tx, line_rx).await;
        }
        Ok(Err(e)) => {
            let _ = event_tx.send(DccEvent::ChatError {
                id,
                error: format!("DCC connect failed: {e}"),
            });
        }
        Err(_) => {
            let _ = event_tx.send(DccEvent::ChatError {
                id,
                error: "DCC connect timed out".to_string(),
            });
        }
    }
}

/// Run the chat session: read lines from socket, write lines from channel.
/// This function returns when the connection is closed by either side.
async fn run_chat_session(
    id: String,
    stream: TcpStream,
    event_tx: mpsc::UnboundedSender<DccEvent>,
    mut line_rx: mpsc::UnboundedReceiver<String>,
) {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);

    let read_id = id.clone();
    let read_tx = event_tx.clone();

    // Reader task
    let reader_handle = tokio::spawn(async move {
        let mut line_buf = String::new();
        loop {
            line_buf.clear();
            match buf_reader.read_line(&mut line_buf).await {
                Ok(0) => {
                    // EOF — peer closed connection
                    let _ = read_tx.send(DccEvent::ChatClosed {
                        id: read_id,
                        reason: None,
                    });
                    return;
                }
                Ok(_) => {
                    let line = line_buf.trim_end_matches(['\r', '\n']).to_string();
                    if line.is_empty() {
                        continue;
                    }
                    // Check for CTCP ACTION
                    if line.starts_with('\x01') && line.ends_with('\x01') && line.len() > 2 {
                        let inner = &line[1..line.len() - 1];
                        if let Some(action_text) = inner.strip_prefix("ACTION ") {
                            let _ = read_tx.send(DccEvent::ChatAction {
                                id: read_id.clone(),
                                text: action_text.to_string(),
                            });
                            continue;
                        }
                    }
                    let _ = read_tx.send(DccEvent::ChatMessage {
                        id: read_id.clone(),
                        text: line,
                    });
                }
                Err(e) => {
                    let _ = read_tx.send(DccEvent::ChatClosed {
                        id: read_id,
                        reason: Some(e.to_string()),
                    });
                    return;
                }
            }
        }
    });

    // Writer task: drain line_rx channel, write to socket
    let write_id = id.clone();
    let write_tx = event_tx;
    let writer_handle = tokio::spawn(async move {
        while let Some(line) = line_rx.recv().await {
            let data = format!("{line}\n");
            if let Err(e) = writer.write_all(data.as_bytes()).await {
                let _ = write_tx.send(DccEvent::ChatClosed {
                    id: write_id,
                    reason: Some(format!("Write error: {e}")),
                });
                return;
            }
            if let Err(e) = writer.flush().await {
                let _ = write_tx.send(DccEvent::ChatClosed {
                    id: write_id,
                    reason: Some(format!("Flush error: {e}")),
                });
                return;
            }
        }
        // Channel closed — main loop dropped the sender (intentional close)
    });

    // Wait for either task to finish, then abort the other
    tokio::select! {
        _ = reader_handle => {
            writer_handle.abort();
        }
        _ = writer_handle => {
            // Writer finished (channel closed or error) — reader will get EOF soon
        }
    }
}

/// Get the actual bound port from a listener address string "0.0.0.0:0" after binding.
/// Used when port_range is (0,0) so we need the OS-assigned port.
pub fn parse_port_range(s: &str) -> (u16, u16) {
    let s = s.trim();
    if s == "0" || s.is_empty() {
        return (0, 0);
    }
    // Try "start end" or "start-end"
    let parts: Vec<&str> = s.split([' ', '-']).collect();
    if parts.len() == 2 {
        if let (Ok(a), Ok(b)) = (parts[0].parse::<u16>(), parts[1].parse::<u16>()) {
            return (a, b);
        }
    }
    // Single port
    if let Ok(p) = s.parse::<u16>() {
        return (p, p);
    }
    (0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_port_range_zero() {
        assert_eq!(parse_port_range("0"), (0, 0));
        assert_eq!(parse_port_range(""), (0, 0));
    }

    #[test]
    fn parse_port_range_single() {
        assert_eq!(parse_port_range("5000"), (5000, 5000));
    }

    #[test]
    fn parse_port_range_space() {
        assert_eq!(parse_port_range("1025 65535"), (1025, 65535));
    }

    #[test]
    fn parse_port_range_dash() {
        assert_eq!(parse_port_range("1025-65535"), (1025, 65535));
    }

    #[tokio::test]
    async fn chat_session_connect_and_exchange() {
        // Spin up a listener, connect to it, exchange one message each direction
        let server_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = server_listener.local_addr().unwrap();

        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let (line_tx, line_rx) = mpsc::unbounded_channel();

        // Spawn connector (client side)
        let tx2 = event_tx.clone();
        let (line_tx2, line_rx2) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            connect_for_chat(
                "test".to_string(),
                addr,
                Duration::from_secs(5),
                tx2,
                line_rx2,
            )
            .await;
        });

        // Accept (server side) — uses the pre-bound listener
        let (stream, _) = server_listener.accept().await.unwrap();
        tokio::spawn(async move {
            run_chat_session("server".to_string(), stream, event_tx, line_rx).await;
        });

        // Wait for ChatConnected from connector side
        let ev = event_rx.recv().await.unwrap();
        assert!(matches!(ev, DccEvent::ChatConnected { .. }));

        // Send a message from "server" side
        line_tx.send("Hello from server".to_string()).unwrap();

        // Receive it on connector side
        let ev = event_rx.recv().await.unwrap();
        match ev {
            DccEvent::ChatMessage { text, .. } => assert_eq!(text, "Hello from server"),
            other => panic!("Expected ChatMessage, got {other:?}"),
        }

        // Send a message from connector side
        line_tx2.send("\x01ACTION waves\x01".to_string()).unwrap();

        // Receive it on server side (as ACTION)
        let ev = event_rx.recv().await.unwrap();
        match ev {
            DccEvent::ChatAction { text, .. } => assert_eq!(text, "waves"),
            other => panic!("Expected ChatAction, got {other:?}"),
        }

        // Close by dropping sender
        drop(line_tx);
        // Should get a ChatClosed
        let mut found_close = false;
        for _ in 0..5 {
            if let Ok(ev) = tokio::time::timeout(Duration::from_secs(1), event_rx.recv()).await {
                if let Some(DccEvent::ChatClosed { .. }) = ev {
                    found_close = true;
                    break;
                }
            }
        }
        assert!(found_close, "Expected ChatClosed event");
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p repartee dcc::chat::tests -- --nocapture 2>&1 | tail -20`
Expected: All chat tests pass

- [ ] **Step 3: Commit**

```bash
git add src/dcc/chat.rs
git commit -m "feat(dcc): async TCP listener, connector, and line reader/writer tasks"
```

---

## Chunk 3: Commands & App Integration

### Task 7: DCC Command Handlers

**Files:**
- Create: `src/commands/handlers_dcc.rs`
- Modify: `src/commands/mod.rs` — export `handlers_dcc`
- Modify: `src/commands/registry.rs` — register `/dcc` command

- [ ] **Step 1: Create `src/commands/handlers_dcc.rs`**

The `/dcc` command dispatches to subcommands: `chat`, `close`, `list`, `reject`.

```rust
use crate::app::App;
use crate::commands::helpers::add_local_event;

/// Main `/dcc` command dispatcher.
pub(crate) fn cmd_dcc(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /dcc <chat|close|list|reject> [args...]");
        return;
    }

    let subcmd = args[0].to_lowercase();
    let sub_args = &args[1..];

    match subcmd.as_str() {
        "chat" => cmd_dcc_chat(app, sub_args),
        "close" => cmd_dcc_close(app, sub_args),
        "list" => cmd_dcc_list(app),
        "reject" => cmd_dcc_reject(app, sub_args),
        _ => add_local_event(app, &format!("Unknown DCC command: {subcmd}")),
    }
}

/// `/dcc chat [nick]` or `/dcc chat -passive nick`
/// - No args: accept the most recent pending request
/// - With nick: accept pending request from nick, or initiate new DCC CHAT
/// - With -passive nick: initiate passive DCC CHAT
fn cmd_dcc_chat(app: &mut App, args: &[String]) {
    let passive = args.first().is_some_and(|a| a == "-passive");
    let nick_args = if passive { &args[1..] } else { args };

    // No args: accept latest pending
    if nick_args.is_empty() && !passive {
        let Some(record) = app.dcc.find_latest_pending() else {
            add_local_event(app, "No pending DCC CHAT requests");
            return;
        };
        let nick = record.nick.clone();
        let id = record.id.clone();
        accept_dcc_chat(app, &nick, &id);
        return;
    }

    let Some(nick) = nick_args.first() else {
        add_local_event(app, "Usage: /dcc chat [-passive] <nick>");
        return;
    };

    // Check if there's a pending request from this nick to accept
    if !passive {
        if let Some(record) = app.dcc.find_pending(nick) {
            let id = record.id.clone();
            let nick = record.nick.clone();
            accept_dcc_chat(app, &nick, &id);
            return;
        }
    }

    // Initiate new DCC CHAT
    initiate_dcc_chat(app, nick, passive);
}

/// Accept a pending DCC CHAT request.
fn accept_dcc_chat(app: &mut App, nick: &str, id: &str) {
    use crate::dcc::types::DccState;
    use std::net::SocketAddr;
    use std::time::Duration;

    let Some(record) = app.dcc.records.get_mut(id) else {
        add_local_event(app, &format!("DCC record not found: {id}"));
        return;
    };

    let addr = record.addr;
    let port = record.port;
    let is_passive = record.passive_token.is_some() && port == 0;
    let dcc_id = id.to_string();

    if is_passive {
        // We need to listen and send our address back
        record.state = DccState::Listening;
        let conn_id = record.conn_id.clone();
        let token = record.passive_token.unwrap();
        let nick_owned = nick.to_string();
        let timeout = Duration::from_secs(app.dcc.timeout_secs);
        let event_tx = app.dcc.dcc_tx.clone();
        let (line_tx, line_rx) = tokio::sync::mpsc::unbounded_channel();
        app.dcc.chat_senders.insert(dcc_id.clone(), line_tx);

        // Determine our IP (config override > IRC socket > fallback)
        let own_ip = resolve_own_ip(app);
        let bind_port = pick_bind_port(app.dcc.port_range);
        let bind_addr: SocketAddr = (std::net::Ipv4Addr::UNSPECIFIED, bind_port).into();

        let irc_sender = app.active_irc_sender().cloned();

        tokio::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(bind_addr).await {
                Ok(l) => l,
                Err(e) => {
                    let _ = event_tx.send(crate::dcc::DccEvent::ChatError {
                        id: dcc_id,
                        error: format!("Failed to bind: {e}"),
                    });
                    return;
                }
            };
            let local_port = listener.local_addr().map(|a| a.port()).unwrap_or(0);

            // Send our address back via IRC
            if let Some(sender) = irc_sender {
                let ctcp = crate::dcc::protocol::build_dcc_chat_ctcp(&own_ip, local_port, Some(token));
                let _ = sender.send_privmsg(&nick_owned, &ctcp);
            }

            // Wait for peer to connect (pass pre-bound listener, NOT re-bind)
            crate::dcc::chat::listen_for_chat(dcc_id, listener, timeout, event_tx, line_rx).await;
        });
    } else {
        // Active accept: connect to peer's address
        record.state = DccState::Connecting;
        let target = SocketAddr::new(addr, port);
        let timeout = Duration::from_secs(app.dcc.timeout_secs);
        let event_tx = app.dcc.dcc_tx.clone();
        let (line_tx, line_rx) = tokio::sync::mpsc::unbounded_channel();
        app.dcc.chat_senders.insert(dcc_id.clone(), line_tx);

        tokio::spawn(async move {
            crate::dcc::chat::connect_for_chat(dcc_id, target, timeout, event_tx, line_rx).await;
        });
    }

    add_local_event(app, &format!("DCC CHAT: connecting to {nick}..."));
}

/// Initiate a new DCC CHAT to a nick.
fn initiate_dcc_chat(app: &mut App, nick: &str, passive: bool) {
    use crate::dcc::types::{DccRecord, DccState, DccType};
    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::{Duration, Instant};

    // Check max connections
    if app.dcc.records.len() >= app.dcc.max_connections {
        add_local_event(app, "Maximum DCC connections reached");
        return;
    }

    let Some(conn_id) = app.active_conn_id().map(str::to_owned) else {
        add_local_event(app, "No active IRC connection");
        return;
    };

    let dcc_id = app.dcc.generate_id(nick);

    if passive {
        // Passive DCC: send request with port 0 and a token
        let token = rand::random::<u32>() % 64;
        let fake_ip = crate::dcc::protocol::PASSIVE_FAKE_IP;

        let record = DccRecord {
            id: dcc_id.clone(),
            dcc_type: DccType::Chat,
            nick: nick.to_string(),
            conn_id: conn_id.clone(),
            addr: fake_ip,
            port: 0,
            state: DccState::WaitingUser,
            passive_token: Some(token),
            created: Instant::now(),
            started: None,
            bytes_transferred: 0,
            mirc_ctcp: true,
            ident: String::new(),
            host: String::new(),
        };
        app.dcc.records.insert(dcc_id, record);

        // Send CTCP via IRC
        if let Some(sender) = app.active_irc_sender() {
            let ctcp = crate::dcc::protocol::build_dcc_chat_ctcp(&fake_ip, 0, Some(token));
            let _ = sender.send_privmsg(nick, &ctcp);
        }

        add_local_event(app, &format!("DCC CHAT: sent passive request to {nick}"));
    } else {
        // Active DCC: bind listener, send our address
        let own_ip = resolve_own_ip(app);
        let bind_port = pick_bind_port(app.dcc.port_range);
        let bind_addr: SocketAddr = (Ipv4Addr::UNSPECIFIED, bind_port).into();
        let timeout = Duration::from_secs(app.dcc.timeout_secs);
        let event_tx = app.dcc.dcc_tx.clone();
        let (line_tx, line_rx) = tokio::sync::mpsc::unbounded_channel();

        let nick_owned = nick.to_string();
        let irc_sender = app.active_irc_sender().cloned();

        let record = DccRecord {
            id: dcc_id.clone(),
            dcc_type: DccType::Chat,
            nick: nick.to_string(),
            conn_id: conn_id.clone(),
            addr: own_ip,
            port: 0, // Will be filled after bind
            state: DccState::Listening,
            passive_token: None,
            created: Instant::now(),
            started: None,
            bytes_transferred: 0,
            mirc_ctcp: true,
            ident: String::new(),
            host: String::new(),
        };
        app.dcc.records.insert(dcc_id.clone(), record);
        app.dcc.chat_senders.insert(dcc_id.clone(), line_tx);

        tokio::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(bind_addr).await {
                Ok(l) => l,
                Err(e) => {
                    let _ = event_tx.send(crate::dcc::DccEvent::ChatError {
                        id: dcc_id,
                        error: format!("Failed to bind: {e}"),
                    });
                    return;
                }
            };
            let local_port = listener.local_addr().map(|a| a.port()).unwrap_or(0);

            // Send CTCP via IRC with our address
            if let Some(sender) = irc_sender {
                let ctcp = crate::dcc::protocol::build_dcc_chat_ctcp(&own_ip, local_port, None);
                let _ = sender.send_privmsg(&nick_owned, &ctcp);
            }

            // Pass pre-bound listener (NOT re-bind)
            crate::dcc::chat::listen_for_chat(
                dcc_id,
                listener,
                timeout,
                event_tx,
                line_rx,
            )
            .await;
        });

        add_local_event(app, &format!("DCC CHAT: waiting for {nick} to connect..."));
    }
}

/// Resolve own IP for DCC offers.
/// Priority: config override > IRC socket local address > 127.0.0.1 fallback with warning.
fn resolve_own_ip(app: &App) -> std::net::IpAddr {
    if let Some(ip) = app.dcc.own_ip {
        return ip;
    }
    // TODO: extract local IP from active IRC connection's TCP socket if available
    // For now, fall back — implementer should wire this from the irc-repartee client's local_addr
    tracing::warn!("DCC: using 127.0.0.1 — set dcc.own_ip for remote connections");
    std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
}

/// Pick a bind port from the configured port range. Returns 0 for OS-assigned.
fn pick_bind_port(range: (u16, u16)) -> u16 {
    if range == (0, 0) {
        return 0;
    }
    if range.0 == range.1 {
        return range.0;
    }
    // Random port in range
    range.0 + (rand::random::<u16>() % (range.1 - range.0 + 1))
}

/// `/dcc close chat <nick>` — close a DCC CHAT connection.
fn cmd_dcc_close(app: &mut App, args: &[String]) {
    // Expected: /dcc close chat <nick>
    if args.len() < 2 || !args[0].eq_ignore_ascii_case("chat") {
        add_local_event(app, "Usage: /dcc close chat <nick>");
        return;
    }
    let nick = &args[1];
    if let Some(record) = app.dcc.close_by_nick(nick) {
        add_local_event(app, &format!("DCC CHAT with {} closed", record.nick));
    } else {
        add_local_event(app, &format!("No DCC CHAT with {nick}"));
    }
}

/// `/dcc list` — list all DCC connections.
fn cmd_dcc_list(app: &mut App) {
    if app.dcc.records.is_empty() {
        add_local_event(app, "No DCC connections");
        return;
    }
    add_local_event(app, "DCC connections:");
    for record in app.dcc.records.values() {
        let state = match record.state {
            crate::dcc::types::DccState::WaitingUser => "waiting",
            crate::dcc::types::DccState::Listening => "listening",
            crate::dcc::types::DccState::Connecting => "connecting",
            crate::dcc::types::DccState::Connected => "connected",
        };
        let duration = if let Some(started) = record.started {
            let secs = started.elapsed().as_secs();
            format!(" ({}m {}s)", secs / 60, secs % 60)
        } else {
            String::new()
        };
        add_local_event(
            app,
            &format!(
                "  {} CHAT {} [{}:{}] {state}{duration}",
                record.nick, record.id, record.addr, record.port
            ),
        );
    }
}

/// `/dcc reject chat <nick>` — reject a pending request and send REJECT CTCP.
fn cmd_dcc_reject(app: &mut App, args: &[String]) {
    if args.len() < 2 || !args[0].eq_ignore_ascii_case("chat") {
        add_local_event(app, "Usage: /dcc reject chat <nick>");
        return;
    }
    let nick = &args[1];
    if let Some(record) = app.dcc.close_by_nick(nick) {
        // Send DCC REJECT via IRC
        if let Some(sender) = app.active_irc_sender() {
            let reject = crate::dcc::protocol::build_dcc_reject();
            let _ = sender.send_notice(&record.nick, &reject);
        }
        add_local_event(app, &format!("DCC CHAT from {} rejected", record.nick));
    } else {
        add_local_event(app, &format!("No pending DCC CHAT from {nick}"));
    }
}
```

- [ ] **Step 2: Add `pub mod handlers_dcc;` to `src/commands/mod.rs`**

- [ ] **Step 3: Register `/dcc` in `src/commands/registry.rs`**

Add to the imports and COMMANDS list:
```rust
use super::handlers_dcc::cmd_dcc;

// In COMMANDS vec:
(
    "dcc",
    CommandDef {
        handler: cmd_dcc,
        description: "DCC CHAT commands (chat, close, list, reject)",
        aliases: &[],
        category: CommandCategory::Connection,
    },
),
```

- [ ] **Step 4: Verify compilation (will have errors until App integration in Task 8)**

Run: `cargo check 2>&1 | head -20`
Note: This will likely show errors about `app.dcc` not existing yet. That's expected — Task 8 wires it.

- [ ] **Step 5: Commit**

```bash
git add src/commands/handlers_dcc.rs src/commands/mod.rs src/commands/registry.rs
git commit -m "feat(dcc): /dcc command handlers (chat, close, list, reject)"
```

---

### Task 8: App Integration — DccManager, Select Arm, Message Routing

**Files:**
- Modify: `src/app.rs` — add `DccManager`, `dcc_rx`, new select arm, `handle_dcc_event`, modify `handle_plain_message` for DCC routing

This is the main wiring task that connects everything together.

- [ ] **Step 1: Add `DccManager` and `dcc_rx` fields to `App`**

In `src/app.rs`, add to the `App` struct:
```rust
pub dcc: crate::dcc::DccManager,
dcc_rx: mpsc::UnboundedReceiver<crate::dcc::DccEvent>,
```

In `App::new()`, initialize:
```rust
let (dcc, dcc_rx) = crate::dcc::DccManager::new();
// Apply config
// dcc.timeout_secs = config.dcc.timeout;
// dcc.own_ip = parse own_ip from config.dcc.own_ip string
// dcc.port_range = parse_port_range(&config.dcc.port_range);
// etc.
```

- [ ] **Step 2: Add `dcc_rx` arm to the `tokio::select!` loop**

In the main event loop (~line 1446), add:
```rust
dcc_ev = self.dcc_rx.recv() => {
    if let Some(ev) = dcc_ev {
        self.handle_dcc_event(ev);
    }
}
```

- [ ] **Step 3: Add DCC timeout purge to the 1-second tick**

In the existing tick handler, add:
```rust
let expired = self.dcc.purge_expired();
for (id, nick) in expired {
    crate::commands::helpers::add_local_event(
        self,
        &format!("DCC CHAT request from {nick} timed out"),
    );
}
```

- [ ] **Step 4: Implement `handle_dcc_event`**

```rust
fn handle_dcc_event(&mut self, event: crate::dcc::DccEvent) {
    use crate::dcc::DccEvent;
    use crate::state::buffer::{
        ActivityLevel, Buffer, BufferType, Message, MessageType,
    };

    match event {
        DccEvent::IncomingRequest {
            nick, conn_id, addr, port, passive_token, ident, host,
        } => {
            // Cross-request auto-allow: if we have a Listening DCC to this nick,
            // tear down our listener and auto-accept the incoming request.
            // This prevents deadlock when both sides initiate simultaneously.
            let mut auto = false;
            if let Some(our_pending) = self.dcc.find_pending(&nick) {
                if our_pending.state == crate::dcc::types::DccState::Listening {
                    let id = our_pending.id.clone();
                    self.dcc.close_by_id(&id);
                    auto = true; // Force auto-accept
                }
            }

            // Also check hostmask-based auto-accept
            if !auto {
                auto = self.dcc.should_auto_accept(&nick, &ident, &host, port);
            }

            // Create DCC record
            let dcc_id = self.dcc.generate_id(&nick);
            let record = crate::dcc::types::DccRecord {
                id: dcc_id.clone(),
                dcc_type: crate::dcc::types::DccType::Chat,
                nick: nick.clone(),
                conn_id: conn_id.clone(),
                addr,
                port,
                state: crate::dcc::types::DccState::WaitingUser,
                passive_token,
                created: std::time::Instant::now(),
                started: None,
                bytes_transferred: 0,
                mirc_ctcp: true,
                ident,
                host,
            };
            self.dcc.records.insert(dcc_id.clone(), record);

            if auto {
                crate::commands::handlers_dcc::cmd_dcc(
                    self,
                    &["chat".to_string(), nick.clone()],
                );
            } else {
                let msg = if passive_token.is_some() {
                    format!("DCC CHAT (passive) request from {nick} — /dcc chat {nick} to accept")
                } else {
                    format!("DCC CHAT request from {nick} [{addr}:{port}] — /dcc chat {nick} to accept")
                };
                crate::commands::helpers::add_local_event(self, &msg);
            }
        }
        DccEvent::ChatConnected { id } => {
            if let Some(record) = self.dcc.records.get_mut(&id) {
                record.state = crate::dcc::types::DccState::Connected;
                record.started = Some(std::time::Instant::now());
                let nick = record.nick.clone();
                let conn_id = record.conn_id.clone();

                // Create =nick buffer
                let buffer_name = format!("={nick}");
                let buffer_id = crate::state::buffer::make_buffer_id(&conn_id, &buffer_name);

                if !self.state.buffers.contains_key(&buffer_id) {
                    self.state.add_buffer(Buffer {
                        id: buffer_id.clone(),
                        connection_id: conn_id,
                        buffer_type: BufferType::DccChat,
                        name: buffer_name,
                        messages: Vec::new(),
                        activity: ActivityLevel::None,
                        unread_count: 0,
                        last_read: chrono::Utc::now(),
                        topic: None,
                        topic_set_by: None,
                        users: std::collections::HashMap::new(),
                        modes: None,
                        mode_params: None,
                        list_modes: std::collections::HashMap::new(),
                        last_speakers: Vec::new(),
                    });
                }

                self.state.set_active_buffer(&buffer_id);
                let msg_id = self.state.next_message_id();
                self.state.add_message(
                    &buffer_id,
                    Message {
                        id: msg_id,
                        timestamp: chrono::Utc::now(),
                        message_type: MessageType::Event,
                        nick: None,
                        nick_mode: None,
                        text: format!("DCC CHAT connection established with {nick}"),
                        highlight: false,
                        event_key: None,
                        event_params: None,
                        log_msg_id: None,
                        log_ref_id: None,
                        tags: std::collections::HashMap::new(),
                    },
                );
            }
        }
        DccEvent::ChatMessage { id, text } => {
            if let Some(record) = self.dcc.records.get_mut(&id) {
                record.bytes_transferred += text.len() as u64;
                let nick = record.nick.clone();
                let conn_id = record.conn_id.clone();
                let buffer_name = format!("={nick}");
                let buffer_id = crate::state::buffer::make_buffer_id(&conn_id, &buffer_name);
                let msg_id = self.state.next_message_id();
                self.state.add_message_with_activity(
                    &buffer_id,
                    Message {
                        id: msg_id,
                        timestamp: chrono::Utc::now(),
                        message_type: MessageType::Message,
                        nick: Some(nick),
                        nick_mode: None,
                        text,
                        highlight: false,
                        event_key: None,
                        event_params: None,
                        log_msg_id: None,
                        log_ref_id: None,
                        tags: std::collections::HashMap::new(),
                    },
                    ActivityLevel::Mention, // DCC messages are always important
                );
            }
        }
        DccEvent::ChatAction { id, text } => {
            if let Some(record) = self.dcc.records.get(&id) {
                let nick = record.nick.clone();
                let conn_id = record.conn_id.clone();
                let buffer_name = format!("={nick}");
                let buffer_id = crate::state::buffer::make_buffer_id(&conn_id, &buffer_name);
                let msg_id = self.state.next_message_id();
                self.state.add_message_with_activity(
                    &buffer_id,
                    Message {
                        id: msg_id,
                        timestamp: chrono::Utc::now(),
                        message_type: MessageType::Action,
                        nick: Some(nick),
                        nick_mode: None,
                        text,
                        highlight: false,
                        event_key: None,
                        event_params: None,
                        log_msg_id: None,
                        log_ref_id: None,
                        tags: std::collections::HashMap::new(),
                    },
                    ActivityLevel::Mention,
                );
            }
        }
        DccEvent::ChatClosed { id, reason } => {
            if let Some(record) = self.dcc.close_by_id(&id) {
                let nick = record.nick;
                let conn_id = record.conn_id;
                let buffer_name = format!("={nick}");
                let buffer_id = crate::state::buffer::make_buffer_id(&conn_id, &buffer_name);
                let msg = match reason {
                    Some(r) => format!("DCC CHAT with {nick} closed: {r}"),
                    None => format!("DCC CHAT with {nick} closed"),
                };
                let msg_id = self.state.next_message_id();
                self.state.add_message(
                    &buffer_id,
                    Message {
                        id: msg_id,
                        timestamp: chrono::Utc::now(),
                        message_type: MessageType::Event,
                        nick: None,
                        nick_mode: None,
                        text: msg,
                        highlight: false,
                        event_key: None,
                        event_params: None,
                        log_msg_id: None,
                        log_ref_id: None,
                        tags: std::collections::HashMap::new(),
                    },
                );
            }
        }
        DccEvent::ChatError { id, error } => {
            // Remove the record on error
            let nick = self.dcc.close_by_id(&id).map(|r| r.nick);
            let display_nick = nick.as_deref().unwrap_or(&id);
            crate::commands::helpers::add_local_event(
                self,
                &format!("DCC CHAT error ({display_nick}): {error}"),
            );
        }
    }
}
```

- [ ] **Step 5: Modify `handle_plain_message` for DCC buffer routing**

In `src/app.rs`, in `handle_plain_message`, change the buffer type check to include `DccChat`:

```rust
// Change from:
if !matches!(buf.buffer_type, BufferType::Channel | BufferType::Query) {
// To:
if !matches!(buf.buffer_type, BufferType::Channel | BufferType::Query | BufferType::DccChat) {
```

Then add DCC routing before the IRC send:
```rust
// After the buffer type check, before IRC send:
if buf.buffer_type == BufferType::DccChat {
    // Route to DCC — find DCC record by buffer name
    let dcc_nick = buf.name.strip_prefix('=').unwrap_or(&buf.name);
    if let Some(record) = self.dcc.find_connected(dcc_nick) {
        let dcc_id = record.id.clone();
        if let Err(e) = self.dcc.send_chat_line(&dcc_id, text) {
            crate::commands::helpers::add_local_event(self, &e);
            return;
        }
        // Display locally
        let id = self.state.next_message_id();
        self.state.add_message(/* own message */);
        return;
    } else {
        crate::commands::helpers::add_local_event(self, "DCC CHAT not connected");
        return;
    }
}
```

- [ ] **Step 6: Modify `/msg` to route `=nick` targets to DCC**

In `src/commands/handlers_irc.rs`, in `cmd_msg`, add a check:
```rust
// At the top of cmd_msg, after parsing target:
if target.starts_with('=') {
    let dcc_nick = &target[1..];
    if let Some(record) = app.dcc.find_connected(dcc_nick) {
        let dcc_id = record.id.clone();
        if let Err(e) = app.dcc.send_chat_line(&dcc_id, text) {
            add_local_event(app, &e);
        }
        // Display message in DCC buffer
        // ...
        return;
    } else {
        add_local_event(app, &format!("No active DCC CHAT with {dcc_nick}"));
        return;
    }
}
```

- [ ] **Step 7: Modify `/me` to route through DCC in DCC buffers**

In `src/commands/handlers_irc.rs`, in `cmd_me`, add DCC buffer check before the IRC send:
```rust
if buf.buffer_type == BufferType::DccChat {
    let dcc_nick = buf.name.strip_prefix('=').unwrap_or(&buf.name);
    if let Some(record) = app.dcc.find_connected(dcc_nick) {
        let dcc_id = record.id.clone();
        let ctcp = format!("\x01ACTION {action_text}\x01");
        if let Err(e) = app.dcc.send_chat_line(&dcc_id, &ctcp) {
            add_local_event(app, &e);
        }
        // Display locally as action
        // ...
        return;
    }
}
```

- [ ] **Step 8: Verify full compilation**

Run: `cargo check 2>&1 | head -20`
Expected: Clean compilation (fix any remaining type errors)

- [ ] **Step 9: Run all tests**

Run: `cargo test 2>&1 | tail -5`
Expected: All existing + new tests pass

- [ ] **Step 10: Commit**

```bash
git add src/app.rs src/commands/handlers_irc.rs src/commands/handlers_dcc.rs
git commit -m "feat(dcc): wire DccManager into App, select arm, message routing"
```

---

## Chunk 4: IRC Event Integration & Edge Cases

### Task 9: Parse Incoming DCC CTCP in events.rs

**Files:**
- Modify: `src/irc/events.rs:734-746` — replace "Non-ACTION CTCP, ignore for now" with DCC parsing

- [ ] **Step 1: Add DCC CTCP parsing in the non-ACTION CTCP branch**

In `src/irc/events.rs`, at line ~745 where it currently says `// Non-ACTION CTCP, ignore for now`:

In `events.rs`, keep the existing `return;` for non-ACTION CTCP. The DCC CTCP parsing happens in `App::handle_irc_event` instead (preserving TEA architecture — no DCC dependency in state layer).

In `App::handle_irc_event`, in the `IrcEvent::Message` arm, **before** calling `handle_irc_message`, check if the message is a CTCP DCC request:

```rust
IrcEvent::Message(conn_id, msg) => {
    // Check for CTCP DCC before normal handling
    if let Command::PRIVMSG(ref target, ref text) = msg.command {
        if text.starts_with('\x01') && text.ends_with('\x01') && text.len() > 2 {
            let inner = &text[1..text.len() - 1];
            if let Some(dcc_msg) = crate::dcc::protocol::parse_dcc_ctcp(inner) {
                if dcc_msg.dcc_type == "CHAT" {
                    let nick = crate::irc::events::extract_nick(msg.prefix.as_ref())
                        .unwrap_or_default();
                    let (_, ident, host) = crate::irc::events::extract_nick_userhost(
                        msg.prefix.as_ref(),
                    );
                    self.handle_dcc_event(crate::dcc::DccEvent::IncomingRequest {
                        nick,
                        conn_id: conn_id.clone(),
                        addr: dcc_msg.addr,
                        port: dcc_msg.port,
                        passive_token: dcc_msg.passive_token,
                        ident,
                        host,
                    });
                    // Don't pass to normal IRC handler
                    return;
                }
            }
        }
    }
    // Normal IRC message handling
    crate::irc::events::handle_irc_message(&mut self.state, &conn_id, &msg);
}
```

**Note:** `extract_nick` and `extract_nick_userhost` need to be made `pub` in `events.rs` (they are currently private). Alternatively, add a public helper that extracts nick+ident+host from a prefix.

- [ ] **Step 2: Handle passive DCC response (incoming CTCP matching our pending token)**

When we receive a DCC CHAT CTCP with a token that matches one of our outgoing passive DCC requests, this is the peer's response — we should connect to their address instead of creating a new incoming request. Add this check before creating a new `IncomingRequest`.

- [ ] **Step 3: Verify compilation**

Run: `cargo check 2>&1 | head -10`

- [ ] **Step 4: Commit**

```bash
git add src/irc/events.rs src/state/mod.rs  # or wherever dcc_event_tx is added
git commit -m "feat(dcc): parse incoming DCC CTCP requests in events.rs"
```

---

### Task 10: Nick Change Hook & ERR_NOSUCHNICK Cleanup

**Files:**
- Modify: `src/irc/events.rs` — add DCC nick update in `handle_nick_change`, add 401 cleanup

- [ ] **Step 1: Add DCC nick update in `handle_nick_change`**

After the existing nick change processing in `handle_nick_change` (~line 1394), add:
```rust
// Update DCC records for this nick change
// This will be called from App after handle_irc_message returns
// by checking if DccManager has records for old_nick
```

The cleanest approach is to add a hook in `App::handle_irc_event` for the NICK command that calls `self.dcc.update_nick(old, new)` and renames any `=nick` buffers.

- [ ] **Step 2: Add ERR_NOSUCHNICK (401) DCC cleanup**

In `handle_response` in `events.rs`, find or add handling for `Response::ERR_NOSUCHNICK`. After the existing error routing, check if the nick has pending DCC records and clean them up:

```rust
Response::ERR_NOSUCHNICK => {
    // args[1] is the nick that doesn't exist
    if let Some(nick) = args.get(1) {
        // Signal DCC cleanup (will be handled in App)
    }
    // ... existing error display ...
}
```

The actual DCC cleanup happens in `App` since `handle_response` only has `&mut AppState`.

- [ ] **Step 3: Handle IRC disconnect — DCC connections survive**

In `App::handle_irc_event` for `IrcEvent::Disconnected`, do NOT close DCC connections. They are peer-to-peer and independent of the IRC server. Just log a note if there are active DCC connections for that `conn_id`.

- [ ] **Step 4: Verify and run tests**

Run: `cargo test 2>&1 | tail -5`

- [ ] **Step 5: Commit**

```bash
git add src/irc/events.rs src/app.rs
git commit -m "feat(dcc): nick change tracking, 401 cleanup, IRC disconnect handling"
```

---

### Task 11: Scripting Events

**Files:**
- Modify: `src/scripting/api.rs` — add DCC event constants
- Modify: `src/app.rs` — emit DCC events to script engine

- [ ] **Step 1: Add DCC event constants to `src/scripting/api.rs`**

```rust
pub mod events {
    // ... existing constants ...

    /// DCC CHAT request received.
    /// Params: connection_id, nick, ip, port
    pub const DCC_CHAT_REQUEST: &str = "dcc.chat.request";

    /// DCC CHAT connection established.
    /// Params: connection_id, nick
    pub const DCC_CHAT_CONNECTED: &str = "dcc.chat.connected";

    /// DCC CHAT message received.
    /// Params: connection_id, nick, text
    pub const DCC_CHAT_MESSAGE: &str = "dcc.chat.message";

    /// DCC CHAT connection closed.
    /// Params: connection_id, nick, reason
    pub const DCC_CHAT_CLOSED: &str = "dcc.chat.closed";
}
```

- [ ] **Step 2: Emit DCC events in `handle_dcc_event`**

In each arm of `handle_dcc_event`, emit the corresponding scripting event before processing. For `dcc.chat.request` and `dcc.chat.message`, check if the script suppressed the event.

- [ ] **Step 3: Commit**

```bash
git add src/scripting/api.rs src/app.rs
git commit -m "feat(dcc): scripting events for DCC CHAT lifecycle"
```

---

## Chunk 5: Polish & Final Testing

### Task 12: Clippy, Tests, and Final Verification

**Files:**
- All modified files

- [ ] **Step 1: Run clippy**

Run: `cargo clippy -- -W clippy::pedantic 2>&1 | head -40`
Fix all warnings.

- [ ] **Step 2: Run full test suite**

Run: `cargo test 2>&1 | tail -10`
Expected: All tests pass (existing + new DCC tests)

- [ ] **Step 3: Run the binary and verify DCC commands exist**

Run: `cargo run --release 2>&1` and type `/help dcc` or `/dcc list` to verify commands are registered.

- [ ] **Step 4: Final commit**

```bash
git add -A
git commit -m "chore(dcc): clippy fixes and final verification"
```

---

## Task Dependency Summary

```
Task 1 (types) ──→ Task 2 (protocol) ──→ Task 3 (DccManager) ──→ Task 4 (BufferType)
                                                                        ↓
Task 5 (config) ──→ Task 6 (chat.rs) ──→ Task 7 (commands) ──→ Task 8 (app integration)
                                                                        ↓
                                        Task 9 (CTCP parsing) ──→ Task 10 (nick/401)
                                                                        ↓
                                                               Task 11 (scripting)
                                                                        ↓
                                                               Task 12 (polish)
```

Tasks 1-3 can be done sequentially (each builds on prior). Task 4 and 5 are independent of each other but both needed before Task 8. Tasks 1-5 can be split across parallel agents. Tasks 6-8 are sequential. Tasks 9-12 are sequential.

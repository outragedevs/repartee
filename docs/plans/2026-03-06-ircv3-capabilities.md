# IRCv3 Capabilities Implementation Plan

> **For Claude:** REQUIRED SUB-SKILLS:
> - Use superpowers:executing-plans to implement this plan task-by-task.
> - Use ratatui-tui skill for all TUI/widget work (layouts, rendering, StatefulWidget patterns).
> - Use rust-best-practices skill for all Rust code (borrowing, error handling, clippy, testing conventions).

**Goal:** Implement full IRCv3.1/3.2 capability negotiation with must-have and high-value caps, plus WHOX and extban custom extensions.

**Architecture:** Refactor the current SASL-only CAP negotiation into an extensible framework. Parse ISUPPORT tokens structurally. Plumb message tags through the entire stack (buffer → storage → scripting). Each cap is a discrete module that registers itself with the negotiation engine.

**Tech Stack:** irc crate 1.1.0 (protocol types), base64 (SASL), sha2/hmac (SCRAM), chrono (server-time parsing)

**Convention:** Every task's final commit MUST update `docs/rfc_ircv3_coverage.md` — mark completed items as **Done** with a short note.

---

### Task 1: ISUPPORT Structured Parsing

**Files:**
- Create: `src/irc/isupport.rs`
- Modify: `src/irc/mod.rs` (add `pub mod isupport;`)
- Modify: `src/irc/events.rs:1169-1188` (use new parser)
- Modify: `src/state/connection.rs:14-41` (add parsed isupport)
- Test: `src/irc/isupport.rs` (inline `#[cfg(test)]`)

**Context:**
ISUPPORT (005) tokens like `PREFIX=(qaohv)~&@%+` and `CHANMODES=beI,k,l,imnpst` drive how the client interprets mode changes, prefixes, and available features. Currently stored as raw `HashMap<String, String>` on `Connection` (line 22). We need structured parsing so later tasks can query `isupport.has_whox()`, `isupport.prefix_modes()`, etc.

**Step 1: Write failing tests for ISUPPORT parsing**

Create `src/irc/isupport.rs` with test module:

```rust
use std::collections::HashMap;

/// Parsed ISUPPORT (005) tokens from the server.
#[derive(Debug, Clone, Default)]
pub struct Isupport {
    /// Raw key=value pairs from server
    pub raw: HashMap<String, String>,
}

impl Isupport {
    /// Parse a list of ISUPPORT tokens (e.g., from RPL_ISUPPORT args).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Merge tokens from a single RPL_ISUPPORT line.
    pub fn parse_tokens(&mut self, _tokens: &[&str]) {
        todo!()
    }

    /// Channel prefix characters mapped to mode letters.
    /// From `PREFIX=(modes)prefixes` — e.g., `PREFIX=(qaohv)~&@%+`
    /// Returns vec of (mode_char, prefix_char) in rank order.
    #[must_use]
    pub fn prefix_map(&self) -> Vec<(char, char)> {
        todo!()
    }

    /// Channel modes grouped by type.
    /// From `CHANMODES=A,B,C,D` where:
    /// - A: list modes (always have param) e.g. b,e,I
    /// - B: param modes (always have param) e.g. k
    /// - C: param modes (param on set only) e.g. l
    /// - D: no-param modes e.g. i,m,n,p,s,t
    #[must_use]
    pub fn chanmode_types(&self) -> (String, String, String, String) {
        todo!()
    }

    /// Network name from `NETWORK=` token.
    #[must_use]
    pub fn network(&self) -> Option<&str> {
        todo!()
    }

    /// Whether server supports WHOX (extended WHO).
    #[must_use]
    pub fn has_whox(&self) -> bool {
        todo!()
    }

    /// Max modes per MODE command from `MODES=N`.
    #[must_use]
    pub fn max_modes(&self) -> usize {
        todo!()
    }

    /// STATUSMSG prefixes (e.g., `@+` for messaging ops/voices).
    #[must_use]
    pub fn statusmsg(&self) -> &str {
        todo!()
    }

    /// Case mapping rule from `CASEMAPPING=` (rfc1459, ascii, strict-rfc1459).
    #[must_use]
    pub fn casemapping(&self) -> &str {
        todo!()
    }

    /// Max channel name length from `CHANNELLEN=`.
    #[must_use]
    pub fn channel_len(&self) -> usize {
        todo!()
    }

    /// Max nick length from `NICKLEN=`.
    #[must_use]
    pub fn nick_len(&self) -> usize {
        todo!()
    }

    /// Max topic length from `TOPICLEN=`.
    #[must_use]
    pub fn topic_len(&self) -> usize {
        todo!()
    }

    /// Available channel types from `CHANTYPES=` (default `#&`).
    #[must_use]
    pub fn chan_types(&self) -> &str {
        todo!()
    }

    /// Extban prefix and types from `EXTBAN=<prefix>,<types>`.
    /// Returns (prefix, types) e.g. ('$', "a") for `EXTBAN=$,a`.
    #[must_use]
    pub fn extban(&self) -> Option<(char, String)> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_tokens() {
        let mut is = Isupport::new();
        is.parse_tokens(&["NETWORK=IRCnet", "WHOX", "MODES=4"]);
        assert_eq!(is.network(), Some("IRCnet"));
        assert!(is.has_whox());
        assert_eq!(is.max_modes(), 4);
    }

    #[test]
    fn parse_prefix() {
        let mut is = Isupport::new();
        is.parse_tokens(&["PREFIX=(qaohv)~&@%+"]);
        let map = is.prefix_map();
        assert_eq!(map.len(), 5);
        assert_eq!(map[0], ('q', '~'));
        assert_eq!(map[2], ('o', '@'));
        assert_eq!(map[4], ('v', '+'));
    }

    #[test]
    fn parse_chanmodes() {
        let mut is = Isupport::new();
        is.parse_tokens(&["CHANMODES=beI,k,l,imnpst"]);
        let (a, b, c, d) = is.chanmode_types();
        assert_eq!(a, "beI");
        assert_eq!(b, "k");
        assert_eq!(c, "l");
        assert_eq!(d, "imnpst");
    }

    #[test]
    fn parse_casemapping() {
        let mut is = Isupport::new();
        is.parse_tokens(&["CASEMAPPING=rfc1459"]);
        assert_eq!(is.casemapping(), "rfc1459");
    }

    #[test]
    fn defaults_for_missing_tokens() {
        let is = Isupport::new();
        assert_eq!(is.network(), None);
        assert!(!is.has_whox());
        assert_eq!(is.max_modes(), 3); // RFC default
        assert_eq!(is.casemapping(), "rfc1459");
        assert_eq!(is.chan_types(), "#&");
        assert_eq!(is.nick_len(), 9); // RFC default
    }

    #[test]
    fn parse_negated_token() {
        let mut is = Isupport::new();
        is.parse_tokens(&["WHOX", "MODES=4"]);
        assert!(is.has_whox());
        is.parse_tokens(&["-WHOX"]);
        assert!(!is.has_whox());
    }

    #[test]
    fn parse_extban() {
        let mut is = Isupport::new();
        is.parse_tokens(&["EXTBAN=$,a"]);
        let (prefix, types) = is.extban().unwrap();
        assert_eq!(prefix, '$');
        assert_eq!(types, "a");
    }

    #[test]
    fn parse_statusmsg() {
        let mut is = Isupport::new();
        is.parse_tokens(&["STATUSMSG=@+"]);
        assert_eq!(is.statusmsg(), "@+");
    }

    #[test]
    fn parse_lengths() {
        let mut is = Isupport::new();
        is.parse_tokens(&["NICKLEN=30", "CHANNELLEN=50", "TOPICLEN=390"]);
        assert_eq!(is.nick_len(), 30);
        assert_eq!(is.channel_len(), 50);
        assert_eq!(is.topic_len(), 390);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test isupport -- --nocapture`
Expected: all tests FAIL with "not yet implemented"

**Step 3: Implement the Isupport struct**

Fill in all method bodies:
- `parse_tokens`: iterate tokens, split on `=`, handle `-TOKEN` negation, store in `raw`
- `prefix_map`: regex parse `PREFIX=(modes)prefixes`
- `chanmode_types`: split on commas
- All getters: look up `raw.get(key)`, parse, return defaults

**Step 4: Run tests to verify they pass**

Run: `cargo test isupport -- --nocapture`
Expected: all 9 tests PASS

**Step 5: Wire into Connection and events.rs**

In `src/state/connection.rs`:
- Add `pub isupport_parsed: crate::irc::isupport::Isupport` field to Connection (keep raw `isupport` for backward compat initially, then migrate callers)

In `src/irc/events.rs` at RPL_ISUPPORT handler (lines 1169-1188):
- After storing raw tokens, call `conn.isupport_parsed.parse_tokens(&token_strs)`
- Keep `update_label_from_network()` call but source from `conn.isupport_parsed.network()`

In `src/irc/mod.rs`:
- Add `pub mod isupport;`

**Step 6: Run full test suite**

Run: `cargo test`
Expected: all 331+ tests PASS

**Step 7: Commit**

```bash
git add src/irc/isupport.rs src/irc/mod.rs src/irc/events.rs src/state/connection.rs docs/rfc_ircv3_coverage.md
git commit -m "feat: structured ISUPPORT parsing with PREFIX, CHANMODES, WHOX detection"
```

Update `docs/rfc_ircv3_coverage.md`: mark "Structured parsing" and "Behavior adaptation" as **Done**.

---

### Task 2: CAP Negotiation Framework

**Files:**
- Create: `src/irc/cap.rs`
- Modify: `src/irc/mod.rs` (refactor `sasl_authenticate()`, add `pub mod cap;`)
- Modify: `src/state/connection.rs` (add `enabled_caps: HashSet<String>`)
- Test: `src/irc/cap.rs` (inline tests)

**Context:**
Current `sasl_authenticate()` (lines 128-243 of `src/irc/mod.rs`) hardcodes the entire CAP flow for SASL only. We need a general framework that:
1. Sends CAP LS 302
2. Collects server-advertised caps (with values, e.g., `sasl=PLAIN,EXTERNAL`)
3. Requests all desired caps that the server supports
4. Runs SASL if enabled (after CAP ACK)
5. Sends CAP END + registration

**Step 1: Write the CapNegotiator struct and tests**

Create `src/irc/cap.rs`:

```rust
use std::collections::{HashMap, HashSet};

/// Capabilities we want to request, in priority order.
pub const DESIRED_CAPS: &[&str] = &[
    "multi-prefix",
    "extended-join",
    "server-time",
    "account-tag",
    "cap-notify",
    "away-notify",
    "account-notify",
    "chghost",
    "echo-message",
    "invite-notify",
    "batch",
    "userhost-in-names",
    "message-tags",
    "sasl",
];

/// Result of CAP LS parsing.
#[derive(Debug, Clone, Default)]
pub struct ServerCaps {
    /// Map of capability name → optional value (e.g., "sasl" → "PLAIN,EXTERNAL")
    pub available: HashMap<String, Option<String>>,
}

impl ServerCaps {
    /// Parse a CAP LS response string into available caps.
    #[must_use]
    pub fn parse(caps_str: &str) -> Self {
        todo!()
    }

    /// Check if a capability is available.
    #[must_use]
    pub fn has(&self, cap: &str) -> bool {
        todo!()
    }

    /// Get the value of a capability (e.g., SASL mechanisms).
    #[must_use]
    pub fn value(&self, cap: &str) -> Option<&str> {
        todo!()
    }

    /// Return the subset of desired caps that the server supports.
    #[must_use]
    pub fn negotiate(&self, desired: &[&str]) -> Vec<String> {
        todo!()
    }

    /// Parse SASL mechanisms from the `sasl=` cap value.
    /// Returns available mechanisms in preference order.
    #[must_use]
    pub fn sasl_mechanisms(&self) -> Vec<String> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_caps() {
        let caps = ServerCaps::parse("multi-prefix extended-join server-time");
        assert!(caps.has("multi-prefix"));
        assert!(caps.has("extended-join"));
        assert!(caps.has("server-time"));
        assert!(!caps.has("monitor"));
    }

    #[test]
    fn parse_caps_with_values() {
        let caps = ServerCaps::parse("sasl=PLAIN,EXTERNAL multi-prefix batch");
        assert!(caps.has("sasl"));
        assert_eq!(caps.value("sasl"), Some("PLAIN,EXTERNAL"));
        assert_eq!(caps.value("multi-prefix"), None);
    }

    #[test]
    fn negotiate_filters_to_available() {
        let caps = ServerCaps::parse("multi-prefix server-time away-notify");
        let requested = caps.negotiate(DESIRED_CAPS);
        assert!(requested.contains(&"multi-prefix".to_string()));
        assert!(requested.contains(&"server-time".to_string()));
        assert!(requested.contains(&"away-notify".to_string()));
        assert!(!requested.contains(&"echo-message".to_string()));
    }

    #[test]
    fn sasl_mechanisms_parsed() {
        let caps = ServerCaps::parse("sasl=PLAIN,SCRAM-SHA-256,EXTERNAL");
        let mechs = caps.sasl_mechanisms();
        assert_eq!(mechs, vec!["PLAIN", "SCRAM-SHA-256", "EXTERNAL"]);
    }

    #[test]
    fn sasl_no_value_means_plain() {
        let caps = ServerCaps::parse("sasl");
        let mechs = caps.sasl_mechanisms();
        assert_eq!(mechs, vec!["PLAIN"]);
    }

    #[test]
    fn empty_caps() {
        let caps = ServerCaps::parse("");
        assert!(caps.negotiate(DESIRED_CAPS).is_empty());
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test cap::tests -- --nocapture`
Expected: all 6 tests FAIL

**Step 3: Implement `ServerCaps`**

- `parse`: split on whitespace, split each on `=` for key/value
- `has`: case-insensitive lookup
- `value`: return value if present
- `negotiate`: filter DESIRED_CAPS to those in `available`
- `sasl_mechanisms`: split value on `,`, default to `["PLAIN"]`

**Step 4: Run tests**

Run: `cargo test cap::tests -- --nocapture`
Expected: all 6 PASS

**Step 5: Refactor `connect_server()` to use `CapNegotiator`**

Rewrite `sasl_authenticate()` → `negotiate_caps()` in `src/irc/mod.rs`:

1. Send `CAP LS 302`
2. Collect all CAP LS lines (handle multiline `*` continuation) → `ServerCaps::parse()`
3. Compute `caps_to_request = server_caps.negotiate(DESIRED_CAPS)`
4. If SASL credentials exist and server has `sasl`, include `sasl` in request
5. Send `CAP REQ :cap1 cap2 cap3 ...`
6. Wait for ACK/NAK — parse which caps were accepted
7. If `sasl` was ACK'd, run SASL flow (keep existing PLAIN logic)
8. Send CAP END + PASS + NICK + USER
9. Return the set of enabled caps

Store enabled caps on `Connection`:
- Add `enabled_caps: HashSet<String>` to `Connection` struct

**Step 6: Always negotiate caps (not just when SASL configured)**

Change the branch in `connect_server()` (line 76):
- Old: only negotiate if `has_sasl`
- New: always call `negotiate_caps()`, SASL is one optional step within it

**Step 7: Run full test suite**

Run: `cargo test`
Expected: all tests PASS

**Step 8: Commit**

```bash
git add src/irc/cap.rs src/irc/mod.rs src/state/connection.rs docs/rfc_ircv3_coverage.md
git commit -m "feat: extensible CAP negotiation framework, request all supported caps"
```

Update `docs/rfc_ircv3_coverage.md`: mark "Capability state machine" as **Done**.

---

### Task 3: Message Tags Plumbing

**Files:**
- Modify: `src/state/buffer.rs:50-67` (add `tags` field to Message)
- Modify: `src/irc/events.rs` (extract tags from `irc::proto::Message`)
- Modify: `src/storage/types.rs` (add `tags` to LogRow)
- Modify: `src/storage/db.rs` (add `tags` column)
- Test: inline tests in events.rs

**Context:**
IRCv3 message tags (`@time=2026-03-06T12:00:00.000Z;account=patrick`) are already parsed by the irc crate into `Message.tags: Option<Vec<Tag>>`. We need to plumb them through to buffer messages and storage.

**Step 1: Add `tags` field to buffer `Message`**

In `src/state/buffer.rs`, add to the Message struct:

```rust
pub tags: HashMap<String, String>,
```

Default to empty HashMap. Update all Message construction sites.

**Step 2: Extract tags in event handlers**

Create a helper in `src/irc/events.rs`:

```rust
fn extract_tags(msg: &irc::proto::Message) -> HashMap<String, String> {
    msg.tags.as_ref().map_or_else(HashMap::new, |tags| {
        tags.iter()
            .filter_map(|tag| {
                Some((tag.0.clone(), tag.1.as_ref()?.clone()))
            })
            .collect()
    })
}
```

Pass tags to every `Message` construction in event handlers.

**Step 3: Add `tags` to storage LogRow**

In `src/storage/types.rs`, add `pub tags: Option<String>` (JSON-serialized).
In `src/storage/db.rs`, add `tags TEXT` column to CREATE TABLE.
In writer, serialize tags to JSON before insert.

**Step 4: Run full test suite**

Run: `cargo test`
Expected: all tests PASS

**Step 5: Commit**

```bash
git add src/state/buffer.rs src/irc/events.rs src/storage/types.rs src/storage/db.rs docs/rfc_ircv3_coverage.md
git commit -m "feat: plumb IRCv3 message tags through buffer and storage layers"
```

Update `docs/rfc_ircv3_coverage.md`: mark `message-tags` as **Done** (plumbing).

---

### Task 4: multi-prefix + userhost-in-names

**Files:**
- Modify: `src/irc/events.rs` (NAMES reply parsing, lines around RPL_NAMREPLY)
- Modify: `src/state/buffer.rs` (NickEntry — already has multi-mode `modes` field)
- Test: inline tests in events.rs

**Context:**
`multi-prefix`: Server sends ALL mode prefixes per nick in NAMES, not just highest. E.g., `@+nick` instead of `@nick`. Our NickEntry already has a `modes: String` field — we just need to parse multiple prefixes.

`userhost-in-names`: Server sends `nick!user@host` in NAMES instead of bare `nick`. We can extract and store ident/host on NickEntry.

Both caps were already requested in Task 2's DESIRED_CAPS list.

**Step 1: Write tests for multi-prefix NAMES parsing**

```rust
#[test]
fn names_multi_prefix() {
    // With multi-prefix: "@+nick" means op + voice
    // Current parser should extract prefix="@+", modes="ov", nick="nick"
}

#[test]
fn names_userhost_in_names() {
    // With userhost-in-names: "@+nick!user@host.com"
    // Should extract nick="nick", prefix="@+", modes="ov", ident="user", host="host.com"
}
```

**Step 2: Update NAMES parsing in handle_response()**

Current RPL_NAMREPLY parsing extracts only the first prefix character. Update to:
1. Consume ALL leading prefix chars (using `isupport_parsed.prefix_map()` to know which chars are prefixes)
2. If `userhost-in-names` is enabled, split `nick!user@host` to extract ident/host
3. Store all modes on NickEntry

**Step 3: Add ident/host fields to NickEntry if not present**

Check if NickEntry needs `ident: Option<String>` and `host: Option<String>` fields.

**Step 4: Run tests**

Run: `cargo test`
Expected: all PASS

**Step 5: Commit**

```bash
git add src/irc/events.rs src/state/buffer.rs docs/rfc_ircv3_coverage.md
git commit -m "feat: multi-prefix and userhost-in-names NAMES parsing"
```

Update `docs/rfc_ircv3_coverage.md`: mark `multi-prefix` and `userhost-in-names` as **Done**.

---

### Task 5: extended-join + account-notify + account-tag

**Files:**
- Modify: `src/irc/events.rs` (JOIN handler, add ACCOUNT handler, extract account tag)
- Modify: `src/state/buffer.rs` (NickEntry already has `account: Option<String>`)
- Test: inline tests

**Context:**
These three caps all deal with account tracking:
- `extended-join`: JOIN message includes account and realname — `nick!user@host JOIN #channel account :realname`
- `account-notify`: Server sends `ACCOUNT` command when user logs in/out
- `account-tag`: Messages include `account=username` tag

**Step 1: Write tests for extended JOIN parsing**

```rust
#[test]
fn extended_join_with_account() {
    // JOIN #channel account :Real Name
    // args = ["#channel", "account", "Real Name"]
    // Should set NickEntry.account = Some("account")
}

#[test]
fn extended_join_no_account() {
    // JOIN #channel * :Real Name
    // args = ["#channel", "*", "Real Name"]
    // account="*" means not logged in → NickEntry.account = None
}
```

**Step 2: Update `handle_join()` to parse extended format**

In `handle_join()` (line 469), check if args has 3 elements (extended) vs 1 (standard):
- `args[1]` = account ("*" if not logged in)
- `args[2]` = realname
- Store account on the new NickEntry

**Step 3: Add ACCOUNT command handler**

Add match arm for `Command::ACCOUNT(account_name)` in `handle_irc_message()`:
- If `account_name == "*"` → user logged out, clear account on all NickEntries
- Otherwise → set account on all NickEntries for that nick across buffers

**Step 4: Extract account from message tags**

In `extract_tags()` helper, also set `NickEntry.account` from `account` tag if present and `account-tag` cap is enabled.

**Step 5: Run tests**

Run: `cargo test`
Expected: all PASS

**Step 6: Commit**

```bash
git add src/irc/events.rs docs/rfc_ircv3_coverage.md
git commit -m "feat: extended-join, account-notify, and account-tag for user account tracking"
```

Update `docs/rfc_ircv3_coverage.md`: mark `extended-join`, `account-notify`, and `account-tag` as **Done**.

---

### Task 6: server-time

**Files:**
- Modify: `src/irc/events.rs` (use `time` tag for message timestamps)
- Test: inline test

**Context:**
When `server-time` cap is enabled, messages include `@time=2026-03-06T12:34:56.789Z`. We should use this as the message timestamp instead of `Utc::now()`. This is critical for bouncer/relay playback where messages arrive out of order.

**Step 1: Write test**

```rust
#[test]
fn server_time_tag_used_as_timestamp() {
    // Message with @time=2026-03-06T12:00:00.000Z
    // Buffer message timestamp should be 2026-03-06T12:00:00Z, not Utc::now()
}
```

**Step 2: Implement**

Create helper:

```rust
fn message_timestamp(tags: &HashMap<String, String>) -> DateTime<Utc> {
    tags.get("time")
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map_or_else(Utc::now, |dt| dt.with_timezone(&Utc))
}
```

Replace all `Utc::now()` calls in event handlers with `message_timestamp(&tags)`.

**Step 3: Run tests**

Run: `cargo test`
Expected: all PASS

**Step 4: Commit**

```bash
git add src/irc/events.rs docs/rfc_ircv3_coverage.md
git commit -m "feat: server-time cap — use server timestamps for message ordering"
```

Update `docs/rfc_ircv3_coverage.md`: mark `server-time` as **Done**.

---

### Task 7: away-notify + chghost

**Files:**
- Modify: `src/irc/events.rs` (add AWAY and CHGHOST handlers)
- Modify: `src/state/buffer.rs` (NickEntry already has `away: bool`)
- Test: inline tests

**Context:**
- `away-notify`: Server sends `:nick!user@host AWAY :reason` or `:nick!user@host AWAY` (back). Update NickEntry.away across all shared channels.
- `chghost`: Server sends `:nick!user@host CHGHOST newuser newhost`. Update displayed host in nick list.

**Step 1: Write tests**

```rust
#[test]
fn away_notify_sets_away() {
    // AWAY :Gone fishing → NickEntry.away = true
}

#[test]
fn away_notify_clears_away() {
    // AWAY (no params) → NickEntry.away = false
}

#[test]
fn chghost_updates_host() {
    // CHGHOST newuser newhost → update in all shared buffers
}
```

**Step 2: Implement AWAY handler**

Add `Command::AWAY(reason)` match in `handle_irc_message()`:
- Extract nick from prefix
- Find all buffers containing this nick
- Update `NickEntry.away` = `reason.is_some()`
- Optionally add event message to shared buffers

**Step 3: Implement CHGHOST handler**

Add `Command::CHGHOST(new_user, new_host)` match:
- Extract nick from prefix
- Update ident/host on NickEntry in all shared buffers
- Add event message: "nick changed host to newuser@newhost"

**Step 4: Run tests**

Run: `cargo test`
Expected: all PASS

**Step 5: Commit**

```bash
git add src/irc/events.rs docs/rfc_ircv3_coverage.md
git commit -m "feat: away-notify and chghost for real-time user status tracking"
```

Update `docs/rfc_ircv3_coverage.md`: mark `away-notify` and `chghost` as **Done**.

---

### Task 8: cap-notify

**Files:**
- Modify: `src/irc/events.rs` (handle CAP NEW/DEL)
- Modify: `src/irc/cap.rs` (add helpers for re-negotiation)
- Modify: `src/state/connection.rs` (update `enabled_caps`)
- Test: inline tests

**Context:**
When `cap-notify` is enabled, the server sends `CAP * NEW :cap1 cap2` when new caps become available and `CAP * DEL :cap1` when caps are removed. We should auto-request new desired caps and remove deleted ones from our enabled set.

**Step 1: Write tests**

```rust
#[test]
fn cap_new_requests_desired_caps() {
    // CAP * NEW :echo-message batch
    // Should auto-request echo-message and batch
}

#[test]
fn cap_del_removes_from_enabled() {
    // CAP * DEL :server-time
    // Should remove server-time from enabled_caps
}
```

**Step 2: Implement CAP NEW/DEL handlers**

In `handle_irc_message()`, add match for `Command::CAP(_, CapSubCommand::NEW, ...)`:
- Parse new caps
- Filter to DESIRED_CAPS
- Send `CAP REQ :new_desired_caps`
- On ACK, add to `conn.enabled_caps`

Add match for `Command::CAP(_, CapSubCommand::DEL, ...)`:
- Parse deleted caps
- Remove from `conn.enabled_caps`

**Step 3: Run tests**

Run: `cargo test`
Expected: all PASS

**Step 4: Commit**

```bash
git add src/irc/events.rs src/irc/cap.rs src/state/connection.rs docs/rfc_ircv3_coverage.md
git commit -m "feat: cap-notify for runtime capability changes"
```

Update `docs/rfc_ircv3_coverage.md`: mark `cap-notify` as **Done**.

---

### Task 9: echo-message

**Files:**
- Modify: `src/irc/events.rs` (detect own echoed messages)
- Modify: `src/app.rs` (suppress local echo when echo-message enabled)
- Test: inline tests

**Context:**
With `echo-message`, the server echoes our own PRIVMSG/NOTICE back to us. This means we should NOT display messages locally when sending — instead wait for the server echo. This ensures consistency (server-applied transformations, server-time stamps).

**Step 1: Write test**

```rust
#[test]
fn echo_message_skips_duplicate() {
    // When echo-message is enabled and we receive our own PRIVMSG,
    // it should be displayed (not suppressed as self-message)
}
```

**Step 2: Implement**

In `handle_privmsg()` and `handle_notice()`:
- Check if message source nick == our nick AND `echo-message` is in `conn.enabled_caps`
- If so: display normally (the server echo IS the authoritative message)
- In `App` where messages are sent (`cmd_msg`, `cmd_me`, etc.): if `echo-message` is enabled, do NOT add local message to buffer — the echo will arrive shortly

**Step 3: Run tests**

Run: `cargo test`
Expected: all PASS

**Step 4: Commit**

```bash
git add src/irc/events.rs src/app.rs docs/rfc_ircv3_coverage.md
git commit -m "feat: echo-message cap — server-authoritative message display"
```

Update `docs/rfc_ircv3_coverage.md`: mark `echo-message` as **Done**.

---

### Task 10: invite-notify

**Files:**
- Modify: `src/irc/events.rs` (enhance INVITE handler)
- Test: inline test

**Context:**
With `invite-notify`, channel members (with appropriate privileges) see when someone is invited to the channel. Our existing `handle_invite()` (lines 1098-1133) already handles INVITE — we just need to:
1. Ensure the cap is requested (already in DESIRED_CAPS)
2. When we're NOT the target of the invite, display it as a channel event

**Step 1: Update handle_invite()**

Current handler only shows invite when we're the target. Add:
- If invite target != our nick, display in channel buffer: "nick invited target to #channel"

**Step 2: Run tests**

Run: `cargo test`
Expected: all PASS

**Step 3: Commit**

```bash
git add src/irc/events.rs docs/rfc_ircv3_coverage.md
git commit -m "feat: invite-notify — show channel invitations to members"
```

Update `docs/rfc_ircv3_coverage.md`: mark `invite-notify` as **Done**.

---

### Task 11: batch (NETSPLIT/NETJOIN)

**Files:**
- Create: `src/irc/batch.rs`
- Modify: `src/irc/mod.rs` (add `pub mod batch;`)
- Modify: `src/irc/events.rs` (BATCH start/end, route batched messages)
- Modify: `src/irc/netsplit.rs` (integrate with batch)
- Test: `src/irc/batch.rs` (inline tests)

**Context:**
IRCv3 `batch` wraps related messages in `BATCH +ref type params` / `BATCH -ref` pairs. Messages within a batch have `@batch=ref` tag. For `NETSPLIT`/`NETJOIN` batch types, this replaces our manual netsplit detection heuristic with server-authoritative grouping.

**Step 1: Write BatchTracker struct and tests**

```rust
/// Tracks open batches by reference tag.
#[derive(Debug, Default)]
pub struct BatchTracker {
    /// Open batches: ref_tag → BatchInfo
    open: HashMap<String, BatchInfo>,
}

#[derive(Debug, Clone)]
pub struct BatchInfo {
    pub batch_type: String,
    pub params: Vec<String>,
    pub messages: Vec<irc::proto::Message>,
}
```

Tests:
- Open a batch, add messages, close it, verify messages collected
- Nested batches
- NETSPLIT batch produces correct summary

**Step 2: Implement BatchTracker**

- `start_batch(ref_tag, batch_type, params)` — create new entry
- `is_batched(msg) -> bool` — check `@batch` tag
- `add_message(msg)` — append to batch's message list
- `end_batch(ref_tag) -> Option<BatchInfo>` — remove and return

**Step 3: Wire into events.rs**

In `handle_irc_message()`:
- Match `Command::BATCH(ref_tag, sub_command, params)`:
  - `+ref type params` → `batch_tracker.start_batch()`
  - `-ref` → `batch_tracker.end_batch()`, process collected messages
- For all other messages: check if `@batch` tag exists → if so, add to batch tracker instead of processing immediately

For NETSPLIT/NETJOIN batch types:
- On batch end, generate summary message using existing netsplit display logic
- Skip individual QUIT/JOIN processing for batched messages

**Step 4: Run tests**

Run: `cargo test`
Expected: all PASS

**Step 5: Commit**

```bash
git add src/irc/batch.rs src/irc/mod.rs src/irc/events.rs docs/rfc_ircv3_coverage.md
git commit -m "feat: IRCv3 batch support with NETSPLIT/NETJOIN integration"
```

Update `docs/rfc_ircv3_coverage.md`: mark `batch` as **Done**.

---

### Task 12: SASL EXTERNAL

**Files:**
- Modify: `src/irc/mod.rs` (add EXTERNAL mechanism to SASL flow)
- Modify: `src/config/mod.rs` (add `sasl_mechanism` and `client_cert_path` fields)
- Test: inline test

**Context:**
SASL EXTERNAL uses a client TLS certificate (CertFP) for authentication. The flow is:
1. `AUTHENTICATE EXTERNAL`
2. Server sends `AUTHENTICATE +`
3. Client sends `AUTHENTICATE +` (empty credential, cert speaks for itself)
4. Server sends 903 (success) or 904 (fail)

The irc crate already supports `client_cert_path` in its config.

**Step 1: Add config field**

Add `sasl_mechanism: Option<String>` to `ServerConfig`. Values: `"PLAIN"`, `"EXTERNAL"`, or `None` (auto-detect).

**Step 2: Implement EXTERNAL flow**

In `negotiate_caps()`, after SASL ACK:
- If mechanism is EXTERNAL (or auto-detected from server's `sasl=EXTERNAL,...`):
  - Send `AUTHENTICATE EXTERNAL`
  - Wait for `AUTHENTICATE +`
  - Send `AUTHENTICATE +` (base64 of empty string = `+`)
  - Wait for 903/904

**Step 3: Add mechanism selection logic**

Priority order when auto-detecting:
1. EXTERNAL (if client cert is configured)
2. SCRAM-SHA-256 (if available, implemented in Task 13)
3. PLAIN (fallback)

**Step 4: Run tests**

Run: `cargo test`
Expected: all PASS

**Step 5: Commit**

```bash
git add src/irc/mod.rs src/config/mod.rs docs/rfc_ircv3_coverage.md
git commit -m "feat: SASL EXTERNAL authentication via client TLS certificate"
```

Update `docs/rfc_ircv3_coverage.md`: mark "SASL EXTERNAL" and "SASL mechanism selection" as **Done**.

---

### Task 13: SASL SCRAM-SHA-256

**Files:**
- Create: `src/irc/sasl_scram.rs`
- Modify: `src/irc/mod.rs` (add SCRAM option to SASL flow, `pub mod sasl_scram;`)
- Modify: `Cargo.toml` (add `sha2`, `hmac`, `pbkdf2`, `rand` deps)
- Test: `src/irc/sasl_scram.rs` (inline tests)

**Context:**
SCRAM-SHA-256 is a challenge-response SASL mechanism that avoids sending the password in cleartext. Flow:
1. `AUTHENTICATE SCRAM-SHA-256`
2. Client → server: `n,,n=username,r=client_nonce` (base64)
3. Server → client: `r=combined_nonce,s=salt,i=iterations` (base64)
4. Client computes salted password, client proof
5. Client → server: `c=biws,r=combined_nonce,p=client_proof` (base64)
6. Server → client: `v=server_signature` (base64, verify)
7. 903 success

**Step 1: Write SCRAM implementation and tests**

Implement RFC 5802 SCRAM-SHA-256:
- `client_first_message(username) -> (String, String)` — returns message + client_nonce
- `client_final_message(server_first, client_first_bare, password, client_nonce) -> (String, Vec<u8>)` — computes proof
- `verify_server(server_final, expected_signature) -> bool`

Tests:
- Known test vectors from RFC 7677
- Round-trip with mock server response

**Step 2: Wire into negotiate_caps()**

After SASL ACK, if mechanism is SCRAM-SHA-256:
- Multi-step AUTHENTICATE exchange
- Base64 encode/decode each step
- Handle 400+ byte messages (split at 400 bytes per AUTHENTICATE line)

**Step 3: Run tests**

Run: `cargo test`
Expected: all PASS

**Step 4: Commit**

```bash
git add src/irc/sasl_scram.rs src/irc/mod.rs Cargo.toml docs/rfc_ircv3_coverage.md
git commit -m "feat: SASL SCRAM-SHA-256 challenge-response authentication"
```

Update `docs/rfc_ircv3_coverage.md`: mark "SASL SCRAM-SHA-256" as **Done**.

---

### Task 14: WHOX (Extended WHO)

**Files:**
- Modify: `src/commands/handlers_irc.rs` (upgrade `/who` command)
- Modify: `src/irc/events.rs` (parse RPL_WHOSPCRPL 354)
- Modify: `src/irc/events.rs` (auto-WHO on channel join)
- Test: inline tests

**Context:**
WHOX extends WHO with field selectors. Instead of `WHO #channel`, send `WHO #channel %tcuihsnfdlar,token`. The response uses numeric 354 (RPL_WHOSPCRPL) instead of 352 (RPL_WHOREPLY). The `a` field returns the user's SASL account name — critical for account tracking without `extended-join`.

Auto-detect: `isupport_parsed.has_whox()`.

**Step 1: Write tests**

```rust
#[test]
fn whox_response_parsed() {
    // :server 354 me 123 #channel ~user 1.2.3.4 host.com server nick H@ 0 42 patrick :Real Name
    // Fields: t=123 c=#channel u=~user i=1.2.3.4 h=host.com s=server n=nick f=H@ d=0 l=42 a=patrick r=Real Name
}

#[test]
fn whox_account_stored_on_nick_entry() {
    // Account field "patrick" → NickEntry.account = Some("patrick")
    // Account field "0" → NickEntry.account = None
}
```

**Step 2: Implement WHOX in `/who` command**

In `cmd_who()`, check if connection has WHOX support:
```rust
if conn.isupport_parsed.has_whox() {
    // WHO #channel %tcuihsnfdlar,123
    sender.send(Command::Raw("WHO".into(), vec![target, "%tcuihsnfdlar,123".into()]))?;
} else {
    sender.send(Command::WHO(Some(target), None))?;
}
```

**Step 3: Handle RPL_WHOSPCRPL (354)**

Add handler for `Response::RPL_WHOSPCRPL` (or match on raw 354):
- Parse fields based on the `%` selector we sent
- Use token (123) to match our request
- Update NickEntry with account, ident, host, realname, away status

**Step 4: Auto-WHO on channel join**

After RPL_ENDOFNAMES (when channel join is complete):
- If WHOX is available, send `WHO #channel %tcuihsnfdlar,<token>`
- This populates account info for all channel members

**Step 5: Run tests**

Run: `cargo test`
Expected: all PASS

**Step 6: Commit**

```bash
git add src/commands/handlers_irc.rs src/irc/events.rs docs/rfc_ircv3_coverage.md
git commit -m "feat: WHOX extended WHO with account tracking and auto-WHO on join"
```

Update `docs/rfc_ircv3_coverage.md`: mark "WHOX" as **Done**.

---

### Task 15: Extban

**Files:**
- Create: `src/irc/extban.rs`
- Modify: `src/irc/mod.rs` (add `pub mod extban;`)
- Modify: `src/irc/events.rs` (display extbans in ban list)
- Modify: `src/commands/handlers_irc.rs` (compose extbans in /ban)
- Test: `src/irc/extban.rs` (inline tests)

**Context:**
Extbans extend the nick!user@host mask format. When the nick field starts with `$`, it's an extban: `$a:account!user@host`. The `$a` type matches by SASL account. Detect support via ISUPPORT `EXTBAN=$,a`.

**Step 1: Write tests**

```rust
#[test]
fn parse_extban_account() {
    let eb = Extban::parse("$a:patrick!*@*");
    assert_eq!(eb.ban_type, 'a');
    assert_eq!(eb.parameter, Some("patrick".into()));
    assert_eq!(eb.user, "*");
    assert_eq!(eb.host, "*");
}

#[test]
fn parse_extban_no_param() {
    let eb = Extban::parse("$a!*@*");
    assert_eq!(eb.ban_type, 'a');
    assert_eq!(eb.parameter, None);
}

#[test]
fn format_extban() {
    let eb = Extban::new('a', Some("patrick"), "*", "*");
    assert_eq!(eb.to_string(), "$a:patrick!*@*");
}

#[test]
fn display_extban_in_banlist() {
    // $a:patrick!*@* should display as "account:patrick (*@*)"
}
```

**Step 2: Implement Extban struct**

```rust
pub struct Extban {
    pub ban_type: char,
    pub parameter: Option<String>,
    pub user: String,
    pub host: String,
}

impl Extban {
    pub fn parse(mask: &str) -> Option<Self> { ... }
    pub fn new(ban_type: char, param: Option<&str>, user: &str, host: &str) -> Self { ... }
    pub fn display_friendly(&self) -> String { ... }
}

impl fmt::Display for Extban {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { ... }
}
```

**Step 3: Integrate with ban list display**

In RPL_BANLIST handler, check if mask starts with `$`:
- If so, parse as Extban and use `display_friendly()` for prettier output
- E.g., `$a:patrick!*@*` → "account:patrick (*@*)"

**Step 4: Add /ban account shorthand**

In `cmd_ban()`, add syntax: `/ban -a account` → generates `$a:account!*@*`
Only available when ISUPPORT EXTBAN includes `a`.

**Step 5: Run tests**

Run: `cargo test`
Expected: all PASS

**Step 6: Commit**

```bash
git add src/irc/extban.rs src/irc/mod.rs src/irc/events.rs src/commands/handlers_irc.rs docs/rfc_ircv3_coverage.md
git commit -m "feat: extban support — account-based bans with display and compose"
```

Update `docs/rfc_ircv3_coverage.md`: mark "Extban" as **Done**.

---

### Task 16: ERROR Command + Missing RFC 2812 Handlers

**Files:**
- Modify: `src/irc/events.rs` (add ERROR, LUSERS, VERSION, STATS, TIME, ADMIN, INFO handlers)
- Modify: `src/commands/handlers_irc.rs` (add /stats, /info, /admin, /lusers commands)
- Test: inline tests

**Context:**
Fill in remaining RFC 2812 gaps:
- `ERROR` command: server sends before forcibly closing connection — display in server buffer
- Server query commands: `/stats`, `/info`, `/admin`, `/lusers`, `/time` — send command, display response

**Step 1: Add ERROR handler**

```rust
Command::ERROR(ref message) => {
    // Display in server buffer, mark connection as errored
}
```

**Step 2: Add server query commands**

Simple pattern for each: send the command, add response handler for the numeric reply.

- `/stats <query>` → `STATS(query, None)` → RPL_STATSCOMMANDS etc.
- `/info` → `INFO(None)` → RPL_INFO/RPL_ENDOFINFO
- `/admin` → `ADMIN(None)` → RPL_ADMINME/RPL_ADMINLOC1/RPL_ADMINLOC2/RPL_ADMINEMAIL
- `/lusers` → `LUSERS(None, None)` → RPL_LUSERCLIENT/RPL_LUSEROP/etc.
- `/time` → `TIME(None)` → RPL_TIME

**Step 3: Register commands in registry**

Add entries in `src/commands/registry.rs`.

**Step 4: Run tests**

Run: `cargo test`
Expected: all PASS

**Step 5: Commit**

```bash
git add src/irc/events.rs src/commands/handlers_irc.rs src/commands/registry.rs docs/rfc_ircv3_coverage.md
git commit -m "feat: ERROR handler and remaining RFC 2812 server query commands"
```

Update `docs/rfc_ircv3_coverage.md`: mark ERROR, LUSERS, STATS, TIME, ADMIN, INFO as **Done**.

---

## Summary

| Task | Component | Caps/Features |
|------|-----------|---------------|
| 1 | ISUPPORT parsing | Foundation for all |
| 2 | CAP framework | Extensible negotiation |
| 3 | Message tags | Tag plumbing to buffer + storage |
| 4 | multi-prefix + userhost-in-names | NAMES parsing |
| 5 | extended-join + account-notify + account-tag | Account tracking |
| 6 | server-time | Authoritative timestamps |
| 7 | away-notify + chghost | User status tracking |
| 8 | cap-notify | Runtime cap changes |
| 9 | echo-message | Server-authoritative messages |
| 10 | invite-notify | Channel invite visibility |
| 11 | batch | NETSPLIT/NETJOIN batching |
| 12 | SASL EXTERNAL | CertFP authentication |
| 13 | SASL SCRAM-SHA-256 | Secure SASL |
| 14 | WHOX | Extended WHO + account |
| 15 | Extban | Account-based bans |
| 16 | RFC 2812 gaps | ERROR, /stats, /info, etc. |

**Dependencies:**
```
Task 1 (ISUPPORT) ──┐
                     ├──→ Task 4 (multi-prefix needs prefix_map())
Task 2 (CAP) ───────┤
                     ├──→ Task 5-11 (all need caps enabled)
Task 3 (tags) ──────┤
                     ├──→ Task 6 (server-time needs tag extraction)
                     └──→ Task 14 (WHOX needs has_whox())
Task 12-13 (SASL) ── independent after Task 2
Task 15 (extban) ─── independent after Task 1
Task 16 (RFC gaps) ── fully independent
```

**Parallel execution:** Tasks 1, 2, 3 are foundations (sequential). After those, tasks 4-11 + 14-16 can run in any order. Tasks 12-13 need Task 2.

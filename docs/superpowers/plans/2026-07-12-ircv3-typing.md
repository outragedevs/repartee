# IRCv3 `+typing` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Repartee shows who is typing in a channel or query, and tells others when we are typing, via the IRCv3 `+typing` client tag on `TAGMSG`.

**Architecture:** A pure protocol module (`src/irc/typing.rs`) parses and builds the tag. An ephemeral `TypingTracker` in `AppState` holds who is typing where. An outbound state machine in `src/app/typing.rs` is driven by exactly two hooks — the keystroke path and the existing 1-second tick — and every send passes the same guard set (cap, `CLIENTTAGDENY`, config, buffer type, flood suppression). TUI and web UI are two input sources feeding that single machine.

**Tech Stack:** Rust 2024, `irc-repartee` 1.5.1 (fork of `irc`), ratatui (TUI), Leptos/WASM (web UI), tokio.

**Spec:** `docs/superpowers/specs/2026-07-12-ircv3-typing-design.md` — read it before starting. Section references below (§3.1, §4.5, …) point into it.

## Global Constraints

- **No changes to the `irc-repartee` / `irc-proto-repartee` crates.** Everything works with the published 1.5.1 / 1.2.2. If you find yourself wanting to edit the fork, stop and re-read §2.2 and §3.1 of the spec.
- **`message-tags` is already negotiated** (`src/irc/cap.rs:20`). Do **not** add a capability. There is no `draft/typing` cap.
- **Builds go through `make`**, never raw cargo/trunk: `make test`, `make clippy`, `make wasm`, `make release`.
- **Clippy is a 0-warning gate**: pedantic=warn, nursery=warn, perf=deny, redundant_clone=deny. `make clippy` must be clean. (Note: ~44 pre-existing warnings exist in unrelated files from a toolchain bump — attribute warnings per-file; only files you touch must be clean.)
- **`src/state/` is UI-agnostic** — no ratatui imports there, ever.
- **Logging is `tracing`**, never `println!`. Errors are `color-eyre`.
- **Never call `Instant::now()` inside protocol or state logic.** Every time-dependent function takes `now: Instant`. This is what makes the whole feature testable without sleeping, and it is not negotiable.
- The crate is aliased: use `irc::proto::…` (it resolves to `irc-repartee`), matching `src/irc/multiline.rs`.
- Work on branch `feat/ircv3-typing-design` (already created, spec committed there). Commit after every task.

---

### Task 1: Protocol layer — `src/irc/typing.rs`

Pure tag parsing and TAGMSG construction. No state, no I/O, no `Instant::now()`.

**Files:**
- Create: `src/irc/typing.rs`
- Modify: `src/irc/mod.rs` (register the module)
- Test: inline `#[cfg(test)] mod tests` in `src/irc/typing.rs` (repo convention — see `src/irc/isupport.rs`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub const TAG: &str` = `"+typing"`, `pub const TAG_LEGACY: &str` = `"+draft/typing"`
  - `pub const THROTTLE: Duration` (3s), `pub const ACTIVE_TTL: Duration` (6s), `pub const PAUSED_TTL: Duration` (30s)
  - `pub enum TypingState { Active, Paused, Done }` — `Copy`, with `as_str(self) -> &'static str`, `parse(&str) -> Option<Self>`, `ttl(self) -> Option<Duration>`
  - `pub fn parse_typing(tags: &HashMap<String, String>) -> Option<TypingState>`
  - `pub fn build_tagmsg(target: &str, state: TypingState) -> irc::proto::Message`
  - `pub fn should_type(input: &str) -> bool`

- [ ] **Step 1: Write the failing tests**

Create `src/irc/typing.rs` containing **only** this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_through_str() {
        for s in [TypingState::Active, TypingState::Paused, TypingState::Done] {
            assert_eq!(TypingState::parse(s.as_str()), Some(s));
        }
    }

    #[test]
    fn parse_rejects_junk_and_wrong_case() {
        // Tag values are matched exactly; nothing licenses case folding.
        assert_eq!(TypingState::parse("ACTIVE"), None);
        assert_eq!(TypingState::parse("typing"), None);
        assert_eq!(TypingState::parse(""), None);
        assert_eq!(TypingState::parse("active "), None);
    }

    #[test]
    fn ttls_match_the_spec() {
        assert_eq!(TypingState::Active.ttl(), Some(Duration::from_secs(6)));
        assert_eq!(TypingState::Paused.ttl(), Some(Duration::from_secs(30)));
        assert_eq!(TypingState::Done.ttl(), None);
    }

    #[test]
    fn parse_typing_reads_the_ratified_tag() {
        let tags = HashMap::from([("+typing".to_string(), "active".to_string())]);
        assert_eq!(parse_typing(&tags), Some(TypingState::Active));
    }

    #[test]
    fn parse_typing_accepts_the_legacy_draft_tag() {
        // Pre-ratification clients still emit `+draft/typing` (spec §1.4).
        let tags = HashMap::from([("+draft/typing".to_string(), "paused".to_string())]);
        assert_eq!(parse_typing(&tags), Some(TypingState::Paused));
    }

    #[test]
    fn parse_typing_ignores_absent_empty_and_unknown() {
        assert_eq!(parse_typing(&HashMap::new()), None);
        let empty = HashMap::from([("+typing".to_string(), String::new())]);
        assert_eq!(parse_typing(&empty), None);
        let junk = HashMap::from([("+typing".to_string(), "wat".to_string())]);
        assert_eq!(parse_typing(&junk), None);
    }

    #[test]
    fn build_tagmsg_matches_the_spec_wire_format() {
        let msg = build_tagmsg("#rust", TypingState::Active);
        assert_eq!(msg.to_string(), "@+typing=active TAGMSG #rust\r\n");
        let msg = build_tagmsg("alice", TypingState::Done);
        assert_eq!(msg.to_string(), "@+typing=done TAGMSG alice\r\n");
    }

    #[test]
    fn build_tagmsg_round_trips_through_the_parser() {
        // Guards the assumption the whole feature rests on: the crate's
        // Display serializes tags, and its FromStr reads them back.
        let wire = build_tagmsg("#rust", TypingState::Paused).to_string();
        let parsed: irc::proto::Message = wire.parse().expect("parses");
        let tags = parsed
            .tags
            .expect("has tags")
            .into_iter()
            .filter_map(|t| Some((t.0, t.1?)))
            .collect::<HashMap<String, String>>();
        assert_eq!(parse_typing(&tags), Some(TypingState::Paused));
    }

    #[test]
    fn should_type_excludes_slash_commands_but_not_actions() {
        assert!(should_type("hello"));
        assert!(should_type("/me waves"));  // an action is a message
        assert!(!should_type(""));
        assert!(!should_type("/join #rust"));
        assert!(!should_type("/me"));       // bare command, no text
    }
}
```

Register the module — add to `src/irc/mod.rs` alongside the other `pub mod` lines (keep alphabetical order with its neighbours):

```rust
pub mod typing;
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `make test`
Expected: FAIL — compile errors, `cannot find type TypingState in this scope`, `cannot find function parse_typing`, etc.

- [ ] **Step 3: Write the implementation**

Prepend to `src/irc/typing.rs`, above the test module:

```rust
//! IRCv3 `typing` client tag — <https://ircv3.net/specs/client-tags/typing>.
//!
//! Typing status rides on `TAGMSG` and depends only on the `message-tags`
//! capability, which repartee already negotiates. This module is the pure
//! protocol layer: no state, no I/O, and no clock. Callers pass `now` in, which
//! is what lets the state machines above this be tested without sleeping.

use std::collections::HashMap;
use std::time::Duration;

/// The ratified tag name — the only one we ever send.
pub const TAG: &str = "+typing";

/// The pre-ratification name. Accepted on receive for interop with older
/// clients; never sent, because emitting both would double our TAGMSG volume
/// and therefore our flood cost.
pub const TAG_LEGACY: &str = "+draft/typing";

/// Spec: "Input event handlers MUST be throttled so that any `typing`
/// notification is not sent within 3 seconds of another one for a given target."
pub const THROTTLE: Duration = Duration::from_secs(3);

/// Spec: a receiver assumes typing has stopped once "at least 6 seconds have
/// passed since the last `typing=active` notification was received".
pub const ACTIVE_TTL: Duration = Duration::from_secs(6);

/// Spec: ditto, "at least 30 seconds" for `typing=paused`.
pub const PAUSED_TTL: Duration = Duration::from_secs(30);

/// The three values the `typing` tag may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypingState {
    Active,
    Paused,
    Done,
}

impl TypingState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Done => "done",
        }
    }

    /// Values are matched exactly. The message-tags spec makes tag names
    /// "case-sensitive opaque identifiers" and nothing licenses folding values,
    /// so `ACTIVE` is not `active`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "paused" => Some(Self::Paused),
            "done" => Some(Self::Done),
            _ => None,
        }
    }

    /// How long a received state survives without a refresh. `Done` is not a
    /// state that lingers — it is an instruction to forget.
    #[must_use]
    pub const fn ttl(self) -> Option<Duration> {
        match self {
            Self::Active => Some(ACTIVE_TTL),
            Self::Paused => Some(PAUSED_TTL),
            Self::Done => None,
        }
    }
}

/// Read a typing state out of already-extracted message tags.
///
/// The ratified `+typing` wins over the legacy `+draft/typing`. An absent,
/// empty, or unrecognised value yields `None`: the message-tags spec makes
/// empty and missing values equivalent, and neither carries a state.
#[must_use]
pub fn parse_typing(tags: &HashMap<String, String>) -> Option<TypingState> {
    tags.get(TAG)
        .or_else(|| tags.get(TAG_LEGACY))
        .map(String::as_str)
        .and_then(TypingState::parse)
}

/// Build `@+typing=<state> TAGMSG <target>`.
///
/// `TAGMSG` has no `Command` variant in `irc-proto-repartee`, so it rides
/// `Command::Raw`, which stringifies verbatim. The crate's `Display for Message`
/// serializes the tag with spec-correct escaping — see spec §2.2/§2.3.
#[must_use]
pub fn build_tagmsg(target: &str, state: TypingState) -> irc::proto::Message {
    irc::proto::Message {
        tags: Some(vec![irc::proto::message::Tag(
            TAG.to_string(),
            Some(state.as_str().to_string()),
        )]),
        prefix: None,
        command: irc::proto::Command::Raw("TAGMSG".to_string(), vec![target.to_string()]),
    }
}

/// Whether the current input text should produce typing notifications.
///
/// Spec: `typing=active` is sent "while the user is making updates to the
/// text-input field **and the text is not a '/slash command'**". `/me` is a
/// message rather than a command, so it counts as typing; a bare `/me` with no
/// text does not.
#[must_use]
pub fn should_type(input: &str) -> bool {
    if input.is_empty() {
        return false;
    }
    !input.starts_with('/') || input.starts_with("/me ")
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `make test`
Expected: PASS — all 9 tests in `irc::typing::tests` green.

Run: `make clippy`
Expected: no new warnings from `src/irc/typing.rs`.

- [ ] **Step 5: Commit**

```bash
git add src/irc/typing.rs src/irc/mod.rs
git commit -m "feat(irc): IRCv3 +typing tag protocol layer"
```

---

### Task 2: `CLIENTTAGDENY` accessor — `src/irc/isupport.rs`

The server may block client-only tags. `Isupport` already captures every 005 token in a generic `HashMap` (`src/irc/isupport.rs:18`); it just needs to answer the question.

**Files:**
- Modify: `src/irc/isupport.rs`
- Test: inline `#[cfg(test)] mod tests` in the same file

**Interfaces:**
- Consumes: nothing.
- Produces: `pub fn client_tag_allowed(&self, tag: &str) -> bool` on `Isupport` (note the type is spelled `Isupport`, and `Connection` holds it as `isupport_parsed` — `Connection.isupport` is the raw token map). `tag` is passed **without** the `+` prefix (e.g. `"typing"`), matching the token's own format.

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `src/irc/isupport.rs`:

```rust
#[test]
fn client_tag_allowed_by_default() {
    // Absent token = everything allowed (spec: "An empty or missing
    // CLIENTTAGDENY matches the default case").
    let isupport = Isupport::new();
    assert!(isupport.client_tag_allowed("typing"));
}

#[test]
fn client_tag_allowed_with_empty_token() {
    let mut isupport = Isupport::new();
    isupport.parse_tokens(&["CLIENTTAGDENY="]);
    assert!(isupport.client_tag_allowed("typing"));
}

#[test]
fn client_tag_blocked_by_wildcard() {
    let mut isupport = Isupport::new();
    isupport.parse_tokens(&["CLIENTTAGDENY=*"]);
    assert!(!isupport.client_tag_allowed("typing"));
}

#[test]
fn client_tag_exempted_from_wildcard_block() {
    // `*,-typing` = block everything except typing.
    let mut isupport = Isupport::new();
    isupport.parse_tokens(&["CLIENTTAGDENY=*,-typing,-example/bar"]);
    assert!(isupport.client_tag_allowed("typing"));
    assert!(isupport.client_tag_allowed("example/bar"));
    assert!(!isupport.client_tag_allowed("react"));
}

#[test]
fn client_tag_blocked_by_explicit_list() {
    let mut isupport = Isupport::new();
    isupport.parse_tokens(&["CLIENTTAGDENY=typing,example/bar"]);
    assert!(!isupport.client_tag_allowed("typing"));
    assert!(isupport.client_tag_allowed("react"));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `make test`
Expected: FAIL — `no method named client_tag_allowed found for struct Isupport`.

- [ ] **Step 3: Write the implementation**

Add to `impl Isupport` in `src/irc/isupport.rs`, next to the other accessors (`network()`, `has_whox()`):

```rust
/// Whether the server will relay a given client-only tag, per the
/// `CLIENTTAGDENY` token (message-tags spec, RPL_ISUPPORT Tokens).
///
/// `tag` is given without the `+` prefix, e.g. `"typing"` — that is the form
/// the token itself uses. An absent or empty token means everything is
/// allowed. `*` blocks all; a `-` entry negates a block.
#[must_use]
pub fn client_tag_allowed(&self, tag: &str) -> bool {
    let Some(value) = self.tokens.get("CLIENTTAGDENY") else {
        return true;
    };
    if value.is_empty() {
        return true;
    }

    let mut allowed = true;
    for entry in value.split(',') {
        match entry.strip_prefix('-') {
            // `-foo` — exempt from a catch-all block.
            Some(exempt) if exempt == tag => allowed = true,
            Some(_) => {}
            None if entry == "*" => allowed = false,
            None if entry == tag => allowed = false,
            None => {}
        }
    }
    allowed
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `make test`
Expected: PASS — the 5 new tests green.

Run: `make clippy`
Expected: no new warnings from `src/irc/isupport.rs`.

- [ ] **Step 5: Commit**

```bash
git add src/irc/isupport.rs
git commit -m "feat(irc): parse CLIENTTAGDENY from ISUPPORT"
```

---

### Task 3: Typing state tracker — `src/state/typing.rs`

Who is typing where. Ephemeral: never persisted, never in the session snapshot. Kept out of `Buffer` because `Buffer` has no constructor and 39 struct literals (spec §4.3).

**Files:**
- Create: `src/state/typing.rs`
- Modify: `src/state/mod.rs` (register module; add `pub typing: TypingTracker` to `AppState`), `src/state/events.rs:10` (one line in `AppState::new()`)
- Test: inline `#[cfg(test)] mod tests` in `src/state/typing.rs`

**Interfaces:**
- Consumes: `crate::irc::typing::TypingState` (Task 1).
- Produces:
  - `pub struct TypingEntry { pub state: TypingState, pub since: Instant }` (`Copy`)
  - `pub struct TypingTracker` (`Default`) with:
    - `set(&mut self, buffer_id: &str, nick: &str, state: TypingState, now: Instant) -> bool` — `true` when the *visible set* changed
    - `clear(&mut self, buffer_id: &str, nick: &str) -> bool`
    - `clear_nick(&mut self, nick: &str) -> Vec<String>` — buffer ids affected (QUIT / NICK change)
    - `remove_buffer(&mut self, buffer_id: &str)`
    - `expire(&mut self, now: Instant) -> Vec<String>` — buffer ids whose visible set changed
    - `nicks(&self, buffer_id: &str) -> Vec<&str>` — sorted, stable render order
  - `AppState.typing: TypingTracker`

- [ ] **Step 1: Write the failing tests**

Create `src/state/typing.rs` with **only** this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // A fixed origin so every test is deterministic — no wall clock anywhere.
    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn set_reports_a_visible_change_only_for_new_nicks() {
        let mut tr = TypingTracker::default();
        let now = t0();
        assert!(tr.set("net/#rust", "alice", TypingState::Active, now));
        // Refreshing an existing typer does not change what is displayed.
        assert!(!tr.set("net/#rust", "alice", TypingState::Active, now + Duration::from_secs(3)));
        assert!(tr.set("net/#rust", "bob", TypingState::Active, now));
    }

    #[test]
    fn done_removes_the_nick() {
        let mut tr = TypingTracker::default();
        let now = t0();
        tr.set("net/#rust", "alice", TypingState::Active, now);
        assert!(tr.set("net/#rust", "alice", TypingState::Done, now));
        assert!(tr.nicks("net/#rust").is_empty());
        // Done for someone who was not typing is not a visible change.
        assert!(!tr.set("net/#rust", "carol", TypingState::Done, now));
    }

    #[test]
    fn active_expires_after_six_seconds_paused_after_thirty() {
        let mut tr = TypingTracker::default();
        let now = t0();
        tr.set("net/#rust", "alice", TypingState::Active, now);
        tr.set("net/#rust", "bob", TypingState::Paused, now);

        // 5s: both still typing.
        assert!(tr.expire(now + Duration::from_secs(5)).is_empty());
        assert_eq!(tr.nicks("net/#rust"), vec!["alice", "bob"]);

        // 6s: alice's `active` is gone, bob's `paused` survives.
        let changed = tr.expire(now + Duration::from_secs(6));
        assert_eq!(changed, vec!["net/#rust"]);
        assert_eq!(tr.nicks("net/#rust"), vec!["bob"]);

        // 30s: bob goes too.
        let changed = tr.expire(now + Duration::from_secs(30));
        assert_eq!(changed, vec!["net/#rust"]);
        assert!(tr.nicks("net/#rust").is_empty());
    }

    #[test]
    fn clear_removes_one_nick_in_one_buffer() {
        let mut tr = TypingTracker::default();
        let now = t0();
        tr.set("net/#rust", "alice", TypingState::Active, now);
        assert!(tr.clear("net/#rust", "alice"));
        assert!(!tr.clear("net/#rust", "alice"));
    }

    #[test]
    fn clear_nick_removes_them_from_every_buffer() {
        let mut tr = TypingTracker::default();
        let now = t0();
        tr.set("net/#rust", "alice", TypingState::Active, now);
        tr.set("net/#tokio", "alice", TypingState::Active, now);
        tr.set("net/#rust", "bob", TypingState::Active, now);

        let mut affected = tr.clear_nick("alice");
        affected.sort();
        assert_eq!(affected, vec!["net/#rust", "net/#tokio"]);
        assert_eq!(tr.nicks("net/#rust"), vec!["bob"]);
        assert!(tr.nicks("net/#tokio").is_empty());
    }

    #[test]
    fn nicks_are_sorted_so_the_status_line_does_not_jitter() {
        let mut tr = TypingTracker::default();
        let now = t0();
        tr.set("net/#rust", "carol", TypingState::Active, now);
        tr.set("net/#rust", "alice", TypingState::Active, now);
        tr.set("net/#rust", "bob", TypingState::Active, now);
        assert_eq!(tr.nicks("net/#rust"), vec!["alice", "bob", "carol"]);
    }

    #[test]
    fn remove_buffer_drops_everything_for_it() {
        let mut tr = TypingTracker::default();
        tr.set("net/#rust", "alice", TypingState::Active, t0());
        tr.remove_buffer("net/#rust");
        assert!(tr.nicks("net/#rust").is_empty());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `make test`
Expected: FAIL — `cannot find type TypingTracker in this scope`.

- [ ] **Step 3: Write the implementation**

Prepend to `src/state/typing.rs`:

```rust
//! Who is currently typing, in which buffer.
//!
//! Ephemeral by design: never written to `SQLite`, never part of the
//! detach/reattach snapshot. On reattach the map is empty and refills from live
//! traffic within seconds.
//!
//! This lives beside `Buffer` rather than inside it: `Buffer` has no constructor
//! and is built from struct literals in ~39 places, so a field there would mean
//! 39 mechanical edits, mostly in test fixtures. It is also the better boundary
//! — typing is session state, not buffer content.

use std::collections::HashMap;
use std::time::Instant;

use crate::irc::typing::TypingState;

/// One peer's typing state in one buffer.
#[derive(Debug, Clone, Copy)]
pub struct TypingEntry {
    pub state: TypingState,
    /// When this state was last refreshed — the TTL clock (spec §1.2).
    pub since: Instant,
}

/// `buffer_id -> nick -> entry`.
#[derive(Debug, Default)]
pub struct TypingTracker {
    entries: HashMap<String, HashMap<String, TypingEntry>>,
}

impl TypingTracker {
    /// Record a peer's typing state.
    ///
    /// Returns `true` when the **visible set** of typing nicks changed — that is
    /// what decides whether the web clients need a push. Refreshing a nick that
    /// is already shown as typing changes nothing on screen.
    pub fn set(&mut self, buffer_id: &str, nick: &str, state: TypingState, now: Instant) -> bool {
        if state == TypingState::Done {
            return self.clear(buffer_id, nick);
        }
        self.entries
            .entry(buffer_id.to_string())
            .or_default()
            .insert(nick.to_string(), TypingEntry { state, since: now })
            .is_none()
    }

    /// Stop showing `nick` as typing in `buffer_id`. Returns `true` if they were.
    pub fn clear(&mut self, buffer_id: &str, nick: &str) -> bool {
        let Some(buf) = self.entries.get_mut(buffer_id) else {
            return false;
        };
        let removed = buf.remove(nick).is_some();
        if buf.is_empty() {
            self.entries.remove(buffer_id);
        }
        removed
    }

    /// Stop showing `nick` as typing anywhere — they quit, or changed nick.
    /// Returns the buffers that changed.
    pub fn clear_nick(&mut self, nick: &str) -> Vec<String> {
        let mut changed = Vec::new();
        self.entries.retain(|buffer_id, buf| {
            if buf.remove(nick).is_some() {
                changed.push(buffer_id.clone());
            }
            !buf.is_empty()
        });
        changed
    }

    /// Drop a closed buffer's state entirely.
    pub fn remove_buffer(&mut self, buffer_id: &str) {
        self.entries.remove(buffer_id);
    }

    /// Expire entries past their spec TTL (6s active, 30s paused).
    /// Returns the buffers whose visible set changed.
    pub fn expire(&mut self, now: Instant) -> Vec<String> {
        let mut changed = Vec::new();
        self.entries.retain(|buffer_id, buf| {
            let before = buf.len();
            buf.retain(|_, e| {
                e.state
                    .ttl()
                    .is_some_and(|ttl| now.duration_since(e.since) < ttl)
            });
            if buf.len() != before {
                changed.push(buffer_id.clone());
            }
            !buf.is_empty()
        });
        changed
    }

    /// Nicks currently typing in `buffer_id`, sorted so the status line does not
    /// reorder itself between frames.
    #[must_use]
    pub fn nicks(&self, buffer_id: &str) -> Vec<&str> {
        let Some(buf) = self.entries.get(buffer_id) else {
            return Vec::new();
        };
        let mut nicks: Vec<&str> = buf.keys().map(String::as_str).collect();
        nicks.sort_unstable();
        nicks
    }
}
```

Register the module in `src/state/mod.rs` (alongside `pub mod buffer;` etc.):

```rust
pub mod typing;
```

Add the field to `pub struct AppState` in `src/state/mod.rs`, after `pending_userhost_requests`:

```rust
    /// Who is typing, per buffer (IRCv3 `+typing`). Ephemeral — never persisted.
    pub typing: typing::TypingTracker,
```

Add one line to `AppState::new()` in `src/state/events.rs` (the `Self { … }` literal starting at line 11):

```rust
            typing: crate::state::typing::TypingTracker::default(),
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `make test`
Expected: PASS — the 7 new tests green, and nothing else breaks (`AppState` has exactly one constructor, so no other call site needs touching).

Run: `make clippy`
Expected: no new warnings from `src/state/typing.rs`.

- [ ] **Step 5: Commit**

```bash
git add src/state/typing.rs src/state/mod.rs src/state/events.rs
git commit -m "feat(state): TypingTracker for IRCv3 +typing"
```

---

### Task 4: Receive `TAGMSG` — `src/irc/events.rs`

Wire the inbound path, with all five drop rules from spec §4.3. Nothing about this task may put a line in a buffer.

**Files:**
- Modify: `src/irc/events.rs` (new match arm + `handle_tagmsg`; clear-typing calls in the PRIVMSG/NOTICE/PART/QUIT/KICK/NICK handlers)
- Test: inline `mod tests` in `src/irc/events.rs` (helpers `make_test_state()` at `:5396` and `make_channel_buffer()` at `:5514` already exist — use them)

**Interfaces:**
- Consumes: `crate::irc::typing::{parse_typing, TypingState}` (Task 1), `AppState.typing` (Task 3).
- Produces: typing state populated in `AppState.typing` from live TAGMSG traffic. No new public API.

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `src/irc/events.rs`:

```rust
fn tagmsg(from: &str, target: &str, tag: &str, value: &str) -> IrcMessage {
    format!("@{tag}={value} :{from}!u@h TAGMSG {target}\r\n")
        .parse()
        .expect("valid TAGMSG")
}

#[test]
fn tagmsg_sets_typing_without_touching_the_buffer() {
    let mut state = make_test_state();
    state.add_buffer(make_channel_buffer("conn1", "#rust"));
    let before = state.buffers["conn1/#rust"].messages.len();

    handle_irc_message(
        &mut state,
        "conn1",
        &tagmsg("alice", "#rust", "+typing", "active"),
    );

    assert_eq!(state.typing.nicks("conn1/#rust"), vec!["alice"]);
    // The message-tags spec forbids showing TAGMSG in history.
    assert_eq!(state.buffers["conn1/#rust"].messages.len(), before);
    assert_eq!(state.buffers["conn1/#rust"].unread_count, 0);
    assert_eq!(
        state.buffers["conn1/#rust"].activity,
        crate::state::buffer::ActivityLevel::None
    );
}

#[test]
fn tagmsg_done_clears_typing() {
    let mut state = make_test_state();
    state.add_buffer(make_channel_buffer("conn1", "#rust"));
    handle_irc_message(&mut state, "conn1", &tagmsg("alice", "#rust", "+typing", "active"));
    handle_irc_message(&mut state, "conn1", &tagmsg("alice", "#rust", "+typing", "done"));
    assert!(state.typing.nicks("conn1/#rust").is_empty());
}

#[test]
fn our_own_tagmsg_echo_is_ignored() {
    // echo-message reflects our own TAGMSG back at us (spec §3.2).
    let mut state = make_test_state();
    state.add_buffer(make_channel_buffer("conn1", "#rust"));
    let our_nick = state.connections["conn1"].nick.clone();
    handle_irc_message(
        &mut state,
        "conn1",
        &tagmsg(&our_nick, "#rust", "+typing", "active"),
    );
    assert!(state.typing.nicks("conn1/#rust").is_empty());
}

#[test]
fn replayed_tagmsg_in_a_batch_is_ignored() {
    // chathistory / event-playback would otherwise show phantom typing (spec §3.3).
    let mut state = make_test_state();
    state.add_buffer(make_channel_buffer("conn1", "#rust"));
    let msg: IrcMessage = "@batch=1;+typing=active :alice!u@h TAGMSG #rust\r\n"
        .parse()
        .expect("valid");
    handle_irc_message(&mut state, "conn1", &msg);
    assert!(state.typing.nicks("conn1/#rust").is_empty());
}

#[test]
fn tagmsg_without_a_typing_tag_is_ignored() {
    let mut state = make_test_state();
    state.add_buffer(make_channel_buffer("conn1", "#rust"));
    handle_irc_message(
        &mut state,
        "conn1",
        &tagmsg("alice", "#rust", "+example-tag", "x"),
    );
    assert!(state.typing.nicks("conn1/#rust").is_empty());
}

#[test]
fn tagmsg_never_creates_a_buffer() {
    // Otherwise a stranger could pop a query window open just by typing.
    let mut state = make_test_state();
    let our_nick = state.connections["conn1"].nick.clone();
    handle_irc_message(
        &mut state,
        "conn1",
        &tagmsg("stranger", &our_nick, "+typing", "active"),
    );
    assert!(!state.buffers.contains_key("conn1/stranger"));
    assert!(state.typing.nicks("conn1/stranger").is_empty());
}

#[test]
fn tagmsg_to_an_existing_query_sets_typing_keyed_by_sender() {
    let mut state = make_test_state();
    let our_nick = state.connections["conn1"].nick.clone();
    let mut buf = make_channel_buffer("conn1", "alice");
    buf.buffer_type = crate::state::buffer::BufferType::Query;
    buf.id = "conn1/alice".to_string();
    buf.name = "alice".to_string();
    state.add_buffer(buf);

    handle_irc_message(
        &mut state,
        "conn1",
        &tagmsg("alice", &our_nick, "+typing", "active"),
    );
    assert_eq!(state.typing.nicks("conn1/alice"), vec!["alice"]);
}

#[test]
fn statusmsg_prefixed_target_resolves_to_the_channel() {
    let mut state = make_test_state();
    state.add_buffer(make_channel_buffer("conn1", "#rust"));
    handle_irc_message(
        &mut state,
        "conn1",
        &tagmsg("alice", "@#rust", "+typing", "active"),
    );
    assert_eq!(state.typing.nicks("conn1/#rust"), vec!["alice"]);
}

#[test]
fn a_message_from_the_sender_clears_their_typing() {
    let mut state = make_test_state();
    state.add_buffer(make_channel_buffer("conn1", "#rust"));
    handle_irc_message(&mut state, "conn1", &tagmsg("alice", "#rust", "+typing", "active"));
    assert_eq!(state.typing.nicks("conn1/#rust"), vec!["alice"]);

    let privmsg: IrcMessage = ":alice!u@h PRIVMSG #rust :done typing\r\n".parse().expect("valid");
    handle_irc_message(&mut state, "conn1", &privmsg);
    assert!(state.typing.nicks("conn1/#rust").is_empty());
}

#[test]
fn quitting_clears_typing_everywhere() {
    let mut state = make_test_state();
    state.add_buffer(make_channel_buffer("conn1", "#rust"));
    handle_irc_message(&mut state, "conn1", &tagmsg("alice", "#rust", "+typing", "active"));

    let quit: IrcMessage = ":alice!u@h QUIT :bye\r\n".parse().expect("valid");
    handle_irc_message(&mut state, "conn1", &quit);
    assert!(state.typing.nicks("conn1/#rust").is_empty());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `make test`
Expected: FAIL — `tagmsg_sets_typing_without_touching_the_buffer` and friends fail on `assertion failed: left == right` (typing map is empty, because nothing handles TAGMSG yet).

- [ ] **Step 3: Write the implementation**

**3a.** Add the dispatch arm in `handle_irc_message` (`src/irc/events.rs`), immediately after the `Command::NOTICE` arm:

```rust
        // IRCv3 `TAGMSG` has no Command variant in the proto crate, so it
        // arrives as Raw. Currently only `+typing` is understood; the
        // message-tags spec forbids ever displaying TAGMSG in history.
        Command::Raw(verb, args) if verb.eq_ignore_ascii_case("TAGMSG") && !args.is_empty() => {
            handle_tagmsg(state, conn_id, &our_nick, msg, &args[0], tags.as_ref());
        }
```

**3b.** Add the handler, next to `handle_privmsg`:

```rust
/// Handle an inbound `TAGMSG`. Only the `+typing` client tag is understood.
///
/// This function must never append a buffer line, bump activity or unread
/// counts, persist anything, or create a buffer. The message-tags spec is
/// explicit: "Clients that receive a `TAGMSG` command MUST NOT display them in
/// the message history by default."
fn handle_tagmsg(
    state: &mut AppState,
    conn_id: &str,
    our_nick: &str,
    msg: &IrcMessage,
    target: &str,
    tags: Option<&HashMap<String, String>>,
) {
    // 1. History replay (`draft/chathistory` / `draft/event-playback`) can hand
    //    us a TAGMSG from hours ago. Typing is a live-only signal (spec §3.3).
    if tags.is_some_and(|t| t.contains_key("batch")) {
        return;
    }

    // 2. No typing tag: nothing else is implemented, so there is nothing to do.
    let Some(typing_state) = tags.and_then(crate::irc::typing::parse_typing) else {
        return;
    };

    let (nick, ident, host) = extract_nick_userhost(msg.prefix.as_ref());
    if nick.is_empty() {
        return;
    }

    // 3. echo-message reflects our own TAGMSG back at us (spec §3.2).
    if nick.eq_ignore_ascii_case(our_nick) {
        return;
    }

    // Strip a STATUSMSG prefix (`@#chan`, `+#chan`) before resolving the buffer.
    let target = target.trim_start_matches(['@', '+', '%', '&', '~']);
    let target_is_channel = is_channel(target);

    // 4. Ignore list.
    let ignore_level = if target_is_channel {
        IgnoreLevel::Public
    } else {
        IgnoreLevel::Msgs
    };
    let channel = target_is_channel.then_some(target);
    if should_ignore(
        &state.ignores,
        &nick,
        Some(&ident),
        Some(&host),
        &ignore_level,
        channel,
    ) {
        return;
    }

    // A channel TAGMSG belongs to the channel buffer; a TAGMSG aimed at us
    // belongs to the sender's query buffer — same rule as PRIVMSG.
    let buffer_name = if target_is_channel { target } else { &nick };
    let buffer_id = make_buffer_id(conn_id, buffer_name);

    // 5. Typing never creates a buffer: otherwise any stranger could pop a
    //    query window open on your screen without ever sending a message.
    if !state.buffers.contains_key(&buffer_id) {
        return;
    }

    if state
        .typing
        .set(&buffer_id, &nick, typing_state, Instant::now())
    {
        push_typing_web_event(state, &buffer_id);
    }
}

/// Enqueue the current typing set for a buffer to the web clients.
/// The full set is sent, not a delta — idempotent and self-healing.
fn push_typing_web_event(state: &mut AppState, buffer_id: &str) {
    let nicks: Vec<String> = state
        .typing
        .nicks(buffer_id)
        .into_iter()
        .map(ToString::to_string)
        .collect();
    state
        .pending_web_events
        .push(crate::web::protocol::WebEvent::Typing {
            buffer_id: buffer_id.to_string(),
            nicks,
        });
}
```

> `Instant::now()` is acceptable *here* — this is the I/O edge where a real clock enters the system. Everything below it (Tasks 1 and 3) takes `now` as a parameter. Add `use std::time::Instant;` to the file's imports.

> `WebEvent::Typing` does not exist yet — it lands in Task 8. **Until then, comment out the body of `push_typing_web_event`** (leave the `let nicks` binding out too, to avoid an unused-variable warning) and restore it in Task 8. The tests in this task do not exercise the web path.

**3c.** Clear typing when the sender speaks or leaves. In `handle_privmsg` and `handle_notice`, right after the buffer id is computed and the ignore check passes, add:

```rust
    // A message from this nick means they are no longer typing (spec §1.2).
    if state.typing.clear(&buffer_id, &nick) {
        push_typing_web_event(state, &buffer_id);
    }
```

In the `PART` and `KICK` handlers, once the channel's `buffer_id` is known:

```rust
    if state.typing.clear(&buffer_id, &nick) {
        push_typing_web_event(state, &buffer_id);
    }
```

In the `QUIT` and `NICK` handlers (which are not scoped to one channel):

```rust
    for buffer_id in state.typing.clear_nick(&nick) {
        push_typing_web_event(state, &buffer_id);
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `make test`
Expected: PASS — all 10 new tests green, and the existing `events.rs` suite still passes.

Run: `make clippy`
Expected: no new warnings from `src/irc/events.rs`.

- [ ] **Step 5: Commit**

```bash
git add src/irc/events.rs
git commit -m "feat(irc): receive +typing via TAGMSG"
```

---

### Task 5: Configuration — `[typing]` section, `/set` keys, statusbar item

**Files:**
- Modify: `src/config/mod.rs` (`TypingConfig`, `AppConfig.typing`, `StatusbarItem::Typing`, new default item order)
- Modify: `src/commands/settings.rs` (get/set arms + completion list)
- Test: inline `mod tests` in `src/config/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct TypingConfig { pub show: bool, pub send_channels: bool, pub send_queries: bool }`, all defaulting to `true`
  - `AppConfig.typing: TypingConfig`
  - `StatusbarItem::Typing`
  - `/set typing.show`, `/set typing.send_channels`, `/set typing.send_queries`

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/config/mod.rs`:

```rust
#[test]
fn typing_defaults_are_on() {
    let config = AppConfig::default();
    assert!(config.typing.show);
    assert!(config.typing.send_channels);
    assert!(config.typing.send_queries);
}

#[test]
fn typing_item_sits_between_channel_and_lag() {
    let config = AppConfig::default();
    let items = &config.statusbar.items;
    let channel = items.iter().position(|i| *i == StatusbarItem::ChannelInfo);
    let typing = items.iter().position(|i| *i == StatusbarItem::Typing);
    let lag = items.iter().position(|i| *i == StatusbarItem::Lag);
    assert!(channel < typing, "typing must come after the buffer name");
    assert!(typing < lag, "typing must come before lag");
}

#[test]
fn typing_config_round_trips_through_toml() {
    let toml = "[typing]\nshow = false\nsend_channels = false\nsend_queries = true\n";
    let config: AppConfig = toml::from_str(toml).expect("parses");
    assert!(!config.typing.show);
    assert!(!config.typing.send_channels);
    assert!(config.typing.send_queries);
}
```

> `StatusbarItem` currently derives `Debug, Clone, PartialEq, Eq, Serialize, Deserialize` — the `position()` comparisons above need `PartialEq`, which it already has.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `make test`
Expected: FAIL — `no field typing on type AppConfig`, `no variant Typing found for enum StatusbarItem`.

- [ ] **Step 3: Write the implementation**

In `src/config/mod.rs`:

```rust
/// IRCv3 `+typing` client tag. Split three ways because the spec asks clients
/// to "provide appropriate privacy controls": you may watch without
/// broadcasting, or broadcast in DMs but not in public channels.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TypingConfig {
    /// Display other people's typing indicators.
    pub show: bool,
    /// Send `+typing` in channels.
    pub send_channels: bool,
    /// Send `+typing` in private queries.
    pub send_queries: bool,
}

impl Default for TypingConfig {
    fn default() -> Self {
        Self {
            show: true,
            send_channels: true,
            send_queries: true,
        }
    }
}
```

Add the field to `pub struct AppConfig` (next to `statusbar`):

```rust
    #[serde(default)]
    pub typing: TypingConfig,
```

Add the enum variant (`src/config/mod.rs:35`):

```rust
pub enum StatusbarItem {
    ActiveWindows,
    NickInfo,
    ChannelInfo,
    Typing,
    Lag,
    Time,
}
```

Change the default item order in `impl Default for StatusbarConfig` (`src/config/mod.rs:233`):

```rust
            items: vec![
                StatusbarItem::Time,
                StatusbarItem::NickInfo,
                StatusbarItem::ChannelInfo,
                StatusbarItem::Typing,
                StatusbarItem::Lag,
                StatusbarItem::ActiveWindows,
            ],
```

In `src/commands/settings.rs`, add an arm to `get_config_value`'s outer `match parts[0]`:

```rust
        "typing" => {
            let val = match parts[1] {
                "show" => config.typing.show.to_string(),
                "send_channels" => config.typing.send_channels.to_string(),
                "send_queries" => config.typing.send_queries.to_string(),
                _ => return None,
            };
            Some(Resolved {
                value: val,
                is_credential: false,
            })
        }
```

Add the mirror arm to `set_config_value` (follow the bool-coercion pattern the neighbouring `statusbar.enabled` arm uses), and add the three keys to the completion list around `src/commands/settings.rs:630`:

```rust
    "typing.show",
    "typing.send_channels",
    "typing.send_queries",
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `make test`
Expected: PASS — 3 new config tests green. The `StatusbarItem::Typing` variant will make `src/ui/status_line.rs` fail to compile with a non-exhaustive match; add a temporary `StatusbarItem::Typing => {}` arm to keep the build green — Task 7 fills it in.

Run: `make clippy`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/config/mod.rs src/commands/settings.rs src/ui/status_line.rs
git commit -m "feat(config): [typing] section and statusbar item"
```

---

### Task 6: Send `+typing` — `src/app/typing.rs`

The outbound state machine, plus its two drivers. This is the task with the most moving parts; re-read spec §4.5 before starting.

**Files:**
- Create: `src/app/typing.rs`
- Modify: `src/app/mod.rs` (`pub mod typing;`, `App.typing: TypingSender` field + init, call the tick hook), `src/app/input.rs:34` (keystroke hook), `src/app/input.rs` (record sent messages)
- Test: inline `mod tests` in `src/app/typing.rs`

**Interfaces:**
- Consumes: `crate::irc::typing::{build_tagmsg, should_type, TypingState, THROTTLE}` (Task 1), `ISupport::client_tag_allowed` (Task 2), `TypingConfig` (Task 5).
- Produces:
  - `pub struct TypingSender` (`Default`) with pure decision methods:
    - `on_input(&mut self, buffer_id: &str, input: &str, now: Instant) -> Option<TypingState>`
    - `on_tick(&mut self, active_buffer_id: Option<&str>, input: &str, now: Instant) -> Vec<(String, TypingState)>` — `(buffer_id, state)` pairs to send
    - `on_submit(&mut self, buffer_id: &str, now: Instant)`
    - `note_message_sent(&mut self, buffer_id: &str, now: Instant)`
  - `impl App { pub(crate) fn typing_tick(&mut self); pub(crate) fn on_input_changed(&mut self); pub(crate) fn expire_typing(&mut self); pub(crate) fn send_typing(&mut self, buffer_id: &str, state: TypingState); }`

- [ ] **Step 1: Write the failing tests**

Create `src/app/typing.rs` with **only** this test module for now. These test the pure decision layer — the `App` glue is exercised by hand and by the existing suite.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn first_keystroke_sends_active() {
        let mut s = TypingSender::default();
        assert_eq!(
            s.on_input("net/#rust", "h", t0()),
            Some(TypingState::Active)
        );
    }

    #[test]
    fn keystrokes_within_three_seconds_are_throttled() {
        // Spec: "MUST be throttled so that any typing notification is not sent
        // within 3 seconds of another one for a given target."
        let mut s = TypingSender::default();
        let now = t0();
        assert_eq!(s.on_input("net/#rust", "h", now), Some(TypingState::Active));
        assert_eq!(s.on_input("net/#rust", "he", now + Duration::from_millis(500)), None);
        assert_eq!(s.on_input("net/#rust", "hel", now + Duration::from_secs(2)), None);
        // At 3s the throttle opens again — but a keystroke, not the tick, is
        // what re-sends here.
        assert_eq!(
            s.on_input("net/#rust", "hell", now + Duration::from_secs(3)),
            Some(TypingState::Active)
        );
    }

    #[test]
    fn slash_commands_never_type() {
        let mut s = TypingSender::default();
        assert_eq!(s.on_input("net/#rust", "/join #x", t0()), None);
    }

    #[test]
    fn actions_do_type() {
        let mut s = TypingSender::default();
        assert_eq!(
            s.on_input("net/#rust", "/me waves", t0()),
            Some(TypingState::Active)
        );
    }

    #[test]
    fn clearing_the_input_sends_done_exactly_once() {
        let mut s = TypingSender::default();
        let now = t0();
        s.on_input("net/#rust", "hello", now);
        assert_eq!(
            s.on_input("net/#rust", "", now + Duration::from_millis(100)),
            Some(TypingState::Done)
        );
        // Still empty: nothing more to say.
        assert_eq!(s.on_input("net/#rust", "", now + Duration::from_millis(200)), None);
    }

    #[test]
    fn done_is_not_throttled_away() {
        // The 3s throttle must not swallow `done` — otherwise clearing the input
        // right after starting to type leaves the peer showing "is typing" for 6s.
        let mut s = TypingSender::default();
        let now = t0();
        assert_eq!(s.on_input("net/#rust", "h", now), Some(TypingState::Active));
        assert_eq!(
            s.on_input("net/#rust", "", now + Duration::from_millis(200)),
            Some(TypingState::Done)
        );
    }

    #[test]
    fn tick_resends_active_while_typing_continues() {
        let mut s = TypingSender::default();
        let now = t0();
        s.on_input("net/#rust", "hello", now);
        // 1s later: still inside the throttle, nothing to send.
        assert!(s.on_tick(Some("net/#rust"), "hello", now + Duration::from_secs(1)).is_empty());
        // Keystroke at 2s keeps it alive; tick at 3s resends.
        s.on_input("net/#rust", "hello ", now + Duration::from_secs(2));
        assert_eq!(
            s.on_tick(Some("net/#rust"), "hello ", now + Duration::from_secs(3)),
            vec![("net/#rust".to_string(), TypingState::Active)]
        );
    }

    #[test]
    fn tick_sends_paused_once_after_three_idle_seconds() {
        let mut s = TypingSender::default();
        let now = t0();
        s.on_input("net/#rust", "hello", now);
        // Text is still there but no keystroke for 3s → paused, exactly once.
        assert_eq!(
            s.on_tick(Some("net/#rust"), "hello", now + Duration::from_secs(3)),
            vec![("net/#rust".to_string(), TypingState::Paused)]
        );
        assert!(s.on_tick(Some("net/#rust"), "hello", now + Duration::from_secs(4)).is_empty());
        assert!(s.on_tick(Some("net/#rust"), "hello", now + Duration::from_secs(30)).is_empty());
    }

    #[test]
    fn typing_again_after_paused_resends_active() {
        let mut s = TypingSender::default();
        let now = t0();
        s.on_input("net/#rust", "hello", now);
        s.on_tick(Some("net/#rust"), "hello", now + Duration::from_secs(3));
        assert_eq!(
            s.on_input("net/#rust", "hello!", now + Duration::from_secs(7)),
            Some(TypingState::Active)
        );
    }

    #[test]
    fn submit_sends_nothing_and_resets() {
        // The PRIVMSG itself clears typing at the receivers, so `done` is noise.
        let mut s = TypingSender::default();
        let now = t0();
        s.on_input("net/#rust", "hello", now);
        s.on_submit("net/#rust", now + Duration::from_secs(1));
        assert_eq!(s.on_input("net/#rust", "", now + Duration::from_secs(1)), None);
        assert!(s.on_tick(Some("net/#rust"), "", now + Duration::from_secs(5)).is_empty());
    }

    #[test]
    fn switching_buffers_sends_done_to_the_old_target() {
        let mut s = TypingSender::default();
        let now = t0();
        s.on_input("net/#rust", "hello", now);
        assert_eq!(
            s.on_tick(Some("net/#tokio"), "", now + Duration::from_secs(1)),
            vec![("net/#rust".to_string(), TypingState::Done)]
        );
    }

    #[test]
    fn a_sent_message_suppresses_typing_for_three_seconds() {
        // Flood budget: a TAGMSG costs as much as a short PRIVMSG (spec §3.1).
        // The message itself already announces our presence.
        let mut s = TypingSender::default();
        let now = t0();
        s.note_message_sent("net/#rust", now);
        assert_eq!(s.on_input("net/#rust", "next", now + Duration::from_secs(1)), None);
        assert_eq!(
            s.on_input("net/#rust", "next!", now + Duration::from_secs(3)),
            Some(TypingState::Active)
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `make test`
Expected: FAIL — `cannot find type TypingSender in this scope`.

- [ ] **Step 3: Write the implementation**

Prepend to `src/app/typing.rs`:

```rust
//! Outbound IRCv3 `+typing`: deciding when to tell the network we are typing.
//!
//! `TypingSender` is the pure decision layer — it takes the current input text
//! and a clock, and returns the notifications to send. It never touches the
//! network, so it is fully testable. `App` supplies the clock, the guards
//! (capability, `CLIENTTAGDENY`, config, buffer type) and the socket.
//!
//! Driven from exactly two places: the keystroke hook in `input.rs`, and the
//! existing 1s tick in `mod.rs`. No new timer.

use std::collections::HashMap;
use std::time::Instant;

use crate::app::App;
use crate::irc::typing::{self, TypingState, THROTTLE};
use crate::state::buffer::BufferType;

/// Decides which typing notifications to emit. One target at a time: you can
/// only type into one buffer at once.
#[derive(Debug, Default)]
pub struct TypingSender {
    /// The buffer we last typed into.
    target: Option<String>,
    /// The last state we actually transmitted for `target`.
    sent: Option<TypingState>,
    /// When we last transmitted — the 3s throttle clock.
    last_sent: Option<Instant>,
    /// When the input last changed — drives the `paused` transition.
    last_keystroke: Option<Instant>,
    /// When we last sent a real message to a buffer. Typing is suppressed for
    /// `THROTTLE` afterwards: the message already announced us, and a TAGMSG
    /// costs as much flood budget as a short PRIVMSG (spec §3.1).
    last_message: HashMap<String, Instant>,
}

impl TypingSender {
    /// The input buffer changed. Returns the notification to send, if any.
    pub fn on_input(
        &mut self,
        buffer_id: &str,
        input: &str,
        now: Instant,
    ) -> Option<TypingState> {
        // Moved to a different buffer without going through a tick: forget the
        // old target rather than mixing their throttle clocks.
        if self.target.as_deref() != Some(buffer_id) {
            self.reset(buffer_id);
        }
        self.last_keystroke = Some(now);

        if !typing::should_type(input) {
            // Input went empty (or became a slash command) while we were
            // typing: retract, exactly once. `done` bypasses the throttle —
            // swallowing it would leave the peer showing "is typing" for 6s.
            return self.sent.is_some().then(|| {
                self.sent = None;
                self.last_sent = Some(now);
                TypingState::Done
            });
        }

        if self.suppressed_by_message(buffer_id, now) || self.throttled(now) {
            return None;
        }
        self.emit(now, TypingState::Active)
    }

    /// The 1s tick. Returns every notification due now.
    pub fn on_tick(
        &mut self,
        active_buffer_id: Option<&str>,
        input: &str,
        now: Instant,
    ) -> Vec<(String, TypingState)> {
        let Some(target) = self.target.clone() else {
            return Vec::new();
        };

        // The user navigated away while we were typing: retract.
        if active_buffer_id != Some(target.as_str()) {
            let pending = self.sent.is_some();
            self.reset_all();
            return if pending {
                vec![(target, TypingState::Done)]
            } else {
                Vec::new()
            };
        }

        if !typing::should_type(input) || self.sent.is_none() {
            return Vec::new();
        }

        if self.suppressed_by_message(&target, now) || self.throttled(now) {
            return Vec::new();
        }

        let idle = self
            .last_keystroke
            .is_none_or(|k| now.duration_since(k) >= THROTTLE);

        let next = if idle {
            // Paused is sent once. Spec: "MAY be sent by clients once when the
            // user has paused typing ... and has not cleared their text."
            if self.sent == Some(TypingState::Paused) {
                return Vec::new();
            }
            TypingState::Paused
        } else {
            TypingState::Active
        };

        self.emit(now, next)
            .map(|state| vec![(target, state)])
            .unwrap_or_default()
    }

    /// A message was submitted. The PRIVMSG clears typing at the receivers, so
    /// we send nothing — we just stop.
    pub fn on_submit(&mut self, buffer_id: &str, now: Instant) {
        self.note_message_sent(buffer_id, now);
        self.reset_all();
    }

    /// Record that a real message went to `buffer_id`.
    pub fn note_message_sent(&mut self, buffer_id: &str, now: Instant) {
        self.last_message.insert(buffer_id.to_string(), now);
    }

    fn emit(&mut self, now: Instant, state: TypingState) -> Option<TypingState> {
        self.sent = Some(state);
        self.last_sent = Some(now);
        Some(state)
    }

    fn throttled(&self, now: Instant) -> bool {
        self.last_sent
            .is_some_and(|t| now.duration_since(t) < THROTTLE)
    }

    fn suppressed_by_message(&self, buffer_id: &str, now: Instant) -> bool {
        self.last_message
            .get(buffer_id)
            .is_some_and(|t| now.duration_since(*t) < THROTTLE)
    }

    fn reset(&mut self, buffer_id: &str) {
        self.target = Some(buffer_id.to_string());
        self.sent = None;
        self.last_sent = None;
        self.last_keystroke = None;
    }

    fn reset_all(&mut self) {
        self.target = None;
        self.sent = None;
        self.last_sent = None;
        self.last_keystroke = None;
    }
}
```

Now the `App` glue, in the same file:

```rust
impl App {
    /// Keystroke hook — the input buffer changed.
    pub(crate) fn on_input_changed(&mut self) {
        let Some(buffer_id) = self.state.active_buffer_id.clone() else {
            return;
        };
        let input = self.input.value.clone();
        if let Some(state) = self.typing.on_input(&buffer_id, &input, Instant::now()) {
            self.send_typing(&buffer_id, state);
        }
    }

    /// 1s tick hook — resend, pause, or retract.
    pub(crate) fn typing_tick(&mut self) {
        let active = self.state.active_buffer_id.clone();
        let input = self.input.value.clone();
        let due = self
            .typing
            .on_tick(active.as_deref(), &input, Instant::now());
        for (buffer_id, state) in due {
            self.send_typing(&buffer_id, state);
        }
    }

    /// Send one typing notification, if every guard allows it.
    ///
    /// `pub(crate)` because `app/web.rs` calls it too (Task 8): the browser is
    /// just another input source feeding the same machine.
    ///
    /// Dropped, never queued: a late "is typing" is worse than none, because the
    /// receiver's 6s window will have moved on.
    pub(crate) fn send_typing(&mut self, buffer_id: &str, state: TypingState) {
        let Some(buf) = self.state.buffers.get(buffer_id) else {
            return;
        };
        // Server, log, shell and DCC buffers have no channel/nick to TAGMSG.
        let allowed_type = match buf.buffer_type {
            BufferType::Channel => self.config.typing.send_channels,
            BufferType::Query => self.config.typing.send_queries,
            _ => false,
        };
        if !allowed_type {
            return;
        }
        let target = buf.name.clone();
        let conn_id = buf.connection_id.clone();

        let Some(conn) = self.state.connections.get(&conn_id) else {
            return;
        };
        if !conn.enabled_caps.contains("message-tags") {
            return;
        }
        if !conn.isupport_parsed.client_tag_allowed("typing") {
            return;
        }

        if let Some(handle) = self.irc_handles.get(&conn_id)
            && handle
                .sender
                .send(typing::build_tagmsg(&target, state))
                .is_err()
        {
            tracing::debug!("failed to send +typing={} to {target}", state.as_str());
        }
    }
}
```

> `Connection` has two ISUPPORT fields (`src/state/connection.rs:29-30`): `isupport: HashMap<String, String>` (raw tokens) and `isupport_parsed: Isupport` (the structured parser). Task 2's accessor lives on the latter.

Wire it up:

**`src/app/mod.rs`** — register the module next to the other `app/` submodules:

```rust
pub mod typing;
```

Add the field to `pub struct App` (near `state`):

```rust
    /// Outbound IRCv3 `+typing` state machine.
    pub typing: crate::app::typing::TypingSender,
```

and initialise it where `App` is constructed (`src/app/mod.rs`, near `let mut state = AppState::new();`):

```rust
            typing: crate::app::typing::TypingSender::default(),
```

Call the tick hook inside the existing 1s tick arm (`src/app/mod.rs:1445`), next to `self.handle_netsplit_tick();`:

```rust
                    self.typing_tick();
                    self.expire_typing();
```

Add `expire_typing` to `src/app/typing.rs` — it drives Task 3's tracker and pushes to the web:

```rust
impl App {
    /// Expire received typing state (6s active, 30s paused).
    pub(crate) fn expire_typing(&mut self) {
        for buffer_id in self.state.typing.expire(Instant::now()) {
            let nicks: Vec<String> = self
                .state
                .typing
                .nicks(&buffer_id)
                .into_iter()
                .map(ToString::to_string)
                .collect();
            self.state
                .pending_web_events
                .push(crate::web::protocol::WebEvent::Typing { buffer_id, nicks });
        }
    }
}
```

> As in Task 4: `WebEvent::Typing` arrives in Task 8. Until then, leave the loop body as `let _ = buffer_id;` and restore it in Task 8.

**`src/app/input.rs:34`** — the keystroke hook. Snapshotting around `handle_key` catches every path that mutates the input (chars, backspace, Ctrl-U/K/W, paste, history recall, spell accept) without touching a dozen match arms:

```rust
            Event::Key(key) => {
                let before = self.input.value.clone();
                self.handle_key(key);
                if self.input.value != before {
                    self.on_input_changed();
                }
            }
```

**`src/app/input.rs`** — in the `Enter` arm (`:293-299`), tell the sender a message went out. Do this **before** `handle_submit` so the snapshot comparison above sees an already-reset machine and does not emit a spurious `done`:

```rust
            (_, KeyCode::Enter | KeyCode::Char('\n' | '\r')) => {
                self.input.spell_state = None;
                let text = self.input.submit();
                if !text.is_empty() {
                    if let Some(buffer_id) = self.state.active_buffer_id.clone() {
                        self.typing.on_submit(&buffer_id, std::time::Instant::now());
                    }
                    self.handle_submit(&text);
                }
            }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `make test`
Expected: PASS — all 12 `app::typing::tests` green, existing suite unaffected.

Run: `make clippy`
Expected: no new warnings from `src/app/typing.rs`, `src/app/input.rs`, `src/app/mod.rs`.

- [ ] **Step 5: Commit**

```bash
git add src/app/typing.rs src/app/mod.rs src/app/input.rs
git commit -m "feat(app): send +typing with throttle, guards and expiry"
```

---

### Task 7: TUI status line — render + separator fix

**Files:**
- Modify: `src/ui/status_line.rs` (implement the `StatusbarItem::Typing` arm stubbed out in Task 5; fix the separator loop)
- Test: inline `mod tests` in `src/ui/status_line.rs`

**Interfaces:**
- Consumes: `AppState.typing` (Task 3), `TypingConfig.show` (Task 5), `StatusbarItem::Typing` (Task 5).
- Produces: `pub fn typing_phrase(nicks: &[&str]) -> Option<String>` — pulled out as a free function precisely so it can be tested without a `Frame`.

- [ ] **Step 1: Write the failing tests**

Add a `mod tests` to `src/ui/status_line.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_typers_renders_nothing() {
        assert_eq!(typing_phrase(&[]), None);
    }

    #[test]
    fn phrases_scale_with_the_number_of_typers() {
        assert_eq!(typing_phrase(&["alice"]).unwrap(), "alice is typing…");
        assert_eq!(
            typing_phrase(&["alice", "bob"]).unwrap(),
            "alice and bob are typing…"
        );
        assert_eq!(
            typing_phrase(&["alice", "bob", "carol"]).unwrap(),
            "alice, bob and carol are typing…"
        );
        assert_eq!(
            typing_phrase(&["alice", "bob", "carol", "dave"]).unwrap(),
            "alice, bob and 2 others are typing…"
        );
        assert_eq!(
            typing_phrase(&["a", "b", "c", "d", "e"]).unwrap(),
            "a, b and 3 others are typing…"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `make test`
Expected: FAIL — `cannot find function typing_phrase in this scope`.

- [ ] **Step 3: Write the implementation**

Add the free function to `src/ui/status_line.rs`:

```rust
/// How the status line words a set of typing nicks. `None` when nobody is
/// typing — the caller then renders no item *and no separator*.
#[must_use]
pub fn typing_phrase(nicks: &[&str]) -> Option<String> {
    match nicks {
        [] => None,
        [one] => Some(format!("{one} is typing…")),
        [a, b] => Some(format!("{a} and {b} are typing…")),
        [a, b, c] => Some(format!("{a}, {b} and {c} are typing…")),
        [a, b, rest @ ..] => Some(format!(
            "{a}, {b} and {} others are typing…",
            rest.len()
        )),
    }
}
```

**Fix the separator loop.** The current loop (`src/ui/status_line.rs:38-44`) pushes the separator *before* the item renders, so an item that emits nothing leaves a dangling `… | | Lag:`. Restructure so the separator is pushed only once an item has actually produced spans. Replace the loop scaffolding:

```rust
    for item in &app.config.statusbar.items {
        let start = spans.len();
        match item {
            // … existing arms unchanged, but WITHOUT the separator push …
            StatusbarItem::Typing => {
                if !app.config.typing.show {
                    continue;
                }
                if let Some(buf) = active_buf {
                    let nicks = app.state.typing.nicks(&buf.id);
                    if let Some(phrase) = typing_phrase(&nicks) {
                        spans.push(Span::styled(phrase, Style::default().fg(fg_muted)));
                    }
                }
            }
        }
        // Only now, having seen whether the item produced anything, decide
        // whether it needs a separator in front of it.
        if spans.len() > start && start > 1 {
            spans.insert(
                start,
                Span::styled(separator.as_str(), Style::default().fg(fg_dim)),
            );
        }
    }
```

> `start > 1` rather than `start > 0` because `spans[0]` is the opening `[` bracket pushed before the loop.
>
> The `ChannelInfo` arm contains a `continue` (the shell-buffer early exit at `:76`). With the separator now inserted *after* the match, that `continue` would skip the insert — so the shell arm must `break`-free: change it to fall through by wrapping its body in an `if`/`else` rather than `continue`. Check the arm as you edit it.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `make test`
Expected: PASS — 2 new tests green.

Run: `make release` then start repartee, connect to a network with `message-tags`, and have a second client type in a shared channel. Expected: `alice is typing…` appears in the status bar between the channel name and `Lag:`, and disappears within ~6 seconds of them stopping. With nobody typing, the bar shows no double separator.

Run: `make clippy`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/ui/status_line.rs
git commit -m "feat(ui): typing indicator in the status line"
```

---

### Task 8: Web protocol — `WebEvent::Typing` and `WebCommand::Typing`

**Files:**
- Modify: `src/web/protocol.rs` (both enums)
- Modify: `src/app/web.rs` (`handle_web_command` arm, `:421`)
- Modify: `src/irc/events.rs` and `src/app/typing.rs` — restore the `push_typing_web_event` / `expire_typing` bodies stubbed out in Tasks 4 and 6
- Test: inline `mod tests` in `src/web/protocol.rs`

**Interfaces:**
- Consumes: `TypingSender::on_input` (Task 6).
- Produces:
  - `WebEvent::Typing { buffer_id: String, nicks: Vec<String> }` — server → browser, **full set**, not a delta
  - `WebCommand::Typing { buffer_id: String, typing: bool }` — browser → server, a *predicate only*

- [ ] **Step 1: Write the failing tests**

Add to `src/web/protocol.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_event_serializes_with_a_type_tag() {
        let ev = WebEvent::Typing {
            buffer_id: "net/#rust".to_string(),
            nicks: vec!["alice".to_string(), "bob".to_string()],
        };
        let json = serde_json::to_string(&ev).expect("serializes");
        assert!(json.contains(r#""type":"Typing""#));
        assert!(json.contains(r#""nicks":["alice","bob"]"#));
    }

    #[test]
    fn typing_command_round_trips() {
        let json = r#"{"type":"Typing","buffer_id":"net/#rust","typing":true}"#;
        let cmd: WebCommand = serde_json::from_str(json).expect("parses");
        match cmd {
            WebCommand::Typing { buffer_id, typing } => {
                assert_eq!(buffer_id, "net/#rust");
                assert!(typing);
            }
            _ => panic!("wrong variant"),
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `make test`
Expected: FAIL — `no variant Typing found for enum WebEvent`.

- [ ] **Step 3: Write the implementation**

Add to `pub enum WebEvent` in `src/web/protocol.rs`:

```rust
    /// Who is currently typing in a buffer (IRCv3 `+typing`).
    ///
    /// Carries the **complete** set, not a delta: deltas drift when a client
    /// reconnects mid-stream, a full set is idempotent and self-healing. Not
    /// included in `SyncInit` — typing is ephemeral, and a client that connects
    /// mid-typing learns about it on the sender's next 3s resend.
    Typing {
        buffer_id: String,
        nicks: Vec<String>,
    },
```

Add to `pub enum WebCommand`:

```rust
    /// The browser's input field changed.
    ///
    /// `typing` is a **predicate**, not a state: "my input holds non-empty,
    /// non-slash text". The browser runs no state machine and knows nothing
    /// about capabilities, `CLIENTTAGDENY`, throttling or config — the core
    /// owns all of that, so the TUI and the browser can never emit two
    /// competing TAGMSGs.
    Typing { buffer_id: String, typing: bool },
```

Add the arm to `handle_web_command` in `src/app/web.rs` (`:421`):

```rust
            WebCommand::Typing { buffer_id, typing } => {
                // Feed the browser's keystroke into the same state machine the
                // TUI uses. `typing: false` reads as "input cleared".
                let input = if typing { "x" } else { "" };
                if let Some(state) =
                    self.typing
                        .on_input(&buffer_id, input, std::time::Instant::now())
                {
                    self.send_typing(&buffer_id, state);
                }
            }
```

> The `"x"` sentinel is deliberate: `TypingSender` only asks `should_type(input)`, and the browser has already applied that predicate locally. Shipping the real text would mean putting every keystroke on the websocket for no gain. (`send_typing` is already `pub(crate)` from Task 6.)

Restore the two stubs:
- `src/irc/events.rs` — uncomment the body of `push_typing_web_event`.
- `src/app/typing.rs` — restore the `expire_typing` loop body.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `make test`
Expected: PASS — 2 new protocol tests green; the Task 4 and Task 6 suites still pass with the web path live.

Run: `make clippy`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/web/protocol.rs src/app/web.rs src/irc/events.rs src/app/typing.rs
git commit -m "feat(web): Typing event and command in the web protocol"
```

---

### Task 9: Web UI — display and send

**Files:**
- Modify: `web-ui/src/protocol.rs` (mirror both variants)
- Modify: `web-ui/src/state.rs` (`typing` signal; `WebEvent::Typing` arm)
- Modify: `web-ui/src/components/status_line.rs` (span between channel and `Lag:`)
- Modify: `web-ui/src/components/input.rs` (debounced `WebCommand::Typing`)
- Test: inline `mod tests` in `web-ui/src/components/status_line.rs`

**Interfaces:**
- Consumes: `WebEvent::Typing`, `WebCommand::Typing` (Task 8).
- Produces: `AppState.typing: RwSignal<HashMap<String, Vec<String>>>` (buffer id → nicks) in the web client.

- [ ] **Step 1: Write the failing test**

The wording must not drift from the TUI. Add to `web-ui/src/components/status_line.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_phrases_match_the_tui() {
        assert_eq!(typing_phrase(&[]), None);
        assert_eq!(
            typing_phrase(&["alice".to_string()]).unwrap(),
            "alice is typing…"
        );
        assert_eq!(
            typing_phrase(&["alice".to_string(), "bob".to_string()]).unwrap(),
            "alice and bob are typing…"
        );
        assert_eq!(
            typing_phrase(&[
                "alice".to_string(),
                "bob".to_string(),
                "carol".to_string(),
                "dave".to_string()
            ])
            .unwrap(),
            "alice, bob and 2 others are typing…"
        );
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `make test`
Expected: FAIL — `cannot find function typing_phrase` in `repartee-web`.

- [ ] **Step 3: Write the implementation**

**`web-ui/src/protocol.rs`** — mirror both variants exactly as in Task 8 (same field names, same `#[serde(tag = "type")]`), in `WebEvent` and `WebCommand`.

**`web-ui/src/state.rs`** — add the signal to `pub struct AppState`:

```rust
    /// buffer_id -> nicks currently typing (IRCv3 `+typing`).
    pub typing: RwSignal<HashMap<String, Vec<String>>>,
```

initialise it in the constructor (`RwSignal::new(HashMap::new())`), and add the arm to `handle_event`:

```rust
            WebEvent::Typing { buffer_id, nicks } => {
                self.typing.update(|t| {
                    if nicks.is_empty() {
                        t.remove(&buffer_id);
                    } else {
                        t.insert(buffer_id, nicks);
                    }
                });
            }
```

**`web-ui/src/components/status_line.rs`** — add the phrasing helper (mirroring the TUI's, since the web status line hardcodes its own layout):

```rust
/// Mirrors `src/ui/status_line.rs::typing_phrase`. Kept in step by test.
#[must_use]
pub fn typing_phrase(nicks: &[String]) -> Option<String> {
    match nicks {
        [] => None,
        [one] => Some(format!("{one} is typing…")),
        [a, b] => Some(format!("{a} and {b} are typing…")),
        [a, b, c] => Some(format!("{a}, {b} and {c} are typing…")),
        [a, b, rest @ ..] => Some(format!("{a}, {b} and {} others are typing…", rest.len())),
    }
}
```

and render it in the `view!` block, **between the channel span and the Lag span**:

```rust
            // Typing — same slot as the TUI: after the buffer name, before lag.
            {move || {
                let buf = active_buf()?;
                let nicks = state.typing.get().get(&buf.id).cloned()?;
                let phrase = typing_phrase(&nicks)?;
                Some(view! {
                    <span class="sep">"|"</span>
                    <span class="muted">{phrase}</span>
                })
            }}
```

**`web-ui/src/components/input.rs`** — the `value` signal already exists (`:167`). Add an `Effect` that reports the predicate, debounced to at most one message per second:

```rust
    // Report typing to the core, which owns the state machine, the throttle and
    // every guard. We send a predicate, never the text.
    let last_report = StoredValue::new(0.0_f64);
    let last_sent_state = StoredValue::new(false);
    Effect::new(move |_| {
        let text = value.get();
        let Some(buffer_id) = state.active_buffer.get() else {
            return;
        };
        let typing = !text.is_empty() && (!text.starts_with('/') || text.starts_with("/me "));
        let now = js_sys::Date::now();
        // Debounce: the core still enforces the 3s IRC throttle, this only keeps
        // the websocket quiet. A change of state always reports immediately.
        if typing == last_sent_state.get_value() && now - last_report.get_value() < 1000.0 {
            return;
        }
        last_report.set_value(now);
        last_sent_state.set_value(typing);
        crate::ws::send_command(&WebCommand::Typing { buffer_id, typing });
    });
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `make test`
Expected: PASS — the web phrasing test green.

Run: `make wasm`
Expected: builds clean.

Then: `make release`, start repartee with the web server on, open the browser UI, and type in a channel from a second IRC client. Expected: the typing phrase appears in the browser status line in the same slot as the TUI. Type in the browser's input: the second client sees you as typing.

Run: `make clippy`
Expected: clean (this includes `clippy-web`).

- [ ] **Step 5: Commit**

```bash
git add web-ui/src/protocol.rs web-ui/src/state.rs web-ui/src/components/status_line.rs web-ui/src/components/input.rs
git commit -m "feat(web-ui): typing indicator and typing reporting"
```

---

### Task 10: Scripting event and documentation

**Files:**
- Modify: `src/scripting/api.rs` (event name constant)
- Modify: `src/app/scripting.rs` (emit — **this** is where script events are dispatched, not `events.rs`)
- Modify: `src/irc/events.rs` (honour script suppression in `handle_tagmsg`)
- Modify: `docs/rfc_ircv3_coverage.md` (coverage table)
- Modify: `README.md` (changelog entry)
- Modify: `docs/src/` + rebuild `docs/*.html` **only if** the docs site documents `/set` keys — check `docs/configuration.html` first and follow whatever build `docs/build.ts` defines.

**Interfaces:**
- Consumes: `crate::irc::typing::parse_typing` (Task 1), `handle_tagmsg` (Task 4).
- Produces: `events::TYPING = "irc.typing"` with params `connection_id`, `nick`, `target`, `state`.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/irc/events.rs`:

```rust
#[test]
fn typing_is_exposed_to_scripts() {
    assert_eq!(crate::scripting::api::events::TYPING, "irc.typing");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `make test`
Expected: FAIL — `cannot find value TYPING in module events`.

- [ ] **Step 3: Write the implementation**

In `src/scripting/api.rs`, alongside the other event constants (`PRIVMSG` at `:16`, etc.):

```rust
    /// IRCv3 `+typing`: someone started, paused, or stopped typing.
    /// Params: `connection_id`, `nick`, `target`, `state` (`active`/`paused`/`done`).
    pub const TYPING: &str = "irc.typing";
```

Emit it from `src/app/scripting.rs` — script events are dispatched from the
`match &msg.command` there (`:686-810`), **not** from `events.rs`. Today every
`Command::Raw` (TAGMSG included) falls into the `_ => return false` arm at `:810` and emits
nothing. Add an arm before it:

```rust
            // IRCv3 `+typing` arrives as Raw (no TAGMSG variant in the proto crate).
            // A TAGMSG with no typing tag carries nothing a script can act on.
            ::irc::proto::Command::Raw(verb, args)
                if verb.eq_ignore_ascii_case("TAGMSG") && !args.is_empty() =>
            {
                let tags: HashMap<String, String> = msg
                    .tags
                    .as_ref()
                    .map(|ts| {
                        ts.iter()
                            .filter_map(|t| Some((t.0.clone(), t.1.clone()?)))
                            .collect()
                    })
                    .unwrap_or_default();
                let Some(typing_state) = crate::irc::typing::parse_typing(&tags) else {
                    return false;
                };
                params.insert("nick".to_string(), extract_nick(msg.prefix.as_ref()));
                params.insert("target".to_string(), args[0].clone());
                params.insert("state".to_string(), typing_state.as_str().to_string());
                events::TYPING
            }
```

(`connection_id` is already inserted into `params` at `:684`, before the match.)

**Honour `EventResult::Suppress`.** The dispatcher's return value flows into
`self.state.suppress_event_display`, which `src/app/irc.rs:930-936` sets around the
`handle_irc_message` call. Since a TAGMSG carries nothing *but* the typing tag, that flag is
exactly the right signal: a script that suppresses `irc.typing` should stop the indicator
from appearing at all. Add one guard at the top of `handle_tagmsg` in `src/irc/events.rs`,
right after the batch check:

```rust
    // A script suppressed this event — do not record the typing state.
    if state.suppress_event_display {
        return;
    }
```

Update `docs/rfc_ircv3_coverage.md`. In the Tier 2 table, replace the `message-tags` row and add one below it:

```markdown
| `message-tags` | Done | 3.2 | Tags extracted from inbound messages, stored on buffer `Message` and in the DB. Outbound tags via `Message { tags: … }`. `TAGMSG` in/out via `Command::Raw`. `CLIENTTAGDENY` honoured from ISUPPORT |
| `+typing` client tag | Done | — | Ratified client tag on `TAGMSG`. Send + receive, 3s throttle, 6s/30s expiry, status-line indicator in TUI and web UI, `[typing]` config with separate channel/query send switches. Legacy `+draft/typing` accepted on receive |
```

Also remove `reply` from the Tier 3 "Not Started" table only if you implemented it — **you did not**; leave that row alone.

Add a changelog entry to `README.md` matching the style of the existing `### vX.Y.Z` entries (user-facing bullets, no version bump — that happens at release time).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `make test`
Expected: PASS.

Run: `make clippy`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/scripting/api.rs src/irc/events.rs docs/rfc_ircv3_coverage.md README.md
git commit -m "feat(scripting): irc.typing event; docs: +typing coverage"
```

---

## Final verification

- [ ] `make clippy` — 0 new warnings across every file touched
- [ ] `make test` — full suite green
- [ ] `make wasm` — web bundle builds; commit `static/web/` if it changed
- [ ] `make release` — release profile builds
- [ ] **Live check against Ergo** (permissive `CLIENTTAGDENY`): two clients in a channel; typing appears within ~3s and clears within ~6s of stopping; sending a message clears it immediately; `/join #x` in the input produces no TAGMSG (watch with `/quote`-level tracing or a raw log)
- [ ] **Live check against Libera** (allowlisted client tags): typing still works; if the server denies the tag, the indicator silently disappears rather than erroring
- [ ] **Privacy check**: `/set typing.send_channels off` — typing in a channel emits nothing while a query still does
- [ ] Open the PR against `main` so the review bot runs

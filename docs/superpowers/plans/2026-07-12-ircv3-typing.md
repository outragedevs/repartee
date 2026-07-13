# IRCv3 `+typing` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Repartee shows who is typing in a channel or query, and tells others when we are typing, via the IRCv3 `+typing` client tag on `TAGMSG`.

**Architecture:** A pure protocol module (`src/irc/typing.rs`) parses and builds the tag. An ephemeral `TypingTracker` on `AppState` holds who is typing where. Outbound, `src/app/typing.rs` models **sources** (the terminal, and each web session — they have independent inputs and independent active buffers) and **targets** (buffers); a target's state is the aggregate of the sources pointing at it. The machine only ever *proposes* notifications; `App` applies the guards (capability, `CLIENTTAGDENY`, config, buffer type, flood budget) and confirms what actually went on the wire.

**Tech Stack:** Rust 2024, `irc-repartee` 1.5.1 (fork of `irc`), ratatui (TUI), Leptos/WASM (web UI), tokio.

**Spec:** `docs/superpowers/specs/2026-07-12-ircv3-typing-design.md` — read it before starting. Section references below (§3.1, §4.5, …) point into it.

## Global Constraints

- **No changes to the `irc-repartee` / `irc-proto-repartee` crates.** Everything works with the published 1.5.1 / 1.2.2. If you find yourself wanting to edit the fork, stop and re-read §2.2 and §3.1 of the spec.
- **`message-tags` is already negotiated** (`src/irc/cap.rs:20`). Do **not** add a capability. There is no `draft/typing` cap.
- **Builds go through `make`**, never raw cargo/trunk: `make test`, `make clippy`, `make wasm`, `make release`.
- **Clippy is a 0-warning gate**: pedantic=warn, nursery=warn, perf=deny, redundant_clone=deny. `make clippy` must be clean for every file you touch. (~44 pre-existing warnings live in unrelated files from a toolchain bump — judge per-file, not by the total.)
- **`src/state/` is UI-agnostic** — no ratatui imports there, ever. It also has no access to `AppConfig`; config values reach it through the existing sync pattern (`AppState.scrollback_limit`, `AppState.nick_color_sat`).
- **Logging is `tracing`**, never `println!`. Errors are `color-eyre`.
- **Never call `Instant::now()` inside protocol or state logic.** Every time-dependent function takes `now: Instant`. This is what makes the whole feature testable without sleeping, and it is not negotiable.
- The crate is aliased: use `irc::proto::…` (it resolves to `irc-repartee`), matching `src/irc/multiline.rs`.
- Work on branch `feat/ircv3-typing-design`. Commit after every task.

## Three assumptions that are false, and cost you if you forget

These killed the first draft of this plan. They are load-bearing:

1. **There is no single "current input".** `handle_web_command` receives a `session_id` (`src/app/web.rs:413-416`) and `web_active_buffers` maps session → buffer, independent of `state.active_buffer_id`. Two browser tabs and a terminal are three sources, possibly in three different buffers.
2. **`Sender::send` does not drop under pressure — it queues.** It feeds an `UnboundedSender` (`irc-repartee-1.5.1/src/client/mod.rs:940-947`), and `Outgoing` *buffers and delays* when the penalty exceeds the threshold (`client/mod.rs:1166-1180`). "Drop, never queue" must therefore be enforced **before** the frame is handed over.
3. **`is_channel` accepts `#`, `&`, `+`, and `!`** (`src/irc/formatting.rs:157-161`). `&chan` and `+chan` are real channels. Trimming a set of status characters off a target would silently turn them into queries.

---

### Task 1: Protocol layer — `src/irc/typing.rs`

Pure tag parsing, TAGMSG construction, and STATUSMSG-aware target resolution. No state, no I/O, no `Instant::now()`.

**Files:**
- Create: `src/irc/typing.rs`
- Modify: `src/irc/mod.rs` (register the module)
- Test: inline `#[cfg(test)] mod tests` in `src/irc/typing.rs` (repo convention — see `src/irc/isupport.rs`)

**Interfaces:**
- Consumes: `crate::irc::formatting::is_channel`.
- Produces:
  - `pub const TAG: &str` = `"+typing"`, `pub const TAG_LEGACY: &str` = `"+draft/typing"`
  - `pub const THROTTLE: Duration` (3s), `pub const ACTIVE_TTL: Duration` (6s), `pub const PAUSED_TTL: Duration` (30s)
  - `pub enum TypingState { Active, Paused, Done }` — `Copy`, with `as_str`, `parse`, `ttl`
  - `pub fn parse_typing(tags: &HashMap<String, String>) -> Option<TypingState>`
  - `pub fn build_tagmsg(target: &str, state: TypingState) -> irc::proto::Message`
  - `pub fn should_type(input: &str) -> bool`
  - `pub fn strip_statusmsg<'a>(target: &'a str, statusmsg: &str) -> &'a str`

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
        // Guards the assumption the whole feature rests on: the crate's Display
        // serializes tags, and its FromStr reads them back.
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
        assert!(should_type("/me waves")); // an action is a message
        assert!(!should_type(""));
        assert!(!should_type("/join #rust"));
        assert!(!should_type("/me")); // bare command, no text
    }

    #[test]
    fn strip_statusmsg_removes_one_advertised_prefix() {
        assert_eq!(strip_statusmsg("@#rust", "@+"), "#rust");
        assert_eq!(strip_statusmsg("+#rust", "@+"), "#rust");
    }

    #[test]
    fn strip_statusmsg_leaves_real_channel_prefixes_alone() {
        // `&` and `+` are CHANNEL prefixes (is_channel accepts # & + !).
        // Stripping them would turn a channel into a query.
        assert_eq!(strip_statusmsg("&local", "@+"), "&local");
        assert_eq!(strip_statusmsg("+modeless", "@+"), "+modeless");
        assert_eq!(strip_statusmsg("#rust", "@+"), "#rust");
    }

    #[test]
    fn strip_statusmsg_strips_at_most_one_character() {
        // `@@#rust` is not a thing; strip one, and only if what's left is a channel.
        assert_eq!(strip_statusmsg("@@#rust", "@+"), "@#rust");
        // Nothing is stripped when the server advertises no STATUSMSG.
        assert_eq!(strip_statusmsg("@#rust", ""), "@#rust");
        // A nick is left alone.
        assert_eq!(strip_statusmsg("alice", "@+"), "alice");
    }
}
```

Register the module — add to `src/irc/mod.rs` alongside the other `pub mod` lines:

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
//! protocol layer: no state, no I/O, no clock.

use std::collections::HashMap;
use std::time::Duration;

use crate::irc::formatting::is_channel;

/// The ratified tag name — the only one we ever send.
pub const TAG: &str = "+typing";

/// The pre-ratification name. Accepted on receive for interop with older
/// clients; never sent, because emitting both would double our TAGMSG volume
/// and therefore our flood cost.
pub const TAG_LEGACY: &str = "+draft/typing";

/// Spec: "Input event handlers MUST be throttled so that any `typing`
/// notification is not sent within 3 seconds of another one for a given target."
/// This binds `done` too, not just `active` — see §4.5.
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

/// Resolve a `TAGMSG` target that may carry a `STATUSMSG` prefix (`@#chan`).
///
/// Deliberately conservative, because `is_channel` accepts `#`, `&`, `+` **and**
/// `!`: `&local` and `+modeless` are *channels*, not status-prefixed targets.
/// So: strip at most one leading character, only if the server advertised it in
/// `STATUSMSG`, and only if what remains is still a channel. Anything else is
/// returned untouched.
#[must_use]
pub fn strip_statusmsg<'a>(target: &'a str, statusmsg: &str) -> &'a str {
    if statusmsg.is_empty() {
        return target;
    }
    let mut chars = target.chars();
    let Some(first) = chars.next() else {
        return target;
    };
    if !statusmsg.contains(first) {
        return target;
    }
    let rest = chars.as_str();
    if is_channel(rest) { rest } else { target }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `make test`
Expected: PASS — all 12 tests in `irc::typing::tests` green.

Run: `make clippy`
Expected: no new warnings from `src/irc/typing.rs`.

- [ ] **Step 5: Commit**

```bash
git add src/irc/typing.rs src/irc/mod.rs
git commit -m "feat(irc): IRCv3 +typing tag protocol layer"
```

---

### Task 2: ISUPPORT accessors — `CLIENTTAGDENY` and `STATUSMSG`

`Isupport` already captures every 005 token in a generic `HashMap` (`src/irc/isupport.rs:18`); it just needs to answer two questions.

**Files:**
- Modify: `src/irc/isupport.rs`
- Test: inline `#[cfg(test)] mod tests` in the same file

**Interfaces:**
- Consumes: nothing.
- Produces, on `Isupport` (note the spelling — `Connection` holds it as `isupport_parsed` at `src/state/connection.rs:30`; `Connection.isupport` is the *raw* token map):
  - `pub fn client_tag_allowed(&self, tag: &str) -> bool` — `tag` **without** the `+` prefix, e.g. `"typing"`, matching the token's own format
  - `pub fn statusmsg(&self) -> &str` — the advertised `STATUSMSG` prefix characters, `""` when absent

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `src/irc/isupport.rs`:

```rust
#[test]
fn client_tag_allowed_by_default() {
    // Absent token = everything allowed ("An empty or missing CLIENTTAGDENY
    // matches the default case").
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

#[test]
fn statusmsg_defaults_to_empty() {
    let isupport = Isupport::new();
    assert_eq!(isupport.statusmsg(), "");
}

#[test]
fn statusmsg_returns_the_advertised_prefixes() {
    let mut isupport = Isupport::new();
    isupport.parse_tokens(&["STATUSMSG=@+"]);
    assert_eq!(isupport.statusmsg(), "@+");
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
            None if entry == "*" || entry == tag => allowed = false,
            None => {}
        }
    }
    allowed
}

/// The `STATUSMSG` prefix characters the server advertises (e.g. `@+`).
/// Empty when the server advertises none.
#[must_use]
pub fn statusmsg(&self) -> &str {
    self.tokens.get("STATUSMSG").map_or("", String::as_str)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `make test`
Expected: PASS — the 7 new tests green.

Run: `make clippy`
Expected: no new warnings from `src/irc/isupport.rs`.

- [ ] **Step 5: Commit**

```bash
git add src/irc/isupport.rs
git commit -m "feat(irc): CLIENTTAGDENY and STATUSMSG accessors"
```

---

### Task 3: Typing state tracker — `src/state/typing.rs`

Who is typing where. Ephemeral: never persisted, never in the session snapshot. Kept out of `Buffer` because `Buffer` has no constructor and 39 struct literals (spec §4.3).

**Files:**
- Create: `src/state/typing.rs`
- Modify: `src/state/mod.rs` (register module; add `pub typing: TypingTracker` and `pub typing_show: bool` to `AppState`), `src/state/events.rs` (two lines in `AppState::new()`; one call in `AppState::remove_buffer`, `:84`)
- Test: inline `#[cfg(test)] mod tests` in `src/state/typing.rs`

**Interfaces:**
- Consumes: `crate::irc::typing::TypingState` (Task 1).
- Produces:
  - `pub struct TypingEntry { pub state: TypingState, pub since: Instant }` (`Copy`)
  - `pub struct TypingTracker` (`Default`) with:
    - `set(&mut self, buffer_id: &str, nick: &str, state: TypingState, now: Instant) -> bool` — `true` when the *visible set* changed
    - `clear(&mut self, buffer_id: &str, nick: &str) -> bool`
    - `clear_nick_on_connection(&mut self, conn_id: &str, nick: &str) -> Vec<String>` — **connection-scoped**
    - `remove_buffer(&mut self, buffer_id: &str) -> bool`
    - `clear_all(&mut self) -> Vec<String>`
    - `expire(&mut self, now: Instant) -> Vec<String>`
    - `nicks(&self, buffer_id: &str) -> Vec<&str>` — sorted, stable render order
  - `AppState.typing: TypingTracker`, `AppState.typing_show: bool`

- [ ] **Step 1: Write the failing tests**

Create `src/state/typing.rs` with **only** this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // A fixed origin: every test is deterministic, no wall clock anywhere.
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

        assert!(tr.expire(now + Duration::from_secs(5)).is_empty());
        assert_eq!(tr.nicks("net/#rust"), vec!["alice", "bob"]);

        assert_eq!(tr.expire(now + Duration::from_secs(6)), vec!["net/#rust"]);
        assert_eq!(tr.nicks("net/#rust"), vec!["bob"]);

        assert_eq!(tr.expire(now + Duration::from_secs(30)), vec!["net/#rust"]);
        assert!(tr.nicks("net/#rust").is_empty());
    }

    #[test]
    fn clear_removes_one_nick_in_one_buffer() {
        let mut tr = TypingTracker::default();
        tr.set("net/#rust", "alice", TypingState::Active, t0());
        assert!(tr.clear("net/#rust", "alice"));
        assert!(!tr.clear("net/#rust", "alice"));
    }

    #[test]
    fn clear_nick_is_scoped_to_one_connection() {
        // The same nick can be typing on two networks. A QUIT on one must not
        // clear the indicator on the other.
        let mut tr = TypingTracker::default();
        let now = t0();
        tr.set("libera/#rust", "alice", TypingState::Active, now);
        tr.set("oftc/#rust", "alice", TypingState::Active, now);
        tr.set("libera/#tokio", "alice", TypingState::Active, now);

        let mut affected = tr.clear_nick_on_connection("libera", "alice");
        affected.sort();
        assert_eq!(affected, vec!["libera/#rust", "libera/#tokio"]);
        // The other network is untouched.
        assert_eq!(tr.nicks("oftc/#rust"), vec!["alice"]);
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
        // Closing and reopening a query inside the 30s paused TTL must not
        // resurrect a stale typer.
        let mut tr = TypingTracker::default();
        tr.set("net/alice", "alice", TypingState::Paused, t0());
        assert!(tr.remove_buffer("net/alice"));
        assert!(tr.nicks("net/alice").is_empty());
        assert!(!tr.remove_buffer("net/alice"));
    }

    #[test]
    fn clear_all_reports_every_affected_buffer() {
        let mut tr = TypingTracker::default();
        let now = t0();
        tr.set("net/#rust", "alice", TypingState::Active, now);
        tr.set("net/#tokio", "bob", TypingState::Active, now);
        let mut cleared = tr.clear_all();
        cleared.sort();
        assert_eq!(cleared, vec!["net/#rust", "net/#tokio"]);
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
//! Lives beside `Buffer` rather than inside it: `Buffer` has no constructor and
//! is built from struct literals in ~39 places, mostly test fixtures. It is also
//! the better boundary — typing is session state, not buffer content.

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

/// `buffer_id -> nick -> entry`. Buffer ids are `{conn_id}/{name}`
/// (`src/state/buffer.rs:213`), which is what makes connection-scoped cleanup
/// a prefix match.
#[derive(Debug, Default)]
pub struct TypingTracker {
    entries: HashMap<String, HashMap<String, TypingEntry>>,
}

impl TypingTracker {
    /// Record a peer's typing state.
    ///
    /// Returns `true` when the **visible set** of typing nicks changed — that is
    /// what decides whether the web clients need a push. Refreshing a nick who is
    /// already shown as typing changes nothing on screen.
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

    /// Stop showing `nick` as typing anywhere **on one connection** — they quit,
    /// or changed nick.
    ///
    /// Scoped deliberately: a QUIT belongs to a single connection, and the same
    /// nick may well be typing on another network. Returns the buffers changed.
    pub fn clear_nick_on_connection(&mut self, conn_id: &str, nick: &str) -> Vec<String> {
        let prefix = format!("{conn_id}/");
        let mut changed = Vec::new();
        self.entries.retain(|buffer_id, buf| {
            if buffer_id.starts_with(&prefix) && buf.remove(nick).is_some() {
                changed.push(buffer_id.clone());
            }
            !buf.is_empty()
        });
        changed
    }

    /// Drop a closed buffer's state entirely. Returns `true` if there was any.
    pub fn remove_buffer(&mut self, buffer_id: &str) -> bool {
        self.entries.remove(buffer_id).is_some()
    }

    /// Forget everything — used when `typing.show` is switched off. Returns every
    /// buffer that had state, so the web clients can be told to clear it.
    pub fn clear_all(&mut self) -> Vec<String> {
        let changed: Vec<String> = self.entries.keys().cloned().collect();
        self.entries.clear();
        changed
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

Register the module in `src/state/mod.rs`:

```rust
pub mod typing;
```

Add both fields to `pub struct AppState` in `src/state/mod.rs`, after `pending_userhost_requests`:

```rust
    /// Who is typing, per buffer (IRCv3 `+typing`). Ephemeral — never persisted.
    pub typing: typing::TypingTracker,
    /// Mirror of `config.typing.show`. `events.rs` has no access to `AppConfig`,
    /// so this follows the same config→state sync as `scrollback_limit`.
    /// It gates *ingestion*, not just rendering — see spec §5.
    pub typing_show: bool,
```

Add two lines to the `Self { … }` literal in `AppState::new()` (`src/state/events.rs:11`):

```rust
            typing: crate::state::typing::TypingTracker::default(),
            typing_show: true,
```

Wire buffer removal into `AppState::remove_buffer` (`src/state/events.rs:84`), right next to the existing `self.buffers.shift_remove(id)` at `:99`:

```rust
        self.typing.remove_buffer(id);
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `make test`
Expected: PASS — the 8 new tests green. `AppState` has exactly one constructor, so no other call site needs touching.

Run: `make clippy`
Expected: no new warnings from `src/state/typing.rs`.

- [ ] **Step 5: Commit**

```bash
git add src/state/typing.rs src/state/mod.rs src/state/events.rs
git commit -m "feat(state): TypingTracker for IRCv3 +typing"
```

---

### Task 4: Receive `TAGMSG` — `src/irc/events.rs`

Wire the inbound path, with all seven drop rules from spec §4.3. Nothing in this task may put a line in a buffer.

**Files:**
- Modify: `src/irc/events.rs` (new match arm + `handle_tagmsg`; clear-typing calls in the PRIVMSG / NOTICE / PART / KICK / QUIT / NICK handlers)
- Test: inline `mod tests` in `src/irc/events.rs` (helpers `make_test_state()` at `:5396` and `make_channel_buffer()` at `:5514` already exist — use them)

**Interfaces:**
- Consumes: `crate::irc::typing::{parse_typing, strip_statusmsg}` (Task 1), `Isupport::statusmsg` (Task 2), `AppState.typing` / `AppState.typing_show` (Task 3).
- Produces: typing state populated from live TAGMSG traffic. Also `fn push_typing_web_event(state: &mut AppState, buffer_id: &str)`, used by Task 6.

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

    handle_irc_message(&mut state, "conn1", &tagmsg("alice", "#rust", "+typing", "active"));

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
fn typing_show_off_drops_the_notification_entirely() {
    // The setting must gate INGESTION: gating only the render arm would leave the
    // tracker full and keep broadcasting WebEvent::Typing to the browser.
    let mut state = make_test_state();
    state.typing_show = false;
    state.add_buffer(make_channel_buffer("conn1", "#rust"));
    handle_irc_message(&mut state, "conn1", &tagmsg("alice", "#rust", "+typing", "active"));
    assert!(state.typing.nicks("conn1/#rust").is_empty());
    assert!(state.pending_web_events.is_empty());
}

#[test]
fn our_own_tagmsg_echo_is_ignored() {
    // echo-message reflects our own TAGMSG back at us (spec §3.2).
    let mut state = make_test_state();
    state.add_buffer(make_channel_buffer("conn1", "#rust"));
    let our_nick = state.connections["conn1"].nick.clone();
    handle_irc_message(&mut state, "conn1", &tagmsg(&our_nick, "#rust", "+typing", "active"));
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
    handle_irc_message(&mut state, "conn1", &tagmsg("alice", "#rust", "+example-tag", "x"));
    assert!(state.typing.nicks("conn1/#rust").is_empty());
}

#[test]
fn tagmsg_never_creates_a_buffer() {
    // Otherwise a stranger could pop a query window open just by typing.
    let mut state = make_test_state();
    let our_nick = state.connections["conn1"].nick.clone();
    handle_irc_message(&mut state, "conn1", &tagmsg("stranger", &our_nick, "+typing", "active"));
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

    handle_irc_message(&mut state, "conn1", &tagmsg("alice", &our_nick, "+typing", "active"));
    assert_eq!(state.typing.nicks("conn1/alice"), vec!["alice"]);
}

#[test]
fn statusmsg_prefixed_target_resolves_to_the_channel() {
    let mut state = make_test_state();
    state
        .connections
        .get_mut("conn1")
        .expect("conn")
        .isupport_parsed
        .parse_tokens(&["STATUSMSG=@+"]);
    state.add_buffer(make_channel_buffer("conn1", "#rust"));
    handle_irc_message(&mut state, "conn1", &tagmsg("alice", "@#rust", "+typing", "active"));
    assert_eq!(state.typing.nicks("conn1/#rust"), vec!["alice"]);
}

#[test]
fn ampersand_channel_is_not_mistaken_for_a_statusmsg_prefix() {
    // `&local` is a CHANNEL. Stripping the `&` would resolve it as a query.
    let mut state = make_test_state();
    state
        .connections
        .get_mut("conn1")
        .expect("conn")
        .isupport_parsed
        .parse_tokens(&["STATUSMSG=@+"]);
    state.add_buffer(make_channel_buffer("conn1", "&local"));
    handle_irc_message(&mut state, "conn1", &tagmsg("alice", "&local", "+typing", "active"));
    assert_eq!(state.typing.nicks("conn1/&local"), vec!["alice"]);
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
fn kick_clears_the_kicked_user_not_the_kicker() {
    // handle_kick binds `kicker` and `kicked_user` — there is no `nick`.
    let mut state = make_test_state();
    state.add_buffer(make_channel_buffer("conn1", "#rust"));
    handle_irc_message(&mut state, "conn1", &tagmsg("alice", "#rust", "+typing", "active"));
    handle_irc_message(&mut state, "conn1", &tagmsg("bob", "#rust", "+typing", "active"));

    // bob kicks alice: alice stops typing, bob does not.
    let kick: IrcMessage = ":bob!u@h KICK #rust alice :out\r\n".parse().expect("valid");
    handle_irc_message(&mut state, "conn1", &kick);
    assert_eq!(state.typing.nicks("conn1/#rust"), vec!["bob"]);
}

#[test]
fn quit_clears_typing_only_on_that_connection() {
    let mut state = make_test_state();
    state.add_buffer(make_channel_buffer("conn1", "#rust"));
    handle_irc_message(&mut state, "conn1", &tagmsg("alice", "#rust", "+typing", "active"));

    let quit: IrcMessage = ":alice!u@h QUIT :bye\r\n".parse().expect("valid");
    handle_irc_message(&mut state, "conn1", &quit);
    assert!(state.typing.nicks("conn1/#rust").is_empty());
}
```

> If `make_test_state()` provides only one connection, `quit_clears_typing_only_on_that_connection` cannot prove the cross-network half. The connection-scoping itself is already proven by `clear_nick_is_scoped_to_one_connection` in Task 3, which is the layer that implements it — this test only checks the handler calls the scoped method.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `make test`
Expected: FAIL — the typing maps are empty, because nothing handles TAGMSG yet.

- [ ] **Step 3: Write the implementation**

**3a.** Add the dispatch arm in `handle_irc_message` (`src/irc/events.rs`), immediately after the `Command::NOTICE` arm:

```rust
        // IRCv3 `TAGMSG` has no Command variant in the proto crate, so it
        // arrives as Raw. Only `+typing` is understood; the message-tags spec
        // forbids ever displaying TAGMSG in history.
        Command::Raw(verb, args) if verb.eq_ignore_ascii_case("TAGMSG") && !args.is_empty() => {
            handle_tagmsg(state, conn_id, &our_nick, msg, &args[0], tags.as_ref());
        }
```

**3b.** Add the handler, next to `handle_privmsg`. Add `use std::time::Instant;` to the file's imports.

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
    // 2. A script suppressed this event.
    if state.suppress_event_display {
        return;
    }
    // 3. The user asked not to see typing. This gates INGESTION, not rendering:
    //    tracking it anyway would keep feeding the web clients (spec §5).
    if !state.typing_show {
        return;
    }
    // 4. No typing tag: nothing else is implemented.
    let Some(typing_state) = tags.and_then(crate::irc::typing::parse_typing) else {
        return;
    };

    let (nick, ident, host) = extract_nick_userhost(msg.prefix.as_ref());
    if nick.is_empty() {
        return;
    }
    // 5. echo-message reflects our own TAGMSG back at us (spec §3.2).
    if nick.eq_ignore_ascii_case(our_nick) {
        return;
    }

    // Resolve the target. `&chan` and `+chan` are CHANNELS, so only a character
    // the server actually advertised in STATUSMSG may be stripped, and only when
    // what remains is still a channel.
    let statusmsg = state
        .connections
        .get(conn_id)
        .map(|c| c.isupport_parsed.statusmsg().to_string())
        .unwrap_or_default();
    let target = crate::irc::typing::strip_statusmsg(target, &statusmsg);
    let target_is_channel = is_channel(target);

    // 6. Ignore list.
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

    // 7. Typing never creates a buffer: otherwise any stranger could pop a query
    //    window open on your screen without ever sending a message.
    if !state.buffers.contains_key(&buffer_id) {
        return;
    }

    if state.typing.set(&buffer_id, &nick, typing_state, Instant::now()) {
        push_typing_web_event(state, &buffer_id);
    }
}

/// Enqueue the current typing set for a buffer to the web clients.
/// The full set is sent, not a delta — idempotent and self-healing.
pub(crate) fn push_typing_web_event(state: &mut AppState, buffer_id: &str) {
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

> `Instant::now()` is fine *here*: this is the I/O edge where a real clock legitimately enters the system. Everything below it (Tasks 1 and 3) takes `now` as a parameter.
>
> `WebEvent::Typing` does not exist until Task 8. Until then, stub the body of `push_typing_web_event` to `let _ = (state, buffer_id);` and restore it in Task 8. The tests in this task do not exercise the web path — except `typing_show_off_drops_the_notification_entirely`, which asserts `pending_web_events` stays *empty*, and passes either way.

**3c.** Clear typing when the sender speaks or leaves.

In `handle_privmsg` and `handle_notice`, once `buffer_id` is computed and the ignore check has passed:

```rust
    // A message from this nick means they are no longer typing (spec §1.2).
    if state.typing.clear(&buffer_id, &nick) {
        push_typing_web_event(state, &buffer_id);
    }
```

In `handle_part`, once the channel's `buffer_id` is known (the leaving nick comes from the prefix):

```rust
    if state.typing.clear(&buffer_id, &nick) {
        push_typing_web_event(state, &buffer_id);
    }
```

In `handle_kick` (`src/irc/events.rs:2810`) — **clear `kicked_user`, not `kicker`.** The prefix identifies who did the kicking; the person who stopped typing is the one who was kicked:

```rust
    if state.typing.clear(&buffer_id, kicked_user) {
        push_typing_web_event(state, &buffer_id);
    }
```

In the `QUIT` and `NICK` handlers, which are not scoped to one channel — but **are** scoped to one connection:

```rust
    for buffer_id in state.typing.clear_nick_on_connection(conn_id, &nick) {
        push_typing_web_event(state, &buffer_id);
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `make test`
Expected: PASS — all 13 new tests green, existing `events.rs` suite unaffected.

Run: `make clippy`
Expected: no new warnings from `src/irc/events.rs`.

- [ ] **Step 5: Commit**

```bash
git add src/irc/events.rs
git commit -m "feat(irc): receive +typing via TAGMSG"
```

---

### Task 5: Configuration — `[typing]`, `/set` keys, statusbar item, `/items`

**Files:**
- Modify: `src/config/mod.rs` (`TypingConfig`, `AppConfig.typing`, `StatusbarItem::Typing`, new default item order)
- Modify: `src/commands/handlers_ui.rs` (`parse_statusbar_item` `:586`, `statusbar_item_name` `:598`, `AVAILABLE_ITEMS`) — **the enum is matched exhaustively there; skipping this does not compile**
- Modify: `src/commands/settings.rs` (get/set arms, completion list, `typing_show` sync)
- Modify: `src/app/mod.rs:547`, `src/commands/handlers_admin.rs:117` (the other two config→state sync sites)
- Modify: `src/ui/status_line.rs` (temporary empty arm; Task 7 fills it in)
- Test: inline `mod tests` in `src/config/mod.rs`

**Interfaces:**
- Consumes: `AppState.typing_show`, `TypingTracker::clear_all` (Task 3).
- Produces:
  - `pub struct TypingConfig { pub show: bool, pub send_channels: bool, pub send_queries: bool }`, all defaulting to `true`
  - `AppConfig.typing: TypingConfig`
  - `StatusbarItem::Typing`, wired into `/items`
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

Add to `mod tests` in `src/commands/handlers_ui.rs`:

```rust
#[test]
fn typing_is_a_manageable_statusbar_item() {
    // /items add typing must work, and the default item must have a name.
    assert_eq!(parse_statusbar_item("typing"), Some(StatusbarItem::Typing));
    assert_eq!(statusbar_item_name(&StatusbarItem::Typing), "typing");
    assert!(AVAILABLE_ITEMS.contains("typing"));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `make test`
Expected: FAIL — `no field typing on type AppConfig`, `no variant Typing found for enum StatusbarItem`.

- [ ] **Step 3: Write the implementation**

In `src/config/mod.rs`:

```rust
/// IRCv3 `+typing`. Split three ways because the spec asks clients to "provide
/// appropriate privacy controls": you may watch without broadcasting, or
/// broadcast in DMs but not in public channels.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TypingConfig {
    /// Receive and display other people's typing indicators. Gates ingestion,
    /// not just rendering (spec §5).
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

Add to `pub struct AppConfig` (next to `statusbar`):

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

Change the default item order in `impl Default for StatusbarConfig` (`:233`):

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

In `src/commands/handlers_ui.rs` — all three sites:

```rust
// parse_statusbar_item, :586
        "typing" => Some(StatusbarItem::Typing),
// statusbar_item_name, :598
        StatusbarItem::Typing => "typing",
```

and add `typing` to the `AVAILABLE_ITEMS` string so `/items` lists it.

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

Add the mirror arm to `set_config_value` (follow the bool-coercion pattern the neighbouring `statusbar.enabled` arm uses), and add the three keys to the completion list (~`:630`):

```rust
    "typing.show",
    "typing.send_channels",
    "typing.send_queries",
```

**Sync `typing_show` into `AppState`, and clear on switch-off.** In `src/commands/settings.rs` next to the existing post-`/set` sync block (`:856`):

```rust
                app.state.typing_show = app.config.typing.show;
                if !app.config.typing.show {
                    // Stop showing what the user just asked not to see — including
                    // on any live web client.
                    for buffer_id in app.state.typing.clear_all() {
                        crate::irc::events::push_typing_web_event(&mut app.state, &buffer_id);
                    }
                }
```

Add the same one-line sync (without the clear) at the other two sites: `src/app/mod.rs:547` and `src/commands/handlers_admin.rs:117`.

Finally, add a temporary empty arm to `src/ui/status_line.rs` so the exhaustive match compiles — Task 7 replaces it:

```rust
            StatusbarItem::Typing => {}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `make test`
Expected: PASS — 4 new tests green.

Run: `make clippy`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/config/mod.rs src/commands/handlers_ui.rs src/commands/settings.rs src/commands/handlers_admin.rs src/app/mod.rs src/ui/status_line.rs
git commit -m "feat(config): [typing] section, statusbar item, /set keys"
```

---

### Task 6: Send `+typing` — `src/app/typing.rs`

The outbound machine. Re-read spec §4.5 and the "three false assumptions" above before starting; this is where all three bite.

**Files:**
- Create: `src/app/typing.rs`
- Modify: `src/app/mod.rs` (`pub mod typing;`, `App.typing: TypingSender` + init, tick hook), `src/app/input.rs` (hooks around `Event::Key` **and** `Event::Paste`; `on_submit` on Enter)
- Test: inline `mod tests` in `src/app/typing.rs`

**Interfaces:**
- Consumes: `crate::irc::typing::{build_tagmsg, should_type, TypingState, THROTTLE}` (Task 1), `Isupport::client_tag_allowed` (Task 2), `TypingConfig` (Task 5).
- Produces:
  - `pub enum TypingSource { Tui, Web(String) }` — `Clone + PartialEq + Eq + Hash`
  - `pub struct TypingSender` (`Default`), a **pure proposer**:
    - `on_activity(&mut self, source: TypingSource, buffer_id: &str, active: bool, now: Instant) -> Vec<(String, TypingState)>`
    - `on_submit(&mut self, source: &TypingSource, buffer_id: &str, now: Instant) -> Vec<(String, TypingState)>`
    - `remove_source(&mut self, source: &TypingSource, now: Instant) -> Vec<(String, TypingState)>`
    - `on_tick(&mut self, now: Instant) -> Vec<(String, TypingState)>`
    - `confirm_sent(&mut self, buffer_id: &str, state: TypingState, now: Instant)`
  - `impl App { pub(crate) fn on_input_changed(&mut self); pub(crate) fn on_web_typing(&mut self, session_id: &str, buffer_id: &str, active: bool); pub(crate) fn on_typing_submit(&mut self, source: TypingSource, buffer_id: &str); pub(crate) fn typing_tick(&mut self); pub(crate) fn expire_typing(&mut self); }`

**Why propose/confirm.** The guards (capability, `CLIENTTAGDENY`, config, buffer type) live in `App`, not in the machine. If the machine recorded a notification as *sent* and then a guard rejected it, its state would diverge from reality and the target would go silent forever. So the machine proposes, `App` sends what passes, and only then calls `confirm_sent`. A rejected proposal is simply re-proposed next tick — cheap, and it self-heals the moment the guard clears (a late `CAP ACK`, a config flip).

- [ ] **Step 1: Write the failing tests**

Create `src/app/typing.rs` with **only** this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn t0() -> Instant {
        Instant::now()
    }

    /// Confirm every proposal, as `App` does when all guards pass.
    fn flush(s: &mut TypingSender, due: &[(String, TypingState)], now: Instant) {
        for (buffer_id, state) in due {
            s.confirm_sent(buffer_id, *state, now);
        }
    }

    fn tui() -> TypingSource {
        TypingSource::Tui
    }

    #[test]
    fn first_keystroke_proposes_active() {
        let mut s = TypingSender::default();
        let now = t0();
        let due = s.on_activity(tui(), "net/#rust", true, now);
        assert_eq!(due, vec![("net/#rust".to_string(), TypingState::Active)]);
        flush(&mut s, &due, now);
    }

    #[test]
    fn keystrokes_within_three_seconds_are_throttled() {
        // Spec: "MUST be throttled so that any typing notification is not sent
        // within 3 seconds of another one for a given target."
        let mut s = TypingSender::default();
        let now = t0();
        let due = s.on_activity(tui(), "net/#rust", true, now);
        flush(&mut s, &due, now);

        let t1 = now + Duration::from_millis(500);
        assert!(s.on_activity(tui(), "net/#rust", true, t1).is_empty());
        let t2 = now + Duration::from_secs(2);
        assert!(s.on_activity(tui(), "net/#rust", true, t2).is_empty());
    }

    #[test]
    fn done_obeys_the_three_second_throttle_and_is_flushed_by_the_tick() {
        // Clearing the input 200ms after `active` must NOT send `done` early —
        // the throttle is unqualified in the spec. It stays pending and the tick
        // emits it once the window opens.
        let mut s = TypingSender::default();
        let now = t0();
        let due = s.on_activity(tui(), "net/#rust", true, now);
        flush(&mut s, &due, now);

        let cleared = now + Duration::from_millis(200);
        assert!(s.on_activity(tui(), "net/#rust", false, cleared).is_empty());
        // Still throttled at 2s.
        assert!(s.on_tick(now + Duration::from_secs(2)).is_empty());
        // At 3s the window opens and the pending `done` goes out.
        let at3 = now + Duration::from_secs(3);
        let due = s.on_tick(at3);
        assert_eq!(due, vec![("net/#rust".to_string(), TypingState::Done)]);
        flush(&mut s, &due, at3);
        // And only once.
        assert!(s.on_tick(now + Duration::from_secs(10)).is_empty());
    }

    #[test]
    fn a_pending_done_is_coalesced_away_if_typing_resumes() {
        // Type, clear, retype — all inside the throttle window. The peer should
        // never see a `done`; the state never actually changed.
        let mut s = TypingSender::default();
        let now = t0();
        let due = s.on_activity(tui(), "net/#rust", true, now);
        flush(&mut s, &due, now);

        s.on_activity(tui(), "net/#rust", false, now + Duration::from_millis(500));
        s.on_activity(tui(), "net/#rust", true, now + Duration::from_secs(1));

        // At 3s the aggregate is `active` again, so a refresh goes out, not a done.
        let at3 = now + Duration::from_secs(3);
        assert_eq!(
            s.on_tick(at3),
            vec![("net/#rust".to_string(), TypingState::Active)]
        );
    }

    #[test]
    fn slash_commands_never_type() {
        // The caller applies `should_type`; this asserts the wiring of that.
        assert!(!crate::irc::typing::should_type("/join #x"));
        assert!(crate::irc::typing::should_type("/me waves"));
    }

    #[test]
    fn tick_resends_active_while_typing_continues() {
        let mut s = TypingSender::default();
        let now = t0();
        let due = s.on_activity(tui(), "net/#rust", true, now);
        flush(&mut s, &due, now);

        // 1s: inside the throttle, nothing.
        assert!(s.on_tick(now + Duration::from_secs(1)).is_empty());
        // Keystroke at 2s keeps it fresh; tick at 3s resends `active`.
        s.on_activity(tui(), "net/#rust", true, now + Duration::from_secs(2));
        assert_eq!(
            s.on_tick(now + Duration::from_secs(3)),
            vec![("net/#rust".to_string(), TypingState::Active)]
        );
    }

    #[test]
    fn tick_sends_paused_once_after_three_idle_seconds() {
        let mut s = TypingSender::default();
        let now = t0();
        let due = s.on_activity(tui(), "net/#rust", true, now);
        flush(&mut s, &due, now);

        // Text still there, no keystroke for 3s → paused, exactly once.
        let at3 = now + Duration::from_secs(3);
        let due = s.on_tick(at3);
        assert_eq!(due, vec![("net/#rust".to_string(), TypingState::Paused)]);
        flush(&mut s, &due, at3);

        assert!(s.on_tick(now + Duration::from_secs(7)).is_empty());
        assert!(s.on_tick(now + Duration::from_secs(30)).is_empty());
    }

    #[test]
    fn switching_buffers_releases_the_old_target_immediately() {
        // The user switches and types in the new buffer before the next tick.
        // The old target must still get its `done` — it must not be forgotten.
        let mut s = TypingSender::default();
        let now = t0();
        let due = s.on_activity(tui(), "net/#rust", true, now);
        flush(&mut s, &due, now);

        let later = now + Duration::from_secs(4);
        let mut due = s.on_activity(tui(), "net/#tokio", true, later);
        due.sort_by(|a, b| a.0.cmp(&b.0)); // TypingState has no Ord — sort by buffer id
        assert_eq!(
            due,
            vec![
                ("net/#rust".to_string(), TypingState::Done),
                ("net/#tokio".to_string(), TypingState::Active),
            ]
        );
    }

    #[test]
    fn an_idle_second_source_does_not_cancel_a_typing_one() {
        // A freshly-opened browser tab reports `typing: false` for the buffer the
        // terminal is actively typing in. The terminal must keep typing.
        let mut s = TypingSender::default();
        let now = t0();
        let due = s.on_activity(TypingSource::Tui, "net/#rust", true, now);
        flush(&mut s, &due, now);

        let tab = TypingSource::Web("tab-1".to_string());
        assert!(
            s.on_activity(tab, "net/#rust", false, now + Duration::from_millis(100)).is_empty(),
            "an idle source must not retract another source's typing"
        );

        // And the aggregate is still `active` at the next window.
        s.on_activity(TypingSource::Tui, "net/#rust", true, now + Duration::from_secs(2));
        assert_eq!(
            s.on_tick(now + Duration::from_secs(3)),
            vec![("net/#rust".to_string(), TypingState::Active)]
        );
    }

    #[test]
    fn done_goes_out_only_when_the_last_source_stops() {
        let mut s = TypingSender::default();
        let now = t0();
        let tab = TypingSource::Web("tab-1".to_string());
        let due = s.on_activity(TypingSource::Tui, "net/#rust", true, now);
        flush(&mut s, &due, now);
        s.on_activity(tab.clone(), "net/#rust", true, now);

        // The terminal stops; the tab is still typing → no done.
        let t4 = now + Duration::from_secs(4);
        s.on_activity(TypingSource::Tui, "net/#rust", false, t4);
        assert_ne!(
            s.on_tick(t4).first().map(|(_, st)| *st),
            Some(TypingState::Done)
        );

        // The tab stops too → done.
        let t8 = now + Duration::from_secs(8);
        s.on_activity(tab, "net/#rust", false, t8);
        assert_eq!(s.on_tick(t8), vec![("net/#rust".to_string(), TypingState::Done)]);
    }

    #[test]
    fn removing_a_source_releases_its_target() {
        // A browser tab closes mid-typing.
        let mut s = TypingSender::default();
        let now = t0();
        let tab = TypingSource::Web("tab-1".to_string());
        let due = s.on_activity(tab.clone(), "net/#rust", true, now);
        flush(&mut s, &due, now);

        let t4 = now + Duration::from_secs(4);
        assert_eq!(
            s.remove_source(&tab, t4),
            vec![("net/#rust".to_string(), TypingState::Done)]
        );
    }

    #[test]
    fn submit_sends_nothing_and_retires_the_target() {
        // The PRIVMSG itself clears typing at the receivers, so `done` is noise.
        let mut s = TypingSender::default();
        let now = t0();
        let due = s.on_activity(tui(), "net/#rust", true, now);
        flush(&mut s, &due, now);

        let t4 = now + Duration::from_secs(4);
        assert!(s.on_submit(&tui(), "net/#rust", t4).is_empty());
        assert!(s.on_tick(now + Duration::from_secs(8)).is_empty());
    }

    #[test]
    fn a_sent_message_suppresses_typing_for_three_seconds() {
        // Flood budget: a TAGMSG costs as much as a short PRIVMSG (spec §3.1).
        // The message itself already announced our presence.
        let mut s = TypingSender::default();
        let now = t0();
        s.on_submit(&tui(), "net/#rust", now);

        assert!(s.on_activity(tui(), "net/#rust", true, now + Duration::from_secs(1)).is_empty());
        assert_eq!(
            s.on_activity(tui(), "net/#rust", true, now + Duration::from_secs(3)),
            vec![("net/#rust".to_string(), TypingState::Active)]
        );
    }

    #[test]
    fn typing_stops_when_the_flood_budget_runs_low() {
        // A frame handed to Sender::send under pressure is QUEUED and delayed,
        // not dropped (client/mod.rs:1166-1180). So we must refuse to send here.
        let mut s = TypingSender::default();
        let mut now = t0();
        // Burn the budget with real messages in another buffer.
        for _ in 0..3 {
            s.on_submit(&tui(), "net/#other", now);
            now += Duration::from_millis(100);
        }
        // Typing in a fresh buffer is now refused: no headroom left.
        assert!(
            s.on_activity(tui(), "net/#rust", true, now).is_empty(),
            "typing must not be handed to the transport when the budget is tight"
        );

        // Once the penalty has drained, typing resumes.
        let later = now + Duration::from_secs(10);
        assert_eq!(
            s.on_activity(tui(), "net/#rust", true, later),
            vec![("net/#rust".to_string(), TypingState::Active)]
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
//! Two layers, because **there is no single "current input"**. The TUI has one,
//! and every web session has its own: `handle_web_command` receives a
//! `session_id` (`src/app/web.rs:413-416`) and `web_active_buffers` maps session
//! to buffer, independently of `state.active_buffer_id`. Two browser tabs can be
//! typing into two buffers while the terminal types into a third.
//!
//! * **Sources** report `(buffer, is-typing, when)`.
//! * **Targets** are buffers. A target's desired state is the *aggregate* of the
//!   sources pointing at it, and the 3s throttle clock is per target.
//!
//! `TypingSender` is pure: it takes `now` and **proposes** notifications. `App`
//! owns the clock, the guards and the socket, and calls `confirm_sent` for the
//! proposals that actually went out. A proposal blocked by a guard is simply
//! re-proposed on the next tick.

use std::collections::HashMap;
use std::time::Instant;

use crate::app::App;
use crate::irc::typing::{self, THROTTLE, TypingState};
use crate::state::buffer::BufferType;

/// Mirrors `flood_penalty_threshold` in `src/irc/mod.rs:396`.
const PENALTY_THRESHOLD_MS: u64 = 10_000;
/// `command_penalty` for `Command::Raw` (2000) + `length_penalty` for a short
/// frame (1000) — `irc-repartee-1.5.1/src/client/mod.rs:992-1045`.
const TAGMSG_COST_MS: u64 = 3_000;
/// The same, for a short PRIVMSG.
const MESSAGE_COST_MS: u64 = 3_000;
/// Budget we refuse to spend on typing, so the user's real messages keep theirs.
const RESERVED_MS: u64 = 4_000;

/// Where a typing signal came from. Each web session is its own source.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypingSource {
    Tui,
    Web(String),
}

#[derive(Debug)]
struct SourceState {
    buffer_id: String,
    /// Its input holds non-empty, non-slash text.
    active: bool,
    /// When its input last changed — drives the `paused` transition.
    last_activity: Instant,
}

#[derive(Debug, Default)]
struct TargetState {
    /// The last state we actually put on the wire for this buffer.
    sent: Option<TypingState>,
    /// The per-target 3s throttle clock.
    last_sent: Option<Instant>,
}

/// A conservative mirror of the crate's outgoing penalty counter.
///
/// This has to exist because `Sender::send` feeds an `UnboundedSender`
/// (`client/mod.rs:940-947`): a frame handed over while the penalty is high is
/// not dropped, it is **buffered and delayed** (`client/mod.rs:1166-1180`). A
/// typing notification that arrives late is worse than none, and it spends
/// budget the user's next real message needs. So the decision not to send has to
/// be taken here, before the frame reaches the queue.
///
/// If the crate's penalty formula changes, this must change with it.
#[derive(Debug, Default)]
struct FloodEstimate {
    penalty_ms: u64,
    last_drain: Option<Instant>,
}

impl FloodEstimate {
    fn drain(&mut self, now: Instant) {
        if let Some(last) = self.last_drain {
            let elapsed = u64::try_from(now.duration_since(last).as_millis()).unwrap_or(u64::MAX);
            self.penalty_ms = self.penalty_ms.saturating_sub(elapsed);
        }
        self.last_drain = Some(now);
    }

    fn charge(&mut self, now: Instant, cost_ms: u64) {
        self.drain(now);
        self.penalty_ms = self.penalty_ms.saturating_add(cost_ms);
    }

    /// Room for one more TAGMSG without eating into the reserve?
    fn has_headroom(&mut self, now: Instant) -> bool {
        self.drain(now);
        self.penalty_ms + TAGMSG_COST_MS <= PENALTY_THRESHOLD_MS.saturating_sub(RESERVED_MS)
    }
}

/// Proposes typing notifications. Pure — no clock, no I/O.
#[derive(Debug, Default)]
pub struct TypingSender {
    sources: HashMap<TypingSource, SourceState>,
    targets: HashMap<String, TargetState>,
    /// Real messages we sent, per target — for the §3.1 suppression window.
    last_message: HashMap<String, Instant>,
    flood: FloodEstimate,
}

impl TypingSender {
    /// A source's input changed. `active` is the already-computed `should_type`
    /// predicate — the browser applies it locally and sends only the boolean.
    pub fn on_activity(
        &mut self,
        source: TypingSource,
        buffer_id: &str,
        active: bool,
        now: Instant,
    ) -> Vec<(String, TypingState)> {
        let previous = self.sources.insert(
            source,
            SourceState {
                buffer_id: buffer_id.to_string(),
                active,
                last_activity: now,
            },
        );

        let mut dirty = vec![buffer_id.to_string()];
        // A source that moved releases its old target *now*, not at the next
        // tick: the user may switch and type again within the same second, and
        // the old target must still get its `done`.
        if let Some(prev) = previous
            && prev.buffer_id != buffer_id
        {
            dirty.push(prev.buffer_id);
        }
        self.propose(&dirty, now)
    }

    /// A source submitted a message. Sends nothing: the PRIVMSG itself clears
    /// typing at the receivers.
    pub fn on_submit(
        &mut self,
        source: &TypingSource,
        buffer_id: &str,
        now: Instant,
    ) -> Vec<(String, TypingState)> {
        self.last_message.insert(buffer_id.to_string(), now);
        self.flood.charge(now, MESSAGE_COST_MS);
        if let Some(s) = self.sources.get_mut(source) {
            s.active = false;
            s.last_activity = now;
        }
        // Retire our state for this target without a `done`.
        if let Some(t) = self.targets.get_mut(buffer_id) {
            t.sent = None;
        }
        // Another source may still be typing here, so re-derive rather than assume.
        self.propose(&[buffer_id.to_string()], now)
    }

    /// A source went away — a web session disconnected.
    pub fn remove_source(
        &mut self,
        source: &TypingSource,
        now: Instant,
    ) -> Vec<(String, TypingState)> {
        let Some(prev) = self.sources.remove(source) else {
            return Vec::new();
        };
        self.propose(&[prev.buffer_id], now)
    }

    /// The 1s tick: refresh `active`, transition to `paused`, flush a pending
    /// `done` once its throttle window opens.
    pub fn on_tick(&mut self, now: Instant) -> Vec<(String, TypingState)> {
        let mut ids: Vec<String> = self.targets.keys().cloned().collect();
        for s in self.sources.values() {
            if !ids.contains(&s.buffer_id) {
                ids.push(s.buffer_id.clone());
            }
        }
        let due = self.propose(&ids, now);
        self.prune(now);
        due
    }

    /// Record that a proposal actually reached the wire. `App` calls this only
    /// after every guard passed and `Sender::send` succeeded.
    pub fn confirm_sent(&mut self, buffer_id: &str, state: TypingState, now: Instant) {
        self.flood.charge(now, TAGMSG_COST_MS);
        let entry = self.targets.entry(buffer_id.to_string()).or_default();
        entry.last_sent = Some(now);
        entry.sent = (state != TypingState::Done).then_some(state);
    }

    fn propose(&mut self, buffer_ids: &[String], now: Instant) -> Vec<(String, TypingState)> {
        let mut out = Vec::new();
        for buffer_id in buffer_ids {
            if let Some(state) = self.next_state(buffer_id, now) {
                out.push((buffer_id.clone(), state));
            }
        }
        out
    }

    /// The notification this target needs right now, if any.
    fn next_state(&mut self, buffer_id: &str, now: Instant) -> Option<TypingState> {
        let desired = self.desired(buffer_id, now);
        let sent = self.targets.get(buffer_id).and_then(|t| t.sent);

        let needed = match (desired, sent) {
            // Nothing to say, nothing outstanding.
            (None, None) => return None,
            // Every source stopped: retract.
            (None, Some(_)) => TypingState::Done,
            // `active` is sent "continuously" — refresh it each window.
            (Some(TypingState::Active), Some(TypingState::Active)) => TypingState::Active,
            (Some(d), Some(s)) if d == s => return None,
            (Some(d), _) => d,
        };

        // The 3s throttle binds EVERY notification, `done` included — the spec is
        // unqualified. A blocked notification is neither dropped nor sent early:
        // desired-vs-sent still differs, so the next tick reconsiders it against
        // whatever is true then. That is also how a `done` gets coalesced away if
        // the user resumes typing before the window opens.
        if self.throttled(buffer_id, now) {
            return None;
        }
        // The message we just sent already announced us (§3.1).
        if self.suppressed_by_message(buffer_id, now) {
            return None;
        }
        // Never hand a frame to the transport when the budget is tight (§3.1).
        if !self.flood.has_headroom(now) {
            return None;
        }
        Some(needed)
    }

    /// What this target's sources collectively imply.
    fn desired(&self, buffer_id: &str, now: Instant) -> Option<TypingState> {
        let mut any = false;
        let mut fresh = false;
        for s in self.sources.values() {
            if s.buffer_id != buffer_id || !s.active {
                continue;
            }
            any = true;
            if now.duration_since(s.last_activity) < THROTTLE {
                fresh = true;
            }
        }
        if !any {
            return None;
        }
        // A source touched its input recently → still `active`. Otherwise text is
        // sitting there untouched → `paused`.
        Some(if fresh {
            TypingState::Active
        } else {
            TypingState::Paused
        })
    }

    fn throttled(&self, buffer_id: &str, now: Instant) -> bool {
        self.targets
            .get(buffer_id)
            .and_then(|t| t.last_sent)
            .is_some_and(|t| now.duration_since(t) < THROTTLE)
    }

    fn suppressed_by_message(&self, buffer_id: &str, now: Instant) -> bool {
        self.last_message
            .get(buffer_id)
            .is_some_and(|t| now.duration_since(*t) < THROTTLE)
    }

    /// Drop retired targets and stale message timestamps so the maps stay bounded.
    fn prune(&mut self, now: Instant) {
        let live: Vec<&str> = self.sources.values().map(|s| s.buffer_id.as_str()).collect();
        self.targets.retain(|id, t| {
            t.sent.is_some()
                || live.contains(&id.as_str())
                || t.last_sent
                    .is_some_and(|s| now.duration_since(s) < THROTTLE)
        });
        self.last_message
            .retain(|_, t| now.duration_since(*t) < THROTTLE);
    }
}
```

Now the `App` glue, in the same file:

```rust
impl App {
    /// The terminal's input changed — a keystroke or a paste.
    pub(crate) fn on_input_changed(&mut self) {
        let Some(buffer_id) = self.state.active_buffer_id.clone() else {
            return;
        };
        let active = typing::should_type(&self.input.value);
        let due = self
            .typing
            .on_activity(TypingSource::Tui, &buffer_id, active, Instant::now());
        self.dispatch_typing(due);
    }

    /// A browser session's input changed. It reports only the predicate.
    pub(crate) fn on_web_typing(&mut self, session_id: &str, buffer_id: &str, active: bool) {
        let due = self.typing.on_activity(
            TypingSource::Web(session_id.to_string()),
            buffer_id,
            active,
            Instant::now(),
        );
        self.dispatch_typing(due);
    }

    /// A message was submitted from `source`.
    pub(crate) fn on_typing_submit(&mut self, source: TypingSource, buffer_id: &str) {
        let due = self.typing.on_submit(&source, buffer_id, Instant::now());
        self.dispatch_typing(due);
    }

    /// A web session disconnected.
    pub(crate) fn on_web_session_gone(&mut self, session_id: &str) {
        let source = TypingSource::Web(session_id.to_string());
        let due = self.typing.remove_source(&source, Instant::now());
        self.dispatch_typing(due);
    }

    /// 1s tick: refresh, pause, flush pending retractions.
    pub(crate) fn typing_tick(&mut self) {
        let due = self.typing.on_tick(Instant::now());
        self.dispatch_typing(due);
    }

    /// Expire *received* typing state (6s active, 30s paused).
    pub(crate) fn expire_typing(&mut self) {
        for buffer_id in self.state.typing.expire(Instant::now()) {
            crate::irc::events::push_typing_web_event(&mut self.state, &buffer_id);
        }
    }

    fn dispatch_typing(&mut self, due: Vec<(String, TypingState)>) {
        let now = Instant::now();
        for (buffer_id, state) in due {
            if self.send_typing(&buffer_id, state) {
                self.typing.confirm_sent(&buffer_id, state, now);
            }
        }
    }

    /// Send one typing notification if every guard allows it.
    /// Returns whether it reached the wire — the machine only records confirmed sends.
    fn send_typing(&mut self, buffer_id: &str, state: TypingState) -> bool {
        let Some(buf) = self.state.buffers.get(buffer_id) else {
            return false;
        };
        // Server, log, shell and DCC buffers have no channel or nick to TAGMSG.
        let allowed = match buf.buffer_type {
            BufferType::Channel => self.config.typing.send_channels,
            BufferType::Query => self.config.typing.send_queries,
            _ => false,
        };
        if !allowed {
            return false;
        }
        let target = buf.name.clone();
        let conn_id = buf.connection_id.clone();

        let Some(conn) = self.state.connections.get(&conn_id) else {
            return false;
        };
        if !conn.enabled_caps.contains("message-tags") {
            return false;
        }
        if !conn.isupport_parsed.client_tag_allowed("typing") {
            return false;
        }

        let Some(handle) = self.irc_handles.get(&conn_id) else {
            return false;
        };
        if handle
            .sender
            .send(typing::build_tagmsg(&target, state))
            .is_err()
        {
            tracing::debug!("failed to send +typing={} to {target}", state.as_str());
            return false;
        }
        true
    }
}
```

Wire it up:

**`src/app/mod.rs`** — register the module, add the field, initialise it, and call the hooks in the existing 1 s tick arm (`:1445`, next to `self.handle_netsplit_tick();`):

```rust
pub mod typing;
```
```rust
    /// Outbound IRCv3 `+typing` state machine.
    pub typing: crate::app::typing::TypingSender,
```
```rust
            typing: crate::app::typing::TypingSender::default(),
```
```rust
                    self.typing_tick();
                    self.expire_typing();
```

**`src/app/input.rs:33-36`** — hook **both** mutating event branches. Paste is a separate branch from keys, and a hook on `Event::Key` alone would miss it, so a pasted draft would not start typing until the next keystroke:

```rust
            Event::Key(key) => {
                let before = self.input.value.clone();
                self.handle_key(key);
                if self.input.value != before {
                    self.on_input_changed();
                }
            }
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Paste(text) => {
                let before = self.input.value.clone();
                self.handle_paste(&text);
                if self.input.value != before {
                    self.on_input_changed();
                }
            }
```

Snapshotting the value catches every path that mutates the input — chars, backspace, Ctrl-U/K/W, history recall, spell accept, paste — without touching a dozen match arms.

**`src/app/input.rs`** — in the `Enter` arm (`:293-299`), drive `on_submit` **before** `handle_submit`, so the snapshot comparison above sees an already-retired machine and does not emit a spurious `done`:

```rust
            (_, KeyCode::Enter | KeyCode::Char('\n' | '\r')) => {
                self.input.spell_state = None;
                let text = self.input.submit();
                if !text.is_empty() {
                    if let Some(buffer_id) = self.state.active_buffer_id.clone() {
                        self.on_typing_submit(
                            crate::app::typing::TypingSource::Tui,
                            &buffer_id,
                        );
                    }
                    self.handle_submit(&text);
                }
            }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `make test`
Expected: PASS — all 14 `app::typing::tests` green, existing suite unaffected.

Run: `make clippy`
Expected: no new warnings from `src/app/typing.rs`, `src/app/input.rs`, `src/app/mod.rs`.

- [ ] **Step 5: Commit**

```bash
git add src/app/typing.rs src/app/mod.rs src/app/input.rs
git commit -m "feat(app): send +typing with per-source aggregation and flood budget"
```

---

### Task 7: TUI status line — render + separator fix

**Files:**
- Modify: `src/ui/status_line.rs` (implement the `StatusbarItem::Typing` arm stubbed out in Task 5; fix the separator loop)
- Test: inline `mod tests` in `src/ui/status_line.rs`

**Interfaces:**
- Consumes: `AppState.typing` (Task 3), `StatusbarItem::Typing` (Task 5).
- Produces: `pub fn typing_phrase(nicks: &[&str]) -> Option<String>` — a free function precisely so it is testable without a `Frame`.

> `typing.show` is **not** checked here. It already gated ingestion in Task 4, so when it is off the tracker is empty and this arm renders nothing. Checking it in both places would be a lie about where the control lives.

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
        [a, b, rest @ ..] => Some(format!("{a}, {b} and {} others are typing…", rest.len())),
    }
}
```

**Fix the separator loop.** The loop at `:38-44` pushes the separator *before* the item renders, so an item that emits nothing — which `Typing` does most of the time — leaves a dangling `… | | Lag:`. Restructure so the separator is inserted only once an item has actually produced spans:

```rust
    for item in &app.config.statusbar.items {
        let start = spans.len();
        match item {
            // … existing arms, with their own separator pushes REMOVED …
            StatusbarItem::Typing => {
                if let Some(buf) = active_buf {
                    let nicks = app.state.typing.nicks(&buf.id);
                    if let Some(phrase) = typing_phrase(&nicks) {
                        spans.push(Span::styled(phrase, Style::default().fg(fg_muted)));
                    }
                }
            }
        }
        // Only now, knowing whether the item produced anything, decide whether it
        // needs a separator in front of it. `start > 1` because spans[0] is the
        // opening `[` pushed before the loop.
        if spans.len() > start && start > 1 {
            spans.insert(
                start,
                Span::styled(separator.as_str(), Style::default().fg(fg_dim)),
            );
        }
    }
```

> The existing `ChannelInfo` arm uses `continue` for its shell-buffer early exit (`:76`). With the separator now inserted *after* the match, that `continue` would skip the insert and lose the separator. Rewrite that early exit as an `if`/`else` so control falls through to the bottom of the loop body.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `make test`
Expected: PASS — 2 new tests green.

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
- Modify: `src/app/web.rs` (`handle_web_command` `:421` — the `Typing` arm, plus `on_submit` on the two message-sending arms, plus source removal on `WebDisconnect`)
- Modify: `src/irc/events.rs`, `src/app/typing.rs` — restore the `push_typing_web_event` body stubbed out in Task 4
- Test: inline `mod tests` in `src/web/protocol.rs`

**Interfaces:**
- Consumes: `App::on_web_typing`, `App::on_typing_submit`, `App::on_web_session_gone`, `TypingSource` (Task 6).
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
    /// reconnects mid-stream; a full set is idempotent and self-healing.
    /// Deliberately absent from `SyncInit` — typing is ephemeral, and a fresh
    /// client learns about it on the sender's next 3s refresh. That is exactly
    /// why the client must CLEAR its typing map when it processes `SyncInit`.
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
    /// about capabilities, `CLIENTTAGDENY`, throttling, flood budget or config —
    /// the core owns all of it. The core keys this by session id, because each
    /// browser tab is an independent source with its own active buffer.
    Typing { buffer_id: String, typing: bool },
```

Add the arm to `handle_web_command` in `src/app/web.rs` (`:421`):

```rust
            WebCommand::Typing { buffer_id, typing } => {
                // One source per session. Collapsing all sessions into one
                // predicate would let a freshly-opened, empty second tab retract
                // the typing that the terminal is doing right now.
                self.on_web_typing(session_id, &buffer_id, typing);
            }
```

**Drive `on_submit` from the web's message paths too.** Without this, a browser-sent message is followed by a redundant `done` (the input goes empty) and the 3-second post-message suppression is never recorded:

```rust
            WebCommand::SendMessage { buffer_id, text } => {
                self.on_typing_submit(
                    crate::app::typing::TypingSource::Web(session_id.to_string()),
                    &buffer_id,
                );
                self.web_send_message(&buffer_id, &text);
            }
```

and the same two lines in the `WebCommand::RunCommand` arm (`:491`).

**Release the source when a session goes away** — in the `WebCommand::WebDisconnect` arm, next to the existing `self.web_active_buffers.remove(session_id);` (`:516`):

```rust
                self.on_web_session_gone(session_id);
```

Restore the stub: put the real body back in `push_typing_web_event` (`src/irc/events.rs`, Task 4 step 3b).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `make test`
Expected: PASS — 2 new protocol tests green; the Task 4 and Task 6 suites still pass with the web path live.

Run: `make clippy`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/web/protocol.rs src/app/web.rs src/irc/events.rs src/app/typing.rs
git commit -m "feat(web): Typing event and per-session Typing command"
```

---

### Task 9: Web UI — display and send

**Files:**
- Modify: `web-ui/src/protocol.rs` (mirror both variants)
- Modify: `web-ui/src/state.rs` (`typing` signal; `Typing` arm; **clear on `SyncInit` and on `BufferClosed`**)
- Modify: `web-ui/src/components/status_line.rs` (span between channel and `Lag:`)
- Modify: `web-ui/src/components/input.rs` (debounced `WebCommand::Typing`)
- Test: inline `mod tests` in `web-ui/src/components/status_line.rs`

**Interfaces:**
- Consumes: `WebEvent::Typing`, `WebCommand::Typing` (Task 8).
- Produces: `AppState.typing: RwSignal<HashMap<String, Vec<String>>>` in the web client.

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

**`web-ui/src/protocol.rs`** — mirror both variants exactly as in Task 8 (same field names, same `#[serde(tag = "type")]`).

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

**Clear the map on `SyncInit`.** Typing is not in `SyncInit`, so a websocket reconnect that missed an expiry or clear leaves a stale "alice is typing…" on screen forever. In the existing `WebEvent::SyncInit { .. }` arm, add:

```rust
                // Typing is not carried in SyncInit — it is ephemeral and refills
                // within 3s from live traffic. Anything we are still showing is by
                // definition stale across a reconnect.
                self.typing.set(HashMap::new());
```

**Clear on `BufferClosed`**, in that arm:

```rust
                self.typing.update(|t| {
                    t.remove(&buffer_id);
                });
```

**`web-ui/src/components/status_line.rs`** — the phrasing helper (the web status line hardcodes its own layout and cannot reuse the TUI's):

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

Render it in the `view!` block, **between the channel span and the Lag span**:

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

**`web-ui/src/components/input.rs`** — the `value` signal already exists (`:167`). Report the predicate, debounced to at most one message per second:

```rust
    // Report typing to the core, which owns the state machine, the throttle, the
    // flood budget and every guard. We send a predicate, never the text.
    let last_report = StoredValue::new(0.0_f64);
    let last_sent_state = StoredValue::new(false);
    Effect::new(move |_| {
        let text = value.get();
        let Some(buffer_id) = state.active_buffer.get() else {
            return;
        };
        let typing = !text.is_empty() && (!text.starts_with('/') || text.starts_with("/me "));
        let now = js_sys::Date::now();
        // A change of state always reports immediately; a steady state is
        // rate-limited. The core still enforces the 3s IRC throttle — this only
        // keeps the websocket quiet.
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

Then `make release`, start repartee with the web server on, open the browser UI, and type from a second IRC client. Expected: the phrase appears in the browser status line in the same slot as the TUI. Type in the browser input: the second client sees you as typing. Open a **second** browser tab and leave it idle on the same buffer: the first tab's typing must not be cancelled.

Run: `make clippy`
Expected: clean (this includes `clippy-web`).

- [ ] **Step 5: Commit**

```bash
git add web-ui/src/protocol.rs web-ui/src/state.rs web-ui/src/components/status_line.rs web-ui/src/components/input.rs
git commit -m "feat(web-ui): typing indicator and per-session typing reporting"
```

---

### Task 10: Scripting event and documentation

**Files:**
- Modify: `src/scripting/api.rs` (event name constant)
- Modify: `src/app/scripting.rs` (emit — **this** is where script events are dispatched, not `events.rs`)
- Modify: `docs/rfc_ircv3_coverage.md`, `README.md`
- Modify: `docs/src/` + rebuild `docs/*.html` **only if** the docs site documents `/set` keys — check `docs/configuration.html` first and follow whatever build `docs/build.ts` defines.

**Interfaces:**
- Consumes: `crate::irc::typing::parse_typing` (Task 1). Suppression already works via `state.suppress_event_display`, checked in `handle_tagmsg` (Task 4, drop rule 2).
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

In `src/scripting/api.rs`, alongside the other event constants (`PRIVMSG` at `:16`):

```rust
    /// IRCv3 `+typing`: someone started, paused, or stopped typing.
    /// Params: `connection_id`, `nick`, `target`, `state` (`active`/`paused`/`done`).
    pub const TYPING: &str = "irc.typing";
```

Emit it from `src/app/scripting.rs`. Script events dispatch from the `match &msg.command` there (`:686-810`); today every `Command::Raw` — TAGMSG included — falls into the `_ => return false` arm at `:810` and emits nothing. Add an arm before it:

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

`EventResult::Suppress` needs no extra wiring: the dispatcher's verdict lands in `state.suppress_event_display`, which `src/app/irc.rs:930-936` sets around the `handle_irc_message` call, and `handle_tagmsg` already returns early on it (Task 4, drop rule 2). A TAGMSG carries nothing but the typing tag, so suppressing the event suppresses the indicator, which is what a script author would expect.

Update `docs/rfc_ircv3_coverage.md` — in the Tier 2 table, replace the `message-tags` row and add one below it:

```markdown
| `message-tags` | Done | 3.2 | Tags extracted from inbound messages, stored on buffer `Message` and in the DB. Outbound tags via `Message { tags: … }`. `TAGMSG` in/out via `Command::Raw`. `CLIENTTAGDENY` honoured from ISUPPORT |
| `+typing` client tag | Done | — | Ratified client tag on `TAGMSG`. Send + receive, 3s throttle, 6s/30s expiry, status-line indicator in TUI and web UI, `[typing]` config with separate channel/query send switches. Legacy `+draft/typing` accepted on receive |
```

Leave the Tier 3 `reply` row alone — `+reply` is an explicit non-goal.

Add a changelog entry to `README.md` matching the style of the existing `### vX.Y.Z` entries (user-facing bullets; no version bump — that happens at release time).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `make test`
Expected: PASS.

Run: `make clippy`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/scripting/api.rs src/app/scripting.rs src/irc/events.rs docs/rfc_ircv3_coverage.md README.md
git commit -m "feat(scripting): irc.typing event; docs: +typing coverage"
```

---

## Final verification

- [ ] `make clippy` — 0 new warnings across every file touched
- [ ] `make test` — full suite green
- [ ] `make wasm` — web bundle builds; commit `static/web/` if it changed
- [ ] `make release` — release profile builds
- [ ] **Live: Ergo** (permissive `CLIENTTAGDENY`) — two clients in a channel. Typing appears within ~3 s and clears within ~6 s of stopping. Sending a message clears it immediately. `/join #x` in the input produces no TAGMSG (verify on a raw trace, not by eye).
- [ ] **Live: Libera** (allowlisted client tags) — typing works; if the server denies the tag, the indicator silently disappears rather than erroring.
- [ ] **Live: `&channel`** — join a `&`-prefixed channel if the network has one and confirm typing resolves to the channel, not a query (this is the STATUSMSG trap).
- [ ] **Multi-source** — terminal typing in `#a`, browser tab 1 typing in `#b`, browser tab 2 idle on `#a`. All three behave independently; the idle tab never retracts the terminal's typing.
- [ ] **Flood** — send several messages in quick succession while typing continuously. The user's messages must not be delayed; typing notifications simply stop while the budget is tight.
- [ ] **Privacy** — `/set typing.send_channels off`: typing in a channel emits nothing, a query still does. `/set typing.show off`: the indicator disappears from **both** the TUI and any open browser tab, immediately.
- [ ] **Lifecycle** — close a query while the peer is `paused`, reopen it: no stale typer. Reload the browser mid-typing: no stale indicator.
- [ ] Push and let the review bot run

# IRCv3 `draft/multiline` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Full, spec-compliant IRCv3 `draft/multiline` support (inbound reassembly + outbound BATCH framing) working identically in the TUI and the web UI, without altering the existing E2E (RPEE2E) path.

**Architecture:** A multi-line message is one in-memory `Message` whose `text` holds embedded `\n`; it feeds both the TUI renderer and the web broadcast unchanged. The wire always uses BATCH framing (`BATCH +ref draft/multiline target` → `@batch=ref PRIVMSG` lines → `BATCH -ref`) because `IrcCodec::sanitize` truncates a raw `\n` in a PRIVMSG. Pure spec logic lives in a new `src/irc/multiline.rs`; inbound reassembly synthesises one PRIVMSG and replays it through the normal `handle_irc_message` pipeline; outbound framing is a new branch in the shared `handle_plain_message`.

**Tech Stack:** Rust 2024, ratatui 0.30+, crossterm, tokio, `irc-repartee` 1.5.1 / `irc-proto-repartee` 1.2.2 (no fork change needed), Leptos WASM (web-ui).

## Global Constraints

- `make clippy` must pass with **0 new warnings** (pedantic+nursery warn, perf=deny, redundant_clone=deny). Baseline ~44 pre-existing warnings in unrelated files — attribute per-file, do not trust the total.
- `make test` must pass. Any test that could allocate unboundedly (e.g. `partition` on huge input) MUST run under `ulimit -v` + `timeout` (prior OOM lesson).
- All builds go through `make` targets, never raw cargo/trunk.
- Never hardcode the app name; use `APP_NAME`.
- Do NOT modify `irc-repartee` / `irc-proto-repartee` — all work is in this repo.
- **E2E invariant (load-bearing):** the `draft/multiline` BATCH branch runs only for non-E2E plaintext (`!is_e2e_encrypted`). The E2E encrypt/echo/rekey path and `apply_shrink_deliver` must remain byte-for-byte unchanged. E2E multi-line works via the existing E2E path (newlines ride inside the ciphertext) — it only needs the single-echo fix and the `wrap_line` render fix.
- **Hot-path invariant:** a single-line message (no `\n`, ≤ `MESSAGE_MAX_BYTES`) must hit the existing code path unchanged. New code runs only when `text` contains `\n` or is over the byte cap.
- Branch: `feat/draft-multiline`. Commit after every task. Do not push/PR until the whole plan is green (then open a PR to `outrage` so the review bot runs).

## Spec quick-reference (normative, from https://ircv3.net/specs/extensions/multiline)

- Cap value keys: `max-bytes` (REQUIRED; total combined payload of one batch = sum of line contents + 1 byte per joining `\n`), `max-lines` (RECOMMENDED; PRIVMSG count per batch).
- Framing: `BATCH +<ref> draft/multiline <target>` then `@batch=<ref> PRIVMSG <target> :<line>` per line then `BATCH -<ref>`.
- `draft/multiline-concat` tag on a line ⇒ join to previous line with NO separator (continuation of one logical line split for the per-PRIVMSG byte limit). Absent ⇒ join with `\n`.
- All lines target the batch target. No `concat` on a blank line; never an all-blank message; never mix PRIVMSG+NOTICE.
- Server FAIL codes: `FAIL BATCH MULTILINE_MAX_BYTES|MULTILINE_MAX_LINES|MULTILINE_INVALID_TARGET|MULTILINE_INVALID`.
- Per-PRIVMSG content stays under the IRC line limit; reuse existing `MESSAGE_MAX_BYTES = 350`.

## File map

| File | Responsibility | Change |
|---|---|---|
| `src/irc/multiline.rs` | **NEW** pure spec logic: `MultilineLimits`, `parse_limits`, `WireLine`, `partition`, `reassemble`, `needs_multiline` | create |
| `src/irc/mod.rs` | module decl, constants, `split_irc_message` | add `pub mod multiline;`, add `MULTILINE_DEFAULT_MAX_LINES`, thread limits through `IrcEvent::Connected` + `negotiate_caps` |
| `src/irc/cap.rs` | desired caps | add `"draft/multiline"` + test fix |
| `src/state/connection.rs` | `Connection` struct | add `multiline: Option<MultilineLimits>`, `batch_ref_counter: u64`, `impl Connection::next_batch_ref` |
| `src/irc/events.rs` | cap-notify handlers, FAIL dispatch, inbound pipeline | runtime cap value capture, `FAIL BATCH MULTILINE_*` arm |
| `src/irc/batch.rs` | batch tracking + completion | `BatchInfo.opener_tags`, `start_batch` arg, `"DRAFT/MULTILINE"` arm (reassemble → synthetic PRIVMSG → `handle_irc_message`) |
| `src/app/irc.rs` | event loop wiring | pass opener tags to `start_batch`; set `conn.multiline` on `Connected`; init new fields |
| `src/app/input.rs` | shared outbound `handle_plain_message`, `handle_paste`, TUI keys | multiline send branch (cases A/B/C), paste coalescing, Alt+Enter arm |
| `src/ui/mod.rs` | `wrap_line`, terminal setup | hard-`\n` support; (Phase 6) keyboard enhancement flags |
| `src/ui/input.rs` | `InputState`, input render | `insert_newline`, multi-line render |
| `src/ui/layout.rs` | layout | dynamic input height |
| `src/ui/chat_view.rs` | render budget/scroll | verify/tune for tall messages |
| `web-ui/src/components/input.rs` | web send | plaintext → single `SendMessage` preserving `\n` |
| construction sites (10) | — | init `multiline: None, batch_ref_counter: 0` |

Construction sites for the two new `Connection` fields (anchor each on `who_token_counter: 0,`): **prod** `src/app/irc.rs:56`, `src/app/mod.rs:972`, `src/app/shell.rs:111`, `src/app/log_browser.rs:131`; **tests** `src/irc/batch.rs:664/912/1670`, `src/irc/events.rs:4434/7333`, `src/state/events.rs:895`.

---

## Phase 0 — Pure core module `src/irc/multiline.rs`

Zero runtime risk; everything here is pure and unit-tested. No `AppState`, no I/O.

### Task 0.1: Create the module skeleton + types

**Files:**
- Create: `src/irc/multiline.rs`
- Modify: `src/irc/mod.rs` (add `pub mod multiline;` near the other `pub mod` lines; add constant after `MESSAGE_MAX_BYTES` at line 192)

**Interfaces — Produces:**
- `pub struct MultilineLimits { pub max_bytes: usize, pub max_lines: usize }` (`#[derive(Debug, Clone, Copy, PartialEq, Eq)]`)
- `pub struct WireLine { pub text: String, pub concat: bool }` (`#[derive(Debug, Clone, PartialEq, Eq)]`)
- `pub const MULTILINE_DEFAULT_MAX_LINES: usize` (in `irc/mod.rs`)

- [ ] **Step 1: Add module decl + constant in `src/irc/mod.rs`.** After `pub const MESSAGE_MAX_BYTES: usize = 350;` (line 192) add:
```rust
/// Conservative fallback for `draft/multiline` `max-lines` when the server
/// advertises the cap without that key (spec marks it RECOMMENDED, not required).
pub const MULTILINE_DEFAULT_MAX_LINES: usize = 24;

/// Runaway backstop for INBOUND reassembly: a reassembled multiline message is
/// truncated to this many lines (well below `batch::MAX_BATCH_MESSAGES`) so a
/// hostile/buggy server cannot materialise a single message whose `wrap_line`
/// output is thousands of visual lines (OOM-class guard; see the v0.8.4 fix).
pub const MULTILINE_MAX_INBOUND_LINES: usize = 100;
```
Add `pub mod multiline;` alongside the existing `pub mod batch;` / `pub mod cap;` declarations.

- [ ] **Step 2: Create `src/irc/multiline.rs` with types only:**
```rust
//! Pure IRCv3 `draft/multiline` logic: cap-value parsing, outbound
//! partitioning, and inbound reassembly. No I/O, no `AppState`.
//!
//! See https://ircv3.net/specs/extensions/multiline.

use crate::irc::{MESSAGE_MAX_BYTES, MULTILINE_DEFAULT_MAX_LINES};

/// Server-advertised per-batch limits from the `draft/multiline` cap value.
///
/// `max_bytes` is the TOTAL combined payload of one batch — the sum of every
/// line's content bytes plus one byte for each joining `\n`. It is NOT a
/// per-line cap; the per-PRIVMSG wire cap is the separate [`MESSAGE_MAX_BYTES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MultilineLimits {
    pub max_bytes: usize,
    pub max_lines: usize,
}

/// One physical PRIVMSG line within a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireLine {
    pub text: String,
    /// `true` ⇒ carries `draft/multiline-concat` (join to previous with no
    /// separator). `false` ⇒ a logical-line boundary (join with `\n`).
    pub concat: bool,
}
```

- [ ] **Step 3: Compile.** Run: `make build` — Expected: PASS (unused warnings on the new types are fine for now; they are used in 0.2+).
- [ ] **Step 4: Commit.** `git add -A && git commit -m "feat(multiline): add irc::multiline module skeleton + limits/wireline types"`

### Task 0.2: `parse_limits`

**Files:** Modify `src/irc/multiline.rs`; Test: inline `#[cfg(test)]` in same file.

**Interfaces — Produces:** `pub fn parse_limits(cap_value: Option<&str>) -> Option<MultilineLimits>`

Semantics: value is comma-separated `key[=value]`. `max-bytes` REQUIRED; if absent or `< MESSAGE_MAX_BYTES` → `None` (lurker: reject servers whose total budget is below one wire line). `max-lines` absent/garbage → `MULTILINE_DEFAULT_MAX_LINES`. Unknown keys ignored.

- [ ] **Step 1: Write failing tests:**
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_limits_full() {
        let l = parse_limits(Some("max-bytes=4096,max-lines=24")).unwrap();
        assert_eq!(l, MultilineLimits { max_bytes: 4096, max_lines: 24 });
    }
    #[test]
    fn parse_limits_missing_max_lines_uses_default() {
        let l = parse_limits(Some("max-bytes=8192")).unwrap();
        assert_eq!(l.max_bytes, 8192);
        assert_eq!(l.max_lines, MULTILINE_DEFAULT_MAX_LINES);
    }
    #[test]
    fn parse_limits_rejects_small_or_absent_max_bytes() {
        assert!(parse_limits(Some("max-lines=10")).is_none());
        assert!(parse_limits(Some("max-bytes=100,max-lines=10")).is_none()); // < 350
        assert!(parse_limits(Some("")).is_none());
        assert!(parse_limits(None).is_none());
    }
    #[test]
    fn parse_limits_ignores_unknown_and_garbage() {
        let l = parse_limits(Some("max-bytes=4096,foo=bar,max-lines=zzz")).unwrap();
        assert_eq!(l.max_bytes, 4096);
        assert_eq!(l.max_lines, MULTILINE_DEFAULT_MAX_LINES);
    }
}
```

- [ ] **Step 2: Run, verify fail.** `make test 2>&1 | rg multiline` — Expected: FAIL (`parse_limits` not found).
- [ ] **Step 3: Implement:**
```rust
/// Parse the `draft/multiline` cap value into limits. `None` ⇒ unusable
/// (no `max-bytes`, or `max-bytes` below one full wire line).
#[must_use]
pub fn parse_limits(cap_value: Option<&str>) -> Option<MultilineLimits> {
    let value = cap_value?;
    let mut max_bytes: Option<usize> = None;
    let mut max_lines: Option<usize> = None;
    for token in value.split(',') {
        let (k, v) = token.split_once('=')?;
        match k {
            "max-bytes" => max_bytes = v.parse().ok(),
            "max-lines" => max_lines = v.parse().ok(),
            _ => {}
        }
    }
    let max_bytes = max_bytes.filter(|&b| b >= MESSAGE_MAX_BYTES)?;
    Some(MultilineLimits {
        max_bytes,
        max_lines: max_lines.filter(|&n| n > 0).unwrap_or(MULTILINE_DEFAULT_MAX_LINES),
    })
}
```
> Note: `token.split_once('=')?` returns `None` for a tokenless garbage value like `foo` (no `=`); that aborts the whole parse. The test `parse_limits_ignores_unknown_and_garbage` uses `foo=bar` (has `=`) so it passes. If a real server can send a bare flag token, change `?` to `else { continue }`. Decide and encode one behavior; the tests above assume the `?` form is acceptable because every real key is `k=v`.
> Note (intentional lurker deviation): lurker defaults `max-bytes` to 4096 when the cap value is empty; we return `None` instead because the spec marks `max-bytes` REQUIRED. Real servers always advertise it, so impact is nil. Add a code comment so a future reviewer does not "fix" this back to lurker's lenient default.

- [ ] **Step 4: Run, verify pass.** `make test 2>&1 | rg multiline` — Expected: PASS.
- [ ] **Step 5: Commit.** `git add -A && git commit -m "feat(multiline): parse_limits with conservative defaults"`

### Task 0.3: `needs_multiline` + `reassemble`

**Interfaces — Produces:**
- `pub fn needs_multiline(text: &str) -> bool` — `text.contains('\n') || text.len() > MESSAGE_MAX_BYTES`
- `pub fn reassemble(lines: &[WireLine]) -> String` — join with `\n` unless a line has `concat` (then no separator). First line never prefixes a separator.

- [ ] **Step 1: Write failing tests:**
```rust
#[test]
fn needs_multiline_triggers() {
    assert!(needs_multiline("a\nb"));
    assert!(needs_multiline(&"x".repeat(MESSAGE_MAX_BYTES + 1)));
    assert!(!needs_multiline("short single line"));
}
#[test]
fn reassemble_newline_and_concat() {
    let lines = vec![
        WireLine { text: "hello".into(), concat: false },
        WireLine { text: "world".into(), concat: false },
        WireLine { text: "!!!".into(),   concat: true  },
    ];
    // line1 \n line2 (concat)line3  => "hello\nworld!!!"
    assert_eq!(reassemble(&lines), "hello\nworld!!!");
}
#[test]
fn reassemble_single() {
    assert_eq!(reassemble(&[WireLine { text: "x".into(), concat: false }]), "x");
}
```
- [ ] **Step 2: Run, verify fail.** `make test 2>&1 | rg multiline` — Expected: FAIL.
- [ ] **Step 3: Implement:**
```rust
#[must_use]
pub fn needs_multiline(text: &str) -> bool {
    text.contains('\n') || text.len() > MESSAGE_MAX_BYTES
}

#[must_use]
pub fn reassemble(lines: &[WireLine]) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 && !line.concat {
            out.push('\n');
        }
        out.push_str(&line.text);
    }
    out
}
```
- [ ] **Step 4: Run, verify pass.** Expected: PASS.
- [ ] **Step 5: Commit.** `git commit -am "feat(multiline): needs_multiline + reassemble"`

### Task 0.4: `partition`

**Interfaces — Produces:** `pub fn partition(text: &str, limits: &MultilineLimits) -> Option<Vec<Vec<WireLine>>>`

Algorithm:
1. Split `text` on `\n` into logical lines (preserve interior blank lines).
2. For each logical line: if byte-len ≤ `MESSAGE_MAX_BYTES`, one `WireLine{concat:false}`. Else `split_irc_message(line, MESSAGE_MAX_BYTES)` → first piece `concat:false`, rest `concat:true`. (Never put `concat` on a blank line — a blank line is ≤ cap so it is a single non-concat line.)
3. Pack logical lines into batches. A logical line's pieces stay together in one batch. Open a new batch when adding this logical line's pieces would exceed `max_lines` (count) or `max_bytes` (cumulative content bytes + 1 per joining `\n` across the batch so far).
4. If a single logical line's pieces alone exceed `max_lines` or `max_bytes` → return `None` (cannot be represented as multiline; caller falls back to legacy per-line send).
5. All-blank input (every logical line empty) → `None` (spec forbids all-blank message).

- [ ] **Step 1: Write failing tests** (bounded inputs only):
```rust
fn lim(b: usize, l: usize) -> MultilineLimits { MultilineLimits { max_bytes: b, max_lines: l } }

#[test]
fn partition_simple_three_lines() {
    let b = partition("a\nb\nc", &lim(4096, 24)).unwrap();
    assert_eq!(b.len(), 1);
    assert_eq!(b[0], vec![
        WireLine{text:"a".into(),concat:false},
        WireLine{text:"b".into(),concat:false},
        WireLine{text:"c".into(),concat:false},
    ]);
}
#[test]
fn partition_long_line_gets_concat() {
    let long = "x".repeat(MESSAGE_MAX_BYTES + 10);
    let b = partition(&long, &lim(4096, 24)).unwrap();
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].len(), 2);
    assert!(!b[0][0].concat);
    assert!(b[0][1].concat);
}
#[test]
fn partition_overflows_max_lines_into_multiple_batches() {
    let text = (0..5).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
    let b = partition(&text, &lim(4096, 2)).unwrap();
    assert_eq!(b.len(), 3); // 2 + 2 + 1
    assert_eq!(b[0].len(), 2);
    assert_eq!(b[2].len(), 1);
}
#[test]
fn partition_roundtrip_lossless() {
    // joining each batch's reassemble() with '\n' reconstructs the original
    let text = "alpha\nbeta gamma\n\ndelta";
    let b = partition(text, &lim(4096, 24)).unwrap();
    let rejoined = b.iter().map(|batch| reassemble(batch)).collect::<Vec<_>>().join("\n");
    assert_eq!(rejoined, text);
}
#[test]
fn partition_roundtrip_exercises_concat_split() {
    // A logical line LONGER than MESSAGE_MAX_BYTES with embedded spaces forces
    // word-boundary concat splitting; this proves reassemble(partition(..)) is
    // lossless against split_irc_message's whitespace trimming.
    let long_line = "word ".repeat(120); // ~600 bytes, > 350, many spaces
    let text = format!("intro\n{long_line}\noutro");
    let b = partition(&text, &lim(8192, 24)).unwrap();
    let rejoined = b.iter().map(|batch| reassemble(batch)).collect::<Vec<_>>().join("\n");
    assert_eq!(rejoined, text);
    // and the long line must have produced at least one concat continuation
    assert!(b.iter().flatten().any(|w| w.concat));
}
#[test]
fn partition_unrepresentable_single_line_returns_none() {
    // one logical line whose concat pieces exceed max_bytes
    let long = "y".repeat(MESSAGE_MAX_BYTES * 5);
    assert!(partition(&long, &lim(MESSAGE_MAX_BYTES + 1, 24)).is_none());
}
#[test]
fn partition_all_blank_returns_none() {
    assert!(partition("\n\n", &lim(4096, 24)).is_none());
}
```

- [ ] **Step 2: Run, verify fail.** Expected: FAIL.
- [ ] **Step 3: Implement.** Over-long logical lines are split with a **byte-exact** char-boundary splitter (NOT `split_irc_message`, which trims whitespace at word boundaries and would make concat-rejoin lossy). `split_lossless` guarantees `pieces.concat() == line`:
```rust
/// Split `line` at UTF-8 char boundaries so the pieces concatenated with NO
/// separator reproduce `line` exactly — required for `draft/multiline-concat`,
/// where the receiver rejoins continuation pieces seamlessly.
fn split_lossless(line: &str, max_bytes: usize) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut cur = String::new();
    for ch in line.chars() {
        if !cur.is_empty() && cur.len() + ch.len_utf8() > max_bytes {
            pieces.push(std::mem::take(&mut cur));
        }
        cur.push(ch);
    }
    if !cur.is_empty() {
        pieces.push(cur);
    }
    pieces
}

/// Partition `text` into one or more batches. `None` ⇒ cannot be represented
/// as multiline (caller must fall back to legacy per-line sending).
#[must_use]
pub fn partition(text: &str, limits: &MultilineLimits) -> Option<Vec<Vec<WireLine>>> {
    // 1+2: logical lines -> pieces (concat on continuations of an over-long line)
    let logical: Vec<Vec<WireLine>> = text
        .split('\n')
        .map(|line| {
            if line.len() <= MESSAGE_MAX_BYTES {
                vec![WireLine { text: line.to_string(), concat: false }]
            } else {
                split_lossless(line, MESSAGE_MAX_BYTES)
                    .into_iter()
                    .enumerate()
                    .map(|(i, piece)| WireLine { text: piece, concat: i > 0 })
                    .collect()
            }
        })
        .collect();

    if logical.iter().all(|pieces| pieces.iter().all(|w| w.text.is_empty())) {
        return None; // all-blank message
    }

    let mut batches: Vec<Vec<WireLine>> = Vec::new();
    let mut cur: Vec<WireLine> = Vec::new();
    let mut cur_bytes = 0usize;

    let bytes_of = |pieces: &[WireLine]| -> usize {
        pieces.iter().map(|w| w.text.len()).sum::<usize>()
    };

    for pieces in logical {
        let add_lines = pieces.len();
        let add_bytes = bytes_of(&pieces);
        // a single logical line that can never fit one batch -> unrepresentable
        if add_lines > limits.max_lines || add_bytes > limits.max_bytes {
            return None;
        }
        // +1 joining '\n' if this is not the first line in the batch
        let join_byte = usize::from(!cur.is_empty());
        let would_lines = cur.len() + add_lines;
        let would_bytes = cur_bytes + join_byte + add_bytes;
        if !cur.is_empty() && (would_lines > limits.max_lines || would_bytes > limits.max_bytes) {
            batches.push(std::mem::take(&mut cur));
            cur_bytes = 0;
        }
        let join_byte = usize::from(!cur.is_empty());
        cur_bytes += join_byte + add_bytes;
        cur.extend(pieces);
    }
    if !cur.is_empty() {
        batches.push(cur);
    }
    Some(batches)
}
```
> Note the deliberate per-line cap: even the FIRST logical line in a batch is rejected (`None`) if its own pieces exceed `max_lines`/`max_bytes`, which is the unrepresentable case.

- [ ] **Step 4: Run, verify pass.** Expected: PASS. Run the partition tests under a memory guard:
`( ulimit -v 2000000; timeout 60 cargo test -p repartee multiline:: )` — Expected: PASS, no OOM.
- [ ] **Step 5: `make clippy`** — Expected: 0 new warnings in `multiline.rs`.
- [ ] **Step 6: Commit.** `git commit -am "feat(multiline): partition with byte/line limits + concat + roundtrip tests"`

---

## Phase 1 — Connection state + capability negotiation

### Task 1.1: Add fields to `Connection` + `next_batch_ref`

**Files:** Modify `src/state/connection.rs`.

**Interfaces — Produces:**
- `Connection.multiline: Option<crate::irc::multiline::MultilineLimits>`
- `Connection.batch_ref_counter: u64`
- `impl Connection { pub fn next_batch_ref(&mut self) -> String }`

- [ ] **Step 1:** Add fields after line 61 (`silent_banlist_channels`), before the closing `}`:
```rust
    /// Negotiated `draft/multiline` limits (None ⇒ cap not active).
    pub multiline: Option<crate::irc::multiline::MultilineLimits>,
    /// Monotonic counter for outbound multiline BATCH reference tags.
    pub batch_ref_counter: u64,
```
- [ ] **Step 2:** Add (first-ever) `impl Connection` block below the struct:
```rust
impl Connection {
    /// Allocate a unique outbound BATCH reference tag for this connection.
    pub fn next_batch_ref(&mut self) -> String {
        self.batch_ref_counter = self.batch_ref_counter.wrapping_add(1);
        format!("ml{}", self.batch_ref_counter)
    }
}
```
- [ ] **Step 3: Build — expect failures at all 10 construction sites.** `make build 2>&1 | rg "missing.*field|connection.rs"` — Expected: FAIL listing each site missing `multiline`/`batch_ref_counter`.
- [ ] **Step 4:** At each of the 10 sites, add `multiline: None,` and `batch_ref_counter: 0,` next to `who_token_counter: 0,`: prod `src/app/irc.rs:56`, `src/app/mod.rs:972`, `src/app/shell.rs:111`, `src/app/log_browser.rs:131`; tests `src/irc/batch.rs:664/912/1670`, `src/irc/events.rs:4434/7333`, `src/state/events.rs:895`.
- [ ] **Step 5: Build.** `make build` — Expected: PASS.
- [ ] **Step 6: Commit.** `git commit -am "feat(multiline): Connection.multiline limits + next_batch_ref + all ctor sites"`

### Task 1.2: Request the cap + capture limits during initial negotiation

**Files:** Modify `src/irc/cap.rs`, `src/irc/mod.rs`, `src/app/irc.rs`.

**Interfaces:**
- Consumes: `multiline::parse_limits`, `ServerCaps::value`.
- Produces: `IrcEvent::Connected(String, HashSet<String>, Option<MultilineLimits>)`; `conn.multiline` set on connect.

- [ ] **Step 1:** In `src/irc/cap.rs` add `"draft/multiline",` to `DESIRED_CAPS` (after `"message-tags"` line 17). Update the test `negotiate_with_full_desired_list` (cap.rs:197-202) advertised string to include `draft/multiline`. Add a test:
```rust
#[test]
fn server_caps_retains_multiline_value() {
    let caps = ServerCaps::parse("draft/multiline=max-bytes=4096,max-lines=24 batch");
    assert_eq!(caps.value("draft/multiline"), Some("max-bytes=4096,max-lines=24"));
}
```
- [ ] **Step 2: Run, verify cap tests pass.** `make test 2>&1 | rg "cap::"` — Expected: PASS.
- [ ] **Step 3:** In `src/irc/mod.rs`: add `multiline_limits: Option<crate::irc::multiline::MultilineLimits>` to `NegotiateResult` (line 47-55). Extend `IrcEvent::Connected` (line 31) to `Connected(String, HashSet<String>, Option<crate::irc::multiline::MultilineLimits>)`. In `negotiate_caps`, after the ACK loop confirms enabled caps, set:
```rust
let multiline_limits = if enabled_caps.contains("draft/multiline") {
    crate::irc::multiline::parse_limits(server_caps.value("draft/multiline"))
} else {
    None
};
```
and add `multiline_limits` to the `Ok(NegotiateResult { ... })` at 704-708. Update BOTH `IrcEvent::Connected(...)` emit sites (mod.rs:428, 449) to pass `neg.multiline_limits` (it's `Copy`, just include it).
- [ ] **Step 4:** In `src/app/irc.rs`: update the `IrcEvent::Connected` match arm (313-317) to destructure the third field and set it:
```rust
IrcEvent::Connected(conn_id, enabled_caps, multiline_limits) => {
    if let Some(conn) = self.state.connections.get_mut(&conn_id) {
        conn.enabled_caps = enabled_caps;
        conn.multiline = multiline_limits;
    }
```
- [ ] **Step 5: Build + test.** `make build && make test 2>&1 | rg -i "cap|negotiat|connected"` — Expected: PASS.
- [ ] **Step 6: Commit.** `git commit -am "feat(multiline): request cap + capture max-bytes/max-lines on connect"`

### Task 1.3: Runtime cap-notify value capture (CAP NEW/DEL/NAK)

**Files:** Modify `src/irc/events.rs` (`handle_cap_new`, `handle_cap_del`, `handle_cap_nak`).

- [ ] **Step 1: Write a test** (in events.rs test module) that a `CAP NEW draft/multiline=...` followed by ACK sets `conn.multiline`, and `CAP DEL draft/multiline` clears it. Use the existing test-connection helpers.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3:** In `handle_cap_new`, capture the raw `draft/multiline` token's value from `caps_str` into a local `Option<MultilineLimits>` (parse it with `parse_limits`) BEFORE the immutable `enabled = state.connections.get(conn_id)...` borrow at 483. Then, AFTER the `to_request` `.collect()` at line 492 (so the immutable `enabled` borrow has ended), do `state.connections.get_mut(conn_id)` and store it onto `conn.multiline` only if `draft/multiline` is in `to_request`/acked. **Borrow ordering matters** — storing via `get_mut` while `enabled` (an immutable `&` of the same connection) is still alive is a borrow-check error; sequence the `get_mut` store strictly after the collect. In `handle_cap_del`, when `draft/multiline` is removed, set `conn.multiline = None`. In `handle_cap_nak`, if `draft/multiline` was NAKed, set `conn.multiline = None`. Note: gate the OUTBOUND branch on the full `multiline_supported` predicate (Task 4.1) so an advertise-time-but-not-yet-ACK'd window cannot emit BATCH frames.
- [ ] **Step 4: Run, verify pass; `make clippy`.**
- [ ] **Step 5: Commit.** `git commit -am "feat(multiline): capture/clear limits on runtime CAP NEW/DEL/NAK"`

---

## Phase 2 — Inbound reassembly

### Task 2.1: Preserve BATCH opener tags

**Files:** Modify `src/irc/batch.rs` (`BatchInfo`, `start_batch`), `src/app/irc.rs` (interception call).

**Interfaces — Produces:** `BatchInfo.opener_tags: Option<Vec<irc::proto::message::Tag>>`; `start_batch(ref_tag, batch_type, params, opener_tags)`.

- [ ] **Step 1:** Add `pub opener_tags: Option<Vec<irc::proto::message::Tag>>` to `BatchInfo` (after `params`, line 34). Update `start_batch` (line 51) to take `opener_tags: Option<Vec<irc::proto::message::Tag>>` and store it; update the `BatchInfo { ... }` literal at 54-60. Update the existing test call sites of `start_batch` in batch.rs to pass `None`.
- [ ] **Step 2:** In `src/app/irc.rs` interception (line 583) pass `msg.tags.clone()`:
```rust
tracker.start_batch(tag, &batch_type, batch_params, msg.tags.clone());
```
(`msg` is the `Box<irc::proto::Message>` BATCH command; `msg.tags` is `Option<Vec<Tag>>`.)
- [ ] **Step 3: Build + existing batch tests.** `make build && make test 2>&1 | rg "batch::"` — Expected: PASS.
- [ ] **Step 4: Commit.** `git commit -am "feat(multiline): preserve BATCH opener tags for @time/@msgid"`

### Task 2.2: `DRAFT/MULTILINE` reassembly arm

**Files:** Modify `src/irc/batch.rs` (`process_completed_batch`).

**Interfaces — Consumes:** `multiline::reassemble`, `handle_irc_message`. **Produces:** new match arm returning `None`.

Approach: build `WireLine`s from `batch.messages` (text = PRIVMSG last param; `concat` = presence of a `draft/multiline-concat` tag on that fragment), `reassemble`, then synthesise one `irc::proto::Message { tags: opener_tags (fallback first fragment tags), prefix: first fragment prefix, command: PRIVMSG(target, joined) }` and call `crate::irc::events::handle_irc_message(state, conn_id, &synthetic)`. Target = `batch.params.first()` (the batch target), else first fragment's PRIVMSG target.

- [ ] **Step 1: Write tests** in batch.rs: feed a `BatchInfo` of type `"DRAFT/MULTILINE"` with 3 PRIVMSG fragments (one with `draft/multiline-concat`) and assert the buffer ends with ONE message whose `text` equals the reassembled string; assert NOTICE/PRIVMSG fragments map correctly; assert an empty fragment set adds no message. Use the existing test harness pattern (build `AppState`, a `Connection`, call `process_completed_batch`). ALSO add: (a) an **E2E-fragment test** — a batch whose fragments are `+RPE2E01…` chunks must produce the SAME per-fragment messages as the legacy default arm (i.e. fall through to per-fragment replay, NOT one joined line); (b) a **runaway test** (under `ulimit -v` + `timeout`) — a batch with > `MULTILINE_MAX_INBOUND_LINES` fragments yields a message capped at `MULTILINE_MAX_INBOUND_LINES` lines; (c) a **tag-merge test** — `@msgid` on the first fragment (not the opener) survives into the reassembled message's `tags`.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** the arm (insert after the CHATHISTORY arm, before `_ =>` at line 308):
```rust
"DRAFT/MULTILINE" => {
    if batch.messages.is_empty() {
        return None;
    }
    // E2E guard: if ANY fragment is an RPEE2E ciphertext chunk, do NOT
    // reassemble — joining multiple +RPE2E01 wires into one string corrupts
    // decrypt. Replay each fragment through the normal path so every chunk
    // decrypts independently (matches the legacy default arm). E2E and
    // multiline are mutually exclusive in practice; this is defensive.
    if batch.messages.iter().any(|m| matches!(&m.command,
        Command::PRIVMSG(_, t) | Command::NOTICE(_, t) if t.starts_with("+RPE2E01")))
    {
        for m in &batch.messages {
            crate::irc::events::handle_irc_message(state, conn_id, m);
        }
        return None;
    }
    let mut lines: Vec<crate::irc::multiline::WireLine> = batch
        .messages
        .iter()
        .filter_map(|m| match &m.command {
            Command::PRIVMSG(_, text) | Command::NOTICE(_, text) => {
                let concat = m.tags.as_ref().is_some_and(|tags| {
                    tags.iter().any(|t| t.0 == "draft/multiline-concat")
                });
                Some(crate::irc::multiline::WireLine { text: text.clone(), concat })
            }
            _ => None,
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    // Runaway backstop (OOM guard): bound the reassembled message's line count.
    if lines.len() > crate::irc::MULTILINE_MAX_INBOUND_LINES {
        tracing::warn!(
            "multiline batch had {} lines; truncating to {}",
            lines.len(),
            crate::irc::MULTILINE_MAX_INBOUND_LINES
        );
        lines.truncate(crate::irc::MULTILINE_MAX_INBOUND_LINES);
    }
    let joined = crate::irc::multiline::reassemble(&lines);

    // target: batch opener param, else first fragment's PRIVMSG/NOTICE target
    let target = batch
        .params
        .first()
        .cloned()
        .or_else(|| batch.messages.iter().find_map(|m| match &m.command {
            Command::PRIVMSG(t, _) | Command::NOTICE(t, _) => Some(t.clone()),
            _ => None,
        }));
    let Some(target) = target else { return None; };

    // determine PRIVMSG vs NOTICE from the first usable fragment
    let is_notice = matches!(
        batch.messages.iter().find(|m| matches!(&m.command, Command::PRIVMSG(..) | Command::NOTICE(..))).map(|m| &m.command),
        Some(Command::NOTICE(..))
    );
    let command = if is_notice {
        Command::NOTICE(target.clone(), joined)
    } else {
        Command::PRIVMSG(target, joined)
    };

    // metadata: @time/@msgid normally ride on the BATCH opener, but a server may
    // place @msgid on the first fragment. MERGE: start from opener tags, then
    // fill any key missing from the opener using the first fragment's tags.
    // Prefix from the first fragment (the sender).
    let mut tags: Vec<irc::proto::message::Tag> = batch.opener_tags.clone().unwrap_or_default();
    if let Some(frag_tags) = batch.messages.first().and_then(|m| m.tags.clone()) {
        for ft in frag_tags {
            if !tags.iter().any(|t| t.0 == ft.0) {
                tags.push(ft);
            }
        }
    }
    let tags = if tags.is_empty() { None } else { Some(tags) };
    let prefix = batch.messages.first().and_then(|m| m.prefix.clone());

    let synthetic = IrcMessage { tags, prefix, command };
    crate::irc::events::handle_irc_message(state, conn_id, &synthetic);
    None
}
```
(`IrcMessage` is already aliased at batch.rs:14 `use irc::proto::{Command, Message as IrcMessage};`.)
- [ ] **Step 4: Run, verify pass.** Run under memory guard.
- [ ] **Step 5: `make clippy`.**
- [ ] **Step 6: Commit.** `git commit -am "feat(multiline): inbound DRAFT/MULTILINE reassembly via synthetic PRIVMSG"`

> Robustness notes baked in: fragments are already capped by `MAX_BATCH_MESSAGES` (batch.rs add_message), bounding per-message line count. Routing through `handle_irc_message` reuses highlight/mention/activity/log/web-broadcast/E2E-decrypt exactly once. echo-message: our own echoed batch reassembles into one displayed message (the single sender echo).

---

## Phase 3 — TUI renders embedded `\n`

### Task 3.1: `wrap_line` honours hard newlines

**Files:** Modify `src/ui/mod.rs` (`wrap_line` 170-264).

**Interfaces:** signature unchanged: `pub fn wrap_line(line: Line<'static>, width: usize, indent: usize) -> Vec<Line<'static>>`.

Approach: split the flattened `styled_chars` stream on the `\n` grapheme into segments; run the existing wrap body per segment; concatenate; emit `Line::default()` for an empty segment; drop the `\n` grapheme (never push it into a span). Treat each segment as a fresh `first_line` (post-`\n` segment is a new logical line, not an indented continuation). Keep `width==0` guard and the per-segment forward-progress (OOM) guard.

- [ ] **Step 1: Write failing tests** (new `#[cfg(test)]` cases near existing ui tests):
```rust
#[test]
fn wrap_line_hard_newline_splits() {
    let l = Line::from("alpha\nbeta");
    let out = wrap_line(l, 80, 0);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].to_string(), "alpha");
    assert_eq!(out[1].to_string(), "beta");
}
#[test]
fn wrap_line_blank_segment_is_blank_line() {
    let out = wrap_line(Line::from("a\n\nb"), 80, 0);
    assert_eq!(out.len(), 3);
    assert_eq!(out[1].to_string(), "");
}
#[test]
fn wrap_line_newline_then_wrap_within_segment() {
    let seg = "w ".repeat(60); // forces soft-wrap within the 2nd segment
    let out = wrap_line(Line::from(format!("short\n{seg}")), 20, 2);
    assert!(out.len() >= 3);
    assert_eq!(out[0].to_string(), "short");
}
#[test]
fn wrap_line_single_line_unchanged() {
    let out = wrap_line(Line::from("no newlines here"), 80, 0);
    assert_eq!(out.len(), 1);
}
```
- [ ] **Step 2: Run, verify fail.** `make test 2>&1 | rg wrap_line` — Expected: FAIL (current code returns 1 line for "alpha\nbeta").
- [ ] **Step 3: Implement.** Refactor: extract the current 195-263 wrap body into a private `fn wrap_segment(styled_chars: &[(String, usize, Style)], width: usize, indent: usize) -> Vec<Line<'static>>`. In `wrap_line`, build `styled_chars` as today (180-188), then split it into segments at `(g == "\n")` tuples (dropping the `\n` tuple), and for each segment call `wrap_segment`; an empty segment pushes `Line::default()`. Preserve the `width==0` early return. Remove the old `total_width <= width` early return OR guard it so it only short-circuits when there is no `\n` in `styled_chars` (otherwise a short "a\nb" would wrongly return one line).
  > **Compile gotcha:** the original fallback at lines 259-261 is `if result.is_empty() { result.push(line); }`, which references the original `line: Line<'static>` binding. That binding does NOT exist inside `wrap_segment` (its param is a `styled_chars` slice). Replace the fallback inside `wrap_segment` with `result.push(crate::ui::build_line_from_styled_chars(styled_chars, false, indent))` (or `Line::default()` defensively, since `wrap_segment` is only called on non-empty segments and the body always yields ≥1 line). Do NOT copy `result.push(line)` verbatim — it will fail to compile.
- [ ] **Step 4: Run, verify pass** (incl. existing wrap tests for soft-wrap, emoji widths, placeholder runs). Run under memory guard.
- [ ] **Step 5: `make clippy`.**
- [ ] **Step 6: Commit.** `git commit -am "feat(multiline): wrap_line treats \\n as a hard line break"`

### Task 3.2: Reconcile render budget + emote layout with tall messages

**Files:** `src/ui/chat_view.rs` (reason about / adjust the budget), tests.

The render budget (`compute_render_budget` = `buffer_len * MAX_WRAPPED_LINES_PER_MSG`, `MAX_WRAPPED_LINES_PER_MSG = 16`) was built on "one Message wraps to ≤16 visual lines" (v0.8.4 OOM guard). A multiline message breaks that assumption. The memory bound is preserved ONLY because a single message's line count is now hard-capped: **inbound** ≤ `MULTILINE_MAX_INBOUND_LINES` (=100, Task 2.2), **outbound echo** ≤ the typed/pasted line count which is bounded by `max-lines` framing and `MAX_PASTE_LINES`. The render loop pushes one message's wrapped lines fully, THEN checks `visual_lines.len() > needed`, so peak memory is `needed + (one message's wrapped lines)` — bounded because the per-message line count is bounded.

- [ ] **Step 1: Decide the budget treatment.** Keep `MAX_WRAPPED_LINES_PER_MSG = 16` (it sizes `needed` for walk-back; correctness of scroll uses the actual `total = visual_lines.len()`, not the cap). Add a doc-comment at `chat_view.rs:13` noting that a single multiline message can exceed 16 visual lines but is hard-bounded by `MULTILINE_MAX_INBOUND_LINES` (inbound) / paste+max-lines (outbound), so the O(buffer_len) + one-message overshoot bound still holds. (Do NOT remove the cap.)
- [ ] **Step 2: Add a render test** that a buffer containing one multi-line `Message` (e.g. 5 `\n`s) produces the expected `visual_lines` count and that `scroll`/`skip` math (chat_view.rs:135-138) stays consistent (no panic, correct take/skip) — and a **bounded-overshoot test** (under `ulimit -v` + `timeout`): one message of `MULTILINE_MAX_INBOUND_LINES` lines renders without OOM and scroll math is correct. Mirror the existing chat_view tests (172-244).
- [ ] **Step 3: Run.** If the v0.8.4 OOM-cap tests assert exact products of `MAX_WRAPPED_LINES_PER_MSG`, confirm they still pass; if one breaks, adjust the TEST expectation (not the cap) and document why memory stays bounded.
- [ ] **Step 4: `make clippy` + commit.** `git commit -am "feat(multiline): reconcile chat_view render budget for tall messages + tests"`

---

## Phase 4 — Outbound BATCH framing (shared TUI + web)

### Task 4.1: Multiline send branch in `handle_plain_message`

**Files:** Modify `src/app/input.rs` (`handle_plain_message` 1207-1462).

**Interfaces — Consumes:** `multiline::{needs_multiline, partition, WireLine}`, `conn.multiline`, `conn.next_batch_ref`, `Sender::send`. **Produces:** wire BATCH frames + a single local echo.

**Step 0 — shrink gate (load-bearing, do first).** Change the shrink gate at `input.rs:1300-1304` to also add `&& !crate::irc::multiline::needs_multiline(text)`. Without it, a SHORT (≤350B) multi-line message that ALSO contains a shrinkable URL passes the existing `text.len() <= MESSAGE_MAX_BYTES` check, enters the async `apply_shrink_deliver` path (NO multiline branch), and is sent as one PRIVMSG truncated at the first `\n` by `IrcCodec::sanitize` — silent data loss. RED test: shrink enabled+outgoing → a short `"a http://<long-url> \n b"` must NOT enqueue to `shrink_outgoing_tx`. (`send_outgoing_substituted` then provably never sees a multiline message; no change there.)

**New helper.** Add `fn multiline_supported(&self, conn_id: &str) -> bool` = `conn.multiline.is_some() && conn.enabled_caps.contains("batch") && conn.enabled_caps.contains("message-tags")`. Gate Case B on it (not just `conn.multiline.is_some()`).

Structure. Insert the branch at **~line 1410** — AFTER `is_e2e_encrypted` (1405-1407), `echo_message_enabled` (1400-1404), `own_mode` (1409) are in scope (no hoist needed; they precede 1410). The branch replaces the existing `for wire in wire_lines {…}` send loop + echo:
- **Case B (non-E2E, multiline):** `if !is_e2e_encrypted && self.multiline_supported(&conn_id) && needs_multiline(text)` and `partition(text,&limits)` is `Some(batches)`: take ref(s) via `conn.next_batch_ref()` (mut borrow) FIRST, send each batch frame (handle borrow, collect a bool), drop the borrow, then (if `!echo_message_enabled`) add ONE echo `Message` with full `text`; `return`.
- **Case C (non-E2E, has `\n`, no/failed multiline):** `else if !is_e2e_encrypted && text.contains('\n')`: for each logical line in `text.split('\n')` — **PRESERVE interior blank lines** (send an empty PRIVMSG for a blank line; do NOT trim or skip empties) — run the existing single-line send (`send_privmsg`; `split_irc_message` for a >350 line) + echo per chunk. `return`.
- **Else (E2E, OR single line no `\n`, OR partition `None` with no `\n`):** the EXISTING send loop (1411-1419) runs over `wire_lines` unchanged, then the echo block (1431-1461) runs with the single Task 4.2 change (E2E → one echo `Message`).
> **No Case A early-return.** E2E is handled entirely by the existing send loop + the in-place Task 4.2 echo edit. A separate returning E2E branch would risk double-echo. The send loop (1411-1419) and `pending_e2e_sends` drain (1427-1429) stay byte-for-byte.

BATCH send (inside `if let Some(handle) = self.irc_handles.get(&conn_id)`, collecting a `bool` success, dropping the borrow before any `add_local_event`):
```rust
// pseudostructure for one batch:
let send_ok = (|| {
    let h = self.irc_handles.get(&conn_id)?;
    h.sender.send(irc::proto::Command::BATCH(
        format!("+{batch_ref}"),
        Some(irc::proto::command::BatchSubCommand::CUSTOM("draft/multiline".into())),
        Some(vec![buffer_name.clone()]),
    )).ok()?;
    for w in &batch {
        let mut tags = vec![irc::proto::message::Tag("batch".into(), Some(batch_ref.clone()))];
        if w.concat {
            tags.push(irc::proto::message::Tag("draft/multiline-concat".into(), None));
        }
        let msg = irc::proto::Message {
            tags: Some(tags),
            prefix: None,
            command: irc::proto::Command::PRIVMSG(buffer_name.clone(), w.text.clone()),
        };
        h.sender.send(msg).ok()?;
    }
    h.sender.send(irc::proto::Command::BATCH(format!("-{batch_ref}"), None, None)).ok()?;
    Some(())
})().is_some();
```
> Do NOT `use irc::proto::Message;` at top (clashes with `crate::state::buffer::Message` imported at input.rs:9). Fully-qualify `irc::proto::...` inline, or add a local `use irc::proto::Message as IrcMessage;` inside the function.
> `next_batch_ref` needs `&mut conn` → take the ref string(s) first via `self.state.connections.get_mut(&conn_id)`, before the immutable `self.irc_handles` borrow.
> Echo (single): build one `crate::state::buffer::Message { text: text.to_string(), nick: Some(nick.clone()), nick_mode: own_mode.map(|c| c.to_string()), message_type: MessageType::Message, id: self.state.next_message_id(), tags: None, .. }` and `self.state.add_message(&active_id, msg)` ONCE → one web `NewMessage` automatically.

- [ ] **Step 1: Write tests.** Unit-test the partition→frame mapping where feasible, plus an `App`-level test (if a test harness for `handle_plain_message` exists; otherwise test the pure framing helper). Extract the frame-building into a small pure helper `fn multiline_frames(target: &str, batch_ref: &str, batch: &[WireLine]) -> Vec<irc::proto::Message>` in `multiline.rs` and unit-test its `to_string()` output:
```rust
#[test]
fn frames_have_batch_open_tagged_lines_close() {
    // NOTE: this proto only emits the trailing ':' when the last param is empty,
    // contains a space, or starts with ':' (see irc-proto stringify). Use
    // multi-word line text so the ':' appears and is asserted realistically.
    let batch = vec![
        WireLine { text: "hello world".into(), concat: false },
        WireLine { text: "more text".into(),   concat: true  },
    ];
    let msgs = multiline_frames("#chan", "ml1", &batch);
    let wire: Vec<String> = msgs.iter().map(|m| m.to_string()).collect();
    assert_eq!(wire[0], "BATCH +ml1 draft/multiline #chan\r\n");
    assert!(wire[1].starts_with("@batch=ml1 ") && wire[1].contains("PRIVMSG #chan :hello world"));
    assert!(wire[2].contains("draft/multiline-concat") && wire[2].contains("PRIVMSG #chan :more text"));
    assert_eq!(wire[3], "BATCH -ml1\r\n");
}
```
Put `multiline_frames` in `multiline.rs` as `#[must_use] pub fn multiline_frames(target: &str, batch_ref: &str, batch: &[WireLine]) -> Vec<irc::proto::Message>` (clippy `must_use_candidate` fires on pure public fns returning a value). `multiline.rs` must name the proto types — fully-qualify inline (`irc::proto::Message`, `irc::proto::Command::{BATCH,PRIVMSG}`, `irc::proto::message::Tag`, `irc::proto::command::BatchSubCommand::CUSTOM`) exactly as `batch.rs:14` does; do NOT `use irc::proto::Message` unqualified. `handle_plain_message` calls `multiline_frames` and sends each frame via `handle.sender.send(msg)`.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** `multiline_frames` in `multiline.rs` and the branch in `handle_plain_message`.
- [ ] **Step 4: Run, verify pass; `make build`.**
- [ ] **Step 5: `make clippy`** (watch borrow/clone lints; `redundant_clone=deny`).
- [ ] **Step 6: Commit.** `git commit -am "feat(multiline): outbound BATCH framing branch in handle_plain_message (E2E-safe)"`

### Task 4.2: Case C fallback + E2E single-echo (in-place edit)

**Files:** `src/app/input.rs`.

The E2E single-echo is the ONLY edit to the existing echo block (1431-1461) — it is NOT a separate returning branch (avoids double-echo). Replace the body of `if !echo_message_enabled || is_e2e_encrypted { … }`:
```rust
if !echo_message_enabled || is_e2e_encrypted {
    if is_e2e_encrypted {
        // E2E: echo the full plaintext as ONE Message so newlines render as
        // one message locally (matches one logical message the user typed).
        let id = self.state.next_message_id();
        self.state.add_message(&active_id, /* Message { text: plain_echo, nick, own_mode, .. } */);
    } else {
        // unchanged: byte-split echo for the non-multiline single-line path
        let local_chunks = if plain_echo.len() <= crate::irc::MESSAGE_MAX_BYTES {
            vec![plain_echo]
        } else {
            crate::irc::split_irc_message(&plain_echo, crate::irc::MESSAGE_MAX_BYTES)
        };
        for chunk in local_chunks { /* existing add_message per chunk */ }
    }
}
```

- [ ] **Step 1: Tests / reasoning:** non-E2E with `conn.multiline == None` and `text` containing `\n` → each logical line sent separately, NO embedded `\n` on the wire (Case C). E2E message with `\n` echoes as ONE local Message.
- [ ] **Step 2: Implement** the Case C loop (Task 4.1) and this E2E single-echo edit.
- [ ] **Step 3: Build + clippy.**
- [ ] **Step 4: Document the E2E peer asymmetry** (code comment + design §7 note): E2E messages are NOT reassembled on the wire — the E2E chunker splits ciphertext at ≤180B/chunk (`e2e/chunker.rs`) and the receiving peer renders ONE Message per chunk (no E2E reassembly exists; out of scope per non-goals). The single-echo change unifies only the SENDER's local view; the peer still sees N chunk-messages. Newlines inside the plaintext are preserved byte-wise across chunks. This is intentional (E2E reassembly is a separate non-goal).
- [ ] **Step 5: Commit.** `git commit -am "feat(multiline): non-cap \\n fallback + single E2E echo (no double-display)"`

### Task 4.3: TUI paste coalescing

**Files:** Modify `src/app/input.rs` (`handle_paste` 377-460).

Behavior: if `self.multiline_supported(&conn_id)` (active connection) AND no pasted line (trimmed) starts with `/`, coalesce the whole paste (prepend current input) into ONE `handle_submit(joined_with_\n)` call (reaches `handle_plain_message` → Case B). When coalescing, **PRESERVE interior blank lines and do NOT trim individual lines** — join the raw lines with `\n` (only strip a trailing `\r` per line from bracketed paste). `partition`'s all-blank guard rejects a fully-blank paste server-side. Otherwise (multiline not supported, or any `/`-line present) keep the EXISTING per-line `paste_queue` behavior with its empty-line filtering (backward compatible; required when multiline unsupported so no `\n` reaches the wire).

- [ ] **Step 1: Test** the decision: pure-plaintext paste + multiline cap → single submit with `\n`-joined text; paste containing a `/`-line OR no multiline cap → existing queued per-line path.
- [ ] **Step 2-4: Implement; build; clippy.**
- [ ] **Step 5: Commit.** `git commit -am "feat(multiline): coalesce plaintext paste into one batch when supported"`

---

## Phase 5 — Web UI send path

### Task 5.1: Plaintext → single `SendMessage` preserving `\n`

**Files:** Modify `web-ui/src/components/input.rs` (`send_text` closure, 257-299).

Behavior: iterate `text.lines()`, accumulate consecutive PLAINTEXT lines into a buffer; when a line whose TRIMMED form starts with `/` is hit, flush the pending plaintext as ONE `SendMessage { text: pending.join("\n") }` then emit the `RunCommand { text: trimmed }`; flush remaining plaintext at end. **For the plaintext accumulation, push RAW (untrimmed) lines and PRESERVE interior blank lines** — do NOT `trim()` each plaintext line nor `continue` on empty ones (that would diverge from the TUI path, which sends `text` verbatim to `partition`). Only classification uses the trimmed form. Keep the wizard/emoji early-returns and the `active_buffer` guard above the loop unchanged. (No protocol change — `SendMessage.text` already carries `\n`; server `web_send_message → handle_submit → handle_plain_message` runs the shared multiline branch.)

- [ ] **Step 1:** Implement the accumulate/flush closure (the server side is already covered by Phase 4; the web change is purely the client send shape).
- [ ] **Step 2: Build the web UI.** `make wasm` — Expected: PASS.
- [ ] **Step 3: Commit.** `git commit -am "feat(multiline): web sends plaintext as a single \\n-preserving message"`

> Web rendering already handles `\n` via CSS `pre-wrap`; `message_to_wire` passes `text` verbatim. No web render change.

---

## Phase 6 — TUI multi-line compose (Alt+Enter) — highest risk, do last

### Task 6.1: `InputState::insert_newline`

**Files:** Modify `src/ui/input.rs`.

- [ ] **Step 1: Test** that `insert_newline` inserts `\n` at the cursor and advances by 1 (insert_char rejects control chars, so a dedicated method is required).
- [ ] **Step 2-4:** Add:
```rust
pub fn insert_newline(&mut self) {
    self.spell_state = None;
    self.value.insert(self.cursor_pos, '\n');
    self.cursor_pos += 1;
    self.tab_state = None;
}
```
Build; test.
- [ ] **Step 5: Commit.** `git commit -am "feat(multiline): InputState::insert_newline"`

### Task 6.2: Alt+Enter key binding

**Files:** Modify `src/app/input.rs` (`handle_key` match, before the Enter arm at line 271).

- [ ] **Step 1:** Add, before the `(_, KeyCode::Enter | ...)` arm:
```rust
(mods, KeyCode::Enter) if mods.contains(KeyModifiers::ALT) => {
    self.input.spell_state = None;
    self.input.insert_newline();
}
```
- [ ] **Step 2: Build.** (Manual key test deferred to 6.4.)
- [ ] **Step 3: Commit.** `git commit -am "feat(multiline): Alt+Enter inserts a newline in the input"`

### Task 6.3: Multi-line input rendering + dynamic layout

**Files:** Modify `src/ui/input.rs` (`render` 579-666), `src/ui/layout.rs` (70-75, 134-135).

- [ ] **Step 1:** In `layout.rs`: compute `n = app.input.value.matches('\n').count() + 1` clamped to e.g. `1..=6`; change bottom-area constraint (line 73) to `Constraint::Length(2 + n)` and the inner split (135) to `[Constraint::Length(1), Constraint::Length(n)]`. The chat area is `Fill(1)` so it shrinks automatically. Keep `regions.input_area` accurate.
- [ ] **Step 2:** In `input.rs::render`: split `app.input.value` on `\n` into rows; emit a multi-line `Paragraph` (`Vec<Line>`); compute cursor row+col from `cursor_pos`; keep the single-line horizontal-scroll fast path when there is no `\n`.
- [ ] **Step 3: Build; manual smoke** via `make release` + run; type Alt+Enter, verify the input grows and renders multiple rows, Enter submits the whole buffer.
- [ ] **Step 4: Commit.** `git commit -am "feat(multiline): multi-line input rendering + dynamic input height"`

### Task 6.4: Keyboard enhancement flags (reliable Alt+Enter), gated

**Files:** Modify `src/ui/mod.rs` (`setup_terminal`, `restore_terminal`).

> Risk: enabling the Kitty protocol changes key reporting for ALL keys on supporting terminals (ESC handling, `Char('\n'|'\r')` fallback at app/input.rs:271, the ESC-combo logic). Gate on `supports_keyboard_enhancement()` and re-test the existing key arms.

- [ ] **Step 1:** In `setup_terminal`, after enabling other modes:
```rust
if crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false) {
    use crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
    let _ = execute!(stdout, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES));
}
```
and `PopKeyboardEnhancementFlags` in `restore_terminal` (guarded the same way). Leave `setup_socket_terminal` (remote attach) on legacy behavior.
- [ ] **Step 2: Manual matrix test** on a Kitty-protocol terminal AND a legacy one: Enter submits; Alt+Enter inserts newline; Backspace/arrows/Esc/Ctrl-combos and the existing ESC-prefix chords still work; paste still works. Document results in the PR.
- [ ] **Step 3: `make clippy` + commit.** `git commit -am "feat(multiline): enable disambiguate-escape-codes where supported for Alt+Enter"`

---

## Phase 7 — FAIL handling + integration + gate

### Task 7.1: `FAIL BATCH MULTILINE_*`

**Files:** Modify `src/irc/events.rs` (incoming `match &msg.command`, add a `Command::Raw(cmd, args) if cmd == "FAIL"` arm after the `cmd == "671"` arm at line 232, before the numeric catch-all at 236).

- [ ] **Step 1: Test** that a `FAIL BATCH MULTILINE_MAX_LINES 24 :too many lines` raw message surfaces a themed event line to the server/active buffer.
- [ ] **Step 2-3: Implement:**
```rust
Command::Raw(cmd, args) if cmd == "FAIL" && args.first().map(String::as_str) == Some("BATCH") => {
    let code = args.get(1).map(String::as_str).unwrap_or("");
    if code.starts_with("MULTILINE_") {
        let desc = args.last().map_or("multiline error", String::as_str);
        let buf = active_or_server_buffer(state, conn_id);
        emit(state, &buf, &format!("%Zff6b6bmultiline: {code} — {desc}%N"));
    }
}
```
- [ ] **Step 4: Run, verify pass; clippy.**
- [ ] **Step 5: Commit.** `git commit -am "feat(multiline): surface FAIL BATCH MULTILINE_* errors"`

### Task 7.2: Integration tests (inbound + outbound roundtrip)

**Files:** add tests in `src/irc/batch.rs` and/or `src/irc/multiline.rs`.

- [ ] **Step 1:** Inbound: simulate the full sequence (`BATCH +ref draft/multiline #c` opener with `@time/@msgid`, N tagged PRIVMSG fragments incl. a `concat` one, `BATCH -ref`) through `start_batch`/`add_message`/`process_completed_batch` and assert exactly one buffer message with the reassembled text, correct nick, and `@msgid` carried in `tags`.
- [ ] **Step 2:** Outbound: assert `multiline_frames` output for a 2-batch overflow case (max_lines forces a split) produces two `+ref/-ref` pairs with distinct refs and correct concat tags.
- [ ] **Step 3:** Echo-message parity: document/test that with echo-message ON no local echo is added (server echo path reassembles), with it OFF exactly one echo Message is added.
- [ ] **Step 4: Run under memory guard; commit.** `git commit -am "test(multiline): inbound/outbound integration + echo parity"`

### Task 7.3: Full gate

- [ ] **Step 1:** `make clippy` — Expected: 0 new warnings (attribute per-file vs the ~44 baseline).
- [ ] **Step 2:** `( ulimit -v 4000000; timeout 600 make test )` — Expected: all pass, no OOM.
- [ ] **Step 3:** `make wasm` then `make release` — Expected: both build.
- [ ] **Step 4:** Manual smoke (`/run`-style): connect to a `draft/multiline` server (or a local mock), receive a multiline message (one grouped message in TUI + web), send a pasted multi-line message (one batch; verify on the wire / peer), confirm E2E channel still encrypts/decrypts and renders newlines, confirm a non-multiline server falls back to separate lines.
- [ ] **Step 5: Final commit + open PR** to `outrage` so the review bot runs. `git commit -am "chore(multiline): docs/changelog"` (add a `README.md` changelog entry under the current version), then push branch and open PR.

---

## Self-review checklist (run before execution)

- **Spec coverage:** cap request (1.2) ✓; max-bytes/max-lines parse + defaults (0.2) ✓; BATCH framing + concat (0.4, 4.1) ✓; total max-bytes incl. join byte (0.4) ✓; per-PRIVMSG cap via `MESSAGE_MAX_BYTES` (0.4) ✓; **lossless concat split via `split_lossless`, not `split_irc_message`** (0.4) ✓; reassembly join rules (0.3, 2.2) ✓; target consistency (2.2 uses batch target) ✓; all-blank/blank-concat rules (0.4) ✓; FAIL codes (7.1) ✓; @time/@msgid via opener tags MERGED with first-fragment tags (2.2) ✓; echo-message single display (4.1, 7.2) ✓.
- **E2E safety:** branch gated `!is_e2e_encrypted` AND `multiline_supported`; E2E path + `apply_shrink_deliver`/`send_outgoing_substituted` untouched; **shrink gate excludes `needs_multiline`** (4.1 Step 0); E2E `\n` rides in ciphertext; **no Case A early-return — single in-place E2E echo** (4.2); **inbound E2E-fragment guard falls back to per-fragment replay** (2.2); peer-side E2E chunk asymmetry documented (4.2) — ✓.
- **Robustness:** **inbound runaway cap `MULTILINE_MAX_INBOUND_LINES`** (0.1, 2.2) ✓; render budget reconciled + bounded-overshoot test (3.2) ✓; partition memory-guarded tests (0.4) ✓.
- **TUI+web parity:** one `Message` with `\n` → both renderers; web send single message preserving interior blank lines (5.1); paste coalescing preserves blank lines (4.3); TUI compose (6); shared `handle_plain_message` (4.1) — ✓.
- **Type/compile:** `MultilineLimits`/`WireLine` in `multiline.rs`, referenced as `crate::irc::multiline::*`; `IrcEvent::Connected` 3-tuple at all 3 sites (mod.rs:428,449; app/irc.rs:313); `start_batch` 4th arg at all call sites; **`wrap_segment` fallback does not reference the missing `line` binding** (3.1); **frames test uses multi-word lines** (proto omits `:` for single tokens) (4.1); **`#[must_use]` on `multiline_frames`** + fully-qualified proto types (4.1); **cap-new borrow ordering: store after the `to_request` collect** (1.3) — ✓.
- **Hot path:** single-line no-`\n` message hits the unchanged `else` branch in `handle_plain_message` — ✓.

## Review history

- 2026-06-25 adversarial plan review (3 lenses): spec-compliance, E2E-safety, type/borrow/clippy. All blockers/majors and actionable minors folded in: shrink-gate guard, `MULTILINE_MAX_INBOUND_LINES`, lossless `split_lossless` (concat correctness), inbound E2E-fragment guard, tag merge, no-double-echo E2E single echo, `multiline_supported` gate, blank-line preservation, `wrap_segment` fallback, frames-test colon, `#[must_use]`, cap-new borrow ordering, render-budget reconciliation.

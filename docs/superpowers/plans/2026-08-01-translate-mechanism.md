# Translate Mechanism Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the transport and display mechanism for near-real-time channel translation, with the translating layer left behind a replaceable seam.

**Architecture:** Mirrors the existing URL-shrink pipeline (`src/app/shrink.rs`) — a dispatch gate inside the `add_message` path, background tokio workers, and a deliver channel drained by the main `select!` loop. The one deliberate divergence: shrink's workers are serial, which would make latency cumulative when *every* line needs translating, so incoming translation runs concurrently behind a per-buffer reorder queue keyed on the monotonic message id.

**Tech Stack:** Rust 2024, tokio, `reqwest` (already a dependency), `mpsc` channels, ratatui.

**Spec:** `docs/superpowers/specs/2026-08-01-translate-mechanism-design.md`

## Global Constraints

- Build only via `make` targets — never raw cargo/trunk. `make test`, `make clippy`.
- Clippy: pedantic=warn, nursery=warn, perf=deny, redundant_clone=deny. **0 warnings.**
- MSRV 1.91, edition 2024. `clippy::incompatible_msrv` is enforced.
- `color-eyre` for errors, `thiserror` for library error types, `tracing` for logging (never `println!`).
- No ratatui imports in `src/state/` — state stays UI-agnostic.
- Never hardcode the app name; use `crate::constants::APP_NAME`.
- Commands are `fn(&mut App, &[String])` function pointers registered in `src/commands/registry.rs`.
- Branch: `feat/translate-mechanism`. Commit after every task.

## Cross-Cutting Decisions

Locked during design; do not re-litigate mid-implementation:

1. **Translation and shrink are mutually exclusive per line, translation wins.** The release path calls the `_unshrunk` variants, so a translated line is never also URL-shrunk. Two external round-trips on one line is worse than losing shrink on translated buffers.
2. **The ordering key is `Message.id`** from `AppState::next_message_id()`, never a timestamp.
3. **E2E gating uses `e2e_possible_for_target`** (fail-closed), never `e2e_enabled_for_target` (advisory). Copy the reasoning comment from `src/app/input.rs:1423-1433`.
4. **Stored text is exactly displayed text.** No new SQLite column, no migration.
5. **The dim tint on the bracketed original is live-render-only**, carried by a non-persisted `Message.orig_offset`.

## File Structure

| File | Responsibility |
|---|---|
| `src/translate/mod.rs` (new) | Contract types only: `TranslateRequest`, `TranslateOutcome`, `UntranslatedReason`, `Direction`, `compose_display`. No App/tokio deps. |
| `src/translate/queue.rs` (new) | The per-buffer reorder queue. Pure state, no I/O — carries the bulk of the tests. |
| `src/translate/backend.rs` (new) | The seam: `TranslateBackend` trait + `StubBackend`. |
| `src/app/translate.rs` (new) | Glue: `TranslateRuntime`, workers, `apply_translate_deliver`. Mirrors `src/app/shrink.rs`. |
| `src/commands/handlers_translate.rs` (new) | `/translate` subcommands. |
| `docs/commands/translate.md` (new) | User-facing docs. |
| `src/config/mod.rs` | `TranslateConfig` + field on `AppConfig`. |
| `src/state/mod.rs` | `translate_queues`, `pending_translate_requests`, `translate_active_buffers`, mirrored config. |
| `src/state/events.rs` | Incoming dispatch gate + queue release. |
| `src/app/mod.rs` | App fields, `TranslateRuntime` build, `select!` arm. |
| `src/app/input.rs` | Outgoing dispatch gate. |
| `src/state/buffer.rs` | `Message.orig_offset`. |
| `src/ui/message_line.rs` | Dim rendering of the bracketed original. |
| `src/commands/registry.rs`, `src/commands/settings.rs`, `src/commands/docs.rs` | Registration, `/set translate.*`, help. |

---

### Task 1: Contract types and config

**Files:**
- Create: `src/translate/mod.rs`
- Modify: `src/config/mod.rs` (add `TranslateConfig`, field on `AppConfig`, `Default` arm)
- Modify: `src/main.rs` or `src/lib` module list — add `pub mod translate;` next to `pub mod shrink;`

**Interfaces:**
- Produces: `TranslateRequest`, `TranslateOutcome`, `UntranslatedReason`, `Direction`, `compose_display(translated, original, show_original) -> (String, Option<usize>)`, `TranslateConfig`.

- [ ] **Step 1: Write the failing test**

In `src/translate/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_appends_original_in_brackets_when_enabled() {
        let (text, offset) = compose_display("albalb", "blabla", true);
        assert_eq!(text, "albalb [blabla]");
        assert_eq!(offset, Some(6), "offset marks the space before the bracket");
        assert_eq!(&text[offset.unwrap()..], " [blabla]");
    }

    #[test]
    fn compose_omits_original_when_disabled() {
        let (text, offset) = compose_display("albalb", "blabla", false);
        assert_eq!(text, "albalb");
        assert_eq!(offset, None);
    }

    #[test]
    fn compose_omits_original_when_identical_to_translation() {
        // A `Filtered` line resolves to its own text; bracketing it would
        // render "moin [moin]".
        let (text, offset) = compose_display("moin", "moin", true);
        assert_eq!(text, "moin");
        assert_eq!(offset, None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `make test 2>&1 | grep -A5 compose_appends`
Expected: FAIL — `compose_display` not found.

- [ ] **Step 3: Write minimal implementation**

```rust
//! Contract between the translation mechanism and whatever performs the
//! actual translation. Deliberately free of `App`, tokio, and I/O so the
//! broker can later be a Rust module, a subprocess, or a local daemon.

/// Which way a line is travelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Incoming,
    Outgoing,
}

/// One line handed to the broker. Exactly one line per request — feeding
/// surrounding context was measured to degrade output, so there is no
/// context field to misuse.
#[derive(Debug, Clone)]
pub struct TranslateRequest {
    /// Correlation key AND display-ordering key, from `next_message_id()`.
    pub id: u64,
    pub direction: Direction,
    pub network: String,
    pub target: String,
    pub nick: String,
    pub text: String,
    pub source_lang: Option<String>,
    pub target_lang: String,
    /// Channel nicklist, so masking behind the seam can protect nicks.
    pub known_nicks: Vec<String>,
}

/// Why a line came back untranslated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UntranslatedReason {
    /// The broker decided this line needs no translation. A CORRECT
    /// outcome, not a failure — renders clean, with no marker.
    Filtered,
    QualityGate,
    DailyLimit,
    NoProvider,
    /// Raised by the mechanism, not the broker.
    Timeout,
    Error(String),
}

impl UntranslatedReason {
    /// `true` when the user should see a marker. `Filtered` is the broker
    /// working correctly; everything else is a gap worth showing.
    pub const fn is_gap(&self) -> bool {
        !matches!(self, Self::Filtered)
    }

    pub fn label(&self) -> String {
        match self {
            Self::Filtered => "filtered".to_string(),
            Self::QualityGate => "quality gate".to_string(),
            Self::DailyLimit => "daily limit".to_string(),
            Self::NoProvider => "no provider".to_string(),
            Self::Timeout => "timeout".to_string(),
            Self::Error(e) => format!("error: {e}"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum TranslateOutcome {
    Translated { id: u64, text: String },
    Untranslated { id: u64, reason: UntranslatedReason },
}

impl TranslateOutcome {
    pub const fn id(&self) -> u64 {
        match self {
            Self::Translated { id, .. } | Self::Untranslated { id, .. } => *id,
        }
    }
}

/// Build the displayed line and, when the original is appended, the byte
/// offset where its ` [original]` suffix starts.
///
/// The offset is what lets the renderer dim only the appended part. It is
/// deliberately NOT persisted: the stored text is flat, so a row reloaded
/// from SQLite renders the same characters undimmed. Re-deriving it by
/// scanning for a trailing `[...]` is wrong — an ordinary message may end
/// that way and the renderer would dim someone else's brackets.
#[must_use]
pub fn compose_display(
    translated: &str,
    original: &str,
    show_original: bool,
) -> (String, Option<usize>) {
    if !show_original || translated == original {
        return (translated.to_string(), None);
    }
    let offset = translated.len();
    (format!("{translated} [{original}]"), Some(offset))
}
```

- [ ] **Step 4: Add `TranslateConfig` to `src/config/mod.rs`**

Model it on `ShrinkConfig` (`src/config/mod.rs:512`). Add the field to `AppConfig` next to `shrink`, and the arm in `impl Default for AppConfig`.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TranslateConfig {
    /// Master switch — when false, nothing is translated in either
    /// direction even if per-buffer flags are set.
    pub enabled: bool,
    /// Language every line is translated INTO.
    pub target_lang: String,
    /// Append ` [original]` to incoming translated lines.
    pub show_original_in: bool,
    /// Append ` [original]` to the local echo of outgoing lines.
    pub show_original_out: bool,
    /// How long a line may sit in the queue before it is released
    /// untranslated. Default is above the measured 4176 ms worst case.
    pub timeout_ms: u64,
    /// Concurrent in-flight translations. Raising this past a provider's
    /// measured limit makes throughput worse, not better.
    pub max_in_flight: u32,
    /// Per-buffer queue ceiling. On overflow the oldest pending entries
    /// release untranslated so the channel keeps flowing.
    pub max_queue: u32,
    /// Per-buffer settings, keyed by buffer id (`<conn_id>/<target>`).
    pub buffers: HashMap<String, TranslateBufferConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TranslateBufferConfig {
    pub incoming: bool,
    pub outgoing: bool,
    /// `None` = let the broker autodetect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_lang: Option<String>,
}

impl Default for TranslateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            target_lang: "en".to_string(),
            show_original_in: true,
            show_original_out: true,
            timeout_ms: 5000,
            max_in_flight: 4,
            max_queue: 200,
            buffers: HashMap::new(),
        }
    }
}
```

- [ ] **Step 5: Run tests and clippy**

Run: `make test && make clippy`
Expected: PASS, 0 warnings.

- [ ] **Step 6: Commit**

```bash
git add src/translate/mod.rs src/config/mod.rs src/main.rs
git commit -m "feat(translate): contract types and configuration"
```

---

### Task 2: The reorder queue

This is the heart of the feature and where most of the risk lives. It is pure state over plain data, so it is fully unit-testable with no `App`, no tokio, and no I/O.

**Files:**
- Create: `src/translate/queue.rs`
- Modify: `src/translate/mod.rs` (add `pub mod queue;`)

**Interfaces:**
- Consumes: `UntranslatedReason` from Task 1.
- Produces:
  - `TranslateQueue::new()`
  - `push_pending(&mut self, id: u64, original: String, pending: PendingPayload)`
  - `push_resolved(&mut self, id: u64, payload: ReadyPayload)`
  - `resolve(&mut self, id: u64, outcome_text: Result<String, UntranslatedReason>) -> bool`
  - `drain_ready(&mut self) -> Vec<ReadyEntry>`
  - `expire(&mut self, now: Instant, timeout: Duration) -> usize`
  - `enforce_ceiling(&mut self, max: usize) -> usize`
  - `flush_all(&mut self) -> Vec<ReadyEntry>`
  - `is_empty(&self) -> bool`, `len(&self) -> usize`, `pending_len(&self) -> usize`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn msg(text: &str) -> crate::state::buffer::Message { /* helper built in step 3 */ }

    #[test]
    fn resolved_head_drains_immediately() {
        let mut q = TranslateQueue::new();
        q.push_pending(1, "hola".into(), payload());
        assert!(q.drain_ready().is_empty(), "pending head blocks");
        q.resolve(1, Ok("czesc".into()));
        let ready = q.drain_ready();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, 1);
    }

    #[test]
    fn later_line_waits_for_earlier_one() {
        // THE case this queue exists for: line 5 comes back before line 4.
        let mut q = TranslateQueue::new();
        q.push_pending(4, "vier".into(), payload());
        q.push_pending(5, "fuenf".into(), payload());
        q.resolve(5, Ok("piec".into()));
        assert!(q.drain_ready().is_empty(), "5 must not overtake 4");
        q.resolve(4, Ok("cztery".into()));
        let ready = q.drain_ready();
        assert_eq!(
            ready.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![4, 5],
            "both release, in arrival order"
        );
    }

    #[test]
    fn non_translated_row_does_not_overtake_pending_lines() {
        // A JOIN arriving while line 4 translates must not render first.
        let mut q = TranslateQueue::new();
        q.push_pending(4, "vier".into(), payload());
        q.push_resolved(5, ready_payload());
        assert!(q.drain_ready().is_empty());
        q.resolve(4, Ok("cztery".into()));
        assert_eq!(q.drain_ready().iter().map(|e| e.id).collect::<Vec<_>>(), vec![4, 5]);
    }

    #[test]
    fn timeout_releases_stuck_head_and_drains_followers() {
        let mut q = TranslateQueue::new();
        let t0 = Instant::now();
        q.push_pending_at(4, "vier".into(), payload(), t0);
        q.push_pending_at(5, "fuenf".into(), payload(), t0);
        q.resolve(5, Ok("piec".into()));
        let expired = q.expire(t0 + Duration::from_millis(5001), Duration::from_millis(5000));
        assert_eq!(expired, 1, "only the still-pending entry expires");
        let ready = q.drain_ready();
        assert_eq!(ready.iter().map(|e| e.id).collect::<Vec<_>>(), vec![4, 5]);
        assert_eq!(ready[0].reason, Some(UntranslatedReason::Timeout));
        assert_eq!(ready[1].reason, None);
    }

    #[test]
    fn ceiling_releases_oldest_first_and_bounds_growth() {
        let mut q = TranslateQueue::new();
        for id in 1..=10 {
            q.push_pending(id, format!("l{id}"), payload());
        }
        let forced = q.enforce_ceiling(4);
        assert_eq!(forced, 6, "six oldest forced out");
        let ready = q.drain_ready();
        assert_eq!(ready.iter().map(|e| e.id).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5, 6]);
        assert!(ready.iter().all(|e| e.reason == Some(UntranslatedReason::Timeout)));
        assert_eq!(q.len(), 4);
    }

    #[test]
    fn resolving_unknown_id_is_a_no_op() {
        let mut q = TranslateQueue::new();
        q.push_pending(1, "a".into(), payload());
        assert!(!q.resolve(999, Ok("x".into())), "late outcome is dropped");
        assert_eq!(q.pending_len(), 1);
    }

    #[test]
    fn flush_all_releases_everything_in_order() {
        let mut q = TranslateQueue::new();
        q.push_pending(1, "a".into(), payload());
        q.push_resolved(2, ready_payload());
        q.push_pending(3, "c".into(), payload());
        let ready = q.flush_all();
        assert_eq!(ready.iter().map(|e| e.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert!(q.is_empty());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `make test 2>&1 | grep -A5 later_line_waits`
Expected: FAIL — `TranslateQueue` not found.

- [ ] **Step 3: Implement the queue**

```rust
//! Per-buffer reorder queue.
//!
//! Translation requests are dispatched the instant a line arrives and run
//! concurrently; this queue governs only WHEN a resolved line is allowed
//! onto the screen. A serial pipeline — line N waiting for N-1 before
//! being sent — would make latency cumulative, and on a live channel
//! arrivals outpace the pipeline so the backlog would grow without bound.
//! Here a line's delay is bounded by the slowest of its predecessors, not
//! their sum.
//!
//! Ordering is by `Message.id`, which is monotonic. Timestamps are not
//! usable: two lines in the same second have no defined order, `@time` may
//! be absent, and a server clock is not guaranteed monotonic. This repo
//! already reached that conclusion when it added `ts_ms` and made `id` the
//! keyset tiebreaker (`src/storage/db.rs`).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::UntranslatedReason;
use crate::state::buffer::{ActivityLevel, Message};

/// What the release path needs in order to deliver a line.
#[derive(Debug, Clone)]
pub struct ReadyPayload {
    pub message: Message,
    pub activity: ActivityLevel,
}

/// A line awaiting its translation.
#[derive(Debug, Clone)]
pub struct PendingPayload {
    /// The message as it would have been delivered untranslated. On
    /// resolution its `text` is replaced by the composed display string.
    pub message: Message,
    pub activity: ActivityLevel,
    pub show_original: bool,
}

#[derive(Debug)]
enum Slot {
    Pending {
        original: String,
        payload: PendingPayload,
        queued_at: Instant,
    },
    Ready {
        payload: ReadyPayload,
        reason: Option<UntranslatedReason>,
    },
}

#[derive(Debug)]
struct Entry {
    id: u64,
    slot: Slot,
}

/// A line cleared for delivery.
#[derive(Debug, Clone)]
pub struct ReadyEntry {
    pub id: u64,
    pub message: Message,
    pub activity: ActivityLevel,
    /// `None` when the line was translated; otherwise why it was not.
    pub reason: Option<UntranslatedReason>,
}

#[derive(Debug, Default)]
pub struct TranslateQueue {
    entries: VecDeque<Entry>,
}

impl TranslateQueue {
    #[must_use]
    pub fn new() -> Self {
        Self { entries: VecDeque::new() }
    }

    pub fn push_pending(&mut self, id: u64, original: String, payload: PendingPayload) {
        self.push_pending_at(id, original, payload, Instant::now());
    }

    /// Injectable-clock variant, so timeout behaviour is testable without
    /// sleeping.
    pub fn push_pending_at(
        &mut self,
        id: u64,
        original: String,
        payload: PendingPayload,
        now: Instant,
    ) {
        self.entries.push_back(Entry {
            id,
            slot: Slot::Pending { original, payload, queued_at: now },
        });
    }

    /// A row that needs no translation (a JOIN, a notice) still takes its
    /// place, so it cannot overtake lines queued before it.
    pub fn push_resolved(&mut self, id: u64, payload: ReadyPayload) {
        self.entries.push_back(Entry {
            id,
            slot: Slot::Ready { payload, reason: None },
        });
    }

    /// Fold an outcome into the matching entry. Returns `false` when the id
    /// is unknown — a late outcome for an already-released line.
    pub fn resolve(
        &mut self,
        id: u64,
        outcome: Result<String, UntranslatedReason>,
    ) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) else {
            return false;
        };
        let Slot::Pending { original, payload, .. } = &entry.slot else {
            return false;
        };
        let (text, orig_offset, reason) = match outcome {
            Ok(translated) => {
                let (text, off) =
                    super::compose_display(&translated, original, payload.show_original);
                (text, off, None)
            }
            Err(reason) => (original.clone(), None, Some(reason)),
        };
        let Slot::Pending { payload, .. } =
            std::mem::replace(&mut entry.slot, Slot::Ready {
                payload: ReadyPayload {
                    message: Message { text: String::new(), ..Default::default() },
                    activity: ActivityLevel::None,
                },
                reason: None,
            })
        else {
            unreachable!("checked above")
        };
        let mut message = payload.message;
        message.text = text;
        message.orig_offset = orig_offset;
        entry.slot = Slot::Ready {
            payload: ReadyPayload { message, activity: payload.activity },
            reason,
        };
        true
    }

    /// Pop from the head for as long as the head is ready.
    pub fn drain_ready(&mut self) -> Vec<ReadyEntry> {
        let mut out = Vec::new();
        while matches!(self.entries.front().map(|e| &e.slot), Some(Slot::Ready { .. })) {
            let entry = self.entries.pop_front().expect("checked by matches!");
            let Slot::Ready { payload, reason } = entry.slot else {
                unreachable!("checked by matches!")
            };
            out.push(ReadyEntry {
                id: entry.id,
                message: payload.message,
                activity: payload.activity,
                reason,
            });
        }
        out
    }

    /// Force every entry older than `timeout` to resolve as `Timeout`.
    /// Returns how many were forced.
    pub fn expire(&mut self, now: Instant, timeout: Duration) -> usize {
        let expired: Vec<u64> = self
            .entries
            .iter()
            .filter_map(|e| match &e.slot {
                Slot::Pending { queued_at, .. }
                    if now.duration_since(*queued_at) >= timeout =>
                {
                    Some(e.id)
                }
                _ => None,
            })
            .collect();
        for id in &expired {
            self.resolve(*id, Err(UntranslatedReason::Timeout));
        }
        expired.len()
    }

    /// Force the oldest pending entries out until the queue fits `max`.
    /// A dead provider must not turn this into an unbounded memory leak
    /// with a frozen channel behind it.
    pub fn enforce_ceiling(&mut self, max: usize) -> usize {
        if self.entries.len() <= max {
            return 0;
        }
        let excess = self.entries.len() - max;
        let ids: Vec<u64> = self
            .entries
            .iter()
            .filter(|e| matches!(e.slot, Slot::Pending { .. }))
            .take(excess)
            .map(|e| e.id)
            .collect();
        for id in &ids {
            self.resolve(*id, Err(UntranslatedReason::Timeout));
        }
        ids.len()
    }

    /// Release everything, in order, untranslated where still pending.
    /// Used on buffer close, disconnect, quit, and `/translate delin`.
    pub fn flush_all(&mut self) -> Vec<ReadyEntry> {
        let pending: Vec<u64> = self
            .entries
            .iter()
            .filter(|e| matches!(e.slot, Slot::Pending { .. }))
            .map(|e| e.id)
            .collect();
        for id in pending {
            self.resolve(id, Err(UntranslatedReason::Timeout));
        }
        self.drain_ready()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool { self.entries.is_empty() }

    #[must_use]
    pub fn len(&self) -> usize { self.entries.len() }

    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e.slot, Slot::Pending { .. }))
            .count()
    }
}
```

Note: the `resolve` body above uses a placeholder `Message::default()` during the
`mem::replace` dance. `Message` does not derive `Default`. Implement `resolve`
without the replace — take the entry by index, match to extract owned values via
`std::mem::replace` with a sentinel `Slot::Taken` variant, or restructure using
`Option<Slot>`. Use whichever the borrow checker accepts most cleanly; the tests
define the contract.

- [ ] **Step 4: Run tests to verify they pass**

Run: `make test 2>&1 | grep -E "later_line_waits|ceiling_releases|timeout_releases"`
Expected: PASS for all queue tests.

- [ ] **Step 5: Run clippy**

Run: `make clippy`
Expected: 0 warnings.

- [ ] **Step 6: Commit**

```bash
git add src/translate/queue.rs src/translate/mod.rs
git commit -m "feat(translate): per-buffer reorder queue with timeout and ceiling"
```

---

### Task 3: `Message.orig_offset` and dim rendering

Done before the wiring so later tasks can set the field.

**Files:**
- Modify: `src/state/buffer.rs` (add field to `Message`)
- Modify: every `Message { .. }` construction site — the compiler lists them
- Modify: `src/ui/message_line.rs` (dim the suffix)

**Interfaces:**
- Produces: `Message.orig_offset: Option<usize>`.

- [ ] **Step 1: Add the field**

In `src/state/buffer.rs`, on `struct Message`:

```rust
    /// Byte offset where the appended ` [original]` suffix begins, when a
    /// translated line is displayed with its original. Live-render only —
    /// deliberately NOT persisted, because the stored text is flat and a
    /// row reloaded from SQLite has no way to recover it. The renderer
    /// dims from here to the end.
    ///
    /// Never derive this by scanning for a trailing `[...]`: an ordinary
    /// message may legitimately end that way, and the renderer would dim
    /// someone else's brackets.
    pub orig_offset: Option<usize>,
```

- [ ] **Step 2: Fix every construction site**

Run: `make test 2>&1 | grep "missing field" | head -40`
Add `orig_offset: None,` to each. All existing sites are `None`.

- [ ] **Step 3: Write the failing renderer test**

In `src/ui/message_line.rs` tests:

```rust
#[test]
fn translated_original_suffix_renders_dimmed() {
    let mut msg = make_test_message("albalb [blabla]");
    msg.orig_offset = Some(6);
    let spans = render_chat_message(&msg, false, &theme(), &config(), None, None);
    let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
    assert!(joined.contains("albalb [blabla]"));
    let dim_text: String = spans.iter().filter(|s| s.dim).map(|s| s.text.as_str()).collect();
    assert_eq!(dim_text.trim(), "[blabla]", "only the appended original is dimmed");
}

#[test]
fn message_without_offset_has_no_dim_spans() {
    let msg = make_test_message("ordinary [not an original]");
    let spans = render_chat_message(&msg, false, &theme(), &config(), None, None);
    assert!(spans.iter().all(|s| !s.dim), "no offset means no dimming");
}
```

- [ ] **Step 4: Run to verify failure**

Run: `make test 2>&1 | grep -A5 translated_original_suffix`
Expected: FAIL.

- [ ] **Step 5: Implement**

In `render_chat_message`, after `spans` are built, split spans at the byte offset
within the body and set `dim = true` on everything at or past it. `StyledSpan`
already carries a `dim: bool` field (`src/theme/`), and `styled_spans_to_line`
already honours it, so no new plumbing is needed — only span splitting.

- [ ] **Step 6: Run tests and clippy**

Run: `make test && make clippy`
Expected: PASS, 0 warnings.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(translate): carry and dim the appended original text"
```

---

### Task 4: Backend seam and stub

**Files:**
- Create: `src/translate/backend.rs`
- Modify: `src/translate/mod.rs`

**Interfaces:**
- Produces: `TranslateBackend` (async translate), `StubBackend::new(delay_ms, fail_every)`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stub_translates_deterministically() {
        let b = StubBackend::new(0, 0);
        let out = b.translate(req(1, "hello world")).await;
        match out {
            TranslateOutcome::Translated { id, text } => {
                assert_eq!(id, 1);
                assert_eq!(text, "world hello", "stub reverses word order");
            }
            other => panic!("expected Translated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stub_injects_failures_on_a_fixed_cadence() {
        let b = StubBackend::new(0, 2);
        assert!(matches!(b.translate(req(1, "a b")).await, TranslateOutcome::Translated { .. }));
        assert!(matches!(
            b.translate(req(2, "a b")).await,
            TranslateOutcome::Untranslated { reason: UntranslatedReason::NoProvider, .. }
        ));
    }

    #[tokio::test]
    async fn stub_filters_a_line_that_is_already_one_word() {
        // Exercises the Filtered path end-to-end without a real filter.
        let b = StubBackend::new(0, 0);
        assert!(matches!(
            b.translate(req(3, "moin")).await,
            TranslateOutcome::Untranslated { reason: UntranslatedReason::Filtered, .. }
        ));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `make test 2>&1 | grep -A5 stub_translates`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
//! The seam. Everything the mechanism knows about translating is this
//! trait; the parser, filter, masking, difficulty router, provider policy
//! and quality gate all live behind it and are specified separately.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::{TranslateOutcome, TranslateRequest, UntranslatedReason};

pub trait TranslateBackend: Send + Sync + 'static {
    fn translate(
        &self,
        req: TranslateRequest,
    ) -> impl std::future::Future<Output = TranslateOutcome> + Send;
}

/// Exercises the whole mechanism with no API key: a fixed delay, a
/// deterministic transformation, and injectable failures. It exists to
/// prove ordered release, timeout, ceiling overflow, the E2E gate and the
/// outgoing refusal before any real provider is written.
pub struct StubBackend {
    delay: Duration,
    /// Every Nth call fails with `NoProvider`. 0 disables injection.
    fail_every: u64,
    calls: AtomicU64,
}

impl StubBackend {
    #[must_use]
    pub const fn new(delay_ms: u64, fail_every: u64) -> Self {
        Self {
            delay: Duration::from_millis(delay_ms),
            fail_every,
            calls: AtomicU64::new(0),
        }
    }
}

impl TranslateBackend for StubBackend {
    async fn translate(&self, req: TranslateRequest) -> TranslateOutcome {
        let n = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        if self.fail_every > 0 && n % self.fail_every == 0 {
            return TranslateOutcome::Untranslated {
                id: req.id,
                reason: UntranslatedReason::NoProvider,
            };
        }
        let words: Vec<&str> = req.text.split_whitespace().collect();
        if words.len() < 2 {
            return TranslateOutcome::Untranslated {
                id: req.id,
                reason: UntranslatedReason::Filtered,
            };
        }
        TranslateOutcome::Translated {
            id: req.id,
            text: words.into_iter().rev().collect::<Vec<_>>().join(" "),
        }
    }
}
```

- [ ] **Step 4: Run tests and clippy**

Run: `make test && make clippy`
Expected: PASS, 0 warnings.

- [ ] **Step 5: Commit**

```bash
git add src/translate/backend.rs src/translate/mod.rs
git commit -m "feat(translate): backend seam and stub backend"
```

---

### Task 5: Runtime, workers and deliver channel

Mirrors `src/app/shrink.rs` closely. Read that file first — the panic isolation,
`spawn_drain`-when-disabled, and capture-at-dispatch idioms are all load-bearing
and were each the fix for a real review finding.

**Files:**
- Create: `src/app/translate.rs`
- Modify: `src/app/mod.rs` (`pub mod translate;`)

**Interfaces:**
- Consumes: Tasks 1, 2, 4.
- Produces: `TranslateRuntime::build(&TranslateConfig)`, `TranslateDeliver`, `PendingTranslate`.

- [ ] **Step 1: Implement the runtime**

```rust
//! Glue between `src/translate/` and the `App` event loop.
//!
//! Incoming and outgoing use SEPARATE workers, as shrink does, so a busy
//! channel cannot starve the user's own messages.
//!
//! The outgoing worker is SERIAL — one message at a time — which is what
//! keeps two outgoing messages in submission order on the wire. The
//! incoming worker is CONCURRENT up to `max_in_flight`, because every line
//! goes through translation and a serial pipeline would build an unbounded
//! backlog on a live channel. Incoming display order is restored by the
//! reorder queue in `src/translate/queue.rs`, not by serialising here.

pub enum TranslateDeliver {
    Outcome { buffer_id: String, outcome: TranslateOutcome },
    Outgoing(OutgoingTranslateDeliver),
}

pub struct OutgoingTranslateDeliver {
    pub conn_id: String,
    pub buffer_id: String,
    pub buffer_name: String,
    pub buffer_type: BufferType,
    /// Original text, kept for the local echo and for restoring the input
    /// line on failure.
    pub original_text: String,
    pub outcome: TranslateOutcome,
    /// Captured at dispatch — see `PendingOutgoing` in shrink.rs for why
    /// re-reading state at deliver time is wrong.
    pub nick: String,
    pub own_mode: Option<char>,
    pub peer_handle: Option<String>,
    pub show_original: bool,
}
```

Build `TranslateRuntime` with `incoming_tx`, `outgoing_tx`, `deliver_tx`,
`deliver_rx`, exactly like `ShrinkRuntime::build`. Spawn `spawn_drain` on both
receivers when `!cfg.enabled`, so `try_send` from the hot paths never
backpressures.

The incoming worker uses a `tokio::sync::Semaphore` with `max_in_flight` permits
and spawns a task per line, so lines translate concurrently. Wrap each per-message
body in `futures::FutureExt::catch_unwind` as both shrink workers do, so a panic
kills one line rather than the worker.

- [ ] **Step 2: Write a worker-level test**

```rust
#[tokio::test]
async fn incoming_worker_returns_outcomes_for_every_line() {
    let (rt, mut deliver_rx) = TranslateRuntime::build_for_test(StubBackend::new(0, 0), 4);
    for id in 1..=3u64 {
        rt.incoming_tx.send(PendingTranslate { buffer_id: "b".into(), req: req(id, "a b") }).await.unwrap();
    }
    let mut seen = Vec::new();
    for _ in 0..3 {
        if let Some(TranslateDeliver::Outcome { outcome, .. }) = deliver_rx.recv().await {
            seen.push(outcome.id());
        }
    }
    seen.sort_unstable();
    assert_eq!(seen, vec![1, 2, 3], "every line gets exactly one outcome");
}
```

- [ ] **Step 3: Run tests and clippy**

Run: `make test && make clippy`
Expected: PASS, 0 warnings.

- [ ] **Step 4: Commit**

```bash
git add src/app/translate.rs src/app/mod.rs
git commit -m "feat(translate): runtime, workers and deliver channel"
```

---

### Task 6: App wiring — fields, build, select! arm, tick

**Files:**
- Modify: `src/app/mod.rs` (fields near the `shrink_*` ones at :479-498; build near :662; `select!` arm near :1536)
- Modify: `src/state/mod.rs` (`translate_queues`, `pending_translate_requests`, `translate_active`, mirrored config)
- Modify: `src/state/events.rs` (`AppState::new` initialisers)

**Interfaces:**
- Produces: `App::apply_translate_deliver`, `App::drain_pending_translate_requests`, `App::tick_translate_queues`.

- [ ] **Step 1: Add state fields**

```rust
    /// Per-buffer reorder queues. Present only for buffers with pending or
    /// queued lines; absent means "deliver straight through".
    pub translate_queues: HashMap<String, crate::translate::queue::TranslateQueue>,
    /// Requests produced by the synchronous IRC handlers, drained by the
    /// App loop — same pattern as `pending_web_events`.
    pub pending_translate_requests: Vec<crate::translate::TranslateRequest>,
    /// Mirror of `config.translate.enabled && a backend exists`, synced
    /// from `/set` so a runtime flip needs no restart. Same pattern as
    /// `shrink_incoming_active`.
    pub translate_active: bool,
    /// Mirror of `config.translate.buffers`, keyed by buffer id.
    pub translate_buffers: HashMap<String, crate::config::TranslateBufferConfig>,
    pub translate_target_lang: String,
    pub translate_show_original_in: bool,
    pub translate_max_queue: usize,
```

- [ ] **Step 2: Add the `select!` arm**

Next to the `shrink_deliver_rx` arm at `src/app/mod.rs:1536`:

```rust
                translate_res = self.translate_deliver_rx.recv() => {
                    if let Some(deliver) = translate_res {
                        self.apply_translate_deliver(deliver);
                        while let Ok(next) = self.translate_deliver_rx.try_recv() {
                            self.apply_translate_deliver(next);
                        }
                        self.drain_pending_web_events();
                    }
                },
```

- [ ] **Step 3: Drive timeouts from the existing tick**

The 1 s tick arm already exists (it rebuilds the script snapshot). Add:

```rust
    /// Expire timed-out queue entries and enforce the ceiling, then release
    /// whatever that unblocked. Called from the 1 s tick, so a stuck head
    /// cannot hold a channel indefinitely even with no further traffic.
    pub(crate) fn tick_translate_queues(&mut self) {
        let timeout = Duration::from_millis(self.config.translate.timeout_ms);
        let max_queue = self.config.translate.max_queue as usize;
        let now = std::time::Instant::now();
        let buffer_ids: Vec<String> = self.state.translate_queues.keys().cloned().collect();
        for buffer_id in buffer_ids {
            let ready = {
                let Some(q) = self.state.translate_queues.get_mut(&buffer_id) else { continue };
                q.expire(now, timeout);
                q.enforce_ceiling(max_queue);
                q.drain_ready()
            };
            self.release_translated(&buffer_id, ready);
        }
        self.state.translate_queues.retain(|_, q| !q.is_empty());
    }
```

- [ ] **Step 4: Implement `release_translated`**

```rust
    /// Deliver cleared lines. Calls the `_unshrunk` variants: the text is
    /// final, and re-entering `add_message` would hand a translated line
    /// straight back to the translation gate. Translation and shrink are
    /// mutually exclusive per line by design.
    fn release_translated(
        &mut self,
        buffer_id: &str,
        ready: Vec<crate::translate::queue::ReadyEntry>,
    ) {
        for entry in ready {
            if let Some(reason) = entry.reason.as_ref().filter(|r| r.is_gap()) {
                tracing::debug!(id = entry.id, reason = %reason.label(), "translate: gap");
            }
            self.state.add_message_with_activity_unshrunk(
                buffer_id,
                entry.message,
                entry.activity,
            );
        }
    }
```

- [ ] **Step 5: Run tests and clippy**

Run: `make test && make clippy`
Expected: PASS, 0 warnings.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(translate): wire runtime into the App event loop"
```

---

### Task 7: Incoming dispatch gate

**Files:**
- Modify: `src/state/events.rs` (`add_message_with_activity` and `add_message`)

**Interfaces:**
- Consumes: Tasks 2, 6.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn translation_enabled_buffer_queues_instead_of_adding() {
    let mut state = make_test_state();
    enable_translation(&mut state, "libera/#dupa");
    let msg = make_test_message(&mut state, "hola que tal");
    state.add_message_with_activity("libera/#dupa", msg, ActivityLevel::Message);
    assert_eq!(
        state.buffers["libera/#dupa"].messages.len(),
        0,
        "line waits for its translation"
    );
    assert_eq!(state.pending_translate_requests.len(), 1);
}

#[test]
fn e2e_possible_target_is_never_translated() {
    // Fail-closed: a keyring that cannot be read must not leak plaintext.
    let mut state = make_test_state_with_unreadable_keyring();
    enable_translation(&mut state, "libera/#sec");
    let msg = make_test_message(&mut state, "hola que tal");
    state.add_message_with_activity("libera/#sec", msg, ActivityLevel::Message);
    assert!(state.pending_translate_requests.is_empty(), "no request built");
    assert_eq!(state.buffers["libera/#sec"].messages.len(), 1, "delivered untranslated");
}

#[test]
fn rows_needing_no_translation_queue_behind_pending_lines() {
    let mut state = make_test_state();
    enable_translation(&mut state, "libera/#dupa");
    let chat = make_test_message(&mut state, "hola que tal");
    state.add_message_with_activity("libera/#dupa", chat, ActivityLevel::Message);
    let join = make_test_event(&mut state, "bob has joined");
    state.add_message("libera/#dupa", join);
    assert_eq!(
        state.buffers["libera/#dupa"].messages.len(),
        0,
        "the JOIN must not overtake the pending line"
    );
}
```

- [ ] **Step 2: Run to verify failure**

Run: `make test 2>&1 | grep -A5 translation_enabled_buffer_queues`
Expected: FAIL.

- [ ] **Step 3: Implement the gate**

In `add_message_with_activity`, **before** the shrink dispatch:

```rust
        // Translation dispatch. Runs before shrink because the two are
        // mutually exclusive per line and translation wins — two external
        // round-trips on one line is worse than losing shrink here.
        //
        // The E2E check uses the FAIL-CLOSED predicate. The advisory
        // `e2e_enabled_for_target` resolves keyring read errors and
        // unresolved DM handles to `false`, which the send gate still
        // REFUSES as E2E-enabled; translating on those would ship the
        // plaintext of an end-to-end-protected conversation to a
        // third-party provider before the refusal ever ran. Same reasoning,
        // and the same predicate, as the shrink gate at src/app/input.rs.
        if self.translate_should_queue(buffer_id, &message) {
            self.enqueue_for_translation(buffer_id, message, level);
            return;
        }
        // A buffer with a non-empty queue takes EVERYTHING through the
        // queue, translated or not — otherwise a JOIN renders before the
        // lines queued ahead of it and the timeline reorders silently.
        if let Some(q) = self.translate_queues.get_mut(buffer_id) {
            let id = message.id;
            q.push_resolved(id, ReadyPayload { message, activity: level });
            return;
        }
```

Add the same two blocks to `add_message` (with `ActivityLevel::None`).

`translate_should_queue` returns false when: the master switch is off, the buffer
has no `incoming` flag, the message is our own echo, or
`e2e_possible_for_target` is true.

- [ ] **Step 4: Run tests and clippy**

Run: `make test && make clippy`
Expected: PASS, 0 warnings.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(translate): incoming dispatch gate with fail-closed E2E check"
```

---

### Task 8: Outgoing path

**Files:**
- Modify: `src/app/input.rs` (`handle_plain_message`, alongside the shrink gate at :1421-1475)
- Modify: `src/app/translate.rs` (`apply_translate_deliver` outgoing arm)

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn outgoing_failure_does_not_send_and_restores_the_input() {
    let mut app = test_app_with_translation("libera/#dupa", Direction::Outgoing);
    app.apply_translate_deliver(TranslateDeliver::Outgoing(outgoing_deliver(
        "libera/#dupa",
        "moje zdanie",
        TranslateOutcome::Untranslated { id: 1, reason: UntranslatedReason::NoProvider },
    )));
    assert!(app.sent_wire_lines().is_empty(), "nothing may reach the wire");
    assert_eq!(app.input_text(), "moje zdanie", "text is returned to the user");
    assert!(app.last_local_event().contains("no provider"));
}

#[test]
fn outgoing_success_sends_translated_text() {
    let mut app = test_app_with_translation("libera/#dupa", Direction::Outgoing);
    app.apply_translate_deliver(TranslateDeliver::Outgoing(outgoing_deliver(
        "libera/#dupa",
        "moje zdanie",
        TranslateOutcome::Translated { id: 1, text: "mein satz".into() },
    )));
    assert_eq!(app.sent_wire_lines(), vec!["mein satz"]);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `make test 2>&1 | grep -A5 outgoing_failure_does_not_send`
Expected: FAIL.

- [ ] **Step 3: Implement the dispatch**

In `handle_plain_message`, before the shrink block, with the same capture-at-dispatch
discipline (`captured_nick`, `captured_own_mode`, `captured_peer_handle`) and the same
`e2e_possible` guard already computed at `src/app/input.rs:1433`.

- [ ] **Step 4: Implement the deliver arm**

On `Translated`, run the existing pipeline — reuse `send_outgoing_substituted`'s
structure from `src/app/shrink.rs:501` (E2E encrypt → send → echo), composing the
local echo with `compose_display(translated, original, show_original_out)`.

On `Untranslated` with `is_gap()`, send nothing, restore the text to the input line,
and emit a local event naming the reason. On `Filtered`, send the original — the
broker correctly decided no translation was needed.

- [ ] **Step 5: Run tests and clippy**

Run: `make test && make clippy`
Expected: PASS, 0 warnings.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(translate): outgoing path, fail-closed on translation failure"
```

---

### Task 9: `/translate` command and `/set translate.*`

**Files:**
- Create: `src/commands/handlers_translate.rs`
- Modify: `src/commands/registry.rs`, `src/commands/settings.rs`, `src/commands/mod.rs`, `src/commands/docs.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn addin_enables_incoming_for_the_named_buffer() {
    let mut app = test_app();
    cmd_translate(&mut app, &["addin".into(), "#dupa".into(), "de".into()]);
    let cfg = &app.config.translate.buffers["test/#dupa"];
    assert!(cfg.incoming);
    assert!(!cfg.outgoing);
    assert_eq!(cfg.source_lang.as_deref(), Some("de"));
}

#[test]
fn addin_refuses_an_e2e_conversation() {
    let mut app = test_app_with_e2e_on("#sec");
    cmd_translate(&mut app, &["addin".into(), "#sec".into()]);
    assert!(!app.config.translate.buffers.contains_key("test/#sec"));
    assert!(app.last_local_event().to_lowercase().contains("e2e"));
}

#[test]
fn delin_flushes_pending_lines_rather_than_losing_them() {
    let mut app = test_app_with_pending_translation("test/#dupa", 2);
    cmd_translate(&mut app, &["delin".into(), "#dupa".into()]);
    assert_eq!(app.state.buffers["test/#dupa"].messages.len(), 2, "released untranslated");
    assert!(!app.state.translate_queues.contains_key("test/#dupa"));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `make test 2>&1 | grep -A5 addin_enables_incoming`
Expected: FAIL.

- [ ] **Step 3: Implement `cmd_translate`**

Subcommands `list`, `addin`, `delin`, `addout`, `delout`, `status`. Register in
`registry.rs` following the `"shrink"` entry at `src/commands/registry.rs:674`.

- [ ] **Step 4: Add `/set translate.*`**

Follow the `shrink.*` arms in `src/commands/settings.rs:142` (get) and `:438` (set).
Enforce floors: `timeout_ms >= 500`, `max_in_flight >= 1`, `max_queue >= 1`.
Setting `translate.enabled` must re-sync `state.translate_active`.

- [ ] **Step 5: Run tests and clippy**

Run: `make test && make clippy`
Expected: PASS, 0 warnings.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(translate): /translate command and /set translate.* settings"
```

---

### Task 10: Flush points, integration test and docs

**Files:**
- Modify: `src/app/mod.rs` (quit/detach), `src/commands/handlers_ui.rs` (`/close`), `src/app/irc.rs` (disconnect)
- Create: `docs/commands/translate.md`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn closing_a_buffer_flushes_pending_lines() {
    let mut app = test_app_with_pending_translation("test/#dupa", 3);
    cmd_close(&mut app, &[]);
    assert!(!app.state.translate_queues.contains_key("test/#dupa"));
}

#[tokio::test]
async fn a_burst_with_random_delays_preserves_arrival_order() {
    // The headline guarantee, end to end against the stub.
    let mut app = test_app_with_translation("test/#dupa", Direction::Incoming);
    let texts: Vec<String> = (1..=20).map(|i| format!("line {i} of the burst")).collect();
    for t in &texts { app.receive_privmsg("#dupa", "alice", t).await; }
    app.drain_until_quiet().await;
    let shown: Vec<String> = app.state.buffers["test/#dupa"]
        .messages.iter().map(|m| m.text.clone()).collect();
    assert_eq!(shown.len(), 20);
    for (i, line) in shown.iter().enumerate() {
        assert!(
            line.contains(&format!("line {} of the burst", i + 1)),
            "position {i} holds the wrong line: {line}"
        );
    }
}
```

The burst test must use a `StubBackend` variant with **per-call varying delay** so
later lines genuinely finish first. Add `StubBackend::with_jittered_delay(seq)`
taking an explicit delay sequence — no `Math.random`, so the test is deterministic.

- [ ] **Step 2: Run to verify failure**

Run: `make test 2>&1 | grep -A5 a_burst_with_random_delays`
Expected: FAIL.

- [ ] **Step 3: Implement the flush points**

Call `flush_all` + `release_translated` on `/close`, `/part`, disconnect, quit and
detach.

- [ ] **Step 4: Write `docs/commands/translate.md`**

Cover the commands, every `/set`, the E2E prohibition and why it has no override,
what each `Untranslated` reason means to the user, and the documented interaction
that translated lines are not URL-shrunk.

- [ ] **Step 5: Run the full gate**

Run: `make test && make clippy && make release`
Expected: PASS, 0 warnings, release builds.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(translate): flush points, ordering integration test, docs"
```

---

## Self-Review Notes

**Spec coverage:** §2 seam → Tasks 1, 4. §3 ordering → Task 2 (queue), Task 6 (tick).
§3.4 escape valves → Task 2 + Task 6. §3.5 flush → Task 10. §4 incoming → Task 7.
§5 outgoing + §5.1 wire/echo split → Task 8 (serial outgoing worker in Task 5 supplies
wire order). §6 E2E → Task 7 gate + Task 9 command refusal. §7 display → Tasks 1, 3.
§8 config/commands → Tasks 1, 9. §9 stub → Task 4. §10 error table → Tasks 7, 8.
§11 tests → distributed, with the headline ordering test in Task 10.

**Known rough edge, deliberately left to implementation:** the `resolve` body in
Task 2 sketches a `mem::replace` that will not borrow-check as written, and the plan
says so at the point of use. The tests define the contract; the exact shape is the
implementer's call.

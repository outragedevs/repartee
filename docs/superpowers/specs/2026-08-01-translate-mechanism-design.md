# `/translate` — transport and display mechanism

**Date:** 2026-08-01
**Status:** approved, ready for implementation planning
**Branch:** `feat/translate-mechanism`

## Summary

Repartee can translate a channel or query in near-real time: an incoming PRIVMSG is
shown already translated, and an outgoing message is translated before it reaches
IRC.

This spec covers **only the mechanism** — capturing the line, handing it to a
translation broker over a defined contract, taking the answer back, and getting it
onto the screen and into the log in the right order. The broker itself (model
choice, prompting, masking, quality gates, provider policy) is deliberately out of
scope and is specified separately; see [Non-goals](#12-non-goals).

The seam exists so the translating layer can be replaced wholesale — an AI backend,
a Google Translate backend, anything — without touching a line of the mechanism.

Two findings from the research phase shaped this design:

- **The Lua scripting API cannot implement this**, so it is native. `EventResult`
  offers only `Continue`/`Suppress` — the Lua event table is built, passed, and
  discarded, so a handler cannot rewrite `ev.message`
  (`src/scripting/lua/mod.rs:922`). There is no outgoing hook at all: `command_input`
  fires only for slash commands (`src/app/input.rs:1236`), while plain chat text goes
  straight through `handle_plain_message` (`src/app/input.rs:1310`) emitting nothing.
  And `emit` runs synchronously inside the main `select!` loop, so a blocking HTTP
  call in a handler would freeze the TUI, IRC I/O and the web server together.
- **Every mechanism this needs already exists natively.** `reqwest` is a dependency;
  the async-fetch → mpsc → dedicated `select!` arm pattern is `preview_rx`
  (`src/app/mod.rs:1419`); mid-pipeline text substitution is what E2E decryption
  already does in `handle_privmsg` (`src/irc/events.rs:1328-1370`); and
  `send_gated_message` (`src/app/e2e_gate.rs:699`) is a single outgoing chokepoint.

---

## 1. Scope of the mechanism

The mechanism owns:

- deciding whether a given line is eligible for translation (per-buffer config, E2E gate)
- issuing one request per line, immediately on arrival
- restoring display order when answers come back out of order
- rendering the result, with or without the original
- writing the result to SQLite
- failing safely and visibly when translation does not happen

The mechanism does **not** own: language detection, masking of nicks/URLs, model
routing, provider policy, retries against a provider, or quality gating. Those live
behind the seam.

---

## 2. The seam: two channels, not a trait

The contract is a pair of `mpsc` channels, not a `trait` with `async fn`.

This matches how every other async subsystem in the app already talks to the event
loop (`irc_rx`, `preview_rx`, `dcc_rx`), and it leaves the broker free to be an
in-process Rust module, a subprocess speaking JSON over stdio, or an HTTP call to a
local daemon — none of which changes the mechanism.

```rust
pub struct TranslateRequest {
    /// Correlation key AND display-ordering key. Allocated from
    /// `AppState::next_message_id()` at the moment the line enters the queue.
    pub id: u64,
    pub direction: Direction,        // Incoming | Outgoing
    pub network: String,
    pub target: String,              // "#dupa" or a nick
    pub nick: String,                // speaker; ourselves when Outgoing
    pub text: String,                // exactly one line, raw, unmasked
    pub source_lang: Option<String>, // None = broker autodetects
    pub target_lang: String,         // resolved per direction — see §2.2
    pub known_nicks: Vec<String>,    // channel nicklist, for masking behind the seam
}

pub enum TranslateOutcome {
    Translated   { id: u64, text: String },
    Untranslated { id: u64, reason: UntranslatedReason },
}

pub enum UntranslatedReason {
    Filtered,        // broker decided this line needs no translation — a CORRECT outcome
    QualityGate,
    DailyLimit,
    NoProvider,
    Timeout,         // raised by the mechanism, not the broker
    Error(String),
}
```

**One line per request. No context lines.** Measurement in the research repo found
that feeding surrounding lines degraded output, so context is absent from the
contract entirely rather than present-but-unused.

`known_nicks` travels in the request because only the client knows the channel's
nicklist, and masking (behind the seam) needs it.

### 2.1 `Filtered` is not a failure

`Untranslated { Filtered }` means the broker correctly decided this line needs no
translation — it is already in the target language, or it is noise like `moin`. The
research measurement puts this at roughly a third of all lines. It renders as a clean
original with **no marker**.

Every other reason is a genuine gap and renders with a dim marker. The rule from the
research architecture — *a visible hole always beats an invisible lie* — must hold
when the mechanism fails, not only when a model does.

### 2.2 Languages are a per-buffer PAIR, not per-direction settings

Each buffer has a language pair: the one the channel speaks (`lang`) and the
one we read and write (`my_lang`, global with a per-buffer override). The two
directions **swap** it:

| direction | source | target |
|---|---|---|
| incoming | `lang` | `my_lang` |
| outgoing | `my_lang` | `lang` |

This is stated explicitly because the first implementation got it wrong.
Modelling `source_lang`/`target_lang` as settings read straight off the config
produced an outgoing direction that told the broker our own text was already
in the channel's language and asked it to produce ours — a no-op at best. It
also made a single global "target language" the write target for every
channel, so a German and a Spanish channel could not both work.

Both dispatch paths therefore resolve the pair through one function,
`translate::resolve_langs`, and ask it for a direction rather than reading the
fields. `LangPair::outgoing()` returns `None` when the buffer has no language:
a target cannot be autodetected, so `addout` requires one and refuses without
it.

---

## 3. Ordering: immediate dispatch, ordered release

Requests are dispatched **the instant a line arrives**. Translation runs concurrently,
up to `max_in_flight`. The queue governs only *when a resolved line is allowed onto
the screen*.

This distinction is the crux. A serial queue — where line N waits for line N-1 to come
back before being sent for translation — would make latency cumulative: ten queued
lines at p50 ≈ 900 ms means nine seconds for the last one, and on a live channel the
backlog grows without bound because arrivals outpace the pipeline. With concurrent
dispatch and ordered release, a line's delay is bounded by the slowest of its
predecessors, not their sum.

### 3.1 Ordering key

The key is `Message.id` from `AppState::next_message_id()`, assigned at arrival —
**not a timestamp**. Timestamps fail in exactly the case this queue exists to prevent:
two lines within the same second have no defined order, `@time` may be absent, and a
server clock is not guaranteed monotonic.

This repository already reached that conclusion once: the `ts_ms` column was added
because same-second rows could not be ordered, and the scroll-back keyset cursor
sorts by `(COALESCE(ts_ms, timestamp * 1000), id)` with the monotonic `id` as
tiebreaker (`src/storage/db.rs:47`). The counter is already the real authority on
order; this reuses it.

### 3.2 Everything for the buffer goes through the queue

While a buffer has pending lines, **all** rows destined for it enter the queue —
including joins, parts, `/me`, and notices that need no translation. They enter
already-resolved and cost nothing, but they hold their place.

Without this, a JOIN arriving while lines 4 and 5 are still translating would render
before them, silently reordering the buffer's timeline — the exact failure the queue
exists to prevent, just from a different direction.

### 3.3 Release rule

A queue is a per-buffer `VecDeque` of entries that are either `Pending { id }` or
`Resolved { id, message }`. On every outcome, and on every tick, the queue pops from
the head for as long as the head is `Resolved`, calling `add_message` for each.

`add_message` is the single path that writes the buffer, `log_tx` → SQLite,
`pending_web_events`, and activity/mentions (asserted by the test at
`src/state/events.rs:1849`). Because a line reaches it only after resolution, the
translated text lands in SQLite and in the web UI for free, with no change to the
storage layer.

### 3.4 Two escape valves

Neither is optional; without them a dead provider turns the queue into an unbounded
memory leak with a frozen channel behind it.

- **Per-line timeout**, measured from entry into the queue, default 5000 ms (above
  the measured 4176 ms worst case). On expiry the line resolves as
  `Untranslated { Timeout }` and the head advances.
- **Queue ceiling**, default 200 entries per buffer. On overflow the oldest pending
  entries resolve as `Untranslated { Timeout }` immediately and release, so the
  channel keeps flowing untranslated instead of stalling.

Late outcomes for an already-released id are dropped, with a `tracing::debug!`.

### 3.5 Flush points

Pending entries must be released — untranslated, in order — rather than lost, when:
the buffer is closed (`/close`, `/part`), the connection drops, the app quits or
detaches, or `/translate delin|delout` disables the buffer.

---

## 4. Incoming path

In `handle_privmsg` (`src/irc/events.rs`), **after** E2E decryption and before
`add_message`:

1. If the buffer has `in` enabled, and the E2E gate (§6) permits, allocate `id`,
   push `Pending { id }` onto the buffer queue, and push a `TranslateRequest` onto
   `state.pending_translate_requests` — drained by the App loop exactly the way
   `pending_web_events` already is.
2. Otherwise `add_message` as today, unless the buffer has a non-empty queue, in
   which case the row enters the queue already-resolved (§3.2).

A new `translate_rx` arm in the `select!` loop, alongside `preview_rx`
(`src/app/mod.rs:1419`), receives outcomes, resolves the matching entry, and drains
the head.

---

## 5. Outgoing path

In `handle_plain_message` (`src/app/input.rs:1310`): if the buffer has `out` enabled
and the E2E gate permits, the message is **not** sent immediately. It is queued, and
on the outcome `send_gated_message` is called with the translated text, with the
local echo composed per `translate.show_original_out`.

**On failure the message is not sent at all.** The text is restored to the input line
and an error row explains why. Sending the original would transmit something other
than what the user intended — the same reasoning that makes the E2E send gate refuse
rather than downgrade (`src/app/e2e_gate.rs:474`).

"Failure" means every way of not translating, not just a provider error: a full
or dead worker queue, a multi-line or oversized message, and a buffer with
`outgoing` set but no language all refuse. Each of those was originally a
silent fall-through to a plaintext send, found in review. The whole decision is
one enumerated function, `outgoing_translate_policy`, so "nothing falls
through" can be checked by reading it rather than by tracing conditions.

### 5.1 The wire and the echo are ordered separately

An outgoing message is sent **as soon as its own translation resolves**. It must not
wait on the buffer's display queue: an incoming line stuck behind a slow provider
would otherwise delay the user's own message by up to the full timeout, which is a
worse failure than a cosmetic reordering.

Ordering is therefore split:

- **Wire order** — a per-connection FIFO ensures two outgoing messages reach IRC
  in submission order, so a fast second message cannot overtake a slow first one.
  Nothing incoming participates, and neither does any other connection: a single
  global queue would let one hung request on network A block translated sends on
  network B.
- **Echo order** — the local echo enters the buffer's display queue like any other
  row (§3.2), taking the `id` it was allocated at submission.

A consequence worth stating: with incoming lines pending, the user's own message can
appear on the wire before its echo appears on their screen. This is accepted. The
alternative — holding the send — makes the user's own typing hostage to someone
else's translation latency.

### 5.2 Every sender goes through the same gate

`/msg`, `/query <nick> <text>`, `/me` and the script senders address a target by
NAME and never touch `handle_plain_message`, so gating only there made the
per-buffer `outgoing` setting depend on HOW a message was submitted. The gate
therefore also runs inside `send_gated_message`, the single chokepoint all of
them share.

A `/me` is translated as its inner text: handing the `\x01ACTION …\x01` framing
to a translator returns anything but a valid CTCP, so the request carries the
prose and the deliver path re-wraps it. Every other CTCP is protocol and passes
through untouched.

---

## 6. E2E: translation is forbidden, fail-closed

Translating an E2E conversation would hand plaintext to a third-party provider,
destroying the guarantee E2E exists to provide. It is prohibited outright — there is
no opt-in flag.

The gate uses **`e2e_possible_for_target`** (`src/app/e2e_gate.rs:568`), never
`e2e_enabled_for_target` (`src/app/e2e_gate.rs:516`). The latter is advisory and
resolves read errors to `false`, so a damaged keyring would silently permit the leak.
`e2e_possible_for_target` is the fail-closed twin: `true` whenever E2E might be in
play — keyring read error, unresolved DM handle, pre-migration multi-network row.

This is not a new pattern. That predicate was written for the URL shortener, which
has the identical shape: an external service receives cleartext *before* the send gate
runs, so it may only be used when E2E is definitively ruled out. Translation is the
same class of leak.

**Placement matters.** The check runs at the point where a `TranslateRequest` is
built, in both directions — not only in `/translate addin`. That makes enabling order
irrelevant: `/e2e on` after `/translate addin` simply stops translation from the next
line, with no state to keep in sync. `/translate addin` on an already-encrypted
conversation is additionally refused up front, with an explanation.

---

## 7. Display and history

The stored text is exactly the displayed text. No new column, no migration, no
`original_text` on the wire message.

```
translate.show_original_in = on    11:32:33 alice> albalb [blabla]
translate.show_original_in = off   11:32:33 alice> albalb
```

Translation first, original appended in brackets. One line, never two.

Consequences, accepted deliberately:

- Toggling the setting does **not** rewrite history. Lines logged with brackets keep
  them. The log is a record of what was actually on screen.
- FTS5 search matches both the translation and the original, for free.
- Because the stored string is flat, **the dimmed tint on the bracketed original is a
  live-render-only affordance**. The in-memory `Message` carries a non-persisted byte
  offset marking where ` [original]` begins; a row reloaded from SQLite has no offset
  and renders the same characters undimmed. Re-deriving the offset by scanning for a
  trailing `[...]` is rejected: an ordinary message may legitimately end that way, and
  the renderer would dim someone else's brackets.

An `Untranslated` line with any reason other than `Filtered` renders the original
plus a dim marker.

---

## 8. Configuration and commands

Command names deliberately mirror the user's existing WeeChat and irssi scripts.

```
/translate list
/translate addin  <#channel|nick> [lang] [my-lang]   /translate delin  <target>
/translate addout <#channel|nick> <lang> [my-lang]   /translate delout <target>
/translate status
```

`/translate status` reports in-flight counts, queue depths, and failures grouped by
reason — the operational view needed to tell "the provider is down" from "the filter
is doing its job".

```toml
[translate]
enabled           = false
my_lang           = "pl"
show_original_in  = true
show_original_out = true
timeout_ms        = 5000
max_in_flight     = 4      # research finding: raising concurrency past measured
                           # provider limits makes throughput worse, not better
max_queue         = 200

[translate.buffers]
"libera/#dupa" = { incoming = true, outgoing = true, lang = "de" }
```

---

## 9. Stub backend

The branch ships a `StubBackend` so the entire mechanism runs and is testable with no
API key: a configurable delay, a deterministic transformation, and injectable
failures and slow responses.

It exists to prove the parts that are easy to get wrong — ordered release under
out-of-order returns, timeout releasing a stuck head, queue-ceiling overflow,
fail-closed E2E, outgoing refusal — before any real provider exists.

---

## 10. Error handling summary

| Situation | Behaviour |
|---|---|
| `Filtered` | original, no marker |
| `QualityGate` / `DailyLimit` / `NoProvider` / `Error` | original + dim marker |
| Timeout (5 s) | original + dim marker, head advances |
| Queue ceiling exceeded | oldest pending released untranslated |
| Outgoing translation fails | **not sent**; text restored to input + error row |
| E2E possible for target | never translated; no request built |
| Outcome for an unknown/released id | dropped, `tracing::debug!` |
| Broker channel closed | translation disabled, one error row, buffers flushed |

---

## 11. Testing

Unit-testable without an `App` (the queue is pure state over `AppState`):

- out-of-order outcomes release in `id` order (the line-5-before-line-4 case)
- a non-translated row (JOIN) queued behind pending lines does not overtake them
- timeout on a stuck head releases it and drains the followers
- queue ceiling releases oldest-first and never grows past the cap
- late outcome for a released id is dropped without panicking
- flush on buffer close / disconnect releases everything in order
- `add_message` is called exactly once per line, so SQLite gets one row
- E2E-possible targets never produce a `TranslateRequest`, including when the keyring
  read fails
- outgoing failure sends nothing and restores the input
- two outgoing messages reach the wire in submission order even when the first
  translates more slowly than the second
- an outgoing send is not delayed by an unrelated incoming line stuck in the queue

Integration, against `StubBackend`: a scripted channel burst with randomised
per-line delays must produce a buffer whose order matches arrival order exactly.

---

## 12. Non-goals

Out of scope for this branch, by explicit decision:

- **The translating layer itself** — parser, filter, masking, difficulty router,
  provider policy, HTTP client, quality gate. Specified separately.
- **Context windows.** Measured to degrade output; absent from the contract.
- **Batching several lines into one request.** Cheaper, but risks context bleeding
  between lines.
- **A translation cache.** Measured repeat rate among translatable lines is
  negligible.
- **Retroactive re-rendering of history** when a display setting changes.
- **Translating anything other than PRIVMSG and outgoing chat text** — no topics, no
  quit reasons, no CTCP.

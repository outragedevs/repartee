# IRCv3 `draft/multiline` — Design

**Date:** 2026-06-25
**Branch:** `feat/draft-multiline`
**Status:** Approved (pending written-spec review)

## 1. Goal

Full, bidirectional IRCv3 [`draft/multiline`](https://ircv3.net/specs/extensions/multiline)
support, matching the behaviour shipped by
[lurker](https://github.com/amiantos/lurker/commit/e79580eb90f5f3b66d2fd53088e2f5719e12315a)
and the spec as closely as practical:

- **Inbound**: reassemble a `draft/multiline` BATCH into a single logical message.
- **Outbound**: frame a multi-line (or over-long) message as a BATCH of
  `@batch`-tagged `PRIVMSG` lines, with `draft/multiline-concat` on wrapped
  continuations.
- Works **identically in the TUI and the web UI** (shared in-memory model and
  shared outbound send path).
- **Does not touch the E2E (RPEE2E) path.** This is a hard constraint:
  the multiline branch only ever runs for non-E2E plaintext.

### Non-goals (explicit scope cuts)

- **E2E messages never use multiline.** E2E keeps its existing chunker framing.
- `/me` (CTCP ACTION) stays single-line (the spec is silent on ACTION; lurker
  does not special-case it).
- Multi-line messages bypass the URL-shrink async path (shrink stays for
  single-line `≤ MESSAGE_MAX_BYTES` messages, as today). A multi-line message
  is usually over the byte budget anyway and already skips shrink.

## 2. Reference: what lurker / the spec require

From the spec and lurker's implementation:

- Capability `draft/multiline`, value is comma-separated `key[=value]`:
  - `max-bytes` — REQUIRED (server MUST advertise). Total byte length of the
    combined message value. Only the **last PRIVMSG parameter** counts; each
    joining `\n` contributes **1 byte**.
  - `max-lines` — RECOMMENDED. Max number of `PRIVMSG`/`NOTICE` in a batch.
- Wire framing: `BATCH +<ref> draft/multiline <target>` → N × `@batch=<ref>
  PRIVMSG <target> :<line>` → `BATCH -<ref>`.
- Receiver joins lines with a single `\n`, **except** lines carrying
  `draft/multiline-concat`, which are joined to the previous line with no
  separator. This tag marks a continuation produced by splitting one logical
  line that exceeded `max-bytes`.
- All lines MUST target the batch target. Mismatched → `FAIL BATCH
  MULTILINE_INVALID_TARGET`. Other server FAIL codes: `MULTILINE_MAX_BYTES`,
  `MULTILINE_MAX_LINES`, `MULTILINE_INVALID`.
- Clients MUST NOT send `concat` on a blank line, MUST NOT send an all-blank
  message, MUST NOT mix PRIVMSG and NOTICE in one batch.
- lurker: requests `batch` + `draft/multiline` + `message-tags`; one local echo
  per batch; **falls back to legacy splitting if the cap trio is not negotiated
  or `max-bytes < 350`**; defaults conservatively when dimensions are omitted.

## 3. Two invariants

1. **Internal representation = one `Message` with embedded `\n`** in its `text`
   (`src/state/buffer.rs`). The same `Message` feeds the TUI renderer and the
   web broadcast (one `WireMessage`). SQLite stores it verbatim — confirmed, no
   storage/query/FTS change needed.
2. **The wire always uses BATCH framing.** A raw `\n` must never be placed in a
   `PRIVMSG`: `IrcCodec::sanitize` (in `irc-proto-repartee`) truncates the line
   at the first `\n`. The BATCH framing is therefore both the spec-correct path
   and the only safe one.

## 4. Library finding — no fork changes required

Verified in `irc-repartee` 1.5.1 / `irc-proto-repartee` 1.2.2 (cargo registry
cache):

- `irc_proto::Message` has a **public** `tags: Option<Vec<Tag>>`, `Tag(pub
  String, pub Option<String>)` is public, and `Display`/`IrcCodec::encode`
  serialise tags (`@key=value …`) with correct escaping.
- `Sender::send<M: Into<Message>>` accepts a raw `Message`, so we build and send
  `Message { tags: Some(..), prefix: None, command: PRIVMSG(target, line) }`
  directly.
- `Command::BATCH(String, Option<BatchSubCommand>, Option<Vec<String>>)`
  serialises correctly outbound; `BatchSubCommand::CUSTOM("draft/multiline")`
  covers the vendor type. We put the `+`/`-` in the ref-tag string ourselves.
- Inbound: tags already parsed into `msg.tags`; `BATCH` parsed into
  `Command::BATCH` with subcommand `CUSTOM("DRAFT/MULTILINE")` (uppercased by
  `FromStr`).
- The client's outgoing penalty throttle (RFC 2813 §5.8) sends in FIFO order;
  batch lines may be spread in time but ordering holds and the batch stays open
  until `BATCH -ref`. Batch lines use the **normal** lane, never the priority
  lane.

**Conclusion: all work is in `repartee`. No change to `irc-repartee` /
`irc-proto-repartee`, no crates.io republish.**

## 5. Architecture & components

### 5.1 New module: `src/irc/multiline.rs` (pure, heavily unit-tested)

Single home for the spec logic. No I/O, no `AppState`.

```rust
/// Server-advertised per-batch limits.
/// `max_bytes` is the TOTAL combined payload of one batch: the sum of every
/// line's content bytes PLUS one byte for each joining '\n'. It is NOT a
/// per-line limit. The per-PRIVMSG wire cap is the separate existing
/// `MESSAGE_MAX_BYTES` (350).
pub struct MultilineLimits { pub max_bytes: usize, pub max_lines: usize }

/// Parse the `draft/multiline` cap value. `None` => unsupported / unusable.
/// Returns None if max-bytes is absent or < MESSAGE_MAX_BYTES (350)
/// (lurker: "rejects servers with max-bytes below one full wire message").
/// max-lines absent => conservative default (MULTILINE_DEFAULT_MAX_LINES).
pub fn parse_limits(cap_value: Option<&str>) -> Option<MultilineLimits>;

/// One physical wire line within a batch.
pub struct WireLine { pub text: String, pub concat: bool }

/// Partition the original (possibly multi-line) plaintext into one or more
/// batches. Algorithm, in order:
///   1. Split `text` on user `\n` into logical lines (concat=false).
///   2. Any logical line whose content exceeds the per-PRIVMSG cap
///      (`MESSAGE_MAX_BYTES`) is split at word boundaries (reuse
///      `split_irc_message`); the 2nd..nth pieces get concat=true. All pieces
///      of one logical line stay in the SAME batch (a concat run must never be
///      split across batches).
///   3. Pack lines into batches, opening a new batch when EITHER limit would be
///      exceeded: line count > max_lines, OR cumulative payload (content bytes +
///      1 per joining '\n') > max_bytes. Batch boundaries fall on logical-line
///      boundaries only.
///   4. Blank-line rules: never concat on a blank line; never emit an all-blank
///      batch.
/// Pathological case (a single logical line whose concat pieces alone exceed
/// max_lines or max_bytes) cannot be represented as multiline => caller falls
/// back to legacy `split_irc_message` for that message.
pub fn partition(text: &str, limits: &MultilineLimits) -> Vec<Vec<WireLine>>;

/// Reassemble one batch's collected inbound lines into one logical string.
/// join with '\n' unless a line carries concat (then no separator).
pub fn reassemble(lines: &[WireLine]) -> String;

/// True if `text` must be sent as multiline: it contains '\n', OR its byte
/// length exceeds the per-PRIVMSG cap (`MESSAGE_MAX_BYTES`), i.e. a single
/// over-long line that benefits from seamless concat reassembly.
pub fn needs_multiline(text: &str) -> bool;
```

Constants in `src/irc/mod.rs`: reuse `MESSAGE_MAX_BYTES = 350` as the
per-PRIVMSG content cap; add `MULTILINE_DEFAULT_MAX_LINES` (conservative, e.g.
24) and `MULTILINE_MAX_INBOUND_LINES` (runaway backstop for inbound
reassembly).

### 5.2 Capability negotiation & limit capture

- `src/irc/cap.rs`: add `"draft/multiline"` to `DESIRED_CAPS`
  (`batch`, `message-tags` already present).
- `src/irc/events.rs`: capture the **cap value** at negotiation. Today
  `handle_cap_new` (and the LS path) strip `=value` (`events.rs:477`). Parse the
  `draft/multiline` value via `multiline::parse_limits` and store it. Re-parse on
  `CAP NEW`. On `CAP DEL`/NAK, clear it.
- `src/state/connection.rs`: add
  - `pub multiline: Option<MultilineLimits>` (None => unsupported),
  - `batch_ref_counter` + `fn next_batch_ref(&mut self) -> String`
    (monotonic, e.g. `"rml{n}"`; unique among concurrently-open outbound
    batches; deterministic, no `rand`).

`multiline_supported(conn)` ⇔ `conn.multiline.is_some()` **and**
`enabled_caps` contains `batch` and `message-tags`.

### 5.3 Inbound: `src/irc/batch.rs`

The batch interception in `src/app/irc.rs:572-624` already buffers
`@batch`-tagged messages and calls `process_completed_batch` on `BATCH -ref`.
Add an arm for `"DRAFT/MULTILINE"`:

- Build `WireLine`s from the collected `PRIVMSG` lines (text = last param;
  `concat` = presence of `draft/multiline-concat` tag).
- Cap at `MULTILINE_MAX_INBOUND_LINES` (drop/truncate excess, `log` it — no
  silent cap).
- `reassemble()` into one string.
- Synthesise one `Message` using the prefix/nick/`@time`/`@msgid` of the
  **first** line and the batch target, then route it through the normal
  delivery (`handle_irc_message` on a reconstructed single PRIVMSG, or directly
  build the buffer `Message`). This makes it land in the buffer once and
  broadcast to web once.
- Empty batch (no usable lines) → no message.

Unknown/again-unknown batch types keep the existing default arm
(`batch.rs:308` replays individually).

> E2E note: a remote peer using E2E will not also use multiline (they are
> mutually exclusive). Defensively, the reassembled message flows through normal
> processing, so an `+RPE2E01` payload would still hit the decrypt path and fail
> gracefully if one ever appeared. No E2E logic changes.

### 5.4 Outbound: `src/app/input.rs::handle_plain_message`

Insert the multiline branch **after** the existing
`e2e_encrypt_or_passthrough` call, branching on its result so E2E is untouched:

1. (unchanged) DCC early-return.
2. **Shrink gate** gains `&& !text.contains('\n')` so multi-line messages skip
   the async shrink path (and thus `apply_shrink_deliver` needs no changes).
   Over-long single lines already skip shrink (`len > MESSAGE_MAX_BYTES`).
3. (unchanged) `let (wire_lines, plain_echo) = e2e_encrypt_or_passthrough(...)`.
4. (unchanged) compute `is_e2e_encrypted` from `wire_lines`.
5. **NEW multiline branch — only when `!is_e2e_encrypted`** and
   `multiline_supported(conn)` and `needs_multiline(text)`:
   - `let batches = multiline::partition(text, &limits)` (partition the
     **original** `text`, never the passthrough `wire_lines`).
   - For each batch: `ref = conn.next_batch_ref()`; send
     `Command::BATCH("+{ref}", Some(CUSTOM("draft/multiline")), Some(vec![target]))`,
     then each `WireLine` as `Message { tags: [batch=ref (+ multiline-concat if
     line.concat)], command: PRIVMSG(target, line.text) }` via
     `handle.sender.send(msg)`, then `Command::BATCH("-{ref}", None, None)`.
   - **Local echo (once)**: if `echo-message` is **off**, add a single buffer
     `Message` whose `text` is the full `\n`-joined plaintext. If `echo-message`
     is **on**, skip local echo — the server echoes the batch back and §5.3
     reassembles it into one displayed message (no double display).
   - `return`.
6. (unchanged) Otherwise: existing `for wire in wire_lines { send_privmsg }`
   loop + existing echo. Covers E2E, single-line, and the no-cap fallback.

The web `SendMessage`/`RunCommand` path converges on `handle_submit ->
handle_plain_message`, so the web automatically gets multiline once it sends a
single `\n`-bearing message (see §5.6).

### 5.5 TUI rendering of `\n` — `src/ui/mod.rs::wrap_line`

`wrap_line` (`src/ui/mod.rs:170`) tokenises by grapheme and does not honour
`\n`. Change: treat `\n` as a **hard line break** before grapheme tokenisation
— split the input into segments on `\n`, wrap each segment independently, and
emit a forced break between segments (an empty segment yields one blank visual
line). All consumers then become correct automatically:

- scroll/height math in `src/ui/chat_view.rs` (`compute_render_budget`,
  `visual_lines`) consumes `wrap_line` output, so it stays in sync.
- `MAX_WRAPPED_LINES_PER_MSG` (chat_view.rs) may need raising or the budget cap
  reconsidered for tall messages; verify with tests.
- emote layout (`src/ui/emote_layout.rs`, `message_line.rs::emotify_message_text`)
  must keep placeholder width runs correct across the new line boundaries —
  verify and adjust.

This change affects all messages, not just multiline; it must be covered by
tests (single-line behaviour unchanged; `\n`, blank lines, CJK/emoji widths).

### 5.6 Web UI

- **Rendering**: already correct. `web-ui/src/format.rs` preserves `\n` in
  spans, and CSS `.chat-line .text { white-space: pre-wrap }` renders breaks.
  `snapshot.rs` passes `text` verbatim. No change.
- **Send path**: `web-ui/src/components/input.rs:282` currently loops
  `text.lines()` sending one `SendMessage` per line. Change so **plaintext** is
  sent as a single `SendMessage` with the full `\n` text; lines starting with
  `/` (commands) keep per-line `RunCommand`. The server then runs the shared
  multiline branch (§5.4).
- **Compose**: the web textarea already produces `\n` naturally.

### 5.7 TUI compose (multi-line input)

- `src/app/input.rs` / `src/ui/input.rs`: bind **Alt+Enter** to insert `\n`
  into the input buffer; **Enter** submits the whole (possibly multi-line)
  buffer. Shift+Enter also inserts `\n` on terminals that report it distinctly
  (Kitty keyboard protocol / crossterm enhanced flags); Alt+Enter is the
  reliable cross-terminal binding.
- The input widget must render a multi-line buffer (cursor across lines). Keep
  the existing single-line fast path when the buffer has no `\n`.

### 5.8 Errors & UX parity

- `src/irc/events.rs`: handle `FAIL BATCH MULTILINE_MAX_BYTES |
  MULTILINE_MAX_LINES | MULTILINE_INVALID_TARGET | MULTILINE_INVALID` → themed
  message in the server/status (or target) buffer. These arrive after send;
  no automatic retry (surface to the user).
- Parity nicety (lower priority): a small "multiline/×N" indicator and counting
  batches (not wire lines) wherever the UI shows long-message split/flood hints.
  Include if an existing indicator exists; otherwise defer.

## 6. Data flow

**Outbound** (TUI compose / web textarea / paste) → text with `\n` reaches
`handle_plain_message` → (not E2E, cap ok, needs multiline) →
`multiline::partition` → BATCH frame through the normal throttle (FIFO) → one
local echo (or none if echo-message on).

**Inbound** `BATCH +ref draft/multiline target` → lines buffered
(`app/irc.rs:619`) → `BATCH -ref` → `reassemble` → one `Message` → buffer →
TUI (`wrap_line` after §5.5) + web (`pre-wrap`).

## 7. Edge cases / robustness

- **E2E**: multiline branch gated on `!is_e2e_encrypted`; E2E send/echo/rekey
  and `apply_shrink_deliver` byte-for-byte unchanged. Shrink gate excludes
  `\n` messages.
- **echo-message on/off**: exactly one display either way; no double echo.
- **Throttle** spreads a large batch over time — safe (batch open until `-ref`,
  bounded by `max-lines`); never use the priority lane.
- **`> max-lines`** → multiple sequential batches. **Single line >
  `max-bytes`** → word-boundary split with `concat` continuations.
- **Fallback to legacy** when: cap trio not negotiated, OR `max-bytes` absent,
  OR `max-bytes < 350`. Identical to today's behaviour.
- **`max-lines` omitted** → conservative default; never unbounded.
- **Blank-line rules**: no `concat` on blank lines; never an all-blank message;
  no empty batch.
- **CAP NEW / CAP DEL** mid-session re-parse/clear limits.
- **Reconnect / chathistory**: a multiline message is one stored row; replay is
  unchanged.
- **Inbound runaway** (huge batch) → `MULTILINE_MAX_INBOUND_LINES` cap, logged.
- **URL preview / mentions / nick-colour** operate on the joined text — work
  unchanged on multi-line content.

## 8. Testing

- **Pure unit** (`multiline.rs`): `parse_limits` (missing/garbage/`<350`/no
  max-lines), `partition` (byte & line boundaries, concat on wrap, user-newline
  no concat, UTF-8 boundaries, blank-line rules, multi-batch overflow),
  `reassemble` (concat/blank/unknown tags), and a **property test**: for
  arbitrary `x`, joining each batch's `reassemble(batch)` with `\n` (batch
  boundaries fall on logical-line boundaries) reconstructs `x` — i.e.
  `partition` then `reassemble` is lossless across the batch split.
- **`wrap_line`**: `\n` hard breaks, blank lines, CJK/emoji widths, scroll/height
  parity, single-line regression.
- **Integration**: simulated inbound `draft/multiline` batch → one `Message`;
  outbound → exact wire (`assert` on `Message::to_string()` incl. tags & BATCH
  frame); echo-message on vs off (one display).
- **Web**: `\n` render; single `SendMessage` for plaintext, per-line for `/`.
- **Safety**: any test that could allocate unboundedly (partition on huge input)
  runs under `ulimit -v` + `timeout` (per the prior OOM lesson).
- `make clippy` + `make test` must stay at 0 warnings (pedantic/nursery/perf).

## 9. Phasing

1. `multiline.rs` core + pure tests (zero runtime risk).
2. Inbound: cap value capture + `batch.rs` arm + `wrap_line` `\n` — immediately
   improves readability of others' multiline messages in TUI **and** web.
3. Outbound: `handle_plain_message` branch + ref counter + single echo +
   fallback; TUI Alt+Enter compose; web single-`SendMessage`.
4. FAIL handling + edge cases + integration tests + optional UI indicator.

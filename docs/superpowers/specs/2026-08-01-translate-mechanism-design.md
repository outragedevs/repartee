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

### 2.0 The backend is untrusted

Its output goes onto an IRC socket under the user's nick, so it is validated on the
way in, not assumed well-formed. **A `Translated` answer must be one line.**
`IrcSender::send_privmsg` breaks only on `\r\n`; a bare `\n` is copied into the
trailing parameter verbatim, and a server that accepts bare-LF line endings reads
everything after it as a fresh command — `hello\nJOIN #evil` would join a channel.

`single_line_or_refuse` trims trailing CR/LF (a model ending its answer with a
newline has produced a correct translation with a stray byte on it) and refuses
anything with a break embedded. Refusing rather than sanitising, because the contract
is one line per request: a multi-line answer is a broken response whatever it says,
and picking one of its lines to publish under the user's nick is not a guess worth
making.

The deliver path re-checks every wire payload immediately before sending. That is
deliberate duplication — the last point before bytes reach the socket, making the
refusal a property of the send rather than of one upstream check, exactly as
`build_outgoing_translate` re-runs the E2E gate.

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
up to `max_in_flight` — one limiter shared by the incoming worker and every
per-connection outgoing lane, because the setting is documented as the *provider's*
concurrency cap. A lane that skipped it would make the real ceiling `max_in_flight +
one per connected network`. The queue governs only *when a resolved line is allowed
onto the screen*.

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

  The **worker** measures from the same instant, not from the moment the request
  reaches the backend. A request can wait a long time first — behind another on its
  connection's serial lane, or on the shared `max_in_flight` permit — and starting
  the clock afterwards makes the deadline meaningless: the reservation expires on
  schedule while the request keeps running. For incoming that wastes a provider call
  whose answer the queue has already discarded; for outgoing it puts the message on
  the channel long after the user gave up, quite possibly after they retyped it. So
  `translate_isolated` bounds the call to the REMAINING budget and skips it entirely
  when none is left, and the outgoing deliver refuses anything that arrives past the
  deadline anyway.
- **Queue ceiling**, default 200 entries per buffer, enforced **on every insertion**
  rather than on the maintenance tick. On overflow the oldest pending entries resolve
  as `Untranslated { Timeout }` immediately and release, so the channel keeps flowing
  untranslated instead of stalling. Checking once a second is not a bound: a stalled
  provider and a busy channel can put hundreds of lines in a queue between two ticks,
  and this number is documented as a limit on memory *and* on how far behind the
  display may fall.

Late outcomes for an already-released id are dropped, with a `tracing::debug!`.

### 3.5 Flush points

Pending entries must be released — untranslated, in order — rather than lost, when:
the buffer is closed (`/close`, `/part`), the connection drops, or the app quits or
detaches. `/translate delin` releases only its own direction — see §3.7.

### 3.6 A buffer id is not a stable key

Everything per-buffer is keyed by buffer id, and a query's id contains the peer's
nick — so the peer typing `/nick` re-keys the buffer out from under all of it. Left
behind, the queue keeps releasing lines toward a buffer that no longer exists (they
are dropped) and the per-buffer settings stop matching, so translation silently stops
mid-conversation for a reason the user has no way to see.

`rekey_buffer_state` moves the state-side maps as part of the rename. The config map
belongs to `App`, so the rename is also queued in `pending_buffer_rekeys` and drained
after the IRC message, alongside `pending_web_events` — and it must be, because
`sync_translate_from_config` re-derives the mirror from the config and would otherwise
undo the move on the next `/set`.

Moving the maps is not enough on its own: **work already handed to the workers carries
the buffer id and target name it was dispatched with**, and no amount of re-keying
reaches into an in-flight request. So the rename also records a redirect, `old_id ->
new_id`, consulted when a result comes back. Without it an incoming outcome finds no
queue and its line sits until the timeout, and — worse — an outgoing send addresses a
nick its owner no longer answers to, which if somebody else has claimed it in the
meantime means sending it to a stranger.

The redirect applies on **time**, not on which buffers exist: work dispatched before
the rename belongs to the conversation that moved, work dispatched after belongs to
whoever holds that nick now. Both questions have to be answered at once and only the
timestamp answers them — which is why `submitted_at` (§3.4) is on both pending
structs. Redirects also expire.

An earlier version keyed on "is there a live buffer under the old id". That gets the
second case right and the first case badly wrong: it leaves the deliver addressing the
stale NAME, so a pending private message is handed to the stranger who claimed the
nick. When a redirect applies but its target window is gone, the send is therefore
**refused** rather than falling through to the old name.

The migrated key is not written to disk. `/translate add*|del*` writes the file and
will carry it along next time; rewriting `config.toml` in response to somebody else's
`/nick` is I/O the user did not ask for. So the setting follows the peer for this
session, and a restart keys it by the nick they actually typed.

### 3.7 Disabling one direction must not flush the other

`delin` and `delout` share a queue, but not its contents. The only outgoing thing in
it is a *reservation* — a place held for an echo whose translation is already running
and will still come back.

So `delout` flushes **nothing**: there is no outgoing work to release, and the full
flush it used to do forced the buffer's incoming lines out as timeouts, punishing a
direction that is still switched on. `delin` releases the pending incoming lines
(they must not be lost — the user saw them arrive) via `flush_pending`, which leaves
reservations intact; dropping one lets the rows queued behind it render first, and
the user's own message then appears below the replies to it.

### 3.8 Lifting a barrier is a release event

An outgoing message holds a `Reserved` slot in its buffer's queue (§5.1). While it
sits at the head it is a barrier: every line that finishes translating behind it is
`Resolved` but undeliverable.

That reservation goes away in exactly two ways — it is **filled** by the local echo,
or **released** because no echo will come. Both make the head deliverable, so both
must be followed by a drain. They are the only release events that do not arrive
through `resolve_incoming_translation`, and the outgoing delivery arm never revisits
the queue afterwards, so a bare `release_echo_slot` leaves the echo *and* the replies
queued behind it invisible until the next one-second maintenance tick.

`App::release_echo_slot_and_drain` pairs the two so the delivery path cannot do one
without the other; the fill case drains explicitly at the end of
`send_outgoing_translated`. Reservations released *at the tail* — a dispatch that
fails its `try_send` the moment after reserving — need no drain, because a tail entry
was never blocking anything.

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
  row (§3.2), taking the `id` it was allocated at submission. When it splits into
  several rows — which `show_original_out` makes routine, because the appended
  original pushes it past the byte budget — *all* of them occupy that one reserved
  place. Fresh ids for the continuations would let a line that arrived during the
  translation sort between them, so the buffer would show the first chunk, somebody
  else's reply, then the rest of the user's own sentence.

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

None of these require an OPEN buffer. A script's `say()` addresses a target by name
and has always been able to reach a conversation the user has no window on; refusing
because there is no buffer would drop sends that used to go out. Translation is a
property of the conversation, not of whether it is on screen — all the buffer
contributes is the nick list, and its absence just means an empty one.

### 5.3 A send that fails after the wait

The composer is cleared at submission, so from the moment a message is dispatched
the only copy of the user's text is inside the pending request. Between dispatch and
delivery the connection can go away — the handle precheck can pass and the send fail
anyway, because the writer task dies independently of the map entry.

Three things have to happen on that path, and which of them depends on how far the
send got:

- **The reservation always goes back** (§3.8), whether or not anything was sent.
- **The error row always carries the text.** This is not belt-and-braces: the
  composer restore is a no-op whenever the user has started typing again, because
  overwriting the next message would be a worse surprise than a lost one — and a
  deferred failure arrives *seconds after* the user pressed Enter, so a busy composer
  is the normal case, not the edge. A row naming only the reason therefore loses the
  message outright while telling the user it was handed back.
- **Nothing was sent** → the retry text is also restored to whoever submitted it,
  exactly as an up-front refusal does.
- **A split message got partway** → the first chunks are already on the channel, so
  the text stays in the row and is deliberately **not** restored to the composer:
  handing back the whole line invites the user to press Enter and publish those
  chunks a second time. The row says so, rather than leaving the truncation to be
  discovered from the channel.

"Whoever submitted it" is the origin captured at dispatch, never the current one —
see §5.7. **What** is handed back is a different question, decided at restore time —
see §5.6.

Every deferred failure runs through `abandon_with_text`, so the rule cannot be
forgotten at one of the four sites that need it.

### 5.4 The connection must be the same session

`irc_handles` is keyed by `conn_id`, and a reconnect reuses that id with a
replacement handle — so "is there a handle" cannot distinguish a live connection
from its successor. A message dispatched before a drop would otherwise be sent on the
session that replaced it: on a channel the user may have left, minutes after they
typed it, quite possibly after they gave up and retyped it by hand. Disconnect
handling flushes display queues but does not cancel the outgoing lane, so nothing
else catches this.

`App::conn_generations` counts sessions per `conn_id`, bumped when a handle is
installed. It is captured with everything else that can move during the wait (§5) and
compared at delivery; a mismatch refuses like any other deferred failure. Bumping on
install rather than on disconnect is what makes a captured value mean "the session I
was written for" — a connection that never comes back is already caught by the handle
being absent.

Both checks run **before** `plan_translated_wires`, because planning calls
`e2e_encrypt_or_passthrough`, which creates or rotates the outgoing session and queues
REKEY NOTICEs. Planning and then failing to send advances our key past a pending
rotate while the peers never receive the new one — the same ordering, for the same
reason, as `send_gated_message`'s own precheck.

### 5.5 `echo-message` reflections are decorated, not replaced

A server advertising `echo-message` reflects our own PRIVMSGs back, which is why the
outgoing paths skip the local echo — the reflection *is* the row, and it reaches the
buffer, the web clients and `SQLite` through the ordinary incoming path.

A translated send with `show_original_out` on cannot be shown by that reflection
alone: the wire carried the translation and nothing else. So the send files a
**decoration** in `own_echo_decorations` — the wire line, what to display instead,
and the `WireOrigin` to record — and `handle_privmsg` applies it when the reflection
arrives.

Decorating rather than writing our own row and dropping the reflection is the point.
The reflection is the copy carrying the server's `@time` and `@msgid`, and those are
exactly what a later CHATHISTORY replay of the same message dedups against (§7.1). A
locally-authored row has a local clock and no msgid, so it matches nothing and the
message comes back a second time — trading one display bug for a duplication bug.

Details that follow from the shape:

- **Only the last wire line is decorated.** A translation long enough to split is
  several reflections and one original; repeating it on each would say the same thing
  N times, and putting it on the first would place it before the text it is the
  original of.
- **An action is decorated inside its frame** (`\x01ACTION … [original]\x01`), because
  matching happens before the CTCP is unwrapped and the result still has to parse as
  one. The recorded `WireOrigin.text` is the frame's BODY, since that is what the row
  holds and what a replay carries.
- **Matching is consuming**, so the same text sent twice files two records and each
  reflection takes one, oldest first.
- **A miss renders the reflection plain** — a netsplit between send and echo, a server
  that rewrites what it reflects, a stranger's identical line. The mechanism can lose
  a suffix; it can never lose or duplicate a message.

### 5.6 A retry must not be aimed at the wrong conversation

`retry_form_for` decides what a refused message should look like in the composer:
the bare body when the active buffer is already the target, an explicit
`/msg <target> …` otherwise. That reasoning is right and its purpose is exactly this
leak — `/msg bob secret` restored as `secret` publishes private content to whatever
channel is open.

But it runs at **dispatch** time, and a deferred send fails seconds later. Switching
away while waiting is the natural thing to do, and by then the composer may belong to
a public channel. So the form is chosen again at restore time, from where that client
is looking now — the TUI's active buffer, or the submitting session's active buffer
for a web client.

Two cases have no safe form and are therefore **not restored at all**, with the text
left in the error row:

- **An action.** `/me` acts on the active buffer and has no re-addressed spelling.
- **A target whose buffer is gone.** Its name is no longer proof of anything: on a
  query, somebody else may have claimed the nick since (§3.6). Re-addressing would
  hand the user a ready-to-send private message aimed at a stranger — the same leak,
  one keystroke away.

### 5.7 A refusal goes back to the client that typed it

Every submitting path scopes `App::submit_origin` for the duration of the submit, so
a refusal restores the text where its author is looking. That includes both web
arms: the browser composer sends plain lines as `SendMessage` and every `/`-prefixed
line as `RunCommand`, and both end in the same `handle_submit`. Scoping only the
first would return a refused `/msg` to the *terminal's* input line — lost for its
author, and dropped into a window nobody is watching.

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

A line marked this way is **final**, and the fallback path delivers it through
`add_message_with_activity_unshrunk` to keep it that way. Handing it back to
`add_message_with_activity` would offer it to incoming URL shrinking: a second
external service would rewrite text already marked as untranslated, and overwrite the
`wire_origin` that carries both the marker offset and the identity (§7.1). Translation
and shrink are mutually exclusive per line by design, and that has to hold on the
failure path too.

The offset travels to the browser as `WireMessage::orig_offset`, and the web renderer
splits the body there and wraps the tail in its own element. Both frontends therefore
show the same line, and neither guesses: a client that receives no offset renders the
text whole. It is omitted from `stored_to_wire` for the same reason the TUI has none
for a reloaded row.

### 7.1 Display text is not identity

A row whose displayed text is not the text that crossed the wire carries
`Message::wire_origin`, holding the wire text alongside that offset. Both dedup paths
key on it — `maybe_log`'s synthetic `msg_id` and `buffer_contains_history_row` — and
never on the displayed text.

Without it a msgid-less server duplicates every translated line. A CHATHISTORY replay
bypasses translation entirely and carries what the network sent, so an identity
derived from the display text differs between a line and its own replay: the unique
`(network, msg_id)` index stops collapsing them, and the reconnect gap-fill splices a
second copy into the buffer. Servers *with* `@msgid` were never affected, which is
what made this invisible.

Direction matters, and the field records the wire text rather than "the original" for
exactly that reason: for an incoming line the wire carried the peer's original, for
our own outgoing echo it carried our translation.

Incoming shrink rewrites text the same way and had the same defect; it now records its
wire text too.

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

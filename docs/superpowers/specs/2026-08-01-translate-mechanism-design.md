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

**CTCP framing is the client's to create, never the backend's.** `\x01` is what
tells every IRC client that a PRIVMSG is a *request* rather than prose, and a plain
translation reaches `send_privmsg` verbatim. An answer of
`\x01DCC SEND secrets.txt …\x01` would therefore put a file-transfer offer on the
wire under the user's own nick — one line, correctly correlated, non-empty, well
inside the byte ceiling, so every other check at the seam waves it through. The
subtler form is an ACTION: `/me` wraps the answer in `\x01ACTION …\x01` *we* build,
so a bare `\x01` inside the body closes that framing early and opens a second,
backend-chosen request behind it. Incoming is not exempt — a reply carrying
`\x01ACTION …\x01` renders as an action from the peer, attributing words to them
they never said — which is why the check lives at the seam and covers both
directions.

The deliver path re-checks every wire payload immediately before sending. That is
deliberate duplication — the last point before bytes reach the socket, making the
refusal a property of the send rather than of one upstream check, exactly as
`build_outgoing_translate` re-runs the E2E gate. The line-break check is on the wire
payloads; the `\x01` check is on the **body**, before wrapping, because the wire
payloads are where our own action delimiters legitimately live and there is no way
to tell ours from smuggled ones once they are in the same string. Every `\x01` that
reaches IRC from this path is one `wrap_outgoing_body` put there.

An **empty answer** is refused as well, whitespace-only included. Accepted, it renders an
incoming line blank whenever the original is hidden, and on the outgoing side puts an
empty PRIVMSG on the channel, reports the send as done, and discards the text the user
typed — a silent loss of their message in exchange for nothing.

The **correlation id** is checked, not believed. An outcome carrying a different id
than the request it answers is refused and re-labelled to the id we asked about.
Trusting it resolves the queue slot the backend named: one line's translation applied
to another — published under the user's nick, in the wrong conversation — while the
line it belonged to sits pending until the timeout. Both halves of that are silent,
which is what makes it worse than a visible failure.

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
one per connected network`.

Lowering the limit can only take permits that are AVAILABLE, so the remainder is
carried as a **debt** — and where that debt is paid is load-bearing. Tokio hands a
returned permit straight to the next waiter, so while anything is queued for a permit
none ever becomes available: a reduction retried only from the tick would go on being
ignored for as long as the traffic lasts, which is exactly when a provider's cap
matters. The debt is therefore an atomic shared with the workers, and each pays a unit
down at the one place permits are handed out (`acquire_translate_permit`) by retiring
the permit it just took instead of using it. The traffic that blocks the reduction is
what applies it. The tick still settles opportunistically, for the opposite case: a
reduction made while requests were in flight, after which the traffic stops and nothing
would acquire again.

Both paths claim their units from the counter *before* touching the semaphore — the
worker one at a time, the tick by taking the whole balance and handing back what it
could not use — so the two cannot pay for the same unit twice and drop the ceiling
below what was asked for. Raising the target cancels outstanding debt before minting
anything: those permits still exist, they were merely promised away, which is also
what stops an abandoned reduction from settling the limiter at a number the config no
longer asks for.

The permit count, the running total and the debt live in **one** object,
`TranslateLimiter`, **behind a lock**, and that is a correctness requirement rather
than tidiness. The ceiling in force is `total - debt`, so those two and the semaphore
are one piece of state under one invariant, not three counters: every change moves at
least two of them together, so none can be read or written on its own. Two attempts to
express that with individually-atomic counters both wedged the client:

- `total` on `App` while the workers retired permits for themselves. It went stale
  high, the effective ceiling read back as the value from before the reduction, and the
  next `sync_translate_from_config` — every `/set`, `/reload` and `/translate add*` —
  applied the same reduction again until no permits were left.
- `total` and `debt` as separate atomics. `retune` read `debt`, worked out how much of
  it to cancel, and subtracted; a worker paying a unit in between made the subtraction
  underflow to `usize::MAX`. The effective ceiling then reads zero and every worker
  retires the permit it just took, forever.

Neither is visible to a single-threaded test, and the second is not visible to any test
that lets its workers release permits promptly — a reduction taken while the limiter is
idle settles immediately and leaves no debt to race on. So the invariant `total > debt`
is asserted after every mutation inside the limiter, and the stress test holds permits
across an await while `retune` runs on another thread. A reduction also never records a
debt that would take the ceiling below one, which is what stops `acquire` retiring its
way into a permanent block.

The queue governs only *when a resolved line is allowed onto the screen*.

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

"Everything" has to mean everything, and two paths were not covered: the day separator
(`add_local_message`) and E2E placeholders (`add_transient_message_with_activity`) both
appended straight to the buffer. A pre-midnight line still being translated then
rendered BELOW the separator that is supposed to date it, and a placeholder rendered
above the lines queued before it.

They now take a resolved place in the queue like any other row — but they keep their own
delivery rules coming out of it, which is why a released entry carries a
`ReadyDelivery`: `Logged` for ordinary chat, `Transient` for a placeholder that must
never be persisted, `Local` for a client-generated line that neither logs nor escalates
activity. A placeholder that started being logged because it went through a queue would
be a worse bug than the reordering.

Command output is deliberately NOT routed this way. `add_local_message` still appends at
once, because holding `/help` behind a pending translation reads as a hung client; the
distinction is chronological rows, which belong in the timeline, versus responses to
something the user just did.

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
- **Queue ceiling**, default 200 entries per buffer, enforced on **every** insertion —
  translated lines, the untranslated fallbacks, non-translatable rows like JOINs and
  notices, echo reservations, and split echoes alike. They all occupy the same queue,
  so they all count; a bound that some insertions skip is not a bound. On overflow the
  oldest pending entries resolve as `Untranslated { Timeout }` immediately and release,
  so the channel keeps flowing untranslated instead of stalling.

  Checking on the maintenance tick instead is not a bound either: a stalled provider
  and a busy channel can put hundreds of lines in a queue between two ticks, and this
  number is documented as a limit on memory *and* on how far behind the display may
  fall. For the same reason `AppState`'s mirror of it has to be right from the first
  line, so every `[translate]` mirror is derived through `sync_translate_from_config`
  — including at startup, where hand-copying the fields had left this one at its
  default until the user next ran `/set`, `/reload` or `/translate`.

Late outcomes for an already-released id are dropped, with a `tracing::debug!`.

### 3.5 Flush points

Pending entries must be released — untranslated, in order — rather than lost, when:
the buffer is closed (`/close`, `/part`, a kick), the connection drops, or the app
quits or detaches. `/translate delin` releases only its own direction — see §3.7.

The flush happens **inside `AppState::remove_buffer`, before the buffer is removed**,
and the order is the whole point. Those lines already arrived from IRC — the queue
governs only when they are allowed on screen, not whether they were received — so
dropping them loses messages the user was sent and never writes them to storage.
Afterwards is too late: the buffer-existence guard in
`add_message_with_activity_unshrunk` refuses every delivery to a buffer that is gone,
so a flush placed after `shift_remove` silently does nothing at all. An earlier
version dropped the queue outright and justified it with exactly that guard — the
refusal was the bug, not the reason.

It lives on `AppState` and not only on `App` because `remove_buffer` is reached from
the IRC event path (a `PART`, a `KICK`), which has no way back up to the App to ask
for a flush first.

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

A buffer id is a **nick**, and a nick passes from one conversation to the next, so one
mapping per id is not enough. Consider: the user sends alice a translated private
message; alice renames to alicia; a stranger claims the freed nick `alice`; the
stranger renames too. Both renames are off `test/alice`, so a last-write-wins map keeps
only the second — and the message still in the translator, dispatched to alice,
resolves to the stranger's new window. `buffer_redirects` therefore holds a **list of
eras** per id, each carrying the window it covers (`started_at`..`ended_at`) and where
that occupant lives now; a result is matched against the era that was current when it
was dispatched. The list is bounded and pruned from the front, which is safe precisely
because every era carries its own start: dropping one cannot widen the one behind it.

The lookup answers with **three** states, not two, and an earlier version of this
paragraph claimed a refusal the code did not perform. `Option` conflates "no rename is
on record" with "we can no longer tell", and those call for opposite actions: the first
means the conversation never moved and the send proceeds, the second means a rename may
have aged out of the history, so sending under the recorded NAME could hand a private
message to whoever holds that nick now. `BufferRedirect::Unknown` is returned when the
work was dispatched further back than the history reaches — past `REDIRECT_TTL`, or
before the earliest era still held — and the outgoing delivery path REFUSES it.

That is reachable in configuration, not just in theory: `translate.timeout_ms` has a
floor of 500 ms and no ceiling, so a request may legitimately stay in flight longer than
the five minutes of rename history kept for it. The incoming path treats `Unknown` as
"stays put" instead, because nothing is sent from there — the outcome simply lands
nowhere and the queue's expiry releases the line.

A peer who renames twice repoints the first era to the new destination but keeps
its ORIGINAL window. The timestamp answers "which work does this apply to", and
that was settled by the rename that created it; a second rename changes only where the
conversation went. Widening it would cover the gap between the two
renames — during which somebody may have claimed the abandoned nick — and a private
message meant for them would follow the original peer instead. Only eras still pointing
AT the renaming id are repointed, and those are by construction the conversation that
is renaming now: any earlier occupant's era was repointed away when IT renamed.

Anything the rename invalidates has to move with it, and that includes text already
rendered for the user. The **retry string** is built at dispatch and spells the target
as it was then, so a redirect rewrites it too: it is handed back into the composer as
ready-to-send text, and `deferred_retry_text` returns it verbatim whenever the author
is looking at the conversation — which after a rename means the NEW id. Left stale it
is the same leak as addressing the old name, one keystroke away. Only the re-addressed
`/msg <nick> <body>` form carries a nick; buffer input retries as itself and an action
retries as `/me <body>`, which names nobody.

A closed window does not end the question. `/close` on a query whose translated
message is still in the worker leaves no buffer for the NICK handler to re-key, so
nothing would record that the conversation moved and the send would go out addressed to
the abandoned nick. The in-flight marker (§3.7) therefore OUTLIVES the window — it is
the only remaining record that a message is still out for that conversation — and a
rename is turned into an era whenever one is live, buffer or no buffer. The delivery
path then follows it, or refuses when the destination has no window either.

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

Leaving the outgoing send in flight has a consequence on the OTHER side, though. Once
outgoing translation is off, the next message takes the ordinary path straight to the
socket while the earlier one is still waiting on the provider — so peers read the two
in the opposite order to the one they were typed in, and the author cannot see it,
because their own buffer is still ordered by the reservation. The same window opens for
`/e2e on`, `translate.enabled false` and `/reload`; it does NOT open while translation
is on, because the next send then takes the Translate path and the connection's serial
lane keeps the order.

A bypass send is therefore **refused** while a send for that buffer is still out
(`has_outgoing_in_flight`), rather than reordered.

The marker for that is tracked apart from the echo RESERVATION, and has to be. A
reservation answers "where does this row go"; the ceiling (§3.8) may take it back while
the translation is still running, so reading it as "no work in flight" let the next
message bypass translation and reach IRC ahead of the earlier one — the very reorder
this guard exists to stop. It is the same mistake as giving `Message::id` two meanings
(§5.5) and as splitting the concurrency counters (§3): one marker cannot answer two
questions. The marker records dispatch TIMES rather than a count, so a delivery path
that fails to clear its entry heals after the send's own budget instead of refusing that
buffer's ordinary sends for the rest of the session. That is the same choice this gate
makes everywhere else — a visible refusal beats an invisible wrong — and the wait is
bounded by the in-flight send's own timeout. Routing plain sends through the
translation lane instead would preserve the order too, but at the cost of putting every
ordinary send behind a provider that may be wedged; refusing keeps the failure where
the user can see and answer it.

### 3.8 Lifting a barrier is a release event

An outgoing message holds a `Reserved` slot in its buffer's queue (§5.1). While it
sits at the head it is a barrier: every line that finishes translating behind it is
`Resolved` but undeliverable.

That reservation goes away in two ordinary ways — it is **filled** by the local echo,
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

There is a third way, and it is a deliberate exception rather than an oversight: the
queue **ceiling** (§3.4) may end a reservation that is among the oldest entries. It
has no alternative. Forcing a pending line out converts its slot *in place*, and a row
only ever leaves through the head — so while a reservation holds the head, ending it is
the single lever that frees anything at all. Exempting reservations would trade a
bounded display lag for unbounded memory, on precisely the busy channel where the
bound exists. `nothing_but_lifting_a_barrier_can_bound_a_queue_behind_one` pins that
down.

The other candidate remedy — refusing the outgoing send so that "no echo will come"
becomes true again — is worse. The message has not reached the wire yet, so it *could*
be refused; but that eats a message the user has already typed, because unrelated
incoming traffic overflowed a display queue. An echo that renders after the replies to
it is a cosmetic fault in an already-degraded buffer; a silently unsent private
message is not. What the ceiling owes instead is **honesty**: `CeilingForced` counts
lifted barriers apart from forced lines and the release path logs them at `warn`,
because "a line is shown untranslated" and "your own message lost its place" are
different failures and folding them into one number leaves nobody able to tell which
happened. Where the reservation is *not* the oldest entry, the older pending lines
cover the excess and the barrier stands — `the_ceiling_spares_a_barrier_that_has_older_work_ahead_of_it`.

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

A highlighted channel line is copied into the `_mentions` aggregate **when it is
delivered**, not when it arrives. Built at arrival it would carry the original while the
channel goes on to show the translation, and the two would disagree permanently — in the
one place someone looks precisely because they were away and cannot re-read the channel.
`add_message_with_activity` reports whether translation took the row over, so the inline
fan-out is skipped exactly then and the delivery does it instead, with the final text,
marker and all. "Delivery" is two places, and both must do it: `deliver_ready` for a row
released from a queue, and `deliver_untranslated_in_order`'s immediate branch — a
multi-line message, or a dead worker, is handed straight to the buffer with no queue
behind it, so nothing would ever release it and the mention would simply be lost. Shrink is deliberately unchanged: it defers the chat row but its mention
is still pushed inline from the original text.

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

- **Wire order** — a per-connection FIFO ensures two TRANSLATED outgoing messages
  reach IRC in submission order, so a fast second message cannot overtake a slow
  first one. Nothing incoming participates, and neither does any other connection:
  a single global queue would let one hung request on network A block translated
  sends on network B.

  A send that bypasses translation is held back only from overtaking pending work
  in the **same conversation** (§3.7), not everywhere on the connection, and that
  narrower scope is deliberate. Same-conversation is where a reorder is plainly
  visible and misleading — a reply sitting above the message it answers, to
  everyone reading it. Across buffers there is no such reader: the two messages
  land in different channels, and nobody follows them as a sequence.

  The wider guard also cannot be paid for. Refusing every ordinary send on a
  connection whenever one translated send is in flight is not a transient window;
  it is the steady state of the feature's ordinary configuration — one channel
  translated, the rest not — where a message to any other buffer within a second
  of a translated one would be bounced. Routing bypass sends through the lane
  instead would preserve the order without refusing anything, but puts every
  ordinary send on the connection behind a provider that may be wedged, which
  trades a nearly unobservable reorder for a visible stall on traffic that has
  nothing to do with translation. `a_pending_send_holds_up_only_its_own_conversation`
  pins the scope so it is not widened by accident.
- **Echo order** — the local echo enters the buffer's display queue like any other
  row (§3.2), under the `id` it was allocated at submission. When it splits into
  several rows — which `show_original_out` makes routine, because the appended
  original pushes it past the byte budget — *all* of them occupy that one reserved
  place. Ordering them by ids allocated at delivery instead would let a line that
  arrived during the translation sort between them, so the buffer would show the first
  chunk, somebody else's reply, then the rest of the user's own sentence.

  That reserved id is the **ordering key only**. Each delivered row keeps its own
  `Message::id`, because that is a transport identity: the web client treats two live
  rows sharing an id as the same message and drops the second, so conflating the two
  silently swallowed every chunk after the first — usually including the one carrying
  ` [original]`. `add_own_message_chunks` takes them as separate arguments for exactly
  this reason.

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
- **And it carries the text verbatim.** An `Event` row with no `event_key` is handed
  to `parse_format_string` whole, which reads it in two passes and each eats
  characters: `substitute_vars` consumes `$0`–`$9`, `$*` and `$[N]D` (with no params
  they expand to nothing, so "costs $5" renders as "costs "), and the format walk
  consumes `%N`, `%_`, `%Zaabbcc` and the rest. That is right for the codes the row
  itself carries and wrong for anything a person typed. `commands::helpers::
  escape_format` doubles both signs — `$$` and `%%`, the escapes both passes define —
  and is applied at composition, never to the finished string, so the styling around
  the text still renders. The composer and the web `RestoreInput` get the text RAW:
  neither is parsed, and escaping there would hand back doubled signs. It applies to
  **all three** refusal rows, and most of all to the partial-send one: that path
  deliberately declines to restore the composer at all, so unlike the other two it has
  no second copy to fall back on.

  **`%` only.** `$` needs no escape, because `substitute_vars` now returns its input
  untouched when there are no params — substituting into nothing can only DELETE
  (`$0` and `$[3]0` expand to nothing, `$*` to the join of nothing), so on every
  `&[]` call site it was eating text nobody meant as a variable. Escaping `$` as `$$`
  instead was tried and was a mistake: the web UI's renderer sees raw IRC message
  bodies through the same entry point as composed rows, so teaching it `$$` made an
  ordinary line like `echo $$` render as `echo $`. The rule the two front ends now
  share is that a `$` in a person's text is a dollar sign.
  `$$` did not previously exist in the theme parser; it was added, because irssi
  defines it (`docs/special_vars.txt`) and without it no `$` before a digit can be
  rendered at all. The web UI's renderer consumes it too, so a row reads the same in
  both front ends.
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

- **The local echo is chunked by the WIRE, not by the display.** The byte budget
  belongs to a PRIVMSG, not to a row on screen. Splitting the composed display —
  translation plus ` [original]`, which `show_original_out` makes the common case —
  cut at boundaries the wire never used, so no chunk's text was a wire text and none
  could carry `wire_origin`. That cost the suffix its dimming and, worse, cost every
  chunk its identity: `dedup_text` falls back to keying the row by what is DISPLAYED,
  so a later CHATHISTORY replay matches nothing and splices a second, untranslated
  copy in beside it. Chunked by wire line the rows correspond one for one, and only
  the LAST carries the original — the same rule `expect_own_reflection` follows for a
  split reflection, so the two paths finally agree. The last row may exceed the wire
  budget once the suffix is on it, which is correct: nothing sends it.
- **Every wire line files a record; only the last carries a decoration.** A
  translation long enough to split is several reflections and one original; repeating
  it on each would say the same thing N times, and putting it on the first would place
  it before the text it is the original of. The earlier records exist for POSITION
  alone.
- **The reservation is not released — the reflection fills it, and only the LAST
  chunk closes it.** Each record carries the id reserved when the user pressed Enter,
  and the reflection uses it as its ORDER key. A split send comes back as several
  reflections, one IRC message at a time, so treating each as a completed echo closes
  the reservation on the first: the barrier lifts, everything queued behind it drains,
  and the rest of the user's own sentence lands after the replies to it. Earlier chunks
  are therefore **parked at the reservation** (`hold_in_reserved`) with the barrier
  still up, and `is_last` on the record is what tells the two apart. Parked rows are
  messages the server already sent us, so every path that ends a reservation releases
  them in place rather than dropping them with the barrier — a disconnect between two
  reflections must not lose the half that arrived. Releasing the barrier at send time lets everything queued behind it drain
  first, and the user's own message then lands below the replies that arrived while it
  was translating: the exact reordering the reservation exists to prevent. If no
  reflection ever comes, the queue's expiry clears the barrier like any other stall.
- **A record and its reservation die together.** The record is what lets a reflection
  find the slot reserved when the user pressed Enter, so dropping one — at the 32-record
  cap, or on its TTL — without releasing the reservation leaves a barrier nothing can
  ever fill. The buffer then stalls until the queue's expiry and the replies that
  finished translating meanwhile are released AHEAD of the message they were replies to.
  Well short of 32 messages, too: one translation long enough to split files a record
  per wire line, and several sends can resolve in a burst before any reflection returns.
  Eviction is therefore by MESSAGE, never by record: all the records sharing a reserved
  id go together. Dropping some while keeping others is worse than dropping all — the
  reflections that lost their record are appended behind the reservation, the one that
  kept its record then FILLS that reservation, and the message renders with its last
  chunk first. A dropped id is released only when no record for it survives, which
  after group eviction is always.
- **And a reservation and its record die together the other way round, too.** The
  point above is one direction of a single coupling; this is the other, and it is the
  sharper of the two. A record left behind is not inert: matching is by wire text,
  oldest first, so a record that outlives its reservation is consumed by the NEXT
  reflection carrying the same text. With a 5-second translation budget against the
  record's 30-second TTL that window is 25 seconds wide, and the thing a user does
  when a message never appears is retype it — so a lost reflection and a repeat of the
  same text are cause and effect, not independent events. The second send's reflection
  then takes the first send's record: it renders with the WRONG original, and the
  reservation it should have filled is left barricading the buffer for another full
  timeout. `Expired::abandoned_reservations` names every reservation the sweep gave up
  on and `forget_own_echo_records` drops what was filed for them, in that order, so a
  reflection arriving in the same tick cannot take a record whose slot has just gone.
  Timeouts only — the **ceiling** also ends reservations, but it is not evidence the
  reflection is lost (it takes the POSITION back while the send is still in flight),
  so there the record stays and the reflection still renders with its original, merely
  out of place.
- **A reflection this client deliberately drops releases its reservation too.** The
  record going missing is one way a reflection never fills its slot; the reflection
  *arriving and being swallowed on purpose* is the other, and the queue cannot tell
  them apart. Two paths do it. An **ignore rule** can match us — `*!*@*` on a channel
  someone is flooding, or any mask that happens to cover our own host — and the
  decoration is consumed before the ignore check runs, so after that return nothing
  can ever fill the slot. A **script** suppressing a PRIVMSG returns from
  `handle_irc_event` before `handle_privmsg` is ever called, so the reflection never
  reaches the code that would fill it. In both cases the message is gone, which is
  what was asked for and what a suppressed reflection has always done on a
  non-translated buffer; only the barrier is wrong. `abandon_own_reflection` is the
  one place that gives a place back, shared by all of these.
- **Our own reflection never enters the flood gate.** Flood protection guards the user
  against other people, and its condition is `!is_own` — not `nick != our_nick`, which
  compares exactly while IRC nicks are case-insensitive and an `echo-message` server
  may reflect ours in a different case. Compared exactly, our own reflection reads as a
  stranger's: duplicate-text suppression eats the *second* time the user sends the same
  line, so their own message never appears in their own buffer, and with the decoration
  already consumed the place held for it is stranded as well.
- **Records die with the connection.** A drop clears them alongside the queues —
  walked separately, because a record outlives the queue whenever the reservation was
  the only thing in it, which is the ordinary case. Ownership is decided by the
  buffer id's connection PREFIX, never by looking up a live buffer: a queue or a record
  routinely outlives its window (a send to a conversation never opened, or one whose
  query was closed mid-translation), and consulting `buffers` skipped exactly those. Left behind, a reconnect inside
  the TTL that resends the same text consumes the stale record: the new reflection
  takes the old reserved id and suffix, and the new reservation blocks the buffer
  until it times out.
- **The echo is concatenated, never joined.** A split translation echoes as the pieces
  run together with nothing between them, because the echo is the row the author sees
  and the line written to their log, and both have to be the message the peers
  received. The pieces already carry their own separators: `split_irc_message` breaks
  after a word's trailing whitespace, leaving it on the chunk before the break, and
  breaks a word too long for one line at a character boundary with no whitespace at
  all. Inserting a space doubled the separator in the first case and put one INSIDE a
  word in the second.
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

Three cases have no safe form and are therefore **not restored at all**, with the
text left in the error row:

- **An action.** `/me` acts on the active buffer and has no re-addressed spelling.
- **A target whose buffer is gone.** Its name is no longer proof of anything: on a
  query, somebody else may have claimed the nick since (§3.6). Re-addressing would
  hand the user a ready-to-send private message aimed at a stranger — the same leak,
  one keystroke away.
- **A composer on another connection.** `/msg` names a TARGET, not a network:
  `cmd_msg` resolves it against whatever connection the active buffer belongs to when
  Enter is pressed (for a web tab too — `web_run_command` runs the retry with that
  tab's buffer active). So the re-addressed form is correct from any buffer ON THIS
  CONNECTION and from no other. Offered on a second network it hands the user a
  ready-to-send private message aimed at whoever holds that nick *there*: the same
  leak as the case above, reached through the other door. That branch asks whether
  the name still means the right person; this one whether it is even being asked on
  the right network. No spelling of `/msg` carries a network, so there is nothing to
  re-address to, and the form is withheld.

### 5.7 A refusal goes back to the client that typed it

Every submitting path scopes `App::submit_origin` for the duration of the submit, so
a refusal restores the text where its author is looking. That includes both web
arms: the browser composer sends plain lines as `SendMessage` and every `/`-prefixed
line as `RunCommand`, and both end in the same `handle_submit`. Scoping only the
first would return a refused `/msg` to the *terminal's* input line — lost for its
author, and dropped into a window nobody is watching.

Where they are looking is answered differently per client, and only one of the answers
is reliable. The TUI's active buffer is authoritative. A web session's is not: a tab
changes buffer without telling us whenever it follows a TUI-driven
`ActiveBufferChanged`, and whether it follows is a `localStorage` flag
(`web_follow_tui_buffer`) only the browser can see — so the server cannot deduce it.
After such a broadcast the recorded buffer is a **guess**, and a guess is not enough to
hand back a bare body: restoring one into a composer that has since moved publishes it
to the wrong conversation the moment the user presses Enter, which is the leak this
whole function exists to prevent.

Sessions are therefore marked unconfirmed when the broadcast goes out and trusted again
when they next speak for themselves. A session already recorded AT the new buffer is
exempt — it ends up there whether it followed or not — which is also what stops a tab's
own `SwitchBuffer` from marking itself, since that switch is what raised the event.
While unconfirmed, a web retry is **withheld entirely**. The re-addressed form is not
a way out here: not knowing which buffer the tab is showing is exactly not knowing
which connection its retry would resolve against, and that is the third case above.

A tab does not stay unconfirmed for long, because any submit answers the question:
`SendMessage` and `RunCommand` both name the buffer their text came from, so both
replace the guess with that fact and clear the doubt. Without it a tab that had just
spoken would still be refused its own text back, on the one path where getting it
back is the point.

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

Identity has to survive a round trip through the log, and `wire_origin` does not — the
log stores the flat display text, by design (§7). What survives instead is the key the
row was **stored under**: the server `@msgid`, or on a server without one a hash of its
WIRE text. `storage_identity` is that rule, used by the writer and by the in-memory
`CHATHISTORY` dedup alike, because the two answering differently is exactly how a replay
ends up spliced beside the row it duplicates.

A row read back from `SQLite` therefore carries `Message::log_key`. Without it, a
translated row reloaded by the log browser holds only its translation, a replay of the
same line arrives carrying the original, and on a msgid-less server nothing matches — so
the untranslated copy is spliced in beside the translation. No new column: the log has
always kept this key, it was simply thrown away on the way back.

### 7.2 The one row whose clock is ours

Every rule above compares identities the WIRE supplied. A local echo — the row written
for our own send when the server does not offer `echo-message` — has none: no `@msgid`,
and a timestamp taken from OUR clock at the moment of the send, while the server's
replay of that same message carries its own `@time`. Both exact tests therefore miss,
and a reconnect gap-fill splices our own line in a second time; with translation on,
once as the translation with ` [original]` and once as the bare text the network
carried.

`own_echo_matches_replay` is the exception written for it, and it is scoped as tightly
as the failure: only for rows whose nick is OURS, only for rows that never carried
tags, and only when type and wire text agree, with a 60-second tolerance covering the
round trip and ordinary clock skew. It cannot affect two identical lines from anybody
else — on a msgid-less server their exact timestamps are all that tells them apart, and
that test is untouched.

**Known residue, not translation-specific.** The same mismatch also puts two rows in
`SQLite`: the echo is stored under a hash of our clock, the replay under the server's
`@msgid` or a hash of its `@time`, and the unique `(network, msg_id)` index has no
reason to collapse them. So the duplicate returns when the buffer is reloaded from the
log. Closing that would mean a fuzzy lookup per ingested CHATHISTORY row, on the
storage path, plus keeping the `oldest_ingested` watermark honest when a row is skipped
as already-stored — a change to reconnect backlog accounting that has nothing to do
with translation and should be specified on its own. It predates this branch and
affects every local echo, translated or not.

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

In-flight counts are reported as three separate numbers because they answer three
questions: lines actually AT the provider (`translating_len`), display positions held
for an echo not yet reflected (`reserved_len`), and total queue depth. Folding
reservations into "in flight" made a healthy client look busy — a reservation's
translation has usually already come back. Outgoing work is not in these queues at all,
since the ceiling can take a reservation back while the send runs on, so it is read from
`outgoing_in_flight` and reported on its own line.

The counters are a session tally on `AppState`, because an outcome is otherwise
consumed the moment it is rendered and nothing would be left to ask. They are reported
whether or not anything is in flight: the question is asked precisely when the queues
have drained and the channel looks untranslated, so returning early on an empty queue
left it unanswerable at the one moment it mattered.

`ReadyEntry` therefore carries a three-state `ReadyOrigin` and not
`Option<UntranslatedReason>`. "Translated" and "never a candidate" are both absences of
a reason, and a channel's JOINs, notices and our own echoes pass through this queue in
bulk — folding them into the translated total would report a healthy provider that has
not answered once. A line that never reached the worker carries its reason through the
queue for the same purpose: those arrive exactly when the provider is in trouble.
Counting happens at the single point a row leaves the queue for a buffer
(`AppState::deliver_ready`), so a fourth release path cannot quietly skip it.

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

It is installed only when `translate.backend = "stub"` names it, never by
`translate.enabled` alone. The stub does not translate — it reverses word
order — and on the outgoing side its answer is what reaches the channel under
the user's own nick, so a user who asked for a translator and got this one
would have every line they send corrupted. `enabled` on its own resolves to no
backend: the mechanism stays wired and every line is delivered as it arrived.
Startup reports which of the three outcomes the config produced (no
translator, an unknown name, or the stub with its warning), because a config
that translates nothing looks exactly like one that works.

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

# IRCv3 `+typing` client tag — design

**Date:** 2026-07-12
**Status:** approved, ready for implementation planning
**Specs:** [`message-tags`](https://ircv3.net/specs/extensions/message-tags) (ratified), [`typing` client tag](https://ircv3.net/specs/client-tags/typing) (ratified)

## Summary

Repartee shows who is typing in a channel or query, and tells others when we are
typing. The mechanism is the IRCv3 `+typing` client-only tag, carried on `TAGMSG`
commands, gated on the `message-tags` capability.

Scope is deliberately narrow: the generic message-tags plumbing (TAGMSG in and out,
client-only tags, `CLIENTTAGDENY`) is built properly, but `+typing` is the only tag
implemented on top of it. `+reply` and `+draft/react` are explicit non-goals — see
[Non-goals](#non-goals).

The headline finding from the research phase: **no change to the `irc-repartee` fork
is required.** Everything needed is already on the wire path.

---

## 1. What the specs actually require

### 1.1 `message-tags` (dependency)

`+typing` has exactly one dependency: *"Clients wishing to use this tag MUST negotiate
the [`message-tags`](../extensions/message-tags.html) capability with the server."*
There is no `typing` capability and no `draft/typing` capability — typing is a **tag**,
not a cap. Repartee already requests `message-tags` (`src/irc/cap.rs:20`), so
**capability negotiation needs no change at all.**

**Tag grammar** (from the spec's extended pseudo-BNF):

```
<message>       ::= ['@' <tags> <SPACE>] [':' <prefix> <SPACE> ] <command> [params] <crlf>
<tags>          ::= <tag> [';' <tag>]*
<tag>           ::= <key> ['=' <escaped_value>]
<key>           ::= [ <client_prefix> ] [ <vendor> '/' ] <key_name>
<client_prefix> ::= '+'
<key_name>      ::= <non-empty sequence of ascii letters, digits, hyphens ('-')>
<escaped_value> ::= <sequence of zero or more utf8 characters except NUL, CR, LF, semicolon (`;`) and SPACE>
<vendor>        ::= <host>
```

**Escaping table** (normative):

| Character       | Sequence in `<escaped value>` |
|-----------------|-------------------------------|
| `;` (semicolon) | `\:` (backslash and colon)    |
| `SPACE`         | `\s`                          |
| `\`             | `\\`                          |
| `CR`            | `\r`                          |
| `LF`            | `\n`                          |
| all others      | the character itself          |

A lone trailing `\` unescapes to nothing (`test\` → `test`); an invalid escape drops the
backslash (`\b` → `b`). `+typing` values are `active` / `paused` / `done`, which contain
no escapable characters, so escaping never actually fires for us — but the codec must
still be correct, and it is (see §2.2).

**Empty vs missing values.** *"Implementations MUST interpret empty tag values (e.g.
`foo=`) as equivalent to missing tag values (e.g. `foo`)."* For `+typing` both forms are
meaningless (no state), so both are ignored. This matters because of a quirk in our tag
extractor — see §4.4.

**Size limits.** Client tag data MUST NOT exceed 4094 bytes; servers reply
`ERR_INPUTTOOLONG (417)` and MUST NOT truncate. Our tag data is ~15 bytes, so this is
unreachable in practice. We do not need a size guard, but we must not crash on a `417`.

**`TAGMSG`.**

```
   Command: TAGMSG
Parameters: <msgtarget>
```

Delivered exactly like `PRIVMSG`/`NOTICE` — honouring channel membership, modes,
`echo-message`, and `STATUSMSG` prefixes. Two normative consequences we must honour:

- *"Servers MUST NOT deliver `TAGMSG` to clients that haven't negotiated the message
  tags capability."* — so a server that gives us `message-tags` may send us TAGMSG at any
  time, and we must not choke on it.
- *"Clients that receive a `TAGMSG` command MUST NOT display them in the message history
  by default."* — **TAGMSG never becomes a buffer line.** This is a hard rule and drives
  several decisions in §4.

**`CLIENTTAGDENY` (RPL_ISUPPORT token).** A comma-separated list of blocked client-only
tags, with the `+` prefix omitted. `*` (which MUST be first when present) blocks all;
`-` negates a block.

| Token value                   | Meaning for us                      |
|-------------------------------|-------------------------------------|
| absent, or `CLIENTTAGDENY=`   | everything allowed → `+typing` OK   |
| `CLIENTTAGDENY=*`             | all client tags blocked → no typing |
| `CLIENTTAGDENY=*,-typing`     | only `typing` allowed → typing OK   |
| `CLIENTTAGDENY=typing`        | `typing` specifically blocked       |

*"Clients MAY still send blocked tags to the server"* — but there is no point, and the
spec's stated purpose for the token is: *"This token allows clients to selectively
remove features from their user interface that rely on any client-only tag that the
server has blocked."* So we honour it on both send and display.

### 1.2 `typing` client tag

Values: `active`, `paused`, `done`. Sent as a `TAGMSG` with the client-only prefix.

Sending rules (normative wording preserved):

- *"The `typing=active` notification SHOULD be sent by clients **continuously** while the
  user is making updates to the text-input field **and the text is not a "/slash
  command"**."*
- *"The `typing=paused` notification MAY be sent by clients **once** when the user has
  paused typing in the text-input field and has not cleared their text."*
- *"The `typing=done` notification MAY be sent by clients **once** when the user clears
  the text-input field without sending a message."*
- *"Input event handlers MUST be throttled so that any `typing` notification is not sent
  within 3 seconds of another one for a given target."*

Receiving rules — assume the sender is still typing until **any** of these happens on
the same target:

| Condition                                                | Our handling                            |
|----------------------------------------------------------|-----------------------------------------|
| A message is received from the sender                    | clear on PRIVMSG / NOTICE from that nick |
| The sender leaves the channel or quits the server        | clear on PART / QUIT / KICK, and on NICK change |
| A `typing=done` notification is received from the sender | clear immediately                       |
| ≥ 30 seconds since the last `typing=paused`              | expire                                  |
| ≥ 6 seconds since the last `typing=active`               | expire                                  |

Wire examples, verbatim from the spec:

```
Client: @+typing=active :nick!ident@host TAGMSG target
Client: @+typing=paused :nick!ident@host TAGMSG target
Client: @+typing=done   :nick!ident@host TAGMSG target
```

Privacy (non-normative but explicit): *"Typing status indicators introduce additional
privacy concerns... Clients are recommended to provide appropriate privacy controls when
enabling this feature."* And: *"clients might choose to not display typing notifications
for their own nickname, while still sending them to others."*

### 1.3 Spec ambiguity we resolve by decision

The typing spec defines, for its own purposes, *"a message is defined as a `PRIVMSG`,
`NOTICE`, `TAGMSG`, or `batch` end"*, and then lists "a message is received from the
sender" as an expiry condition. Read literally, a `typing=active` TAGMSG would expire
the very typing state it establishes — self-contradictory.

**Decision:** expiry-on-message fires for `PRIVMSG`, `NOTICE`, and batch end only. A
`TAGMSG` carrying a `+typing` tag sets state rather than clearing it. A `TAGMSG` carrying
*no* `+typing` tag is ignored entirely (we implement no other client tags; see
[Non-goals](#non-goals)). This matches what deployed clients do.

### 1.4 Legacy tag name: `+draft/typing`

The tag was originally specified in the draft namespace as `+draft/typing` and was
renamed to the unprefixed `+typing` on ratification. Clients that predate ratification
may still emit the draft name.

**Rule:** on **receive**, accept both `+typing` and `+draft/typing` (identical semantics).
On **send**, emit only the ratified `+typing`. Sending both would double our TAGMSG
volume — and therefore our flood cost (§3.1) — to serve clients that are, by now,
vanishingly rare.

### 1.5 Deployment reality

Server support for `message-tags` / TAGMSG / client-only tags: **Ergo**, **InspIRCd**,
**UnrealIRCd**, **Solanum**, **ircd-hybrid**. Libera.Chat (Solanum) announced `+typing`
support on 2026-02-11, stating it *"allows clients to send optional typing
notifications"* and that it evaluates client tags individually and validates their
values — i.e. an allowlist, which is exactly what `CLIENTTAGDENY` communicates. Testing
against Ergo (permissive) and Libera (allowlisted) covers both regimes.

Client-side, `+typing` is implemented by mIRC, WeeChat, senpai, IRCCloud, Goguma, and
others — so there is real traffic to interoperate with, and real clients that will see
ours.

---

## 2. What the codebase already gives us

### 2.1 Capability negotiation — done, no change

`DESIRED_CAPS` (`src/irc/cap.rs:4-25`) already contains `message-tags` (line 20).
Negotiation runs in `negotiate_caps` (`src/irc/mod.rs:533`), and the active per-connection
set lives in `Connection.enabled_caps: HashSet<String>` (`src/state/connection.rs:50`).
`CAP NEW` / `CAP DEL` re-negotiation is already handled (`src/app/irc.rs:570-620`).

The guard we will write everywhere is the established idiom:
`conn.enabled_caps.contains("message-tags")`.

### 2.2 Outbound tags — already work end to end

This is the finding that collapses the scope of the work:

- `Message { pub tags: Option<Vec<Tag>>, .. }` — `irc-proto-repartee-1.2.2/src/message.rs:18`,
  with `pub struct Tag(pub String, pub Option<String>)` at `:267`.
- `impl Display for Message` **serializes tags** — `message.rs:240-259` — writing `@`,
  joining with `;`, and running values through `escape_tag_value` (`:269`), which
  implements the escaping table above exactly.
- `Encoder::encode` calls `msg.to_string()` (`irc.rs:47-53`), so `Display` *is* the wire
  format.
- `Sender::send<M: Into<Message>>` (`irc-repartee-1.5.1/src/client/mod.rs:945`) accepts a
  fully-formed `Message`, not just a `Command`.
- **Caveat:** `impl From<Command> for Message` sets `tags: None` (`message.rs:117-125`).
  Every existing `sender.send(Command::…)` call site therefore sends untagged. To send
  tags you must build the `Message` literal yourself.
- **Working precedent in-repo:** `multiline_frames()` (`src/irc/multiline.rs:171`,
  literal at `:181-194`) already constructs `Message { tags: Some(tags), prefix: None,
  command: … }` and sends it via `handle.sender.send(frame)` (`src/app/input.rs:1564-1570`).
  Our TAGMSG builder is the same shape.

### 2.3 `TAGMSG` has no `Command` variant — and does not need one

`Command` has no `TAGMSG`; unknown verbs fall back to `Command::Raw(String, Vec<String>)`
(`command.rs:204`, `:964-971`), which stringifies verbatim via `stringify`
(`command.rs:411-413`). For a single-argument command with no spaces, `stringify` emits no
trailing colon. Therefore:

```rust
Message {
    tags: Some(vec![Tag("+typing".into(), Some("active".into()))]),
    prefix: None,
    command: Command::Raw("TAGMSG".into(), vec!["#rust".into()]),
}
// → "@+typing=active TAGMSG #rust\r\n"
```

is exactly the wire form the spec shows. Inbound, `TAGMSG` parses to
`Command::Raw("TAGMSG", [target])` with `msg.tags` populated. **Both directions work with
zero crate changes.**

### 2.4 ISUPPORT — token already captured

`ISupport` stores every token generically in `tokens: HashMap<String, String>`
(`src/irc/isupport.rs:18`, populated by `parse_tokens` at `:34`). `CLIENTTAGDENY` is
therefore already being captured today; only an accessor is missing.

### 2.5 Timers, input, state, render, web, scripting

| Need                | Existing hook                                                                 |
|---------------------|-------------------------------------------------------------------------------|
| 1 s expiry sweep **and** 3 s resend | `tick` interval — `src/app/mod.rs:1227`, select arm `:1445`. **No new timer is needed.** The throttle floor is 3 s and the receiver's `active` window is 6 s, so 1 s granularity resends comfortably inside the window; the render loop already redraws on every wakeup. |
| keystroke hook      | `App::handle_key` — `src/app/input.rs:132`; char arms `:352-390`, clears `:240-244,:303-305` |
| message submit      | `input.submit()` — `src/app/input.rs:296`                                      |
| per-buffer state    | `Buffer` — `src/state/buffer.rs:139`                                           |
| TUI status line     | `src/ui/status_line.rs:13`, item loop `:38`, items from `config.statusbar.items` |
| web push            | `state.pending_web_events` → `App::drain_pending_web_events` (`src/app/web.rs:203`) → `Broadcaster::send` (`src/web/broadcast.rs:28`) |
| web protocol        | `WebEvent` (`src/web/protocol.rs:11`) mirrored in `web-ui/src/protocol.rs:7`   |
| scripting           | string-keyed events, constants in `src/scripting/api.rs:10`, emitted from `src/app/scripting.rs:686-814` |

---

## 3. Three constraints that shape the design

### 3.1 Flood budget — typing costs about one message of burst headroom

Repartee enables the crate's penalty throttle with a 10 000 ms threshold when
`flood_protection` is on (`src/irc/mod.rs:395-399`, the default). The crate charges, per
outgoing message, `length_penalty` + `command_penalty`
(`irc-repartee-1.5.1/src/client/mod.rs:992-1045`):

- `length_penalty = (1 + len/100) * 1000` → **1000 ms** for a ~40-byte TAGMSG
- `command_penalty` for `Command::Raw` falls into the catch-all `_ => 2000` → **2000 ms**

So **one typing TAGMSG costs 3000 ms of a 10 000 ms budget** — the same as a short
PRIVMSG. The penalty drains at 1 ms per ms of elapsed time.

At the spec's 3-second cadence, cost (3000) and drain (3000) are exactly balanced, so
during continuous typing the penalty oscillates between 0 and 3000 rather than growing.
The real cost is the reduced headroom for actual messages: instead of ~3 short PRIVMSGs
before the throttle starts delaying, a user who is typing continuously gets ~2.
**Typing costs roughly one message of burst headroom.** That is acceptable and does not
justify forking the crate.

Three mitigations make it a non-issue in practice:

1. **Never send `+typing` to a target within 3 s of sending a real message to it.** The
   message itself already tells the receiver we are present, so the notification is
   redundant precisely when the budget is tightest.
2. **Drop, never hand to the transport, when the budget is tight.** This has to be enforced
   *before* `Sender::send`, not after: `Sender` is an `UnboundedSender`
   (`irc-repartee-1.5.1/src/client/mod.rs:940-947`), and once a frame is in it, `Outgoing`
   will **buffer and delay** it when the penalty exceeds the threshold
   (`client/mod.rs:1166-1180`) rather than dropping it. A typing frame handed over under
   flood pressure does not vanish — it sits in the queue, arrives stale, and spends budget
   that the user's next real message needed.

   So `TypingSender` keeps its own estimate of the penalty, mirroring the crate's formula
   (`length_penalty` + `command_penalty`, `client/mod.rs:992-1045`), and simply does not
   send when the estimate leaves less than a reserve for real messages. The estimate is
   approximate and deliberately conservative; being wrong costs a missing typing
   notification, which is the failure we want.
3. **A late notification is never sent.** If the throttle or the budget blocks a send, the
   notification is reconsidered on the next tick against the *current* state, not replayed.
   The receiver's 6-second window has moved on; what matters is what is true now.

*Rejected alternative:* bumping the fork to add a real `Command::TAGMSG` variant with a
lower penalty. It would be marginally cleaner (typed inbound match, tuned penalty) but
costs two crates.io releases (`irc-proto-repartee`, `irc-repartee`) plus a lockfile bump,
and buys ~1 message of burst headroom. Revisit only if `+reply`/`+draft/react` land and
TAGMSG traffic multiplies.

### 3.2 `echo-message` reflects our own TAGMSG back at us

Repartee negotiates `echo-message` (`src/irc/cap.rs`), so **the server echoes our own
TAGMSG to us**. Without a filter we would render ourselves as typing. The existing
own-message test is a prefix comparison, not a tag —
`nick.eq_ignore_ascii_case(our_nick)` (`src/irc/events.rs:1153`) — and the TAGMSG handler
must apply the same check and drop the message. This aligns with the spec's own advice
about not displaying typing for one's own nickname.

### 3.3 Chathistory replay would produce phantom typing

Repartee negotiates `draft/chathistory` and `draft/event-playback` (`src/irc/cap.rs:22-23`).
A server replaying history can hand us a `TAGMSG` from hours ago inside a batch; naively
handled, it would show "alice is typing…" for a conversation that ended last night.

**Rule:** a `TAGMSG` carrying a `batch` tag is ignored. Typing is strictly a live-traffic
signal. (Batch membership is already resolved — `src/irc/batch.rs:155`, parent lookup
`src/app/irc.rs:648-650`.)

---

## 4. Design

### 4.1 Module layout

| File | Change |
|------|--------|
| `src/irc/typing.rs` | **new** — protocol layer: `TypingState`, tag parse, TAGMSG builder, timing constants |
| `src/irc/events.rs` | new `TAGMSG` arm in `handle_irc_message` (~`:57`); clear-typing calls in PRIVMSG/NOTICE/PART/QUIT/KICK/NICK handlers |
| `src/irc/isupport.rs` | new `client_tag_allowed(&self, tag: &str) -> bool` (parses `CLIENTTAGDENY`) |
| `src/state/typing.rs` | **new** — `TypingTracker`: `buffer_id → nick → TypingEntry`, with set/clear/expire/list |
| `src/state/mod.rs`, `src/state/events.rs:10` | `AppState.typing: TypingTracker` + one line in `AppState::new()` |
| `src/app/typing.rs` | **new** — 14th `app/` domain submodule: source/target state machine, flood estimate, expiry sweep, web event emission |
| `src/app/input.rs` | input hooks around **both** `Event::Key` and `Event::Paste` (`handle_event`, `:33-36` — paste is a separate branch and would otherwise be missed); `on_submit` on Enter |
| `src/commands/handlers_ui.rs` | `parse_statusbar_item` (`:586`), `statusbar_item_name` (`:598`), `AVAILABLE_ITEMS` — all match `StatusbarItem` exhaustively |
| `src/state/events.rs` | `AppState::remove_buffer` (`:84`) drops the buffer's typing entries |
| `src/app/mod.rs` | expiry in the 1 s tick arm; new self-rearming 3 s resend timer |
| `src/config/mod.rs` | `TypingConfig`; `StatusbarItem::Typing`; new default item order |
| `src/commands/settings.rs` | `typing.*` keys in get/set dispatch + completion list |
| `src/ui/status_line.rs` | `Typing` item arm; **separator fix** (see §4.6) |
| `src/web/protocol.rs` | `WebEvent::Typing`, `WebCommand::Typing` |
| `web-ui/src/protocol.rs` | mirror both |
| `web-ui/src/state.rs` | `typing` signal + `WebEvent::Typing` arm |
| `web-ui/src/components/status_line.rs` | typing span between channel name and `Lag:` |
| `web-ui/src/components/input.rs` | debounced `WebCommand::Typing` emission |
| `src/scripting/api.rs` | `events::TYPING = "irc.typing"` |
| `src/app/scripting.rs` | emit `irc.typing` from the TAGMSG arm |
| `docs/rfc_ircv3_coverage.md` | mark `+typing` Done; expand the `message-tags` row |

### 4.2 Protocol layer — `src/irc/typing.rs`

Pure and UI-agnostic. **Every time-dependent function takes `now: Instant` as a
parameter** rather than calling `Instant::now()` internally, so the whole state machine is
testable with a fake clock and no `sleep` anywhere in the test suite.

```rust
pub const TAG: &str = "+typing";              // sent
pub const TAG_LEGACY: &str = "+draft/typing"; // accepted on receive only (§1.4)
pub const THROTTLE: Duration = Duration::from_secs(3);   // spec: MUST NOT send within 3s
pub const ACTIVE_TTL: Duration = Duration::from_secs(6); // spec: expire after 6s
pub const PAUSED_TTL: Duration = Duration::from_secs(30);// spec: expire after 30s

pub enum TypingState { Active, Paused, Done }

impl TypingState {
    pub const fn as_str(self) -> &'static str;      // "active" | "paused" | "done"
    pub fn parse(value: &str) -> Option<Self>;      // case-sensitive per spec; unknown → None
    pub const fn ttl(self) -> Option<Duration>;     // Active → 6s, Paused → 30s, Done → None
}

pub fn build_tagmsg(target: &str, state: TypingState) -> irc::proto::Message;
pub fn parse_typing(tags: &HashMap<String, String>) -> Option<TypingState>; // checks TAG, then TAG_LEGACY
```

Tag key names are *"case-sensitive opaque identifiers"* per the message-tags spec, so
`+typing` and `+draft/typing` are matched exactly, with no case folding. Tag *values* are
matched exactly too (`active`, not `ACTIVE`); an unrecognised value is ignored rather than
guessed at.

### 4.3 Receiving

New arm in `handle_irc_message` (`src/irc/events.rs`, ~`:57`):

```rust
Command::Raw(verb, args) if verb.eq_ignore_ascii_case("TAGMSG") => { … }
```

Target resolution reuses the `handle_privmsg` logic: a channel target maps to the channel
buffer; a target equal to our nick maps to the query buffer keyed by the *sender*.

**`STATUSMSG` stripping must be precise.** `is_channel` (`src/irc/formatting.rs:157-161`)
accepts `#`, `&`, `+`, **and** `!` — so `&chan` and `+chan` are real channels, not
status-prefixed ones. Blindly trimming a set of prefix characters would turn `&chan` into
`chan` and resolve it as a query. The rule: strip **at most one** leading character, only if
it appears in the connection's advertised `STATUSMSG` ISUPPORT token, and only if what
remains is still a channel. Otherwise use the target as-is.

Then, in order, a message is **dropped** if any of these holds:

1. it carries a `batch` tag (§3.3 — history replay)
2. `typing.show` is off (§5 — the setting gates *ingestion*, not just rendering)
3. it carries no valid `+typing` tag (unknown/absent/empty value)
4. the sender is us (§3.2 — `echo-message`)
5. the sender is on the ignore list (`src/irc/ignore.rs`)
6. the target buffer does not already exist

Script suppression is **not** one of them — it is enforced a layer up, before
`handle_tagmsg` is reached at all. See §4.8.

Rule 6 is deliberate: **typing never creates a buffer.** Otherwise any stranger could pop
a query window open on your screen just by starting to type, with no message ever sent —
a spam vector with no analogue in PRIVMSG handling, since we would have nothing to show.

Surviving messages update `AppState.typing` and, per the message-tags spec's *"MUST NOT
display them in the message history"*, do **not**: append a buffer line, bump
activity/unread, write to SQLite (`src/storage/`), or trigger mention/notification logic.
They enqueue a `WebEvent::Typing`; the TUI repaints on the next loop iteration.

**Where the state lives.** A dedicated `TypingTracker` in `src/state/typing.rs`, held as
`AppState.typing`, keyed `buffer_id → nick → TypingEntry { state, since: Instant }`.

*Not* a field on `Buffer`: `Buffer` has no constructor and is built from 39 struct literals
across the codebase (mostly test fixtures), so a new field there means 39 mechanical edits.
`AppState` has exactly one constructor (`src/state/events.rs:10`), so the tracker costs one
line. It is also the better boundary — typing is ephemeral session state, not buffer content
— and it lets the expiry sweep walk one small map instead of every buffer.

**Clearing is connection-scoped.** Buffer ids are `{conn_id}/{name}`
(`src/state/buffer.rs:213`), and the same nick can be typing on two networks at once. A
`QUIT` or a `NICK` change belongs to **one** connection, so the cleanup must only touch
buffer ids under that connection's prefix — a tracker-wide sweep by nick would clear
`alice` on Libera because `alice` quit on OFTC. `PART` and `KICK` are already scoped to a
single channel buffer; note that `KICK` must clear the **kicked user**, not the kicker
(`handle_kick` binds `kicker` and `kicked_user` — there is no `nick` in scope).

**Buffer lifecycle.** `AppState::remove_buffer` (`src/state/events.rs:84`) must drop the
buffer's typing entries, and the web client must clear its own map on `BufferClosed`.
Without this, closing and reopening a query inside the 30 s `paused` TTL resurrects a stale
typer.

### 4.4 Known quirk: `extract_tags` drops valueless tags

`extract_tags` (`src/irc/events.rs:797-803`) builds its `HashMap` with
`filter_map(|tag| Some((tag.0.clone(), tag.1.as_ref()?.clone())))` — a tag with no value
is silently dropped. Per the message-tags spec, empty and missing values are equivalent,
and for `+typing` both are meaningless (no state to apply), so a bare `+typing` *should*
be ignored — which is what already happens. The behaviour is correct by accident. We rely
on it, and note it here so a future `+draft/react`-style valueless tag does not trip over
it silently.

### 4.5 Sending — sources, targets, and the state machine

Lives in `src/app/typing.rs`.

**There is no single "current input".** The TUI has one, and *each* web session has its own
— `handle_web_command` is handed a `session_id` (`src/app/web.rs:413-416`), and
`web_active_buffers` maps session → buffer, independently of the global
`state.active_buffer_id`. Two browser tabs can be typing into two different buffers while
the terminal types into a third. A single-target machine would let an idle second tab's
"input is empty" cancel the terminal's live typing.

So the machine is two-layered: **sources** report what they are doing, **targets** are what
gets sent.

```rust
enum TypingSource { Tui, Web(String) }   // String = web session id

struct SourceState {
    buffer_id: String,     // where this source is typing
    active: bool,          // its input holds non-empty, non-slash text
    last_activity: Instant // when its input last changed
}

struct TargetState {
    sent: Option<TypingState>, // last state we transmitted for this buffer
    last_sent: Option<Instant> // the 3s throttle clock, per target
}

struct TypingSender {
    sources: HashMap<TypingSource, SourceState>,
    targets: HashMap<String, TargetState>,
    last_message: HashMap<String, Instant>, // real messages, for §3.1 suppression
    flood: FloodEstimate,                   // §3.1 budget mirror
}
```

**Aggregation.** For each buffer, the desired state is derived from every source pointing at
it:

| Sources pointing at the buffer | Desired |
|--------------------------------|---------|
| none active                    | `Done` (if we have sent anything for it) |
| at least one active, some source changed within 3 s | `Active` |
| at least one active, none changed for ≥ 3 s | `Paused` |

A source that switches buffer, goes empty, submits, or disconnects simply stops holding its
old target — and the old target's aggregate collapses to `Done` on its own. There is no
special "buffer switch" case to get wrong.

**Input predicate**, computed per source:

```
active = !input.is_empty() && !is_slash_command(input)
is_slash_command(s) = s.starts_with('/') && !s.starts_with("/me ")
```

`/me` is exempted because it *is* a message — the spec excludes slash commands because they
are not conversation, and an action is conversation. (`/me` alone, with no trailing space or
text, is still a bare command and stays excluded.) The browser applies this predicate
locally and reports only the resulting boolean (§4.7).

**Emission.** A target emits when its desired state differs from `sent`, or when `sent` is
`Active` and the throttle window has reopened (the spec asks for `active` "continuously").
`Paused` is emitted once. `Done` is emitted once.

**The 3-second throttle applies to every notification, including `Done`.** The spec is
unqualified: *"any `typing` notification is not sent within 3 seconds of another one for a
given target."* A `Done` that is due but throttled is **not dropped and not sent early** —
it stays pending, and the 1 s tick emits it as soon as the window opens. If the user resumes
typing before then, the pending `Done` is simply superseded by the recomputed aggregate, and
never goes out. This is why the machine stores desired-vs-sent per target rather than a
queue of notifications: coalescing falls out for free.

The cost of obeying the throttle here is that a peer can keep showing "is typing" for up to
3 s after the user clears their input — against a 6 s expiry they would have hit anyway.

**Drivers.** Two, and only two: the input hooks (TUI keystroke and paste; `WebCommand::Typing`
from a browser) and the existing 1 s tick, which resends, transitions to `Paused`, flushes
pending `Done`s, and expires received state. **No new timer** — see §2.5.

**Guards.** Every send is gated on all of:

- `conn.enabled_caps.contains("message-tags")`
- `isupport_parsed.client_tag_allowed("typing")`
- config: `typing.send_channels` for `BufferType::Channel`, `typing.send_queries` for `BufferType::Query`
- buffer type is `Channel` or `Query` (never Server / Log / Shell / DCC — DCC chat has no server to route a TAGMSG through)
- no real message sent to this target in the last 3 s (§3.1)
- the flood estimate leaves headroom for the user's real messages (§3.1)

A send blocked by a guard is *not* queued. The target keeps its desired-vs-sent gap and the
next tick reconsiders it against the state that is true then.

### 4.6 TUI rendering

New `StatusbarItem::Typing`. Default item order becomes:

```rust
items: vec![
    StatusbarItem::Time,
    StatusbarItem::NickInfo,
    StatusbarItem::ChannelInfo,
    StatusbarItem::Typing,      // ← new: after the buffer name, before lag
    StatusbarItem::Lag,
    StatusbarItem::ActiveWindows,
]
```

Text, in English to match the rest of the bar (`Lag:`, `Act:`), with nicks sorted by the
existing nick ordering so it does not jitter:

| Typing nicks | Rendered |
|--------------|----------|
| 1 | `alice is typing…` |
| 2 | `alice and bob are typing…` |
| 3 | `alice, bob and carol are typing…` |
| >3 | `alice, bob and 2 others are typing…` |

`paused` and `active` render identically — the distinction is a transport detail, and
showing "paused" would be noise. The 30-second `paused` TTL means an indicator can
legitimately linger; that is what the spec prescribes.

**Separator fix (required, not optional).** The item loop at `src/ui/status_line.rs:38-44`
pushes the separator unconditionally for `i > 0`, *before* the item renders. An item that
produces nothing — which `Typing` does most of the time — would leave a dangling
`… | | Lag:`. The loop must push the separator only once the item has actually emitted
spans. This is a small refactor of the loop, and it is a latent bug for any future
conditional item, not just ours.

Nick coloring reuses `src/nick_color.rs`; the item honours
`config.statusbar.item_formats["typing"]` like other items.

### 4.7 Web UI

**Server → browser:** `WebEvent::Typing { buffer_id: String, nicks: Vec<String> }` — the
*complete* set for that buffer, not a delta. Deltas drift when a client reconnects
mid-stream; a full set is idempotent and self-healing. Emitted whenever the set changes
(new typer, expiry, clear). Not included in `SyncInit`: typing is ephemeral, and a client
that connects mid-typing simply learns about it within 3 seconds on the next resend.

Because typing is *not* in `SyncInit`, the web client **must reset its typing map when it
processes `SyncInit`**. A websocket reconnect can miss the expiry or clear event that would
have retired an indicator, and a stale "alice is typing…" would then persist indefinitely.

**Browser → server:** `WebCommand::Typing { buffer_id: String, typing: bool }`. The browser
reports **only** the predicate — "my input field currently holds non-empty, non-slash text"
— debounced to at most 1 message per second to keep the websocket quiet. It runs no state
machine, no throttle, and knows nothing about caps or `CLIENTTAGDENY`.

The core keys this by `session_id` (which `handle_web_command` already receives) and feeds
it into the source layer of §4.5. **This is load-bearing:** two browser tabs plus a terminal
are three independent sources. Collapsing them into one predicate lets a freshly-opened,
empty second tab emit `typing: false` and cancel the typing that the terminal is doing right
now. Each source holds its own `(buffer, active)` and the target's state is the aggregate.

Web submissions must also drive the machine: `WebCommand::SendMessage` and
`WebCommand::RunCommand` call the same `on_submit` the terminal's Enter key does. Otherwise
a browser-sent message is followed by a redundant `Done` (from the input going empty), and
the 3-second post-message suppression never gets recorded.

The structural decision that makes all of this safe: **the state machine exists in exactly
one place.** TUI and browsers are input sources; the core owns caps, `CLIENTTAGDENY`,
throttling, flood budget and config. Turning typing off disables it everywhere by
construction.

`web-ui/src/state.rs` gains a `typing: RwSignal<HashMap<String, Vec<String>>>` keyed by
buffer id, cleared on `SyncInit` and on `BufferClosed`;
`web-ui/src/components/status_line.rs` renders the span for the active buffer between the
channel name and `Lag:`, matching the TUI order. (The web status line hardcodes its order
and does not read `config.statusbar.items`; keeping the two in sync is manual, as it already
is today.)

### 4.8 Scripting

New event constant `events::TYPING = "irc.typing"` (`src/scripting/api.rs:10`), with params
`connection_id`, `nick`, `target`, `state`. Emitted from `src/app/scripting.rs` — that is
where script events are dispatched (`match &msg.command`, `:686-810`), and today every
`Command::Raw`, TAGMSG included, falls into its `_ => return false` arm and emits nothing.

`EventResult::Suppress` suppresses the state update, and it comes for free — but **not** via
`state.suppress_event_display`, as an earlier draft of this document claimed. That flag is
`script_suppressed && state_mutating`, and a TAGMSG arrives as `Command::Raw`, which is not
in the `state_mutating` list (`src/app/irc.rs`). A script that suppresses a TAGMSG therefore
takes the dispatcher's *early return* — `handle_irc_message` is never called, so
`handle_tagmsg` never runs and has no flag to check. (Adding `Raw` to `state_mutating` would
change the handling of every other raw command, so we do not.)

### 4.9 Persistence and sessions

TAGMSG is never written to SQLite — no `src/storage/` changes. Typing state is not part of
the detach/reattach snapshot (`src/session/`): on reattach the state is empty and
repopulates within seconds from live traffic. Both are consequences of typing being an
ephemeral, live-only signal.

---

## 5. Configuration

```toml
[typing]
show          = true   # display other people's typing indicators
send_channels = true   # send +typing in channels
send_queries  = true   # send +typing in private queries
```

Runtime: `/set typing.show off`, `/set typing.send_channels off`, `/set typing.send_queries off`.
All three default to `true`. The send flags are independent per the spec's privacy guidance,
so a user can watch without broadcasting, or broadcast in DMs but not in public channels.

**`show = false` must gate ingestion, not rendering.** Checking it only in the status-line
render arm would leave the tracker filling up and `WebEvent::Typing` still being broadcast —
so the browser would keep displaying typing that the user asked not to see. The flag is
therefore checked in `handle_tagmsg` (drop rule 3, §4.3), and toggling it off **clears the
tracker and broadcasts the now-empty sets** so live web clients drop what they are showing.

`handle_tagmsg` lives in `src/irc/events.rs`, which has no access to `AppConfig`. The flag
is mirrored into `AppState.typing_show`, following the existing config→state sync pattern
used by `scrollback_limit` and `nick_color_sat` — set at startup (`src/app/mod.rs:547`), on
config reload (`src/commands/handlers_admin.rs:117`), and on `/set`
(`src/commands/settings.rs:856`).

Wiring the keys themselves follows the existing pattern in `src/commands/settings.rs`: arms
in `get_config_value` / `set_config_value` plus entries in the completion list (~`:630`).

**`StatusbarItem::Typing` is not just a render arm.** The enum is matched exhaustively in
`src/commands/handlers_ui.rs` too — `parse_statusbar_item` (`:586`), `statusbar_item_name`
(`:598`), and the `AVAILABLE_ITEMS` string that `/items` prints. All three must learn the
new variant, or the build breaks and `/items` cannot manage the new default item.

---

## 6. Testing

All timing tests use an injected `now: Instant` (§4.2) — **no `sleep`, no wall-clock
dependency, no unbounded loops.**

**Protocol (`src/irc/typing.rs`)**
- `build_tagmsg` produces exactly `@+typing=active TAGMSG #chan\r\n` for each of the three states
- round-trip: `Message::from_str(build_tagmsg(..).to_string())` recovers the tag
- `parse_typing` accepts `active`/`paused`/`done`, rejects `ACTIVE`, `typing`, empty, absent, and junk
- `parse_typing` accepts the legacy `+draft/typing` key (§1.4); `build_tagmsg` never emits it
- escaping table holds for a synthetic value containing `; \s \\ \r \n` (guards the codec we depend on)

**Receiving (`src/irc/events.rs`)**
- TAGMSG never appends a buffer line, never bumps activity/unread, never persists
- TAGMSG inside a batch is ignored (chathistory replay, §3.3)
- own TAGMSG is ignored (echo-message, §3.2)
- TAGMSG from an ignored nick is ignored
- TAGMSG to a non-existent query does not create a buffer
- `STATUSMSG` target (`@#chan`) resolves to `#chan`

**Expiry**
- `active` expires at 6 s, not 5.9 s; `paused` at 30 s; `done` immediately
- PRIVMSG / NOTICE from the nick clears typing; PART / QUIT / KICK / NICK clear it

**Sending (`src/app/typing.rs`)**
- two keystrokes 1 s apart produce exactly one TAGMSG (3 s throttle)
- `/join #x` produces none; `/me waves` produces one (slash-command rule)
- clearing the input sends exactly one `done`, not one per keystroke
- 3 s idle with text present sends exactly one `paused`, then silence
- Enter sends no `done`
- switching buffers mid-typing sends `done` to the *old* target
- no `message-tags` cap → nothing sent; `CLIENTTAGDENY=*` → nothing sent; config off → nothing sent
- a real message to the target suppresses typing for 3 s (§3.1)

**Render (`src/ui/status_line.rs`)**
- zero typers → no item **and no stray separator** (§4.6)
- 1 / 2 / 3 / 4 typers → the four phrasings in §4.6

**Web**
- `WebEvent::Typing` serde round-trip matches the `web-ui` mirror
- `WebCommand::Typing { typing: true }` drives the same state machine as a TUI keystroke

---

## 7. Non-goals

- **`+reply` / `+draft/react`.** Both ride the same TAGMSG plumbing this design builds, and
  both are cheap *afterwards* — but they need `msgid` tracking, `echo-message` msgid
  capture, message-id persistence in SQLite, and non-trivial buffer-render work
  (threading, reaction rows). Out of scope; the TAGMSG layer is left clean enough that
  they are additive.
- **Forking `irc-repartee` for a typed `Command::TAGMSG`.** See §3.1 — rejected on cost.
- **Typing in the nicklist.** Status line only, per the chosen design.
- **Distinguishing `paused` from `active` in the UI.** Transport detail; no user value.

## 8. Risk register

| Risk | Mitigation |
|------|------------|
| Typing frames queue up under flood pressure and delay real messages | §3.1 — the transport queue is unbounded and *delays* rather than drops, so the budget check happens before `Sender::send`, using an app-side estimate |
| Self-typing shown via `echo-message` | §3.2 — `is_own` filter in the TAGMSG arm |
| Phantom typing from history replay | §3.3 — drop TAGMSG carrying a `batch` tag |
| An idle second browser tab cancels the terminal's typing | §4.5 — per-source state, aggregated per target |
| `&chan` / `+chan` misparsed as a query by STATUSMSG stripping | §4.3 — strip at most one char, only if advertised in `STATUSMSG` and the remainder is still a channel |
| `alice` quitting on one network clears her indicator on another | §4.3 — QUIT/NICK cleanup is scoped to the connection's buffer-id prefix |
| Stale typer resurrected by closing and reopening a buffer | §4.3 — `AppState::remove_buffer` drops the entries; web clears on `BufferClosed` |
| Stale indicator surviving a websocket reconnect | §4.7 — the web client resets its typing map on `SyncInit` |
| `typing.show = off` still leaking typing to the web UI | §5 — the flag gates ingestion in `handle_tagmsg`, not just the render arm |
| Stray separator in the status bar | §4.6 — conditional-separator refactor, covered by a test |
| Strangers opening query windows by typing | §4.3 rule 7 — typing never creates a buffer |
| Server silently drops our tag | `CLIENTTAGDENY` accessor; UI feature gated on it |
| Privacy: "typed and thought better of it" leaks | Three independent config flags, `/set`-able at runtime |

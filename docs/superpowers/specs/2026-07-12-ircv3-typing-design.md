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
| 1 s expiry sweep    | `tick` interval — `src/app/mod.rs:1227`, select arm `:1445`                    |
| 3 s resend timer    | self-rearming `sleep` pattern — `src/app/mod.rs:1234`, re-armed `:1276-1296`, arm `:1501` |
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

Two mitigations make it a non-issue in practice:

1. **Never send `+typing` to a target within 3 s of sending a real message to it.** The
   message itself already tells the receiver we are present, so the notification is
   redundant precisely when the budget is tightest.
2. **Drop, never queue.** A typing notification that cannot be sent now is discarded, not
   buffered. A late "is typing" is worse than none — the receiver's 6-second window will
   have moved on.

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
| `src/state/buffer.rs` | `Buffer.typing: HashMap<String, TypingEntry>` + set/clear/expire/list methods |
| `src/app/typing.rs` | **new** — 14th `app/` domain submodule: outbound state machine, expiry sweep, web event emission |
| `src/app/input.rs` | keystroke hook in `handle_key`; `done` on clear; reset on submit |
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
buffer; a target equal to our nick maps to the query buffer keyed by the *sender*; a
`STATUSMSG` prefix (`@#chan`, `+#chan`) is stripped before lookup.

Then, in order, a message is **dropped** if any of these holds:

1. it carries a `batch` tag (§3.3 — history replay)
2. the sender is us (§3.2 — `echo-message`)
3. the sender is on the ignore list (`src/irc/ignore.rs`)
4. it carries no valid `+typing` tag (unknown/absent/empty value)
5. the target buffer does not already exist

Rule 5 is deliberate: **typing never creates a buffer.** Otherwise any stranger could pop
a query window open on your screen just by starting to type, with no message ever sent —
a spam vector with no analogue in PRIVMSG handling, since we would have nothing to show.

Surviving messages update `Buffer.typing` and, per the message-tags spec's *"MUST NOT
display them in the message history"*, do **not**: append a buffer line, bump
activity/unread, write to SQLite (`src/storage/`), or trigger mention/notification logic.
They mark the buffer dirty for redraw and enqueue a `WebEvent::Typing`.

### 4.4 Known quirk: `extract_tags` drops valueless tags

`extract_tags` (`src/irc/events.rs:797-803`) builds its `HashMap` with
`filter_map(|tag| Some((tag.0.clone(), tag.1.as_ref()?.clone())))` — a tag with no value
is silently dropped. Per the message-tags spec, empty and missing values are equivalent,
and for `+typing` both are meaningless (no state to apply), so a bare `+typing` *should*
be ignored — which is what already happens. The behaviour is correct by accident. We rely
on it, and note it here so a future `+draft/react`-style valueless tag does not trip over
it silently.

### 4.5 Sending — state machine

Lives in `src/app/typing.rs`. Because a user can only type into one buffer at a time, the
outbound machine tracks a **single** target:

```rust
struct TypingSender {
    target: Option<(ConnectionId, String)>, // buffer we last typed into
    sent: Option<TypingState>,              // last state we transmitted
    last_sent: Option<Instant>,             // for the 3s throttle
    last_keystroke: Option<Instant>,        // for the paused transition
}
```

Input predicate, applied on every keystroke that mutates the input buffer:

```
should_type = !input.is_empty() && !is_slash_command(input)
is_slash_command(s) = s.starts_with('/') && !s.starts_with("/me ")
```

`/me` is exempted because it *is* a message — the spec excludes slash commands because
they are not conversation, and an action is conversation. (`/me` alone, with no trailing
space or text, is still a bare command and stays excluded.)

Transitions:

| Trigger | Condition | Action |
|---------|-----------|--------|
| keystroke | `should_type`, ≥ 3 s since last send | send `active` |
| keystroke | `should_type`, < 3 s since last send | record keystroke only (throttle) |
| keystroke | `!should_type`, we previously sent `active`/`paused` | send `done` once, reset |
| 3 s timer | `should_type`, last keystroke < 3 s ago | resend `active` |
| 3 s timer | `should_type`, last keystroke ≥ 3 s ago, state is `Active` | send `paused` once |
| 3 s timer | state is `Paused` | nothing (stay silent until keys resume) |
| submit (Enter) | — | reset to idle; send nothing (the PRIVMSG clears typing at receivers) |
| buffer switch | we had sent `active`/`paused` to the old target | send `done` to the old target, reset |
| disconnect | — | reset (QUIT clears typing at receivers) |

Guards — **every** send is gated on all of:

- `conn.enabled_caps.contains("message-tags")`
- `isupport.client_tag_allowed("typing")`
- config: `typing.send_channels` for `BufferType::Channel`, `typing.send_queries` for `BufferType::Query`
- buffer type is `Channel` or `Query` (never Server / Log / Shell / DCC — DCC chat has no server to route TAGMSG through)
- no real message sent to this target in the last 3 s (§3.1)

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

**Browser → server:** `WebCommand::Typing { buffer_id: String, typing: bool }`. The
browser reports **only** the predicate — "my input field currently holds non-empty,
non-slash text" — debounced to at most 1 message per second to keep the websocket quiet.
It runs no state machine, no throttle, and knows nothing about caps or `CLIENTTAGDENY`.
The core treats an arriving `typing: true` exactly as it treats a TUI keystroke for that
buffer, and `typing: false` as an input-cleared event.

This is the key structural decision for the web side: **the state machine exists in
exactly one place.** TUI and browser are two input sources feeding the same machine, so
they can never emit two competing TAGMSGs, and turning typing off in config disables it
everywhere by construction.

`web-ui/src/state.rs` gains a `typing: RwSignal<HashMap<String, Vec<String>>>` keyed by
buffer id; `web-ui/src/components/status_line.rs` renders the span for the active buffer
between the channel name and `Lag:`, matching the TUI order. (The web status line
hardcodes its order and does not read `config.statusbar.items`; keeping the two in sync is
manual, as it already is today.)

### 4.8 Scripting

New event constant `events::TYPING = "irc.typing"` (`src/scripting/api.rs:10`), emitted
from the TAGMSG arm with params `nick`, `target`, `state`, `network`. `EventResult::Suppress`
from a script suppresses the state update, consistent with how other events behave.

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
All three default to `true`. `show = false` hides the status-line item and stops tracking;
the send flags are independent per spec's privacy guidance, so a user can watch without
broadcasting, or broadcast in DMs but not in public channels.

Wiring follows the existing pattern in `src/commands/settings.rs`: arms in
`get_config_value` / `set_config_value` plus entries in the completion list (~`:630`).

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
| Flood throttle delays real messages | §3.1 — 3 s post-message suppression; drop-don't-queue |
| Self-typing shown via `echo-message` | §3.2 — `is_own` filter in the TAGMSG arm |
| Phantom typing from history replay | §3.3 — drop TAGMSG carrying a `batch` tag |
| Stray separator in the status bar | §4.6 — conditional-separator refactor, covered by a test |
| Strangers opening query windows by typing | §4.3 rule 5 — typing never creates a buffer |
| Server silently drops our tag | `CLIENTTAGDENY` accessor; UI feature gated on it |
| Web and TUI double-send | §4.7 — one state machine, browser sends a predicate only |
| Privacy: "typed and thought better of it" leaks | Three independent config flags, `/set`-able at runtime |

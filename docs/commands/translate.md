---
category: Other
description: Translate a channel or query in near-real time
---

# /translate

## Syntax

    /translate list
    /translate status
    /translate addin  <#channel|nick> [lang] [my-lang]
    /translate delin  <#channel|nick>
    /translate addout <#channel|nick> <lang> [my-lang]
    /translate delout <#channel|nick>

Alias: `/tr`

## Description

Turn a channel or query into one you can read — and write — in your own
language. Incoming lines are shown already translated; outgoing messages are
translated before they reach IRC.

Translation is per buffer and per direction. `addin` translates what others
say to you; `addout` translates what you send. They are independent, so
reading a German channel in Polish while still writing German yourself is
just `addin` without `addout`.

Nothing happens until the master switch is on **and** a translator is named:

    /set translate.enabled true
    /set translate.backend <name>

The switch and the translator are separate settings on purpose. `enabled`
says the mechanism may run; `backend` says what it runs on. With
`translate.backend = "none"` — the default — every line is delivered exactly
as it arrived, however many buffers are configured, and `/translate status`
says so.

Available backends:

| name | what it does |
|---|---|
| `none` | nothing is translated (default) |
| `ai` | production AI translator with ordered OpenAI-compatible providers |
| `stub` | test backend — **reverses word order**, no network, no API key |

`stub` exists only to exercise the mechanism end to end. It is never installed
implicitly, because its output does not stay on your screen: on the outgoing
side it is what goes to the channel, under your nick. Repartee says so loudly
at startup when it is on. Do not leave it on for a real conversation.

### AI setup

The quickest production setup uses the default OpenRouter model. Put the key
in `~/.repartee/.env`, never in `config.toml`:

    OPENROUTER_API=your-key

Then select the backend and restart Repartee:

    /set translate.enabled true
    /set translate.backend ai

The AI backend is a policy, not one hard-wired vendor. It can use any
OpenAI-compatible chat-completions endpoint and tries configured models in
order. The built-in easy-line order is OpenRouter Gemma 4 31B, Ollama Gemma 4
31B, Groq GPT-OSS 120B, then Groq Qwen 3.6 27B. Lines classified as difficult
start with Gemini 3.6 Flash, followed by the same fallbacks. A model whose key
is absent is skipped; one usable key is enough. The recognized default `.env`
names are:

| provider | key |
|---|---|
| OpenRouter | `OPENROUTER_API` |
| Ollama Cloud | `OLLAMA_API` |
| Gemini | `GEMINI_API` |
| Groq | `GROQ_API_KEY` |

Before a provider call, Repartee filters lines that do not need translation,
masks URLs, channel names, nicks and technical identifiers, then routes the
remaining text to the easy or strong policy. Every answer passes a local
quality gate. Missing or altered placeholders, refusals, explanations,
unchanged source text, a confidently wrong target language, empty output and
multi-line output all cause fallback to the next model. If the policy is
exhausted, the existing untranslated/refusal behavior applies.

The supplied prompt is embedded in the binary. Set
`translate.ai.prompt_path` in `config.toml` to load a custom prompt at startup.
The provider-independent translation seam remains outside this AI module, so
a future Google Translate backend does not change queues, ordering, E2E rules
or line replacement.

## Languages

A buffer has a **language pair**, not a per-direction setting:

- `<lang>` — the language the channel or query is written in
- `translate.my_lang` — the language *you* read and write

The two directions swap them:

| direction | from | into |
|---|---|---|
| incoming | the channel's `lang` | your `my_lang` |
| outgoing | your `my_lang` | the channel's `lang` |

So the language you write *into* is per channel, which is what makes a German
and a Spanish channel work at the same time. `/translate list` prints the
arrows for each direction so you can check at a glance:

    test/#german  in: de→pl   out: pl→de
    test/#somos   in: es→pl   out: pl→es

`[my-lang]` overrides `translate.my_lang` for one buffer only — useful when
machine translation into your own language is poor for a particular source
and English reads better.

For **incoming**, `<lang>` may be left off and the language is detected
automatically. For **outgoing** it is required and `addout` refuses without
it: there is nothing to detect which language to *write* in from, so guessing
is the one thing that must not happen.

## What you see

A translated line replaces the original in place — one line, never two:

    11:32:33 alice> albalb [blabla]

The bracketed part is the original, dimmed. Turn it off and only the
translation is shown:

    /set translate.show_original_in  false
    /set translate.show_original_out false

The two directions have separate switches, because wanting your own words
echoed back is a different question from wanting everyone else's.

**What is on screen is what is logged.** Stored history keeps exactly the
text that was displayed, brackets included, so `/search` finds both the
translation and the original. The consequence is that changing
`show_original_*` does not rewrite history: lines logged with brackets keep
them.

The bracketed original is dimmed in the terminal and in the browser alike. A
line reloaded from history renders it undimmed in both: the stored text is
flat, and the client will not guess which trailing brackets are an original —
an ordinary message may simply end that way.

On servers that echo your own messages back to you, that echo is still the
copy you see — Repartee adds the original to it as it arrives, rather than
printing a second line of its own. The server's copy is the one carrying the
message's identity, and keeping it is what stops your own messages coming
back a second time after a reconnect.

## Ordering

Lines are sent for translation the instant they arrive and translate
concurrently, but they are only *shown* in the order they arrived. A short
line that comes back quickly will wait for the longer one in front of it, so
an answer can never appear above its question.

A line that has waited longer than `translate.timeout_ms` is shown
untranslated rather than holding up the channel, and the queue is capped at
`translate.max_queue` so a dead provider cannot stall a busy channel or grow
without bound. That deadline counts from the moment the line arrived — or,
for your own messages, from the moment you pressed Enter — not from when the
translator was actually reached, so a busy queue cannot quietly stretch it.
A message of yours whose translation misses the deadline is not sent; the
text comes back as with any other refusal. The same happens to a private
message that has been in flight so long the client can no longer be sure
whether the person you addressed still answers to that nick — better to hand
it back than to risk delivering it to whoever picked the nick up. Losing the connection releases that server's queued lines at
once rather than waiting the timeout out for a server that is gone.

The queue is held to `translate.max_queue` as lines arrive, not on a timer,
so a stalled translator cannot let a busy channel build a backlog between
checks. If the queue reaches the cap while a message of yours is still being
translated, that message can lose the place held for it and will then appear
below the replies that arrived meanwhile. It is still sent — a full display
queue never eats something you typed — and the client logs a warning saying
so.

`/translate delin` releases the lines it was still waiting on, shown
untranslated. `/translate delout` releases nothing: it does not touch the
incoming direction, and a message of yours already being translated is still
sent when it comes back. Until it does, the next message to that same
conversation is refused with a note rather than sent — it would otherwise
reach the channel ahead of the one still being translated, and you would have
no way to see that it had. Press Enter again once the earlier one lands.
Turning off `translate.enabled`, running `/reload`, or enabling E2E on the
conversation all behave the same way for the same reason.

Closing a window — `/close`, leaving the channel, or being kicked from it —
also shows whatever it was still waiting on, untranslated, before the window
goes. Those lines had already reached you; the queue only decides when they
are allowed on screen, so they are displayed and written to your log rather
than discarded.

If the person you are talking to changes nick, the settings and any lines in
flight follow the conversation — including a message of yours that was still
being translated, which goes to them under their new nick and never to
whoever may have picked up the old one. That holds however many times the
nick changes hands: each conversation that held it is tracked with the period
it held it for, so a message you sent to the first person cannot be delivered
to the second. If their window has closed by then,
the message is refused and handed back rather than sent to a name that is no
longer theirs. The saved setting still names the nick you typed, so it is
that name a restart looks for.

If they **quit** instead of renaming, nothing can be followed: the server
frees the nick there and then, and the next person to ask for it gets it. A
private message still being translated when that happens is refused and handed
back rather than addressed to a name that may already mean somebody else.
Sending without translation races the same reassignment by milliseconds; a
translation makes the gap seconds wide, so the mechanism that opened it is the
one that closes it. Text you type *after* they have gone is sent as it always
was — that is the same gamble with or without translation, and not this
feature's to refuse.

That tracking has a horizon of **five minutes**. A private message whose
translation comes back later than that is refused and handed back: the nick it
was addressed to can no longer be shown to still mean the same person. This is
only reachable with a `translate.timeout_ms` above five minutes, which is far
past any sane provider budget — the default is five *seconds*. Channels are
never affected, in any configuration: a channel name cannot change hands, so
there is nothing to trace and nothing to doubt.

## When translation does not happen

A line that was not translated is shown as its original, marked with the
reason:

    11:32:33 alice> hola que tal [untranslated: timeout]

A translation that comes back as more than one line is refused outright. The
translator is an outside service and its answer is about to be sent under
your nick, so anything that could be read as a second IRC command never
reaches the network.

For the same reason, a translation carrying the CTCP byte (`\x01`) is refused.
That byte is what tells an IRC client a message is a *request* rather than
text — a DCC file offer, a VERSION query — so an answer containing one would
send a request under your nick instead of a sentence, and inside a `/me` it
would break out of the action your client wrapped round it. Any framing that
reaches the network from a translated message is framing this client built.
Incoming is checked too: a reply carrying it would render as a `/me` from the
other person, putting words in their mouth.

A translation *beginning* with one of the E2E wire prefixes is refused too.
Those are recognised as protocol on every conversation, encrypted or not —
that is how the first handshake from a peer can arrive at all — so an answer
shaped like one would be swallowed as a handshake or fed to the decryption
path by every Repartee that received it, your own included, instead of being
read. Only the prefix is reserved: mentioning the protocol mid-sentence is
ordinary conversation and passes.

A translation that comes back empty is refused too — the original comes
back to you rather than an empty line going out under your nick. One that
comes back implausibly long is refused rather than sent:
it would be split across the wire budget and published as a long run of
messages under your nick. Never a guess, never a partial translation. A fluent sentence that means
something else is the one failure a reader cannot detect, so a visible gap is
always preferred — and the gap has to be visible, or it is just the original
text with nothing to say it is not a translation.

The one exception carries no marker, because it is not a gap: see below.

For **outgoing** messages the rule is stricter: if translation fails, the
message is **not sent at all**. Your text goes back into the input line you
sent it from — the terminal or the browser tab that submitted it — and an
error explains why.

If you have already started typing the next message, the composer is left
alone rather than overwritten — and since a translation takes a moment, that
is the usual case rather than the exception. The error row always carries the
refused text for exactly that reason, so it is recoverable either way — and it
carries it **character for character** — including `%`, which the theme engine
would otherwise read as a formatting code and swallow. A row you are meant to
retype from has to be exactly what you typed.
Sending the untranslated original would put something other than what you
intended in front of the channel.

If you have moved to a different conversation while it was translating, what
comes back is re-addressed — a private message returns as
`/msg <nick> <text>`, so pressing Enter cannot publish it to the channel you
are now in. Where no such form exists nothing is put in the composer at all
and the text stays in the error row, which names where it was going. That
covers a `/me`, a conversation that has since closed, and — because `/msg`
names a person, not a server — **any** move to a different network: the same
nick there is somebody else.

The local echo names whoever the message actually went out as. If you `/nick`
while a translation is running, the message reaches the channel under your new
name, and that is the name your own copy carries too.

A connection that drops and comes back while your message is being translated
also refuses it, rather than sending a message you typed on the old session
into the new one.

That applies to every way of not translating, not just a provider error: a
full or dead translation queue, a message too long to translate in one piece,
a multi-line paste, and a buffer with `outgoing` enabled but no language all
refuse rather than sending as-is.

Incoming multi-line messages are the mirror of this: they are shown intact
and marked untranslated rather than being flattened into one request, which
would let a translator reorder words across the line breaks. `/translate delout <target>` is how you
send something untranslated on purpose.

It also applies however the message was submitted. `/msg`, `/query <nick>
<text>`, `/me` and messages sent by scripts go through the same gate as text
typed in the buffer, so whether a line is translated depends on the buffer,
never on which command you used. A `/me` is translated as its text; other
CTCPs (`VERSION`, DCC negotiation) are protocol and pass through untouched.

There is one case where your text is **not** put back in the composer: a long
message is split into several lines before it goes out, and if the connection
dies partway through, the first parts are already on the channel. Restoring
the whole thing would invite you to press Enter and send those parts twice, so
it stays in the error row only — which says as much.

On a server with `echo-message`, your own translated message is the copy the
server sends back, and the client holds its place in the buffer until that
copy arrives. If something in your own setup throws that copy away — an
`/ignore` mask broad enough to cover you, or a script that swallows the
message — the line is gone, as you asked, and the conversation carries on
immediately rather than pausing for a reply that is never coming.

A line the translator deliberately skipped — already in the target language,
or too short to be worth translating — is a correct outcome, not a failure.
It is sent and shown with no marker; marking it would cry wolf on a large
share of ordinary traffic.

## What leaves your client

Translating means sending text to an outside service. The AI backend sends a
system prompt containing the source and target language plus the line after
local masking. URLs, channel names, recognized technical identifiers and
known nicks are replaced with opaque placeholders first and restored only
after the answer passes validation.

The network name, channel/query name, speaker nick and channel nick list are
not sent as metadata. The nick list is used locally only to find names that
must be masked. Text not recognized by the masker still leaves the client, so
translation should be treated as disclosure of the conversation content to
the configured provider.

Nothing is sent for a channel you have not explicitly enabled, and nothing is
sent for history — only lines arriving live, and only while the master switch
is on.

## End-to-end encrypted conversations

**Translation is refused on any conversation E2E might be protecting, and
this cannot be overridden.**

Translating means sending the text to an outside provider. For an encrypted
conversation that would hand its plaintext to exactly the party the
encryption exists to exclude.

The refusal is enforced where each request is built, not just in
`/translate addin`, so the order you enable things in does not matter:
turning E2E on for a channel that already had translation enabled stops it
from the next line — and `/e2e on` says so, rather than leaving a channel
that silently stopped translating to read as a broken translator. When E2E
status cannot be determined at all — an unreadable keyring, a DM whose peer
handle has not resolved yet — the conversation is treated as encrypted and
left untranslated.

## Interaction with URL shortening

A translated line is not also URL-shortened. Sending one line through two
outside services is worse than losing the shortening, so translation takes
precedence on buffers where both are enabled.

## Examples

    /set translate.enabled true
    /set translate.my_lang pl

    /translate addin  #german de     # read it in Polish
    /translate addout #german        # write it in German (lang already known)
    /translate addin  #somos  es     # a second channel, a different language
    /translate addout #somos  es
    /translate addin  #cn zh en      # read this one in English, not Polish
    /translate list
    /translate status
    /translate delout #german

## Configuration

    /set translate.enabled           false
    /set translate.backend           none
    /set translate.my_lang           en
    /set translate.show_original_in  true
    /set translate.show_original_out true
    /set translate.timeout_ms        5000
    /set translate.max_in_flight     4
    /set translate.max_queue         200

`/translate status` shows what is in flight, and — whether or not anything
is, and for both directions — how lines have come back this session: how many were translated, how
many the translator decided needed no translation, and any failures split
into timeouts, provider refusals, and answers this client would not use.
That split is the point: a channel sitting in its original language looks
identical whether the translator is filtering correctly or not answering at
all.

`translate.max_in_flight` caps concurrent translations. Raising it past what
your provider actually allows makes throughput worse, not better. It takes
effect immediately. Lowering it applies as work already in flight finishes —
including under sustained traffic, where the reduction is applied by the next
translations to start rather than waiting for a lull that may never come.

Outgoing messages are translated one at a time **per connection**, so they
reach IRC in the order you sent them without a slow request on one network
holding up another.

Per-buffer settings live under `[translate.buffers]` in `config.toml` and are
managed with the `add*` / `del*` subcommands rather than `/set`.

AI policies and models are configured directly in `config.toml`. This minimal
example replaces the built-in policy with one OpenAI-compatible model:

    [translate.ai]
    easy = ["primary"]
    strong = ["primary"]
    prompt_path = ""

    [[translate.ai.models]]
    name = "primary"
    base_url = "https://openrouter.ai/api/v1"
    model = "google/gemma-4-31b-it"
    api_key_env = "OPENROUTER_API"
    rpm = 60
    tpm = 0
    max_retries = 2
    max_output_tokens = 256

`api_key_env` names a variable in `~/.repartee/.env`. API keys loaded from
that file are held only in memory and are never serialized into `config.toml`.
`/reload` applies added, removed and rotated keys to an already-running AI
backend. If no AI backend was built at startup, adding the first usable key
still requires a restart and `/reload` reports that explicitly.
`rpm` and `tpm` are local safety limits; zero disables that dimension. Server
rate-limit headers and `Retry-After` are honored, `429` and transient server
errors use bounded retries, and authentication, access and daily-quota errors
disable that model for the rest of the session.

Enabling `translate.enabled`, or naming a `translate.backend`, at runtime when
translation was not running at startup reports that a restart is required: the
translation workers and the backend are built once, when the client starts.
`/reload` says the same thing — editing `enabled = true` in `config.toml` and
reloading cannot build a backend either, so the reload warns instead of
reporting a plain success you would have no reason to distrust.

Turning it **off** needs no restart, by either switch. `/set translate.backend
none`, `/set translate.enabled false`, and the same edits followed by
`/reload` all stop translation immediately: deciding your lines should stop
leaving for a translator is not a decision that should wait.

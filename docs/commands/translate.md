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

Nothing happens until the master switch is on:

    /set translate.enabled true

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

Your own translated messages are shown by Repartee itself when the original
is configured to be visible, even on servers that echo your messages back to
you. The echo carries only the translation, so it cannot show the original;
the copy the server sends back is dropped in favour of the one that can.

## Ordering

Lines are sent for translation the instant they arrive and translate
concurrently, but they are only *shown* in the order they arrived. A short
line that comes back quickly will wait for the longer one in front of it, so
an answer can never appear above its question.

A line that has waited longer than `translate.timeout_ms` is shown
untranslated rather than holding up the channel, and the queue is capped at
`translate.max_queue` so a dead provider cannot stall a busy channel or grow
without bound. Losing the connection releases that server's queued lines at
once rather than waiting the timeout out for a server that is gone.

`/translate delin` releases the lines it was still waiting on, shown
untranslated. `/translate delout` releases nothing: it does not touch the
incoming direction, and a message of yours already being translated is still
sent when it comes back.

If the person you are talking to changes nick, the settings and any lines in
flight follow the conversation. The saved setting still names the nick you
typed, so it is that name a restart looks for.

## When translation does not happen

A line that was not translated is shown as its original, marked with the
reason:

    11:32:33 alice> hola que tal [untranslated: timeout]

Never a guess, never a partial translation. A fluent sentence that means
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
refused text for exactly that reason, so it is recoverable either way.
Sending the untranslated original would put something other than what you
intended in front of the channel.

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

A line the translator deliberately skipped — already in the target language,
or too short to be worth translating — is a correct outcome, not a failure.
It is sent and shown with no marker; marking it would cry wolf on a large
share of ordinary traffic.

## What leaves your client

Translating means sending text to an outside service. For each line that is
translated, the request carries:

- the line itself, in full
- the nick who said it, and the channel or query name
- the network name, and the two languages for that direction
- **the channel's nick list**

The nick list travels because only your client knows it, and the translator
needs it to recognise nicks as names rather than words to translate. If that
is more than you want to share with a provider, do not enable translation on
that channel.

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
from the next line. When E2E status cannot be determined at all — an
unreadable keyring, a DM whose peer handle has not resolved yet — the
conversation is treated as encrypted and left untranslated.

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
    /set translate.my_lang           en
    /set translate.show_original_in  true
    /set translate.show_original_out true
    /set translate.timeout_ms        5000
    /set translate.max_in_flight     4
    /set translate.max_queue         200

`translate.max_in_flight` caps concurrent translations. Raising it past what
your provider actually allows makes throughput worse, not better. It takes
effect immediately; lowering it applies as work already in flight finishes.

Outgoing messages are translated one at a time **per connection**, so they
reach IRC in the order you sent them without a slow request on one network
holding up another.

Per-buffer settings live under `[translate.buffers]` in `config.toml` and are
managed with the `add*` / `del*` subcommands rather than `/set`.

Enabling `translate.enabled` at runtime when it was off at startup reports
that a restart is required: the translation workers are built once, when the
client starts. `/reload` says the same thing — editing `enabled = true` in
`config.toml` and reloading cannot build a backend either, so the reload
warns instead of reporting a plain success you would have no reason to
distrust.

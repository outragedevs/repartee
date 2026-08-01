---
category: Other
description: Translate a channel or query in near-real time
---

# /translate

## Syntax

    /translate list
    /translate status
    /translate addin  <#channel|nick> [source-lang]
    /translate delin  <#channel|nick>
    /translate addout <#channel|nick> [source-lang]
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

`source-lang` is a hint. Leave it off and the language is detected
automatically; give it when a channel is reliably one language and you would
rather not pay for detection.

Nothing happens until the master switch is on:

    /set translate.enabled true

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

## Ordering

Lines are sent for translation the instant they arrive and translate
concurrently, but they are only *shown* in the order they arrived. A short
line that comes back quickly will wait for the longer one in front of it, so
an answer can never appear above its question.

A line that has waited longer than `translate.timeout_ms` is shown
untranslated rather than holding up the channel, and the queue is capped at
`translate.max_queue` so a dead provider cannot stall a busy channel or grow
without bound.

## When translation does not happen

A line that was not translated is shown as its original — never a guess, and
never a partial translation. A fluent sentence that means something else is
the one failure a reader cannot detect, so a visible gap is always preferred.

For **outgoing** messages the rule is stricter: if translation fails, the
message is **not sent at all**. Your text goes back into the input line and
an error explains why. Sending the untranslated original would put something
other than what you intended in front of the channel.

The exception is a line the translator deliberately skipped — already in the
target language, or too short to be worth translating. That is a correct
outcome, not a failure, so the original is sent and shown with no marker.

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
    /set translate.target_lang pl

    /translate addin #german de
    /translate addout #german de
    /translate list
    /translate status
    /translate delout #german

## Configuration

    /set translate.enabled           false
    /set translate.target_lang       en
    /set translate.show_original_in  true
    /set translate.show_original_out true
    /set translate.timeout_ms        5000
    /set translate.max_in_flight     4
    /set translate.max_queue         200

`translate.max_in_flight` caps concurrent translations. Raising it past what
your provider actually allows makes throughput worse, not better.

Per-buffer settings live under `[translate.buffers]` in `config.toml` and are
managed with the `add*` / `del*` subcommands rather than `/set`.

Enabling `translate.enabled` at runtime when it was off at startup reports
that a restart is required: the translation workers are built once, when the
client starts.

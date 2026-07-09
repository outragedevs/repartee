# RPE2E companion scripts — align with the Rust E2E gate

**Date:** 2026-07-09
**Branch:** `fix/rpe2e-companion-scripts`
**Scope:** `scripts/weechat/rpe2e.py`, `scripts/irssi/rpe2e.pl`

## Goal

Make the WeeChat (Python) and irssi (Perl) companion scripts enforce the same
end-to-end semantics as the Rust `repartee` client. The overriding rule is
**fail-closed**: if E2E is enabled for a conversation, the scripts must NEVER put
plaintext on the wire and NEVER render raw ciphertext; every refusal must be
visible to the user; cleartext is allowed only after E2E is explicitly disabled.

Reference implementation to mirror (do not diverge from it):

- `src/app/e2e_gate.rs` — `e2e_encrypt_or_passthrough`, `e2e_send_plan_for_target`,
  the `E2eRefusal` variants, the channel-only single-line bot bypass.
- `src/e2e/manager.rs` — `encrypt_outgoing_ctcp` (frame-aware ACTION split).
- `src/e2e/chunker.rs` — `MAX_PLAINTEXT_PER_CHUNK` (180), `MAX_CHUNKS` (16),
  stateless chunks.
- `src/e2e/mod.rs` — `context_key`, `is_channel_target`, `scoped_context`
  (`"{network}\x1F{wire}"`), `wire_context`.
- `docs/rpe2e-dm-addendum.md` — recipient-keyed DM context.

## Findings being fixed

### F1 [HIGH] — outgoing `/me`, `/msg`, `/say` leak plaintext (both scripts)

The only outbound gate bails on any `/`-command:

- weechat `rpe2e.py` `hook_input_text_for_buffer`: `if text.startswith("/"): return text`.
  Registered hooks are only `irc_in2_privmsg`, `input_text_for_buffer`,
  `irc_in2_notice` — there is **no outgoing PRIVMSG hook**.
- irssi `rpe2e.pl` `signal_send_text`: `return if ... $data =~ m{^/}`. Only the
  high-level `send text` signal is hooked.

So `/me` (ACTION), `/msg`, `/say`, `/amsg`, and any other command that emits a
PRIVMSG reach the wire as plaintext even when the conversation has E2E enabled.

### F2 [MED] — irssi mis-renders incoming encrypted ACTIONs

irssi decrypts on `message public` / `message private`, which fire **after**
irssi has spliced out CTCP/ACTION. An encrypted ACTION arrives on the wire as
`+RPE2E01…` (no `\x01`), so it is treated as a normal message, decrypted to
`\x01ACTION…\x01`, and re-emitted with `signal_continue` **past** the CTCP
splitter → renders with raw control characters. WeeChat decrypts in the raw-line
modifier `irc_in2_privmsg`, before CTCP parsing, so it is already correct.

### F3 [LOW-MED] — no per-network context scoping (both scripts)

Storage keys on the bare wire context (`#chan` / `@handle`) with no network
column. One process on two networks shares `#rust`'s config and outgoing key
across both networks — a confidentiality-boundary violation. Rust scopes
storage as `scoped_context(network, wire) = "{network}\x1F{wire}"` while every
byte that leaves the process (AAD, handshake `c=`) uses the bare `wire_context`.

## Design

### Architecture principle

Mirror Rust's single fail-closed chokepoint. In the Rust client every send path
routes through `e2e_encrypt_or_passthrough`. The scripts get the structural
equivalent: **one low-level outgoing chokepoint per client** that every PRIVMSG
passes through, regardless of which command produced it.

- weechat: `weechat.hook_modifier("irc_out1_privmsg", …)`.
- irssi: `Irssi::signal_add_first("server sending command", …)`.

At this layer `/me`, `/msg`, `/say`, `/amsg`, and plain input all appear as a
raw `PRIVMSG target :…` line. The existing input-text hooks stay in place for
plain-line local echo; their already-encrypted `+RPE2E01` output passes through
the chokepoint unchanged. The chokepoint is the **authoritative** gate
(defense-in-depth): any plaintext PRIVMSG to an E2E target that slips past the
input hook is still caught.

### F1 — the outgoing gate (mirror of `e2e_encrypt_or_passthrough`)

Order of checks, per PRIVMSG line reaching the chokepoint:

1. **Re-entrancy guard.** If the message body already starts with `+RPE2E01`,
   pass through unchanged. This covers both the input-hook's own encrypted
   output and the script's `_send_raw_privmsg` re-emissions.
2. **Classify** the target as channel vs DM by the `#&!+` prefix
   (`is_channel_target` / `context_key`).
3. **Bot-command bypass.** Prefix `.`/`!`, **channel-only**, **single-line**
   (no embedded `\n`), emit a **visible** `[E2E] … CLEARTEXT` warning when E2E
   is enabled on that channel (mirror of `warn_e2e_bot_bypass`). DMs never
   bypass. A multi-line body never bypasses.
4. **Resolve the storage context.**
   - Channel: `scoped_context(network, channel)`.
   - DM: resolve the peer handle (live/cached, same resolution `/e2e on` used),
     then `scoped_context(network, "@" + handle)`.
5. **Enabled check + fail-closed refusals**, mirroring the `E2eRefusal` variants
   with a distinct, visible `[E2E]` line each:
   - DM handle unresolved but an enabled config exists → refuse (`NoPeerHandle`
     analogue: "cannot encrypt PM without peer handle — wait for a message from
     them first").
   - keyring/DB read error → refuse (`KeyringRead`: "message NOT sent").
   - encryption itself fails → refuse (`EncryptFailed`: "message NOT sent as
     plaintext").
   - not enabled and no E2E state → pass through as plaintext.
6. **CTCP ACTION framing.** If the body is a `\x01ACTION …\x01` frame, use the
   frame-aware splitter (mirror `encrypt_outgoing_ctcp`): a frame that fits one
   chunk encrypts as-is; a longer ACTION splits into independent, individually
   wrapped `\x01ACTION piece\x01` frames with per-piece budget
   `MAX_PLAINTEXT_PER_CHUNK - len("\x01ACTION ") - 1`. Never fragment the CTCP
   envelope across bare chunks.
7. Emit N `+RPE2E01` PRIVMSG lines (drop/replace the original), draining any
   lazy-rotation REKEY NOTICEs exactly as the input path already does.

**Known limitation (accepted).** For command paths the client core echoes the
plaintext locally *before* the chokepoint decides. On a refusal the user sees
their line locally although nothing reached the wire. Mitigation: a prominent
`[E2E] message NOT sent …` line. Fail-closed is preserved — zero plaintext on
the wire. The plain-input path is unaffected (it echoes only after encrypting).

**NOTICEs are never encrypted.** The chokepoint is PRIVMSG-only; KEYREQ/KEYRSP/
REKEY continue to travel as plaintext CTCP NOTICEs.

### F2 — decrypt irssi before the CTCP split

Move irssi decryption off `message public`/`message private` onto the raw
incoming line signal **`server incoming`** (the analogue of weechat's
`irc_in2_privmsg`). Decrypting there rewrites the raw line so the recovered
`\x01ACTION…\x01` re-enters irssi's normal pipeline **before** the CTCP splitter
and renders as a proper action. Channel/DM/CTCP handling is otherwise unchanged.
Non-RPE2E lines pass through untouched.

### F3 — per-network scoping (mirror `scoped_context` / `wire_context`)

- **Storage key** = `scoped_context(network, wire)` = `"{network}\x1F{wire}"`,
  separator `U+001F`. Network label: weechat `localvar_server`, irssi
  `$server->{tag}`.
- **Interop-critical:** `build_aad` and the handshake `c=` field ALWAYS use the
  bare `wire_context`, never the scoped key — byte-identical to Rust. The scoped
  form exists only in local storage. Inbound handshake `c=` is re-scoped to the
  live network before storage; outbound `c=` carries the bare wire.
- weechat: add a `network` column to `channels`, `incoming`, `outgoing`,
  `outgoing_recipients`, `pending`, `pending_inbound` (and the primary keys);
  migrate existing rows to a best-effort/legacy-unscoped form that still
  resolves (mirroring Rust's legacy fallback so a single-network user is not
  locked out).
- irssi: hash keys become `"{network}\x1F{wire}"`; the JSON keyring gains the
  same scoping with a legacy fallback for pre-scoping keys.

Legacy fallback rule (mirror Rust): a scoped lookup that misses falls back to
the unscoped row **only when it cannot cause cross-network leakage** — i.e. keep
the fallback for reads so an existing single-network user keeps working, but a
send that resolves a handle only via an unscoped legacy row must refuse rather
than send plaintext under a possibly-wrong network (mirror the multi-network
upgrade guard). This is the sharp edge; it gets dedicated review.

## Testing

Tooling present locally: `python3`, `perl` (+ `Crypt::NaCl::Sodium`), `weechat`,
`irssi`. A full two-peer handshake harness is out of scope; verification is:

- weechat: `python3 -m py_compile scripts/weechat/rpe2e.py`.
- irssi: `perl -I<stub> -c scripts/irssi/rpe2e.pl` (stub `Irssi.pm` for the
  runtime-only module).
- Fidelity review against the Rust reference for each finding.
- Where feasible, a script-load smoke test.

## Phasing (separate commits, per-phase code review)

1. **F1 weechat** — `irc_out1_privmsg` gate + CTCP ACTION splitter.
2. **F1 irssi** — `server sending command` gate + CTCP ACTION splitter.
3. **F2 irssi** — move decryption to `server incoming`.
4. **F3 both** — per-network scoping (interop-critical, isolated, own review).
5. PR to `outrage/main`. No merge without approval.

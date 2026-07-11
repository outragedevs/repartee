# RPE2E companion scripts — port the recipient-keyed DM addendum

**Date:** 2026-07-09
**Branch:** `fix/rpe2e-dm-addendum`
**Scope:** `scripts/weechat/rpe2e.py`, `scripts/irssi/rpe2e.pl`
**Protocol spec (normative):** `docs/rpe2e-dm-addendum.md`
**Rust reference:** `src/irc/events.rs` (`incoming_e2e_context`, `set_own_handle`,
`handle_userhost_reply`/`parse_userhost_reply`), `src/app/irc.rs` (one-shot
self-USERHOST at RPL_WELCOME + own-handle reset), `src/commands/handlers_e2e.rs`
(`current_e2e_own_context` command split), `src/e2e/wire.rs`
(`build_aad_golden_vector_dm`).

## Problem

The scripts predate the recipient-keyed DM agreement (June 2026, repartee +
lurker): inbound DM decrypt and auto-KEYREQ key the context off the **sender's**
handle, and there is no own-handle concept at all. Against current repartee:
repartee→script DMs never authenticate (AAD `@<recipient>` vs recomputed
`@<sender>`), script↔script DMs fail both ways. The irssi script additionally
had no DM enable path (`/e2e on|off|mode` were channel-only).

## The rule being ported

- DM context = `@<recipient_handle>`. Encrypt → `@<peer>` (unchanged);
  decrypt → `@<own>` (changed).
- KEYREQ is sent by the party that wants to RECEIVE and stamps `c=@<own_handle>`;
  the responder echoes `c=` verbatim (both scripts already echo verbatim — the
  seam is only in what we stamp and what we look up).
- While our own handle is unknown: inbound DM ciphertext is dropped with a
  notice — never keyed by the sender's handle, never auto-KEYREQ'd (that would
  negotiate the wrong direction). Next message re-establishes after the handle
  is learned. Own handle resets at registration and re-seeds.

## Own-handle capture (prefix-visible sources ONLY)

Final design (review rounds 2026-07-10/11 collapsed an interim ranked-source
scheme): the store only ever holds values PEERS themselves see in our message
prefix — anything else eventually disagrees with it and breaks the
recipient-keyed context. Sources, all equal-trust (newest write wins):
echo-message echoes (weechat), our own JOIN, our own CHGHOST, RPL_HOSTHIDDEN
(396), self-WHOIS RPL_WHOISUSER (311), and a live own-nicklist lookup
(promoted into the store on discovery, because nicklists vanish with the
last part). The one-shot at registration/load is a self-WHOIS, not a
self-USERHOST: solanum-family ircds (Libera) answer a self-USERHOST with the
REAL host (`m_userhost.c`: `target == source` uses `sockhost`/`orighost`),
while 311 carries the DISPLAYED host by definition (`m_whois.c` uses
`target_p->host`; the real host travels only in 338, never parsed). There is
deliberately NO weaker fallback tier: irssi core's `$server->{userhost}` is
not updated on our own CHGHOST (stale risk) and USERHOST is self-view — with
both removed, "unknown" is a transient one-round-trip state (held wires +
visible notice), after which every source in play equals the prefix.
Historical notes: the claim that weechat's `irc_server_connected` fires at
socket-connect was refuted against weechat source (it is emitted in
`IRC_PROTOCOL_CALLBACK(001)`, the welcome numeric); weechat's `irc_in2_*`
modifiers retain IRCv3 tags, so every hook parses through
`_split_irc_tags()` and the decrypted PRIVMSG reconstruction re-prepends the
original tag section.

Per-server volatile store (never persisted):

- **weechat**: module dict `_own_handle[server]`.
  - `irc_server_connected` signal → reset + one-shot `/quote USERHOST <nick>`
    (gated to the connect event, never per message — the 302 reply is itself a
    message and would loop).
  - `irc_in2_302` modifier → parse `nick[*]=[+|-]ident@host` entries (same
    normalization as Rust `parse_userhost_reply`: strip oper `*`, strip away
    `+`/`-`), match own nick case-insensitively, seed the store. Pass the line
    through unmodified.
  - `irc_in2_chghost` modifier → own nick → update store.
  - echo-message: own-nick prefix in `irc_in2_privmsg` seeds the store, and an
    own `+RPE2E01` echo is swallowed (weechat requests all caps by default, so
    echo-message is commonly active; without swallowing, our own ciphertext echo
    would be treated as a peer's → decrypt fail → KEYREQ to ourselves).
  - `irc_server_disconnected` → reset.
  - Script load while already connected: iterate connected servers, send the
    one-shot USERHOST (no 001 will fire for them).
- **irssi**: `%own_handle` keyed by `$server->{tag}` + `_own_handle($server)`
  accessor falling back to `$server->{userhost}` (irssi core seeds that from our
  first own-JOIN and updates it on 396, but NOT on own CHGHOST and not before
  the first join — hence the script-side store on top).
  - `event 001` → reset tag + one-shot `USERHOST <nick>` (raw).
  - `event 302` → parse (same normalization), match `$server->{nick}`, seed.
  - `event chghost` → own nick → update.
  - Load-time: for already-connected servers without a known handle, send the
    one-shot USERHOST.

## Context seams changed

Inbound (both scripts):
- PRIVMSG wire decrypt, DM case: ctx `@<own_handle>`; unknown → drop the wire
  (fail-closed, throttled notice), NO auto-KEYREQ.
- auto-KEYREQ (DM): `c=@<own_handle>`; the config-enabled gate stays keyed by
  the PEER (`@<sender_handle>`) — config describes "E2E with that peer", the
  wire context names the direction's recipient. (weechat's auto-KEYREQ has no
  config gate by design — trigger is undecryptable ciphertext; unchanged.)
- KEYRSP/REKEY handlers: no change — they already install under the verbatim
  wire `c=`, which now arrives as `@<own>` for sessions we requested.
- Reciprocal KEYREQ (serve/accept paths): stamped `@<own>` for DMs instead of
  echoing the requester's context (which named the wrong direction). Rust has a
  known benign residual here (`@<peer>`); the scripts stamp correctly — wire-
  compatible either way since responders serve `c=` verbatim.

Commands:
- Unchanged (peer-keyed stores): on/off/mode (config), accept/decline
  (pending_inbound is keyed by the requester's own-stamped `c=` = `@<peer>` as
  we see them), rotate/outgoing/outgoing_recipients, reverify (handle-only).
- Changed (real incoming DM sessions now live under `@<own>`): verify reads
  `(peer_handle, @<own>)`; revoke/unrevoke/forget touch BOTH `@<own>` (real
  sessions) and `@<peer>` (KEYREQ-direction trust markers); list in a query
  filters by peer handle instead of ctx.
- handshake (DM): enabled check stays `@<peer>`; the KEYREQ stamps `c=@<own>`
  (refused with a clear message while the own handle is unknown).
- irssi `/e2e on|off|mode` extended to query buffers (peer-keyed config),
  matching weechat and the Rust client.

## Migration

None needed: old sender-keyed incoming DM rows simply never match again and the
transport self-heals via session miss → auto-KEYREQ → fresh handshake under the
correct context (the addendum's volatile-handle path). Old DM configs
(`@<peer>`) remain valid as-is.

## Verification

- Golden DM AAD vector from `src/e2e/wire.rs::build_aad_golden_vector_dm`
  asserted byte-for-byte against both scripts' `build_aad`.
- Behavioral harnesses (stubbed weechat/Irssi + NaCl): 302/CHGHOST/echo
  own-handle capture incl. normalization, DM decrypt ctx selection, unknown-own
  drop with no KEYREQ, auto-KEYREQ `c=` stamping, command-path ctx routing,
  channel paths unchanged.
- `python3 -m py_compile` + `perl -c` (stubbed runtime modules).

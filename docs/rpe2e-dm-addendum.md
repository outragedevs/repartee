# RPE2E — Direct Message (private message) addendum

This addendum settles how the `+RPE2E01` end-to-end protocol keys **direct
messages (DMs / private queries)**. Channels are unchanged; everything here is
DM-only. It exists because the original DM behaviour was *internally
inconsistent* (the encrypt and decrypt paths derived the context from different
parties' handles), so DM ciphertext never authenticated between two clients.
Agreed between the `repartee` and `lurker` implementations, June 2026.

## The rule: recipient-keyed context

A message's keyring/AEAD **context** (the `channel` string fed to `build_aad`,
the session lookup, and the HKDF wrap info) is, for a DM, the **recipient's**
server-stamped handle:

```
context(DM message) = "@" + <ident@host of the party that RECEIVES the message>
```

For a channel it remains the channel name verbatim. The `@<handle>` form and
the AAD byte layout are unchanged — only *which* handle goes in the string is
specified here. (`ident@host` is the raw server-stamped value, including any
leading `~` and any cloak/vhost, never the nick.)

### Why the recipient (and not the sender, nor a sorted pair)

- The message wire carries **no channel field**; the receiver reconstructs the
  context from local state, so it must be derivable from wire-visible
  identifiers (`ident@host`).
- Keying on the recipient means the context string in `c=` / the AAD is owned
  and **stamped by exactly one party — the one it names** — and used verbatim by
  the other. There is a single source of truth, so the two sides cannot disagree
  on the bytes (e.g. `~bob@ip` vs `bob@cloak`). A sorted pair of both handles
  would force each side to independently reconstruct the *other* party's handle,
  re-introducing that mismatch as a load-bearing AEAD input.

### Per-direction contexts

A DM conversation between Alice and Bob therefore uses **two** contexts, one per
direction (this is expected — sessions are directional):

| Direction        | context                | Alice's view              | Bob's view                |
|------------------|------------------------|---------------------------|---------------------------|
| Alice → Bob      | `@<bob_handle>`        | outgoing (peer = Bob)     | incoming (self = Bob)     |
| Bob → Alice      | `@<alice_handle>`      | incoming (self = Alice)   | outgoing (peer = Alice)   |

So locally: **encrypt** keys by `@<peer_handle>` (the recipient is the peer);
**decrypt** keys by `@<own_handle>` (the recipient is us).

## Handshake

The KEYREQ is sent by the party that wants to **receive** (the recipient of the
direction being established). It stamps `c=` with its **own** handle:

```
recipient (initiator) ── KEYREQ c=@<own_handle> ──▶ sender (responder)
recipient            ◀── KEYRSP c=@<own_handle> ── sender   (echoed VERBATIM)
```

- The responder uses `c=` **verbatim** — it does not recompute or remap it. The
  signed payload, the HKDF `wrapInfo`/`rekeyInfo`, the installed session row's
  `channel`, and the per-message AAD all use this one agreed string.
- The context is therefore **negotiated once and pinned to the session**, not
  recomputed per message. "Needing your own handle" is only a handshake-time
  concern for the recipient stamping `c=`.

An unsolicited REKEY is likewise stamped by the recipient with `@<own_handle>`
(the peer is rotating the key it uses to send *to us*).

## Volatile handles → re-handshake on miss

`ident@host` can change mid-session (services vhost, oper vhost, CHGHOST). The
inbound session map keys on the live `ident@host`, so a handle change forces a
session miss → auto-KEYREQ → re-handshake, exactly as a peer reconnect does.
Note this re-handshake is **not** silently auto-accepted: a known fingerprint
arriving under a new handle is classified `HandleChanged` and the session stays
refused until `/e2e reverify` (the same TOFU gate as any handle change), unless
an autotrust/auto-accept rule covers it. The point is only that the *transport*
self-heals — the trust decision still applies. Implementations must keep both
the peer's handle (from the prefix / CHGHOST) and their **own** handle current
(from an echo-message echo of their own line, a self-`USERHOST`, or their own
CHGHOST).

While our own handle is **unknown** (e.g. between registration and the
self-`USERHOST` reply), an incoming DM has no recipient context. Implementations
must **not** fall back to keying it by the sender's handle — that would decrypt
and, worse, send a KEYREQ under `@<sender>`, negotiating the wrong DM direction.
Wait until the own handle is learned and let the next message re-establish. On
(re)connect the own handle should be **reset** at registration and re-seeded,
since the server may assign a different ident/host/cloak.

## AAD format (unchanged)

`build_aad` is byte-identical to channels; only the channel string differs. DM
golden vector (`src/e2e/wire.rs::build_aad_golden_vector_dm`):

```
build_aad("@~bob@b.host", msgid=[01;8], ts=100, part=1, total=1) =
  52 50 45 32 45 30 31                          "RPE2E01"
  00 0c 40 7e 62 6f 62 40 62 2e 68 6f 73 74     be16(12) || "@~bob@b.host"
  00 08 01 01 01 01 01 01 01 01                 be16(8)  || msgid
  00 08 00 00 00 00 00 00 00 64                 be16(8)  || ts=100 (be64)
  00 01 01                                       be16(1)  || part=1
  00 01 01                                       be16(1)  || total=1
```

## Summary for implementers

- Channels: context = channel name (one context, both directions). Unchanged.
- DMs: context = `@<recipient_handle>`. Encrypt → `@<peer>`; decrypt → `@<own>`.
- KEYREQ/REKEY: stamped by the recipient with `@<own_handle>`; responder echoes
  `c=` verbatim; context is pinned to the session.
- Keep own + peer `ident@host` current; handle changes self-heal via
  re-handshake on session miss.

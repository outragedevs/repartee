# RPE2E companion scripts — F3: per-network context scoping (follow-up)

**Status:** deferred from the F1/F2 branch `fix/rpe2e-companion-scripts`
(decision 2026-07-09). This doc captures the design and the full change-surface
map so the follow-up can be done as one focused, interop-tested change.

## The finding (F3, LOW-MED)

Both scripts use a single global keyring (one SQLite DB / one JSON `$kr`) shared
across all networks, keyed by the **bare** wire context (`#rust`, `@ident@host`).
One process on two networks therefore shares `#rust`'s config and outgoing key
across both — a confidentiality-boundary violation.

## Target model (mirror Rust exactly)

- **Storage key** = `scoped_context(network, wire)` = `"{network}\x1F{wire}"`
  (U+001F). Encoded as a single string — no schema change; the SQLite `channel`
  column / JSON hash key just holds the scoped string.
- **On the wire, BARE always:** `build_aad`, the KEYREQ/KEYRSP/REKEY `c=` field,
  the signature payloads (`KEYREQ:`/`KEYRSP:`/`REKEY:` …), and the HKDF
  `RPE2E01-WRAP:` / `RPE2E01-REKEY:` info strings all use `wire_context(ctx)`
  (the part after `\x1F`, or the whole string if unscoped). Interop with the
  Rust client and between the two scripts must stay byte-identical.
- **Inbound handshake:** verify the signature against the received **bare** `c=`,
  then store under `scoped_context(local_network, c=)`. Mirrors
  `src/irc/events.rs:4499-4507` (scope `req.channel` for storage) while the
  manager verifies/derives against `wire_context`.

## The migration sharp-edge (why this is not a mechanical change)

Existing users have **bare** rows (`#rust`, `@handle`). After the upgrade the
gate looks up `scoped(net, #rust)` and misses. Two wrong ways out:

- **Fail-open:** treat the miss as "not enabled" → silently downgrade a
  previously-E2E conversation to **plaintext**.
- **Cross-network leak:** a naive bare-row fallback lets network A's `#rust`
  encrypt under network B's `#rust` config/key — reintroducing the F3 bug.

Rust avoids both with a **configured-networks list**: it only falls back to /
adopts a legacy bare row when a **single** network is configured
(`legacy_adoption_allowed`), and refuses (fail-closed) otherwise. The scripts
have no such list yet and must add one:

- weechat: enumerate IRC servers via the `irc_server` infolist.
- irssi: `Irssi::servers()`.

Rule (mirror Rust): scoped lookup first; on miss, consult the bare row **only
when exactly one network is configured** (then adopt it — rewrite to scoped and
drop the bare row). With >1 network, a send that resolves only via a bare row
must **refuse**, not send plaintext. Enumerate the 0/1/2+ configured-network
cases explicitly (see the project memory `e2e-fail-closed-gate-rule`).

## Hazards found in the surface map (MUST address)

1. **weechat already uses `\x1F`** inside `_pending_key`
   (`_pending_key(channel, handle) → f"{channel}\x1f{handle}"`, `rpe2e.py:506`).
   Scoping with the same separator collides. Scope the **outer** key and keep
   the composite: `"{network}\x1F{channel}\x1F{handle}"`, or pick a distinct
   separator for one layer. irssi uses `|` for the composite (`"$channel|$handle"`),
   so it only needs the outer `\x1F` — but be consistent.
2. **irssi has NO network-label source** anywhere in the file. Add
   `$server->{tag}` (or `->{chatnet}`). `$server` is threaded to every gate /
   decrypt / notice site EXCEPT the handshake handlers `handle_keyreq` /
   `handle_keyrsp` / `handle_rekey` (`rpe2e.pl:734/803/880`), which currently
   take `($kr, $sender_handle, $nick, $body)` — thread the network label in.
3. **Aggregate/scan sites span networks** and need scope-filtering, not just
   key-rewriting: `/e2e status` counts (`rpe2e.py:2364-2365`, `rpe2e.pl:1075-1076`),
   `/e2e list`, and the `handle LIKE`/handle-pattern deletes in `/e2e reverify`
   and `/e2e forget -all` (`rpe2e.py:2609/2637/2612/2639`,
   `rpe2e.pl:1256-1259/1313-1315/1331-1333`) — decide per-command whether they
   should be per-network or all-network.
4. **Handshake handlers store the received `c=` verbatim** — the single most
   important set of sites. Each needs BOTH forms: bare `ctx` for
   verify + `c=`/sig/KDF (echoed back on the wire), and
   `scope(local_network, ctx)` for every storage write.
5. **`_config_enabled` / gate context derivation** (`rpe2e.py:1827`, the
   `_e2e_gate_wire` `context =` lines; `rpe2e.pl` `_gate_decide` `$ctx =` lines)
   must scope, and the network label must be threaded from the `server` hook
   param / `$server->{tag}`.
6. **export/import** (`/e2e export`/`import`) round-trips the raw keys — decide
   whether exported keys are scoped or bare (bare is more portable across
   networks; scoped preserves the boundary). Rust exports/imports its stored
   form; match whatever keeps cross-tool import working.

## Full change-surface map

See the appendix below (generated 2026-07-09). Each site is tagged
`[STORAGE-KEY | AAD | WIRE-c= | SIG | NETWORK-LABEL]`. STORAGE-KEY sites get
scoped; AAD/WIRE-c=/SIG/KDF sites stay bare.

### weechat (`scripts/weechat/rpe2e.py`)

- **Helpers producing the key:** `context_key` (505-512), `_ctx_for_target`
  (839-846), `_ctx_for_command`/`_ctx_or_error` (848-883), `_e2e_gate_wire`
  context lines (~1971-1992), `_pending_key` (505-506, already `\x1F`).
- **`channels` (PK=channel):** 1282, 1827, 2230, 2328-2329, 2336, 2350,
  2364-2365 (agg), 2370, 2772 (export), 2899-2902 (import).
- **`outgoing` (PK=channel):** 1757/1763, 1772/1779, 2471, 2651, 2770 (export),
  2896 (import).
- **`incoming` (PK=handle,channel):** 1226-1227, 1247-1248, 1349-1350,
  1363-1364, 1496-1497, 1521-1522, 1605-1606, 1624-1625, 2134-2135,
  2364 (agg), 2382-2383, 2423-2424, 2447-2448, 2464-2465, 2486-2487,
  2503-2504, 2543-2544, 2609/2637 (`handle LIKE`, cross-network), 2767/2882.
- **`outgoing_recipients` (PK=channel,handle):** 1211-1212, 1655-1656,
  2468-2469, 2612-2614/2639-2641 (`handle LIKE`), 2777/2913.
- **`pending` (PK=channel=_pending_key):** 502 (wipe), 1160/1167, 1174-1175,
  1229-1232, 1250-1253, 1423-1426, 1428-1430 (bare fallback), 1435, 1437-1440.
- **`pending_inbound` (PK=handle,channel):** 1367-1370, 2402-2404, 2409-2411,
  2443-2444.
- **AAD (bare):** def 551-552; calls 1868 (encrypt), 2162 (decrypt).
- **Wire `c=` (bare):** parse 1061/1089/1127; build 1178, 1215, 1644.
- **SIG/KDF (bare):** payload defs 643-694; sign 1170/1195-1197/1641; verify
  1269-1272/1410-1418/1536-1544; KDF info 1190/1444/1597/1637.
- **Network label:** `localvar_server` 1005-1007/2196-2197/2317-2318; hook
  `server` param in `hook_irc_out_privmsg` (2040), `hook_irc_in_privmsg` (2101),
  `hook_irc_in_notice` (2248).
- **Handshake verbatim-store:** `handle_keyreq` `ctx=req["channel"]` (1278) →
  1282/1349-1350/1363-1364/1367-1370; `handle_keyrsp` `ctx=rsp["channel"]`
  (1408) → 1424-1439/1496-1497/1521-1522; `handle_rekey` `ctx=rk["channel"]`
  (1546) → 1605-1606/1624-1625.

### irssi (`scripts/irssi/rpe2e.pl`)

- **Helpers producing the key:** `_ctx_for_target` (515-519),
  `_resolve_ctx_for_command` (1014-1021), `_gate_decide` `$ctx=` (1669/1681),
  `_decrypt_wire_message` `$ctx` (1769), `signal_event_privmsg` `$ctx_target`
  (1822-1826), `_pending_key` (639-641, uses `|`).
- **`$kr->{channels}`:** 741, 1035, 1046, 1060, 1076 (agg), 1143, 1658, 1677,
  1684, 1781, 1386-1387 (export), 1435 (import).
- **`$kr->{outgoing}`:** 610/615, 625/631, 1214, 1346/1349, 1690,
  1381-1382 (export), 1427 (import).
- **`$kr->{incoming}` (key `"$handle|$ctx"`):** 718, 727, 773, 779, 847, 921,
  1075 (agg), 1103-1105, 1125, 1131, 1178, 1195, 1210, 1231, 1256-1259,
  1270-1272, 1288, 1313-1315, 1331-1333, 1376-1377, 1416-1418, 1777.
- **`$kr->{outgoing_recipients}` (key `"$channel|$handle"`):** 697, 932-934,
  1213, 1256-1259, 1313-1315, 1331-1333, 1391, 1441-1443.
- **`$kr->{pending}` (key `_pending_key`):** 648/652/657, 719, 730, 809,
  811 (bare fallback), 1256-1259.
- **`$kr->{pending_inbound}` (key `"$handle|$ctx"`):** 785, 1162, 1194,
  1256-1259, 1862.
- **AAD (bare):** def 321; calls 1558 (encrypt), 1796 (decrypt).
- **Wire `c=` (bare):** parse 432/460/492; build 666, 706, 870.
- **SIG/KDF (bare):** payload defs 396-408; sign 656/684/866; verify
  740/808/885; KDF info 679/816/862/905.
- **Network label:** ABSENT — add `$server->{tag}`; thread it into
  `handle_keyreq/keyrsp/rekey` (734/803/880), which currently lack `$server`.
- **Handshake verbatim-store:** `handle_keyreq` `$ctx=$req->{channel}` (738) →
  741/773/779/785; `handle_keyrsp` `$ctx=$rsp->{channel}` (807) →
  809/811/847; `handle_rekey` `$ctx=$rk->{channel}` (884) → 921.

## Test plan for the follow-up

- Unit/headless: scoped storage round-trips; AAD/`c=`/sig verified BARE (a wire
  produced under `scoped(netA,#x)` must decrypt with `build_aad("#x", …)`).
- Interop: script↔script AND script↔Rust — same peer, same `#chan`, on two
  networks must NOT share a key; a wire from one must not AEAD-verify under the
  other's context.
- Migration: single-network keyring with bare rows adopts to scoped and keeps
  working; multi-network keyring with a same-name channel refuses rather than
  cross-contaminating (mirror `multi_network_upgraded_legacy_dm_refuses_*`).

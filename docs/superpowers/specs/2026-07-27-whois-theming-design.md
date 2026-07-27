# Full themable coverage for the WHOIS block (TUI)

**Date:** 2026-07-27
**Branch:** `fix/whois-numeric-theming`
**Scope:** `src/irc/events.rs`, `themes/default.theme`, `themes/spring.theme`,
`docs/src/content/theming.md`
**Reference implementations:** irssi `~/dev/erssi/src/fe-common/irc/fe-whois.c`
(numeric table + `whois_special` fallback), `~/dev/erssi/src/fe-common/irc/module-formats.c`
(the `{whois }` abstract), IRCnet ircd 2.12.0 `common/numeric_def.h` + `ircd/s_user.c`.

## Problem

Two complaints, verified separately.

**1. Errors in a WHOIS reply are not themable.** `401 ERR_NOSUCHNICK`,
`402 ERR_NOSUCHSERVER` and `263 RPL_TRYAGAIN` all fall through to the generic
numeric catch-alls in `src/irc/events.rs` and are emitted with
`event_key: None`, so no theme entry can reach them. 263 additionally lands in
the **server buffer** (`server_buffer`, taken because `Response::is_error()` is
false for a 2xx) while the rest of the WHOIS block goes to the active window —
one logical reply split across two buffers.

**2. The block cannot be restyled coherently.** Each of the 20 existing
`whois_*` keys hardcodes its own colour and its own two-space indent. Changing
how the block looks means editing 20 entries in lockstep and keeping them
consistent by hand. irssi solves this with a `{whois }` abstract that every
WHOIS format routes through; repartee's theme engine already supports
abstracts (`msgnick`, `timestamp`) but does not use them here.

Not a problem, verified and explicitly out of scope: on IRCnet the TLS and
cloak lines are **both** numeric 320 (`RPL_WHOISEXTRA`, with `RPL_WHOISTLS` and
`RPL_WHOISCLOAKED` aliased to it in `common/numeric_def.h:238-240`), and their
text comes from the site-local `WHOISTLS` / `CLOAK_WHOISEXTRA` defines in the
server's `config.h` (`ircd/s_user.c:2210-2220`). They are indistinguishable by
numeric and their wording is chosen by the server admin. Repartee therefore
renders 320 verbatim through `whois_special` and does not attempt to classify
it. Libera's `secure: TLS` (numeric 671) looking different from IRCnet's
`is a Secure Connection (SSL/TLS)` (numeric 320) is a faithful reflection of
what arrived on the wire, not a defect.

For the two networks the user actually runs, current coverage of the
*informational* numerics is already complete: Libera sends 311, 319, 312, 671,
378, 317, 330, 318; IRCnet 2.12 sends 311, 319, 312, 301, 313, 320, 320, 317,
318. Every one of those already has a theme key. The numeric-table additions
below are hardening for other networks, not a fix for present-day breakage.

## Design

### 1. Numeric table

Detection stays **stateless** — a numeric-keyed table, no WHOIS-in-flight
tracking. A numeric outside the table keeps today's behaviour.

Unchanged (17 existing mappings): 311 → `whois_header` + `whois`, 312 →
`whois_server`, 313 → `whois_oper`, 317 → `whois_idle` / `whois_idle_signon`,
319 → `whois_channels`, 301 → `whois_away`, 330 → `whois_account`, 276 →
`whois_certfp`, 671 → `whois_secure`, 307 → `whois_registered`, 310 →
`whois_help`, 320 → `whois_special`, 335 → `whois_bot`, 338 → `whois_actually`,
378 → `whois_host`, 379 → `whois_modes`, 318 → `end_of_whois`.

Added:

| Numeric | Key | Kind |
|---|---|---|
| 401 `ERR_NOSUCHNICK` | `no_such_nick` | new key, **not** whois-prefixed — see below |
| 402 `ERR_NOSUCHSERVER` | `no_such_server` | new key |
| 263 `RPL_TRYAGAIN` | `try_again` | new key, **and** routed to the active window instead of the server buffer |
| 326 | `whois_modes` | reuse |
| 327 `RPL_WHOISHOST` (rusnet) | `whois_host` | reuse |
| 337 `RPL_WHOISTEXT` (hybrid) | `whois_special` | reuse |
| 275 `RPL_USINGSSL` (Bahamut) | `whois_special` | reuse |
| 377 | `whois_modes` | reuse, **guarded** |

Two mappings need justification.

**275 maps to `whois_special`, not `whois_secure`.** 275 is `RPL_USINGSSL` on
Bahamut but `RPL_STATSDLINE` on hybrid/charybdis. A stateless table cannot tell
the two apart. Routing it to `whois_secure` would print `secure: TLS` on a
`/stats` line — an assertion that is simply false. Routing it to
`whois_special` at worst indents a `/stats` line: cosmetic, not wrong.

**377 is guarded on `params[1] == "usermodes"`.** In AustHex 377 is `RPL_SPAM`,
post-MOTD announcement text; mapping it unconditionally would restyle MOTD
output. The guard tests a protocol literal, not admin-authored prose, so it is
not the content-sniffing the user rejected. When the guard fails, 377 falls
through to today's behaviour unchanged.

Deliberately excluded: **431** (`cmd_whois` never sends a WHOIS without a nick —
`whois_request_parts` returns `None` for empty args and the command aborts
locally with a usage message, so 431 cannot arrive in reply to `/whois nick` or
`/whois nick nick`), and **308/309/316** (308 collides with `RPL_RULESSTART`,
309 with `RPL_WHOISSERVICE`, 316 is marked "redundant and not needed but
reserved" in IRCnet's own header and is not sent). Adding any of them later is
one line in the table.

### 2. Error routing and framing

**The three error keys are deliberately *not* `whois_`-prefixed.** 401, 402 and
263 are not WHOIS-specific: 401 also answers a PRIVMSG, NOTICE, INVITE or KICK
aimed at a nick that does not exist, 402 answers any command taking a server
parameter, and 263 throttles any command at all. Because detection is stateless
(§1), the handler cannot know which command provoked the reply. Naming the keys
`whois_no_such_nick` would assert a context we cannot verify and would drag
WHOIS block styling onto a failed `/msg`. Generic names state exactly what is
known.

They render as a single line, styled through the theme's existing `error`
abstract rather than the `whois` abstract, e.g. `no_such_nick = "{error $1}"`.
No `whois_header` / `end_of_whois` frame is synthesised around them: an error
reply carries no 311, so there is no block to frame, and inventing one would
misrepresent the wire.

401 and 402 keep today's destination (active window, via `is_error()`); only
their `event_key` changes from `None` to the new keys. 263 changes destination
from the server buffer to the active window, which is the behaviour change that
makes the rate-limit notice visible where the user is looking.

### 3. The `whois` abstract

Two new abstracts, and every `whois_*` format in both shipped themes rewritten
in terms of them:

```toml
[abstracts]
whois       = "%Z565f89  $*%N"     # indent + base colour for a block line
whois_value = "%Za9b1d6$*%N"       # highlighted value

[formats.events]
whois_channels = "{whois channels: {whois_value $1}}"
whois_special  = "{whois {whois_value $1}}"
whois_secure   = "{whois secure: %Z9ece6a$1%N}"
```

This is theme-file-only work: `resolve_abstractions` already runs before
parameter substitution in `render_event` (`src/ui/message_line.rs:80-86`),
already supports nesting (`{msgnick $2 {ownnick $0}}`), and `$*` joins the
abstract's arguments with a single space (`src/theme/parser.rs:121-124`). No
theme-engine change is required.

The hardcoded fallback strings built in `src/irc/events.rs` (used when a theme
omits a key) keep their literal colours — they are the last-resort rendering
path and cannot reference theme abstracts.

### 4. Parameter convention

For every `whois_*` key: `$0` is always the nick, `$1` is the line's primary
value, and further parameters are detail. This already holds for most keys by
accident; the design makes it a tested invariant and documents it.

The three generic error keys are outside that invariant — they are not part of
the block. `no_such_nick` gets `$0` = the nick that was not found, `$1` = the
server's reason text; `no_such_server` gets `$0` = the server name, `$1` =
reason; `try_again` gets `$0` = the command that was throttled, `$1` = reason.

`whois_secure` keeps `$1 = "TLS"` with the server's own text in `$2`. Changing
`$1` to the server text would silently alter the output of every existing user
theme; `$2` is already available to anyone who wants the verbatim wording.

## Testing

Unit tests in `src/irc/events.rs`, following the existing table-driven style at
`events.rs:9118-9123`:

1. Each newly mapped numeric emits its expected `event_key` — one case per row
   of the added table.
2. 263 lands in the WHOIS buffer, not the server buffer.
3. 377 with `params[1] == "usermodes"` maps to `whois_modes`; 377 with any
   other second parameter does **not** produce a `whois_*` key.
4. Every `whois_*` emission puts the nick in `event_params[0]` — one test
   iterating the full set of WHOIS numerics, asserting the invariant from §4.
5. Both shipped themes parse and define every `whois_*` key the code can emit —
   a test that walks the key list against `themes/default.theme` and
   `themes/spring.theme` so a new key can never ship with only one theme
   updated.

Verification: `make test` and `make clippy` clean (0 warnings, pedantic +
nursery + perf=deny + redundant_clone=deny).

## Out of scope

- **Web UI theming.** `WireMessage` carries `event_key` but not
  `event_params`, and `message_to_wire` ships the pre-rendered `text`
  (`src/web/snapshot.rs:130`), so the web frontend shows the hardcoded fallback
  colours regardless of theme. Fixing that means moving theme rendering to the
  WASM client — a separate, larger piece of work, deferred by the user to a
  later session.
- Host normalisation across 327/338/378 (irssi's `whois_realhost` folding).
- `RPL_REOPLIST` 344/345 (IRCnet reop list — not a WHOIS reply).

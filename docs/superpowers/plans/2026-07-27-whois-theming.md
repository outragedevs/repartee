# WHOIS Block Theming Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every line that can appear in reply to `/whois nick` and `/whois nick nick` carry a theme key, and make the whole WHOIS block restylable from a single place in the theme file.

**Architecture:** Stateless numeric→theme-key mapping in `src/irc/events.rs` (extend the existing `whois_freeform_key` table and add explicit `Response` arms for the three error numerics), plus a `whois` / `whois_value` abstract pair in the shipped `.theme` files that every `whois_*` format routes through. No theme-engine changes — `resolve_abstractions` already runs before parameter substitution.

**Tech Stack:** Rust 2024, `irc-repartee` / `irc-proto-repartee` 1.2.2, TOML themes, `cargo test` via `make test`.

**Spec:** `docs/superpowers/specs/2026-07-27-whois-theming-design.md`

## Global Constraints

- Build only through `make` targets — never raw `cargo`/`trunk`. (`make test`, `make clippy`.)
- Clippy: pedantic=warn, nursery=warn, perf=deny, redundant_clone=deny — **0 warnings**.
- No ratatui imports in `src/state/`; `src/irc/events.rs` stays UI-agnostic.
- Never hardcode the app name — use `APP_NAME`.
- Tests live in the existing `#[cfg(test)] mod tests` block at the bottom of `src/irc/events.rs`, using the existing `make_test_state()` / `make_irc_msg()` helpers. `make_test_state()` creates buffer id `test/testserver`.
- Error keys are `no_such_nick` / `no_such_server` / `try_again` — **not** whois-prefixed. See spec §2.

---

### Task 1: Theme keys for the three error numerics

**Files:**
- Modify: `src/irc/events.rs` — add `Response::ERR_NOSUCHNICK`, `Response::ERR_NOSUCHSERVER`, `Response::RPL_TRYAGAIN` arms before the `_ =>` catch-all at `src/irc/events.rs:4161`
- Test: `src/irc/events.rs` (`mod tests`)

**Interfaces:**
- Consumes: `emit_event(state, buffer_id, event_key, text, event_params)`, `active_or_server_buffer(state, conn_id)` — both already defined in this file.
- Produces: event keys `no_such_nick`, `no_such_server`, `try_again`, consumed by Task 3's theme entries and Task 3's `WHOIS_EVENT_KEYS` sibling list.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/irc/events.rs`:

```rust
    #[test]
    fn whois_error_numerics_get_event_keys() {
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");

        for (response, args, expected_key, expected_params) in [
            (
                Response::ERR_NOSUCHNICK,
                vec!["me", "ghost", "No such nick/channel"],
                "no_such_nick",
                vec!["ghost", "No such nick/channel"],
            ),
            (
                Response::ERR_NOSUCHSERVER,
                vec!["me", "irc.example.net", "No such server"],
                "no_such_server",
                vec!["irc.example.net", "No such server"],
            ),
            (
                Response::RPL_TRYAGAIN,
                vec!["me", "WHOIS", "Please wait a while and try again."],
                "try_again",
                vec!["WHOIS", "Please wait a while and try again."],
            ),
        ] {
            let msg = make_irc_msg(
                None,
                Command::Response(
                    response,
                    args.iter().map(|s| (*s).to_string()).collect(),
                ),
            );
            handle_irc_message(&mut state, "test", &msg);

            let buf = state.buffers.get("test/testserver").unwrap();
            let m = buf.messages.back().unwrap();
            assert_eq!(
                m.event_key.as_deref(),
                Some(expected_key),
                "{response:?} should map to {expected_key}"
            );
            assert_eq!(
                m.event_params.as_deref(),
                Some(
                    &expected_params
                        .iter()
                        .map(|s| (*s).to_string())
                        .collect::<Vec<_>>()[..]
                ),
                "{response:?} params"
            );
        }
    }

    #[test]
    fn try_again_lands_in_active_window_not_server_buffer() {
        // 263 is a 2xx, so the generic catch-all sent it to the server buffer
        // while the rest of the WHOIS reply went to the active window —
        // splitting one logical reply across two buffers.
        let mut state = make_test_state();
        state.set_active_buffer("test/#chan");

        let msg = make_irc_msg(
            None,
            Command::Response(
                Response::RPL_TRYAGAIN,
                vec![
                    "me".to_string(),
                    "WHOIS".to_string(),
                    "Please wait a while and try again.".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &msg);

        let buf = state.buffers.get("test/#chan").expect("active buffer");
        let m = buf.messages.back().expect("263 must land in active window");
        assert_eq!(m.event_key.as_deref(), Some("try_again"));
    }
```

If `make_test_state()` does not already create `test/#chan`, create it in the
second test with the same helper the other channel tests in this module use
(search `mod tests` for `#chan` and copy that setup verbatim).

- [ ] **Step 2: Run test to verify it fails**

Run: `make test 2>&1 | grep -A5 whois_error_numerics`
Expected: FAIL — `event_key` is `None`, not `Some("no_such_nick")`.

- [ ] **Step 3: Write minimal implementation**

Insert immediately before the `_ =>` arm at `src/irc/events.rs:4161`:

```rust
        // 401/402/263 can answer a WHOIS but are not WHOIS-specific: 401 also
        // answers PRIVMSG/NOTICE/INVITE/KICK at a missing nick, 402 any command
        // taking a server parameter, 263 throttles any command. Detection here
        // is stateless, so the keys stay generic rather than asserting a WHOIS
        // context we cannot verify.
        Response::ERR_NOSUCHNICK | Response::ERR_NOSUCHSERVER => {
            if args.len() >= 3 {
                let key = if response == Response::ERR_NOSUCHNICK {
                    "no_such_nick"
                } else {
                    "no_such_server"
                };
                let target_buf = active_or_server_buffer(state, conn_id);
                emit_event(
                    state,
                    &target_buf,
                    key,
                    format!("%Zf7768e! %Za9b1d6{}%N %Z565f89{}%N", args[1], args[2]),
                    vec![args[1].clone(), args[2].clone()],
                );
            }
        }
        // RPL_TRYAGAIN is a 2xx, so the catch-all routed it to the server
        // buffer — away from the command that provoked it.
        Response::RPL_TRYAGAIN => {
            if args.len() >= 3 {
                let target_buf = active_or_server_buffer(state, conn_id);
                emit_event(
                    state,
                    &target_buf,
                    "try_again",
                    format!("%Zf7768e! %Za9b1d6{}%N %Z565f89{}%N", args[1], args[2]),
                    vec![args[1].clone(), args[2].clone()],
                );
            }
        }
```

If `Response` does not implement `PartialEq`, replace the `if response == …`
comparison with a `matches!(response, Response::ERR_NOSUCHNICK)` call.

- [ ] **Step 4: Run tests to verify they pass**

Run: `make test`
Expected: PASS, including the two new tests and the pre-existing
`silent_banlist` tests that also touch `ERR_NOSUCHCHANNEL` (the new arms must
not intercept `ERR_CHANOPRIVSNEEDED` / `ERR_NOSUCHCHANNEL` / `ERR_NOTONCHANNEL`,
whose silent-banlist suppression still lives in the `_ =>` arm).

- [ ] **Step 5: Run clippy**

Run: `make clippy`
Expected: 0 warnings.

- [ ] **Step 6: Commit**

```bash
git add src/irc/events.rs
git commit -m "feat(irc): theme keys for 401/402/263 and route 263 to the active window"
```

---

### Task 2: Extend the freeform numeric table

**Files:**
- Modify: `src/irc/events.rs:4343-4354` (`whois_freeform_key`) and `src/irc/events.rs:4397` (`handle_whois_freeform`)
- Test: `src/irc/events.rs` (`mod tests`)

**Interfaces:**
- Consumes: `whois_freeform_key(numeric: &str) -> Option<&'static str>`, `handle_whois_freeform(state, conn_id, numeric, args)` — both already defined.
- Produces: `whois_freeform_key` gains a second parameter, becoming
  `whois_freeform_key(numeric: &str, args: &[String]) -> Option<&'static str>`,
  so the 377 guard can inspect `args[1]`. The dispatch guard at
  `src/irc/events.rs:243` must pass `args` too.

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
    #[test]
    fn whois_extra_freeform_numerics_get_event_keys() {
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");

        for (numeric, expected_key, text) in [
            ("326", "whois_modes", "has oper privs: +Aa"),
            ("327", "whois_host", "real.host.example 1.2.3.4 :Real hostname/IP"),
            ("337", "whois_special", "is connected via a webirc gateway"),
            ("275", "whois_special", "is using a secure connection (SSL)"),
        ] {
            let msg = make_irc_msg(
                None,
                Command::Raw(
                    numeric.to_string(),
                    vec!["me".to_string(), "alice".to_string(), text.to_string()],
                ),
            );
            handle_irc_message(&mut state, "test", &msg);

            let buf = state.buffers.get("test/testserver").unwrap();
            let m = buf.messages.back().unwrap();
            assert_eq!(
                m.event_key.as_deref(),
                Some(expected_key),
                "numeric {numeric} should map to {expected_key}"
            );
            assert_eq!(
                m.event_params.as_deref(),
                Some(&["alice".to_string(), text.to_string()][..]),
                "numeric {numeric} params"
            );
        }
    }

    #[test]
    fn whois_377_maps_to_modes_only_with_usermodes_literal() {
        // AustHex uses 377 as RPL_SPAM for post-MOTD text. Only the
        // `<me> usermodes <nick> <modes>` shape is a WHOIS usermode line.
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");

        let whois_form = make_irc_msg(
            None,
            Command::Raw(
                "377".to_string(),
                vec![
                    "me".to_string(),
                    "usermodes".to_string(),
                    "alice".to_string(),
                    "+iwx".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &whois_form);
        let buf = state.buffers.get("test/testserver").unwrap();
        assert_eq!(
            buf.messages.back().unwrap().event_key.as_deref(),
            Some("whois_modes")
        );

        let spam_form = make_irc_msg(
            None,
            Command::Raw(
                "377".to_string(),
                vec![
                    "me".to_string(),
                    "alice".to_string(),
                    "Network announcement text".to_string(),
                ],
            ),
        );
        handle_irc_message(&mut state, "test", &spam_form);
        let buf = state.buffers.get("test/testserver").unwrap();
        let m = buf.messages.back().unwrap();
        assert_eq!(
            m.event_key, None,
            "RPL_SPAM form of 377 must not be themed as a WHOIS line"
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `make test 2>&1 | grep -A5 whois_extra_freeform`
Expected: FAIL — `event_key` is `None` for 326/327/337/275.

- [ ] **Step 3: Write minimal implementation**

Replace `whois_freeform_key` at `src/irc/events.rs:4343-4354`:

```rust
/// Theme event key for WHOIS numerics whose payload is freeform prose and
/// which irc-proto has no `Response` variant for. Single source of truth for
/// both the dispatch guard and the per-numeric theming.
///
/// `args` is the full numeric argument list (`[our_nick, ...]`); it is only
/// inspected for 377, which is `RPL_SPAM` (post-MOTD announcement text) in
/// AustHex and a WHOIS usermode line elsewhere. The `usermodes` literal is a
/// protocol token, not admin-authored prose, so keying on it is safe.
fn whois_freeform_key(numeric: &str, args: &[String]) -> Option<&'static str> {
    match numeric {
        "307" => Some("whois_registered"),
        "310" => Some("whois_help"),
        "320" => Some("whois_special"),
        "335" => Some("whois_bot"),
        "338" => Some("whois_actually"),
        "378" => Some("whois_host"),
        "379" => Some("whois_modes"),
        // rusnet RPL_WHOISHOST; hybrid RPL_WHOISTEXT.
        "327" => Some("whois_host"),
        "337" => Some("whois_special"),
        // 326 carries oper privileges as a mode string.
        "326" => Some("whois_modes"),
        // 275 is RPL_USINGSSL on Bahamut but RPL_STATSDLINE on
        // hybrid/charybdis. A stateless table cannot tell them apart, so it
        // renders as prose rather than asserting "secure: TLS" over a /stats
        // line.
        "275" => Some("whois_special"),
        "377" if args.get(1).is_some_and(|a| a == "usermodes") => Some("whois_modes"),
        _ => None,
    }
}
```

Update the dispatch guard at `src/irc/events.rs:243`:

```rust
        Command::Raw(cmd, args) if args.len() >= 3 && whois_freeform_key(cmd, args).is_some() => {
            handle_whois_freeform(state, conn_id, cmd, args);
        }
```

Update the lookup inside `handle_whois_freeform` at `src/irc/events.rs:4398`:

```rust
    let Some(event_key) = whois_freeform_key(numeric, args) else {
        return;
    };
```

377's WHOIS form is `[me, "usermodes", nick, modes]`, so the existing
`args[1]` = nick / `args[2..]` = text extraction would put `"usermodes"` in the
nick slot. Add a shape fix at the top of `handle_whois_freeform`, right after
the `args.len() < 3` guard:

```rust
    // 377 WHOIS form is `<me> usermodes <nick> <modes>` — the nick is one
    // position further right than in every other freeform numeric.
    let (nick_idx, text_from) = if numeric == "377" { (2, 3) } else { (1, 2) };
    if args.len() <= text_from {
        return;
    }
    let text = args[text_from..].join(" ");
    let nick = args[nick_idx].clone();
```

then use `nick` and `text` in the `emit_event` call in place of
`args[1].clone()` and the old `text`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `make test`
Expected: PASS, including the pre-existing
`whois_raw_freeform_numerics_get_event_keys`,
`whois_raw_320_short_form_falls_to_catch_all` and
`whois_raw_actually_joins_middle_args`.

- [ ] **Step 5: Run clippy**

Run: `make clippy`
Expected: 0 warnings.

- [ ] **Step 6: Commit**

```bash
git add src/irc/events.rs
git commit -m "feat(irc): cover 275/326/327/337 and guarded 377 in the WHOIS theme table"
```

---

### Task 3: `whois` abstract + theme coverage test

**Files:**
- Modify: `themes/default.theme` (`[abstracts]`, `[formats.events]`)
- Modify: `themes/spring.theme` (`[abstracts]`, `[formats.events]`)
- Modify: `src/irc/events.rs` — add the `WHOIS_EVENT_KEYS` constant
- Test: `src/irc/events.rs` (`mod tests`)

**Interfaces:**
- Consumes: event keys emitted by Tasks 1 and 2.
- Produces: `pub const WHOIS_EVENT_KEYS: &[&str]` — the authoritative list of theme keys the WHOIS path can emit, used by the coverage test.

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
    #[test]
    fn shipped_themes_define_every_whois_key() {
        for (name, src) in [
            ("default.theme", include_str!("../../themes/default.theme")),
            ("spring.theme", include_str!("../../themes/spring.theme")),
        ] {
            let theme: crate::theme::ThemeFile =
                toml::from_str(src).unwrap_or_else(|e| panic!("{name} must parse: {e}"));
            for key in WHOIS_EVENT_KEYS {
                assert!(
                    theme.formats.events.contains_key(*key),
                    "{name} is missing event format `{key}`"
                );
            }
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `make test 2>&1 | grep -A5 shipped_themes_define`
Expected: FAIL — `WHOIS_EVENT_KEYS` is not defined yet (compile error), then
after Step 3's constant lands but before the theme edits, FAIL with
"default.theme is missing event format `no_such_nick`".

- [ ] **Step 3: Write minimal implementation**

Add near `whois_freeform_key` in `src/irc/events.rs`:

```rust
/// Every theme key the WHOIS path can emit, plus the three generic error keys
/// a WHOIS can provoke. The shipped themes are tested against this list, so a
/// new key cannot ship with only one theme updated — add new keys here.
pub const WHOIS_EVENT_KEYS: &[&str] = &[
    "whois_header",
    "whois",
    "whois_server",
    "whois_oper",
    "whois_idle",
    "whois_idle_signon",
    "whois_channels",
    "whois_away",
    "whois_account",
    "whois_secure",
    "whois_certfp",
    "whois_keyvalue",
    "whois_special",
    "whois_registered",
    "whois_help",
    "whois_bot",
    "whois_actually",
    "whois_host",
    "whois_modes",
    "end_of_whois",
    "no_such_nick",
    "no_such_server",
    "try_again",
];
```

Then rewrite the WHOIS block in `themes/default.theme`. Add to `[abstracts]`:

```toml
whois = "%Z565f89  $*%N"
whois_value = "%Za9b1d6$*%N"
```

and replace `themes/default.theme:52-71` with:

```toml
whois_header = "%Z7aa2f7───── WHOIS $0 ──────────────────────────%N"
whois = "%Zc0caf5$0%Z565f89 ($1@$2)%N %Za9b1d6$3%N"
whois_server = "{whois server: {whois_value $1}%Z565f89$3}"
whois_oper = "  %Zbb9af7$1%N"
whois_idle = "{whois idle: {whois_value $1}}"
whois_idle_signon = "{whois idle: {whois_value $1}%Z565f89, signon: {whois_value $2}}"
whois_channels = "{whois channels: {whois_value $1}}"
whois_away = "{whois away: %Ze0af68$1%N}"
whois_account = "{whois account: {whois_value $1}}"
whois_secure = "{whois secure: %Z9ece6a$1%N}"
whois_certfp = "{whois certfp: {whois_value $1}}"
whois_keyvalue = "{whois $1: {whois_value $3}}"
whois_special = "{whois {whois_value $1}}"
whois_registered = "{whois {whois_value $1}}"
whois_help = "{whois {whois_value $1}}"
whois_bot = "{whois {whois_value $1}}"
whois_actually = "{whois {whois_value $1}}"
whois_host = "{whois {whois_value $1}}"
whois_modes = "{whois {whois_value $1}}"
end_of_whois = "%Z7aa2f7─────────────────────────────────────────────%N"
no_such_nick = "{error $0} %Z565f89$1%N"
no_such_server = "{error $0} %Z565f89$1%N"
try_again = "{error $0} %Z565f89$1%N"
```

`whois_header`, `whois`, `whois_oper` and `end_of_whois` stay literal: the
first two and the last are the block's frame rather than indented body lines,
and `whois_oper` uses a different indent-plus-emphasis shape. The `error`
abstract already exists in both themes (`themes/default.theme:23`).

Apply the same structure to `themes/spring.theme:149-168`, keeping spring's own
palette — add to its `[abstracts]`:

```toml
whois = "%Z64748b  $*%N"
whois_value = "%Z94a3b8$*%N"
```

and rewrite its `whois_*` entries in the same `{whois …}` / `{whois_value …}`
form, preserving spring's accent colours (`%Z06b6d4` for idle/server values,
`%Z10b981` for account/secure, `%Z3b82f6` for channels, `%Zf59e0b` for away).
Add spring's three error keys using its existing `error` abstract.

- [ ] **Step 4: Run tests to verify they pass**

Run: `make test`
Expected: PASS. Also confirm the pre-existing
`src/ui/message_line.rs` rendering tests still pass — they assert on rendered
output that now flows through the new abstracts.

- [ ] **Step 5: Run clippy**

Run: `make clippy`
Expected: 0 warnings.

- [ ] **Step 6: Commit**

```bash
git add src/irc/events.rs themes/default.theme themes/spring.theme
git commit -m "feat(theme): route the WHOIS block through a whois abstract"
```

---

### Task 4: Nick-parameter invariant test and docs

**Files:**
- Test: `src/irc/events.rs` (`mod tests`)
- Modify: `docs/src/content/theming.md:70-83`

**Interfaces:**
- Consumes: `WHOIS_EVENT_KEYS` from Task 3, all event keys from Tasks 1-2.
- Produces: nothing consumed downstream.

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
    #[test]
    fn every_whois_line_puts_the_nick_first() {
        // $0 is the nick for every whois_* key, so a theme author can write
        // "$0" without checking which numeric produced the line.
        let mut state = make_test_state();
        state.set_active_buffer("test/testserver");

        for (numeric, args) in [
            ("307", vec!["me", "alice", "is a registered nick"]),
            ("310", vec!["me", "alice", "is available for help"]),
            ("320", vec!["me", "alice", "is a Cloaked Connection (Spoof)"]),
            ("326", vec!["me", "alice", "has oper privs: +Aa"]),
            ("327", vec!["me", "alice", "real.host 1.2.3.4"]),
            ("335", vec!["me", "alice", "is a Bot"]),
            ("337", vec!["me", "alice", "webirc gateway"]),
            ("338", vec!["me", "alice", "is actually using host"]),
            ("275", vec!["me", "alice", "is using a secure connection (SSL)"]),
            ("378", vec!["me", "alice", "is connecting from *@h 1.2.3.4"]),
            ("379", vec!["me", "alice", "is using modes +iwx"]),
            ("377", vec!["me", "usermodes", "alice", "+iwx"]),
        ] {
            let msg = make_irc_msg(
                None,
                Command::Raw(
                    numeric.to_string(),
                    args.iter().map(|s| (*s).to_string()).collect(),
                ),
            );
            handle_irc_message(&mut state, "test", &msg);

            let buf = state.buffers.get("test/testserver").unwrap();
            let m = buf.messages.back().unwrap();
            assert!(
                m.event_key.as_deref().is_some_and(|k| k.starts_with("whois")),
                "numeric {numeric} lost its whois key"
            );
            assert_eq!(
                m.event_params.as_ref().and_then(|p| p.first()).map(String::as_str),
                Some("alice"),
                "numeric {numeric} must put the nick in $0"
            );
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `make test 2>&1 | grep -A5 every_whois_line_puts`
Expected: FAIL on 377 before Task 2's shape fix is in place; PASS afterwards.
If it passes immediately, that is fine — it is a regression guard.

- [ ] **Step 3: Update the docs**

Replace the WHOIS paragraph in `docs/src/content/theming.md` (currently lines
70-83) with:

```markdown
WHOIS event keys are `whois_header`, `whois`, `whois_server`, `whois_oper`,
`whois_idle`, `whois_idle_signon`, `whois_channels`, `whois_away`,
`whois_account`, `whois_secure`, `whois_certfp`, `whois_keyvalue`,
`whois_special`, `whois_registered`, `whois_help`, `whois_bot`,
`whois_actually`, `whois_host`, `whois_modes`, and `end_of_whois`.

For every `whois_*` key, `$0` is the nick and `$1` is the line's primary value;
further parameters carry detail. `whois` receives nick, user, host, realname;
`whois_server` receives nick, server, server info, formatted server info;
`whois_idle_signon` receives nick, idle duration, signon time; `whois_secure`
receives nick, display value (`TLS`), and the server's own wording.

Numerics with no dedicated key — including IRCnet's 320, which carries both the
cloak and the TLS line with server-configured text — render through
`whois_special`.

The block's indent and base colour come from the `whois` and `whois_value`
abstracts, so restyling every line at once means editing those two entries
rather than all twenty formats:

```toml
[abstracts]
whois = "%Z565f89  $*%N"
whois_value = "%Za9b1d6$*%N"
```

A WHOIS can also be answered with an error. Those keys are not whois-prefixed,
because the same numerics answer other commands too: `no_such_nick` (401),
`no_such_server` (402), and `try_again` (263, rate limiting). Each receives the
subject in `$0` and the server's reason text in `$1`.
```

- [ ] **Step 4: Run the full suite**

Run: `make test && make clippy`
Expected: PASS, 0 warnings.

- [ ] **Step 5: Commit**

```bash
git add src/irc/events.rs docs/src/content/theming.md
git commit -m "test(irc): pin the nick-first parameter invariant; document WHOIS theming"
```

---

## Self-Review

**Spec coverage:** §1 numeric table → Tasks 1 (errors) and 2 (freeform additions, 377 guard). §2 error routing and naming → Task 1. §3 `whois` abstract → Task 3. §4 parameter convention → Task 4 (test) and Task 3 (`WHOIS_EVENT_KEYS`). Testing items 1-5 → Task 1 step 1 (item 1, errors), Task 1 step 1 second test (item 2), Task 2 step 1 second test (item 3), Task 4 step 1 (item 4), Task 3 step 1 (item 5). Out-of-scope items are not implemented anywhere, as intended.

**Placeholder scan:** no TBD/TODO; every code step carries the actual code. Two conditional instructions remain (the `#chan` fixture in Task 1 and the `PartialEq` fallback for `Response`) — both name the exact fallback to apply rather than deferring the decision.

**Type consistency:** `whois_freeform_key` gains its `args` parameter in Task 2 and is called with it at both call sites in the same task. `WHOIS_EVENT_KEYS` is defined in Task 3 and consumed in Tasks 3 and 4. Event key strings are spelled identically across tasks, the spec, and the theme files.

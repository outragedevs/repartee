# Bouncer presence validation

Status: merged in PR 71 after the second pinned Sol medium review returned no
actionable findings. Application-level real-bouncer integration passes. Remaining
validation limits are recorded below.

## Pinned-source behavior

Use the same Lurker and Soju commits recorded in `BOUNCER_SUPPORT.md`.

- Both implementations accept preregistration AWAY when draft/pre-away is
  negotiated. A bound background connection must start with AWAY * before CAP END.
- Soju tracks away per downstream. With network AutoAway enabled, an upstream
  becomes present when any downstream is present; with AutoAway disabled the
  bouncer leaves upstream away unchanged.
- Lurker AWAY * marks only that downstream absent. A nonempty manual reason sets
  account-wide away, while bare AWAY clears account-wide away, including manual
  away. A generated child resuming automatically must not clear manual intent
  set through another child of the same configured account.
- A control connection is not user presence. Neither a headless daemon nor an
  open browser socket proves that a person is viewing the conversation.

## Required acceptance evidence

1. Registration starts bound bouncer connections absent using negotiated
   draft/pre-away, with no transient present interval. Direct IRC is unchanged.
2. A focused attached terminal or authenticated focused/visible browser reports
   presence. Detach, blur, hidden tabs, disconnect and stale browser reports
   retire that source. Multiple local sources combine without fighting.
3. Explicit /away intent survives reconnect within the appropriate scope and
   takes precedence over automatic focus changes. Independent accounts stay
   isolated; Lurker account-wide behavior differs from Soju network behavior.
4. Transitions and retries are bounded. A failed write is not recorded as sent.
5. Disposable real upstream/server fixtures exercise two downstream clients,
   Lurker manual away and Soju AutoAway disabled. Integration tests must drive
   Repartee itself; raw-protocol probes alone do not prove client behavior.
6. Native/web clippy and tests, WASM build, and a clean pinned gpt-5.6-sol medium
   review are required before merging this stage.

## Existing research evidence

Disposable raw-protocol prototypes already verified two-client aggregate away
transitions against both real pinned bouncers and a local IRC upstream. Lurker
manual away survives another client's AWAY * but is cleared by bare AWAY. The
Soju disabled-AutoAway case also passed. These prototypes are test infrastructure
research; the application-level checks below now exercise the same behavior.


## Initial implementation checks

The native presence controller combines confirmed terminal focus with registered
browser reports, expires presence reports after 45 seconds, sends state
transitions and retains explicit away intent through reconnect. Soju intent is
network-scoped; Lurker intent is shared across generated sibling network scopes.
Provider recognition uses Soju 004 and Lurker's own BOUNCER_NETID 005.

Browser sockets install focus/blur/visibility listeners and a 15-second report
heartbeat, removed when that socket loop exits. The first report follows SyncInit.

Seven controller regressions cover focus transitions, source aggregation/expiry,
manual away scope/reconnect and exclusion of direct/control connections. The TCP
registration fixture verifies pre-away negotiation and BIND/AWAY/CAP END order.
These passed with the full 2458 native and 142 web test suite and clean project
clippy. This is not yet evidence for all acceptance rows above.

## Real-bouncer application checks

`scripts/test_bouncer_presence.py PROVIDER SOURCE` starts the pinned bouncer and
an isolated local IRC upstream, then runs the ignored `pinned_bouncer_presence`
test through `make test`. Both clients are actual App instances using the normal
connection, IRC event, terminal-input and web-command paths.

The Soju run passes twice, with network AutoAway enabled and disabled. The Lurker
run passes with account AutoAway enabled. Recorded upstream AWAY transitions
cover preregistration absence without a transient present interval, two-client
aggregation, terminal blur, a second client's browser presence, explicit manual
away, reconnect and manual clear. The disabled-Soju run verifies that all these
client changes leave upstream away untouched.

The browser source in this fixture is driven through the App web-command handler;
it does not prove browser focus/visibility event delivery. A separate Playwright
WebKit run against the actual WASM bundle confirms the initial Presence report
after SyncInit and the 15-second heartbeat, without JavaScript errors. Switching
pages in that environment did not change document.hasFocus()/document.hidden,
so native focus/visibility event delivery remains unverified. The WASM build passes.

The controller additionally tests a failed send followed by a recovered sender:
no successful state is cached on failure, a retry within five seconds is skipped,
and the next eligible attempt sends exactly one transition.

## Review corrections

The first pinned Sol medium review found that network-scoped Soju intent could
survive deletion while a sibling network remained and leak into a reused network
ID. Intent now records the identified provider. Only Lurker retains intent through
a surviving sibling; Soju requires the exact network scope. A regression deletes
and reuses the original scope for both providers and checks the resulting AWAY.


## Real browser focus and visibility acceptance

`scripts/fixtures/presence-browser.cjs` starts a separate headed Chromium process
with a disposable profile, then attaches Playwright with `noDefaults: true`.
This matters: Playwright's normal Chromium connection enables focus emulation,
which makes background tabs report `document.hasFocus() == true`. Sending a
second CDP session's disable command does not undo the first session's override.
The successful scenario therefore starts without those default overrides.

With `REPARTEE_PRESENCE_BROWSER_SCRIPT` pointing to this script, the existing
presence provider runner serves the compiled WASM frontend through the real App
web server. Two browser tabs open independent WebSocket clients. Actual tab
activation, backgrounding, hiding and closing are checked against both native
document properties and outgoing Presence frames. The local upstream IRC server
then verifies AWAY state through the real bouncer. No focus event or Presence
command is injected, and document properties are not overridden.

This passes for Soju with AutoAway enabled and disabled, and for Lurker with
AutoAway enabled. It closes the earlier browser event-delivery gap. Native
terminal attach/detach acceptance is separate; attempted GUI control of Ghostty
was rejected by the computer-use tool and provides no evidence for that path.

## Native shim acceptance

Set `REPARTEE_DAEMON_NATIVE_ATTACH=1` on the existing presence runner with
`--live-history --daemon-image IMAGE`. The image must include the native detach
key correction. `scripts/bouncer_native_attach.py` runs the actual attach binary
inside each disposable daemon container, through an owned PTY, after the browser
client closes. No desktop terminal application is controlled.

It checks initial absence, keyboard activity, focus-report handling, detach by
both the control character and `/detach`, reattach, upstream AWAY, and continued
daemon operation. The surrounding scenario checks two daemon lifecycles and
actual SQLite/TRACE exclusion. CSI focus reports are deliberately injected;
this proves Repartee's input/transport handling, not an OS terminal application's
emission of focus events.

The scenario reproduced a native bug: legacy terminals encode `Ctrl+\` as
`0x1c`, which crossterm 0.29 decodes as `Ctrl+4`. The shim recognized only
`Ctrl+\` and `Ctrl+Z`, so it forwarded the legacy chord instead of detaching.
It now accepts both representations. The existing ordered-input regression
covers all three decoded keys. The corrected actual-daemon scenarios pass both cycles on the pinned Lurker
and Soju providers, with AutoAway enabled.

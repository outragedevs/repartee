# Browser WebPush validation

## Scope and lifecycle

This increment connects the backend from PR 92 to browser subscriptions and
notifications. Full pinned `gpt-5.6-sol` / `medium` review round 13 found no
actionable defects. The full bouncer acceptance matrix remains a separate
completion gate. The chronological notes below retain earlier failures and fixes.

The settings dialog enables/disables notifications separately for each bound
network. Permission is requested only on an explicit Enable/repair click.
Registration uses a distinct service-worker scope for each opaque account/network
identity and the current VAPID key. The authenticated `/api/webpush` endpoint
reuses the application acknowledgment state machine, validates the session and
intent header before dispatch, bounds request size/concurrency, and returns only
the correlated response with `Cache-Control: no-store`. Stable-scope lookup does
not fall back to another account or transient connection ID.

The worker displays private messages, channel highlights, actions, invitations
and registration notes. It closes notifications on remote MARKREAD and suppresses
older messages delivered afterward. Clicks navigate only to the application
origin; after authentication the current scope resolves the correct account and
conversation. Delayed messages remain routable after a nick change. Status
prefixes are stripped only when followed by a channel prefix.

IndexedDB stores network configuration, read timestamps, renewal status and
retired subscription endpoints awaiting removal. It does not archive message
bodies. Browser subscriptions and Soju own push credentials; the worker does not
cache application pages or history. Pending cleanup does not expose endpoint
values in chat, notifications, HTTP errors or diagnostic output.

Renewal registers the current browser endpoint before retiring the old endpoint.
A failed renewal records repair status. Reconnect reconciles previously enabled
subscriptions; uncertain outcomes require explicit repair rather than a retry
loop. VAPID changes trigger subscription replacement for enabled networks.
Offline disable immediately suppresses notifications and unsubscribes the browser,
then retains an endpoint cleanup request for the next available connection.
Logout suppresses/unsubscribes locally and retains cleanup metadata; a late
subscribe response cannot re-enable notifications after logout. Merely closing
the page retains the user's explicit notification preference.

## Deterministic tests

- `node --test scripts/test_webpush_payload.mjs`: IRC/tag parsing, private/channel
  routing, status/channel prefix ambiguity, formatting/CTCP ACTION, delayed nick
  changes, read timestamps, invitations and same-origin click navigation.
- `scripts/test_webpush_settings.cjs`: real Chromium dialog with mocked
  subscription/API boundaries. Explicit enable/disable, permission refusal,
  unknown outcomes, correct-account navigation, offline disable, reconnect
  cleanup, logout and logout during a pending browser subscribe.
- `scripts/test_webpush_worker.cjs`: actual module worker, IndexedDB and
  Notifications API with CDP-injected push events. Display, read-marker ordering,
  delayed-message suppression, mocked renewal ordering and persisted offline
  endpoint retirement/cleanup. This test does not prove browser push transport.
- Native regressions validate HTTP authorization/intent, correlated responses,
  no-store caching and stable-scope lookup rejecting another account.

Playwright must be resolvable by Node. `PLAYWRIGHT_CHROMIUM_EXECUTABLE` can select
a locally installed full Chromium executable. The installed headless-shell
rejected notification permission; full Chromium 151.0.7922.34 passes. Tests do not
replace `showNotification` with a fake success response.

## Actual provider and browser transport

`scripts/test_bouncer_webpush.py <pinned-soju-source>` uses disposable Docker
Soju/upstream/HTTPS-receiver resources with verified IRC TLS. Two optional modes
extend the independently verified encryption fixture:

- `--browser`: a disposable persistent Chromium profile creates a real browser
  push subscription. Repartee registers it with Soju. REGISTER's NOTE and an
  actual upstream private message travel through the browser push service and
  appear through the production worker. No injected push events are used.
  Evidence: `/tmp/repartee-webpush-real-browser1.log`.
- `--ui`: actual compiled WASM, authenticated Repartee HTTP/WebSocket, pinned Soju,
  actual browser subscription and push-service delivery. Enable receives a real
  notification; an upstream message is delivered despite an immediate nick change;
  reload and Disable succeed; the fixture confirms Soju has no remaining
  subscription. No HTTP, WebSocket or push mocks. Evidence:
  `/tmp/repartee-webpush-real-ui1.log` and `/tmp/repartee-webpush-real-ui2.log`.

The browser fixture uses a trusted loopback HTTP origin and its own profile;
production web serving remains HTTPS. Fixture credentials, browser profiles,
containers and networks are removed after each run. Only disposable synthetic
IRC messages are sent through the browser push service.

Clippy and 2655 native/145 web tests pass in
`/tmp/repartee-webpush-browser-final-{clippy,test}.log`. Later source changes still
need final rebuilding, provider/UI revalidation and full pinned Sol medium review.
The initial dialog screenshot exposed missing modal centering and unstyled
buttons; both have been corrected in the source and require final visual readback.

Protocol sources: pinned Soju `doc/ext/webpush.md`, `upstream.go` push delivery,
and `downstream.go` MARKREAD broadcast. Browser contracts:
https://www.w3.org/TR/push-api/ and https://notifications.spec.whatwg.org/.

## Review round 1

Full Sol medium review reported two actionable defects. Failed enable attempts
could leave the worker active; configuration now enters a pending state and only
confirmed registration enables display. REGISTER's early NOTE is held as a
boolean pending notice, displayed after confirmation, and discarded on failure.
Every failed attempt suppresses/unsubscribes locally and retires its endpoint.
Worker ordering serializes confirmation/disable with push events. A real-worker
regression proves the early NOTE is invisible until confirmation; the settings
regression proves an unknown result leaves no active browser subscription.

Notification lookup now matches only query/channel buffers, excluding a server
buffer whose label happens to equal the sender. The account-navigation test uses
exactly that collision.

The extended real-UI click/read-marker test exposed a test race: an active page
could read and dismiss a notification between observing it and extracting its
route. The fixture now places the application in the background before triggering
the upstream message, captures notification routing there, then focuses the page
and verifies read-marker dismissal. Recent failed attempts are retained in
`/tmp/repartee-webpush-real-ui3.log` through `ui5.log`; final revalidation follows.

## Review round 2

Transient startup failures previously cleared the session hint and were mistaken
for logout, removing browser subscriptions. Cleanup now requires an authenticated
session-status request to return HTTP 401. Network failures, server errors and a
still-valid session preserve subscriptions and preferences. The response is not
cached, and stale responses cannot clean up a subsequently authenticated session.
The browser regression covers valid sessions and HTTP 503 after hint loss, plus
confirmed revocation and logout during a pending subscribe.

Stable `/push/*.js` modules now require cache revalidation so a newly deployed
hashed WASM loader cannot reuse incompatible modules for an hour. Native tests
cover module headers and session validity before and after revocation.

The extended UI fixture also reads notification routing in the same polling
operation that detects delivery, avoiding a second read after MARKREAD dismissal.
UI8 reached actual message delivery but failed at that former second read; the
revised full scenario is still awaiting a passing run.

Current rebuild (`/tmp/repartee-webpush-wasm10.log`) and checks
(`/tmp/repartee-webpush-reviewfix2-final-{clippy,test}.log`) pass: no project
Clippy warnings, 2657 native tests and 145 web tests. Browser settings, worker and
payload regressions pass after the second-round fixes. The Codex in-app browser
also logged into the disposable real Soju fixture and rendered the Notifications
dialog correctly; this visual inspection did not register another subscription.
UI9 received a rejected registration from the provider. A subsequent fixture run
adds only categorical provider error diagnostics, never raw endpoints or keys.
Final extended provider/UI validation and clean review are still required.

## Review round 3

Confirmed logout cleanup now tolerates missing Web Locks and inactive/unresponsive
workers: local unsubscribe, preference removal and notification closure do not
depend on successful worker messaging, and each registration is isolated from
another's failure. The real Chromium settings regression covers two subscriptions
with inactive workers and no Web Locks. Delayed successful cleanup unregisters
disabled receivers under the same scope lock as Enable, avoiding stale offline
settings entries. The regression verifies actual registration removal.

Status-prefix parsing preserves a valid channel prefix for `++local`, `@+local`
and `@++local` when `+` is also a channel prefix. Payload regressions pass.
UI10 reached the overall fixture timeout without provider push errors; subsequent
runs emit only fixed stage names to locate the unfinished browser operation.

## Review round 4

Failed Enable cleanup no longer depends on worker messaging success. The browser
regression makes confirmation time out with an inactive worker and proves that
its subscription and preference are still removed. Push delivery no longer filters
messages against the stored nick: Soju excludes self-authored pushes, while the
stored nick may be stale after an offline rename and reassignment to another user.

Notification navigation distinguishes unknown channels from private targets:
channel invitations use `/join`, while private conversations use `/query`.
Regression coverage includes an unseen invited channel and old-nick reassignment.
Unhashed generated WASM JavaScript snippets also require cache revalidation.

UI11's failure was classified as provider HTTP 410. UI12 successfully registered
and delivered the private message but did not finish notification-route resolution.
Fixed diagnostic strings now distinguish absent buffers, route errors and each
WebPush API action, without printing endpoints, keys or message bodies.

## End-to-end routing diagnosis and passing evidence

UI14/UI15 captured a boolean `false` instead of the object expected from an
asynchronous notification predicate. The fixture now explicitly awaits each
browser evaluation and checks its value before proceeding; no second notification
read is needed. The same change strengthens asynchronous settings checks.
UI16 (`/tmp/repartee-webpush-real-ui16.log`) passes the full real scenario:
registration NOTE, upstream private message while backgrounded, scope/target
validation, actual WASM route resolution, MARKREAD dismissal, reload and Disable.
The provider database is empty afterward. This does not waive final revalidation
after further production changes.

## Review round 5

An inactive worker could lose the remote endpoint during Disable. The page now
persists endpoint retirement independently of the worker before unsubscribing,
then retries acknowledged UNREGISTER after reconnection. Successful cleanup clears
the local record; active newly enabled endpoints are protected by per-scope locks
and are not unregistered. Failed Enable and confirmed logout also retain endpoint
cleanup metadata. Only endpoint credentials are retained, never message history.
The browser regression disables an inactive worker with the provider unavailable,
checks retained cleanup and no local subscription, then verifies removal of the
endpoint record and inactive registration after reconnect.

UI17 (`/tmp/repartee-webpush-real-ui17.log`) passes after the independent endpoint
retirement change. `/tmp/repartee-webpush-reviewfix5-{clippy,test}.log` confirms
zero project warnings and 2657 native/145 web tests. The closed-page transport
variant additionally closes the application page before exposing registration
success to the upstream-message trigger, and requires `pageClosed` evidence.
Its first run received provider HTTP 410 before registration and is not a pass.

## Review round 6

A nonresponsive worker combined with a browser refusing unsubscribe could leave
push display enabled. Disable, confirmed logout and failed Enable now also persist
suppression directly in IndexedDB, independently of worker messaging and browser
unsubscribe. If all three mechanisms fail, worker unregistration is the final
fallback. The browser regression forces an inactive worker and failed unsubscribe,
checks the stored disabled flag, verifies the visible retry status, and then
successfully retries removal. Provider endpoint retirement remains durable.

Latest evidence after suppression changes:

- `/tmp/repartee-webpush-real-ui18.log`: complete real WASM/Soju/browser transport,
  routing, MARKREAD dismissal, reload and Disable, with zero remaining provider
  subscriptions.
- `/tmp/repartee-webpush-real-browser-closed2.log`: actual upstream message delivered
  with the application page already closed; native assertions require this state.
- `/tmp/repartee-webpush-settings-review6.log`: inactive-worker and failed-browser-
  unsubscribe regression, retained endpoint cleanup and successful retry.
- `/tmp/repartee-webpush-worker-review6.log`: a real worker does not display a push
  after page-side IndexedDB suppression, without a worker Disable message; a
  subsequent serialized acknowledgment ensures the tested push has finished.
- `/tmp/repartee-webpush-reviewfix6-{clippy,test}.log`: no project warnings,
  2657 native tests and 145 web tests pass.

The review/fix loop remains open; no merge is claimed yet.

## Review round 7

Final receiver removal after Disable now reacquires the per-scope lock and checks
for a concurrent Enable before unregistering. If another tab enabled notifications,
the current dialog reports that conflict instead of removing the active worker or
claiming success. Refresh cleanup of inactive registrations uses the same lock.
The settings regression simulates a completed concurrent Enable at the cleanup
boundary and verifies the subscription survives and the conflict is reported.

The post-race-fix build (`/tmp/repartee-webpush-wasm16.log`), settings regression
(`/tmp/repartee-webpush-settings-review7.log`) and complete real provider/UI
scenario (`/tmp/repartee-webpush-real-ui19.log`) pass. The eighth full pinned Sol
medium review is in progress. HEAD and fetched origin/main remain
`c40064b0d740aa4fd1ff7e398b5e412df4934e2a`; this increment is still uncommitted.

## Review round 8

Enabled intent is persisted before the worker's confirmation can activate push
display, so terminating the tab between those operations remains recoverable on
next launch. Failed confirmation still clears intent and suppresses the receiver.
A regression checks that the preference exists at the confirmation boundary.
Snapshot updates still refresh notification-navigation data, but schedule network
reconciliation only when authentication or connection metadata changes. Repeated
unread-count changes now produce no additional Get requests; the browser regression
waits beyond the debounce between updates and checks the request counter.

## Review round 9

Serialized worker Disable now closes displayed notifications after prior push
handling finishes, and page-side suppression closes them again after acknowledgment.
The real-worker regression holds an in-flight showNotification, queues Disable,
releases display, and verifies no notification remains afterward.

Settings discovery isolates each worker status failure and retains a Disable row
for an unavailable saved subscription. A broken offline registration no longer
prevents healthy network rows from rendering. Refresh also isolates individual
receiver and connection failures. The browser regression introduces a broken
worker alongside two healthy networks and verifies its Disable action remains
usable. Both new regressions pass.

The post-round-9 settings and worker regressions pass in
`/tmp/repartee-webpush-settings-review9.log` and
`/tmp/repartee-webpush-worker-review9.log`. The rebuilt full UI/provider run
`/tmp/repartee-webpush-real-ui21.log` also passes, including confirmed remote
UNREGISTER and zero remaining subscriptions. The tenth full pinned Sol medium
review is in progress.

## Review round 10

When Web Locks are absent, page and worker operations now share a per-scope
IndexedDB readwrite transaction lock instead of relying on a tab-local busy set.
Requests keep the transaction alive for the operation; closing the owner tab
aborts it and releases waiters without an expiring lease. Different scopes use
separate lock databases. `scripts/test_webpush_locks.cjs` exercises two actual
browser tabs, concurrent independent scopes, owner closure and rejected operations.
Chromium passes; the actual-worker suite also runs renewal and cleanup with Web
Locks disabled. Settings and worker regressions pass after this change.

The same actual-tab fallback lock test also passes under WebKit
(`REPARTEE_TEST_ENGINE=webkit`, `/tmp/repartee-webpush-locks-webkit.log`).

The post-lock-change real provider/UI run
`/tmp/repartee-webpush-real-ui22.log` passes. The generated assets were rebuilt in
`/tmp/repartee-webpush-wasm19.log`. The eleventh full pinned Sol medium review is
running; the browser increment remains unmerged.

## Review round 11

A failed Check / repair preflight now preserves the previous enabled preference
and existing subscription. Cleanup begins only after a receiver has been obtained
for the pending/mutating phase. The settings regression fails the initial Lookup
for an enabled network and verifies its preference and browser subscription remain
intact, with no registration/unregistration mutation.
The regression passes in `/tmp/repartee-webpush-settings-review11.log` and the
assets rebuild passes in `/tmp/repartee-webpush-wasm20.log`.

## Review round 12

The pinned Sol medium review found channel-context WebPush messages routed to
private queries. The notification parser now honors a syntactically valid
`+draft/channel-context` channel target, including configured channel prefixes.
Invalid channel targets fall back to ordinary message routing. Payload tests
cover PRIVMSG, NOTICE, invalid tags and case folding; the Chromium worker test
verifies a channel-context notification is removed by the matching channel
MARKREAD. Both focused suites pass. This receiver has no channel membership
snapshot and therefore validates target syntax rather than claiming to repeat
Soju's upstream membership check.

The repository currently has no other channel-context consumer. Native/live and
history routing require a separate follow-up audit in the remaining full bouncer
support matrix.

After round 12, six payload tests and the Chromium worker/settings suites pass.
The regenerated assets match source (`/tmp/repartee-webpush-wasm21.log`).
The real provider/UI run 24 failed with an upstream push-service HTTP 410 during
initial registration; the client reported failure and unregistered. That run is
not counted as passing. A fresh disposable profile in run 25 passes the complete
registration, delivery, routing, read dismissal, reload and disable sequence
(`/tmp/repartee-webpush-real-ui25.log`). Native source remains covered by the last
Clippy and 2657 native/145 web test run in
`/tmp/repartee-webpush-reviewfix6-{clippy,test}.log`.

## Final review

Full review round 13 passes with no actionable findings, including untracked
source and generated browser assets. Evidence:
`/tmp/repartee-webpush-browser-review13.log`. All three model/reasoning overrides
were explicitly pinned. The final source passed focused browser regressions and
the actual provider/WASM test described above.

# Bouncer FILEHOST implementation audit

## Scope and current status

PR #82 completed confirmed message redaction. The next increment implements
FILEHOST for both pinned providers. Completion requires native and web file
selection, safe credential resolution, originating-conversation result routing,
provider integration tests, full local gates, and clean Sol medium review.
None of those integration gates is claimed complete yet.

## Source evidence

Lurker `server/routes/filehost.ts` accepts unauthenticated OPTIONS and raw-byte
POST with Basic bouncer credentials or OAuth Bearer. OPTIONS advertises
Accept-Post MIME ranges. POST returns 201 plus Location, uses the existing upload
processor and caps, and reports 401/403/413/429 errors. Username selectors are
parsed using bouncer login rules.

Soju `fileupload/fileupload.go` accepts Basic and Bearer, strips network/client
selectors for Basic, validates content metadata, and returns 201 with a possibly
relative Location. Configured browser CORS differs from Lurker. Both UIs should
use daemon-owned transport so browser clients never receive bouncer secrets.

Protocol source: pinned Soju `doc/ext/filehost.md`. It requires refusing plain
HTTP for TLS IRC connections, OPTIONS discovery, raw POST, and 201 plus Location.

## Work in progress

`src/filehost.rs` implements endpoint checks, bounded in-memory input, filename
encoding, MIME discovery, authenticated raw upload, relative Location resolution,
and disabled redirects. Transport errors do not include credential-bearing URLs
or response bodies. The API is not yet wired to App or either UI.

Initial local tests include a real local HTTP server for OPTIONS without auth,
raw upload, redirect rejection, and MIME refusal before authenticated POST.
The initial suite passed 2551 native and 144 web tests, with 11 native fixtures
ignored. Initial Clippy reported expected unused-code warnings until integration,
plus two style warnings now corrected. This is not a clean validation gate.
Logs: `/tmp/repartee-filehost-initial-clippy.log` and
`/tmp/repartee-filehost-initial-test.log`.

## Native integration

Added native `/upload <path> [content-type]`, limited to one upload at a time.
Only native submissions may read daemon paths; web/script invocation is rejected
before file access. The current connected bound bouncer provides FILEHOST and
its original connection configuration provides credentials. Automatic children
inherit that bouncer configuration. Explicit certificate/key authentication does
not fall back to unrelated stored passwords. OAuth Bearer upload selection is
implemented, but end-to-end IRC OAuth support remains a separate acceptance gap.

Regular files are read asynchronously with a 64 MiB + 1 bound, then uploaded.
The result is a local event in the originating conversation and enters only an
empty native composer still on that conversation. It never auto-sends to IRC.
Changed network scopes discard the old result. Native command help was added.

Current checks: 2557 native and 144 web tests passed, 11 fixture tests ignored;
Clippy has no project warnings. Logs:
`/tmp/repartee-filehost-native3-clippy.log` and
`/tmp/repartee-filehost-native3-test.log`. New tests cover credential selection,
remote file-read rejection, conversation changes, and account replacement.
Web file selection, actual provider fixtures, browser verification and full
review remain outstanding. No commit or merge is claimed.

## Web transport integration

Added authenticated POST `/api/upload?buffer_id=...&filename=...` accepting raw
selected bytes. A required X-Upload-Intent header prevents simple cross-origin
form submissions; no upload CORS is enabled. A single process-wide HTTP upload
permit is acquired before reading the bounded body, with a 30-second read timeout.
The internal UploadFile command is skipped by serde and cannot be invoked through
WebSocket JSON. Credentials and endpoints are resolved in App from the connection,
not from browser-supplied values. HTTP responses report the URL or a bounded error;
timeouts explicitly state that the result is unknown and forbid automatic retries.

Native and web uploads share the App concurrency gate. Web completions never
restore the terminal composer; the browser will consume its HTTP response and
apply its own originating-buffer check. Current regression covers authentication,
explicit upload intent, raw-byte handoff, returned URL, and rejection of forged
UploadFile WebSocket commands. 2558 native and 144 web tests pass; Clippy has no
project warnings. Logs: `/tmp/repartee-filehost-http2-clippy.log` and
`/tmp/repartee-filehost-http2-test.log`. Browser picker and actual providers remain
outstanding. No review, commit or merge yet.

## Browser file picker

Added a native browser file chooser alongside the composer. It posts the selected
File as raw bytes with the required upload-intent header; credentials remain in
the daemon. The chooser checks the 64 MiB limit, shows progress and errors,
preserves an existing draft, and inserts the returned URL only if the originating
buffer is still active. It does not automatically send an IRC message.

`make clippy` has no project warnings and `make test` passes 2558 native plus 144
web tests (11 native fixture tests ignored). `make wasm` passed with NO_COLOR=true.
Logs: `/tmp/repartee-filehost-picker-clippy.log`,
`/tmp/repartee-filehost-picker-test.log`, and
`/tmp/repartee-filehost-picker-wasm.log`.

Built WASM was exercised in headless WebKit through a real local HTTP server,
with WebSocket state supplied by the test. The server verified exact selected
bytes, MIME and upload-intent headers, Unicode filename, and originating buffer.
Browser assertions covered successful URL insertion, draft preservation, switching
buffers during upload, server errors and no automatic SendMessage. Initial
Playwright route interception did not expose File request bytes in WebKit, so the
successful test uses actual server-side body reads. Script:
`/tmp/repartee-browser-qa/upload.cjs`; log:
`/tmp/repartee-filehost-picker-browser.log`; screenshot:
`/tmp/repartee-filehost-picker-web.png` (visually inspected). Actual pinned-provider
uploads and full clean review remain outstanding.

## Pinned real-provider validation

The new `--filehost` fixture starts the pinned provider with a disposable HTTPS
upload service and an isolated local file store. Soju uses its real HTTPS
listener and filesystem uploader. Lurker uses its real buildApp, seeded local
uploader and bouncer harness. Both retain the audited Git revisions.
A disposable CA signs a separate server certificate; the test-only client trust
hook adds that CA without disabling hostname or certificate verification.
Production builds do not read the fixture CA environment variable.

The first Soju run exposed an integration bug: FILEHOST was read from the legacy
Connection.isupport map rather than the actively updated isupport_parsed store.
The application and fixture now read the live parser. A fixture certificate
initially used CA and server roles in one certificate and was correctly rejected;
separate CA/server certificates fixed the test setup without weakening TLS.

Both providers passed native `/upload`, App web-submission upload, unauthenticated
GET of each resulting URL with exact text-byte comparison, preservation of native
input after web completion, and rejection of incorrect credentials. These are
real provider tests, separate from the built-browser HTTP test above; a single
browser-to-App-to-provider test has not yet been run.
Logs: `/tmp/repartee-filehost-soju3.log` and
`/tmp/repartee-filehost-lurker.log`. The latest full suite and clippy are in
`/tmp/repartee-filehost-provider-parser-test.log` and
`/tmp/repartee-filehost-provider-parser-clippy.log`: 2558 native, 144 web tests,
12 ignored provider fixtures, no project warnings. Full Sol medium review is
next; no clean-review or merge claim yet.

## First review and quoted-path fix

Full Sol medium review round 1 found that the documented quoted upload path was
split by the generic whitespace parser. `/upload` now uses quote-aware shell-word
tokenization, preserving spaces and literal # characters without executing any
shell expansion. Malformed quoting produces no arguments and therefore the usage
error, rather than starting an unintended upload. Regression tests cover quoted
and escaped spaces, literal shell metacharacters, and unterminated quotes.

After this fix, 2559 native and 144 web tests pass, 12 provider fixtures ignored,
and Clippy has no project warnings. Logs: `/tmp/repartee-filehost-quoted-clippy.log`
and `/tmp/repartee-filehost-quoted-test.log`. Full browser-to-App-to-provider
validation and a further clean review remain outstanding.

## Full browser-to-provider integration

The optional `REPARTEE_FILEHOST_BROWSER_SCRIPT` fixture now runs the built browser
against an actual Repartee router, authenticated disposable session, real
WebSocket, and App event pump. No HTTP or WebSocket responses are substituted.
The checked-in `scripts/fixtures/filehost-browser.cjs` selects a Unicode-named
text file, waits for the actual upload response and composer insertion, then
retrieves the provider URL using the disposable CA and compares exact bytes.

Both pinned providers pass this complete chain, alongside the existing native
and credential-rejection scenarios. Logs:
`/tmp/repartee-filehost-browser-soju.log` and
`/tmp/repartee-filehost-browser-lurker.log`. Browser runtime was supplied via
`REPARTEE_PLAYWRIGHT_MODULE=/tmp/repartee-browser-qa/node_modules/playwright`.

Latest full checks: 2559 native plus 144 web tests passed; 12 provider fixtures
ignored in the normal suite. Clippy has no project warnings. Logs:
`/tmp/repartee-filehost-browser-preflight2-clippy.log` and
`/tmp/repartee-filehost-browser-preflight2-test.log`. The first preflight had one
Duration style warning, corrected before these checks. Full Sol medium review
round 2 follows. Earlier entries describe historical gaps, superseded by this
complete provider/browser result.

## Second review fixes

Round 2 reported percent-encoded URLs being interpreted as theme formatting in
the local event row, and a hardcoded browser fixture storage key. Uploaded URLs
are now escaped only for event formatting; the composer retains the original
URL. The regression asserts literal escaped event content and raw composer text.
The browser fixture receives its storage key from constants::APP_NAME.

2560 native and 144 web tests pass, with 12 fixtures ignored; Clippy has no project
warnings. Logs: `/tmp/repartee-filehost-review2-fixes2-clippy.log` and
`/tmp/repartee-filehost-review2-fixes2-test.log`. The complete browser-to-provider
chain passed again for both providers in
`/tmp/repartee-filehost-browser-soju2.log` and
`/tmp/repartee-filehost-browser-lurker2.log`. Full round 3 follows.

## Clean review gate

Full CLI Sol medium review round 3 completed with no actionable issues in the
entire diff, including new untracked source files. Both model and review_model
were explicitly gpt-5.6-sol with medium reasoning. Log:
`/tmp/repartee-filehost-review3.log`. All findings from rounds 1–2 are resolved.

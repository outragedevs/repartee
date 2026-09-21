# Settings panel delivery

## Scope and delivery

Deliver one complete feature PR on `feat/settings-panel`, covering the shared settings model, native terminal UI, and responsive web UI. Review with explicitly pinned `gpt-5.6-sol` and medium reasoning, fix findings until clean, run required checks, then merge and synchronize main. Git author and committer are `outragedevs <mac@dabrowski.biz>`; GitHub operations must authenticate as `outragedevs`.

All interface text is English. Open Settings through `/wizard` or the top-right gear in both clients. Preserve `/wizard server [id]`. All controls must support mouse input; TUI also supports keyboard input. Web must work at phone widths.

## Shared model

- [x] Extract existing `/set` getters, setters, validation, and setting paths into `src/config/settings.rs`.
- [x] Extract runtime application of settings into a reusable command-layer helper.
- [x] Validate batches on a cloned configuration, rejecting unknown paths and control-character injection without mutating the original.
- [x] Define a shared, searchable catalog with labels, descriptions, types, defaults, and reconnect/restart effects.
- [x] Include settings beyond `/set`: scripts, logging options, E2E, statusbar items/formats, aliases, ignores, translation configuration, and emote sizing.
- [x] Ensure public snapshots contain no secret values. Show configured status and blank replacement inputs; secrets remain in `.env`.
- [x] Save validated drafts before updating runtime state. Surface persistence failures without claiming success.
- [x] Handle concurrent configuration changes without overwriting unrelated edits.

## Sections in both clients

- [x] Networks & Connections: identity, network creation/editing, TLS/SASL/bouncer settings, reconnect requirements.
- [x] Appearance: themes, colors, sidebars, statusbar, nick columns, timestamps; browser overrides remain local.
- [x] Messages: typing, images, translation, URL shortening, spellchecking and applicable display behavior.
- [x] Notifications: existing mentions and browser push controls, with capability/permission feedback.
- [x] History & Privacy: history retention/backlog, logging, ignores and E2E settings.
- [x] Keyboard: existing keyboard behavior/reference and aliases. Verify supported configuration before inventing a new binding system.
- [x] Extensions: scripts and emotes.
- [x] Advanced: web server and remaining existing settings.

## Interaction and acceptance

- [x] Search across labels, descriptions, and setting paths.
- [x] Save, Cancel, and section defaults are mouse-accessible in both clients.
- [x] Cancel also restores any preview, including browser-local appearance changes.
- [x] Keyboard navigation and scrolling reach every TUI control at smaller terminal sizes.
- [x] Web requests return targeted success/errors; no optimistic success before persistence.
- [x] Preserve existing browser-local appearance and related preference storage.
- [x] Native practical check: gear, section/search navigation, editing, validation, defaults, cancel, save and reopen/persistence.
- [x] Browser practical check of the same flows at desktop and phone widths.
- [x] Required `make clippy`, then `make test`; build WASM for web changes.
- [x] Valuable regressions only: failed batch/save, secret redaction, draft cancellation, persistence and client isolation.
- Delivery gate: clean pinned review, PR creation/attachment, merge, main synchronization and clean worktree verification. Review and merge evidence is recorded on PR #130.

## Current evidence

Initial main is `35bb3d509276810012b2709058d8a8f323fd484a` (PR 129). The worktree was clean before this feature. Git author/email and `gh api user` were verified. Fetch uses the explicit `gh auth git-credential` helper rather than the default macOS keychain.

The first extraction passes `make clippy` without project warnings. `make test TEST_ARGS=settings` passes 31 native tests, including failed-batch isolation and preservation of literal text/secrets in a draft. This is foundation verification only, not panel acceptance. Logs: `/tmp/repartee-settings-clippy.log`, `/tmp/repartee-settings-tests.log`.

The repository references Rust/TUI skills that are absent from the advertised local skill locations; MemPalace tools are unavailable. No implementation depends on these tools.

## Implemented verification

Both clients consume `shared/settings.rs` DTOs and `config/settings/catalog.rs` definitions. The catalog covers the existing scalar settings and compound collections. Writes validate against a cloned configuration, compare original values against current state, redact credential snapshots, and restore the previous credential file if configuration persistence fails. Native and web forms share this save path.

Local checks passed: `make clippy` with no project warnings; `make test` with 2,710 native tests and 146 web tests passing (33 existing ignored integration fixtures); `make wasm`; `make docs`; and `make build`. The WASM command requires `env -u NO_COLOR` in this host environment because Trunk rejects `NO_COLOR=1`.

Practical checks used an isolated daemon on HTTPS localhost port 18879 with synthetic credentials and no external IRC connection. Playwright exercised desktop and 390px-wide phone layouts, gear opening, draft cancellation, literal quote/percent/trailing-space persistence, invalid port rejection, preview rollback, browser-local persistence, section-default cancellation, and credential redaction. An attached PTY exercised the gear, `/wizard`, mouse edit/cancel, keyboard save, persisted reopen, and `/wizard server`.

Temporary evidence: `/tmp/repartee-settings-audit/browser.log`, `desktop.png`, `mobile.png`, `tui.log`, and `tui-screen.txt`. A final pinned review and delivery audit remain required.

The first pinned Sol medium review of `a7e5b06` returned no actionable findings. A subsequent UI audit tightened two interaction guards: fields are disabled while a web save is in flight, and section defaults require clearing cross-section search first. These final changes require a fresh review before merge. Restarting the isolated daemon also preserved the tested settings.

The second review identified stale SASL username writes and a wrapped footer control outside very narrow TUI panels. The fixes preserve SASL username conflict checks and calculate footer height from its actual wrapped controls. Dedicated regressions cover both. Runtime-effect labels also distinguish CTCP reconnects, manual image cleanup, and reserved configuration fields.

The third review identified the missing inherited choice for optional auto-reconnect and a browser-only draft bypass of the add-network guard. Both controls now preserve those states. SASL username edits also update an existing legacy `.env` override so restarting cannot restore the old username.

After the inheritance and legacy-credential fixes, the complete suite passes with 2,712 native tests and 146 web tests (33 existing ignored fixtures). Absent `auto_reconnect` values retain `None` through TOML reload; effective reconnect behavior remains enabled through the existing `unwrap_or(true)` runtime policy.

# Dependency update plan

Audit baseline: `18ed769` on 2026-10-15. Plan seven implementation PRs,
ordered below. Version targets are the latest non-yanked stable releases
observed during the audit; recheck them before starting each PR.

The audit covered all registry packages in Cargo.lock, including optional
features and other platforms, plus the documentation's npm dependency.
Of 72 direct Rust dependencies, 27 were current, 23 had updates within their
existing requirements, and 22 required a migration or a pin change.
Resolver experiments succeeded after removing PBKDF2's obsolete `simple`
feature, but upgraded application code has not yet been compiled or tested.

## Delivery rules

- [ ] Create a separate branch and PR for each numbered scope below.
- [ ] Start each PR from main after its prerequisite PRs have merged.
- [ ] Preserve Rust 1.91 as the declared minimum; validate affected targets
      on that compiler as well as the development toolchain. Any required
      increase needs a separate decision and documentation update.
- [ ] Preserve current features, file formats, wire protocols, and saved data.
- [ ] Run `make clippy` before `make test`; both must pass without warnings.
- [ ] Run `make dependency-check` and retain the retired-macro package ban.
- [ ] For frontend changes, run `make wasm` before `make release`, commit
      both generated asset directories, and exercise the affected browser UI.
      If the environment sets `NO_COLOR=1`, use `env NO_COLOR=true make wasm`.
- [ ] For native changes, run `make release`. Use the project's make targets
      for builds and tests, including with alternate compiler environments.
- [ ] Review every PR with the command below, fix actionable findings, and
      repeat until clean. Include validation and remaining limitations in
      the PR description. Leave each PR for the operator to merge.

```sh
codex review --base origin/main -c model=gpt-6.1-sol -c review_model=gpt-6.1-sol -c model_reasoning_effort=medium
```

## PR 1 — Refresh compatible dependencies

Branch: `chore/refresh-compatible-dependencies`

Status: completed on 2026-10-15 — [PR #197](https://github.com/outragedevs/repartee/pull/197).

Prerequisite: none. Keep manifest requirements and the ratatui-core pin.

- [x] Update Cargo.lock within existing requirements, including transitives.
- [x] Update these direct dependencies:

| Package | Baseline | Target |
| --- | --- | --- |
| futures-channel, futures-util | 0.3.32 | 0.3.34 |
| hyper-util | 0.1.20 | 0.1.21 |
| indexmap | 2.14.0 | 2.14.2 |
| js-sys, web-sys | 0.3.103 | 0.3.106 |
| libc | 0.2.186 | 0.2.190 |
| log | 0.4.33 | 0.4.34 |
| rcgen | 0.14.8 | 0.14.10 |
| regex | 1.13.0 | 1.13.1 |
| reqwest | 0.13.4 | 0.13.5 |
| rust-embed | 8.12.0 | 8.13.0 |
| serde | 1.0.228 | 1.0.229 |
| serde_json | 1.0.150 | 1.0.154 |
| thiserror | 2.0.18 | 2.0.21 |
| time | 0.3.53 | 0.3.55 |
| tokio | 1.52.3 | 1.53.3 |
| tokio-rustls | 0.26.4 | 0.26.6 |
| toml | 1.1.2+spec-1.1.0 | 1.1.8+spec-1.1.0 |
| uuid | 1.23.5 | 1.28.0 |
| wasm-bindgen | 0.2.126 | 0.2.129 |
| wasm-bindgen-futures | 0.4.76 | 0.4.79 |
| zeroize | 1.9.0 | 1.9.1 |

- [x] Verify removal of yanked chacha20 0.10.1 and spin 0.9.8; the resolver
      selected 0.10.2 and 0.9.9 respectively during the audit.
- [x] Inspect transitive additions, removals, duplicate versions, and MSRV.
- [x] Build native and WASM; smoke-test browser login, reconnect, buffer
      switching, chat copy, and terminal paste.
- [x] Complete review/fix and open the PR.

Completion evidence: manifests and the ratatui-core 0.1.0 pin are unchanged.
Clippy and all 2,996 application tests pass on Rust 1.97.1 and 1.91.0 with
no warnings (34 existing native tests ignored on each compiler). Native
release and WASM builds pass; WASM was also built on Rust 1.91.0 in an
isolated workspace. Fixture-controlled browser checks cover login, buffer
switching, chat copy, terminal paste, and WebSocket reconnect/resync. The
retired-macro policy passes; all 678 registry entries are non-yanked, and
both generated asset directories and their integrity hashes match. Three
small source adjustments resolve older Clippy lints. Repository-wide
formatting differences also exist on the unchanged main baseline and
remain outside this update. Linux/FreeBSD validation was not performed.

## PR 2 — Update small utilities and browser dependencies

Branch: `chore/update-browser-utilities`

Status: completed on 2026-10-15 — [PR #198](https://github.com/outragedevs/repartee/pull/198).

Prerequisite: PR 1. Keep Leptos at its latest stable 0.8 line; do not adopt
0.9 prereleases. Keep the terminal renderer unchanged in this PR.

- [x] Update base64 0.22.1 → 0.23.1.
- [x] Update gloo-net 0.6.0 → 0.7.0 and gloo-timers 0.3.0 → 0.4.0.
- [x] Update emojis 0.8.2 → 0.9.0.
- [x] Update marked 17.0.4 → 18.1.1 and docs/bun.lock.
- [x] Validate base64 round trips and existing protocol fixtures.
- [x] Exercise HTTP authentication, WebSocket reconnect, timers, emoji
      lookup, and the emoji picker in the compiled browser application.
- [x] Run `make docs`, inspect generated HTML changes, then `make docs-check`;
      check headings, tables, code blocks, links, and blank-line handling.
- [x] Build native and WASM; complete review/fix and open the PR.

Completion evidence: base64 0.23.1, gloo-net 0.7.0, gloo-timers 0.4.0,
emojis 0.9.0, and marked 18.1.1 are in use without application source
changes. Leptos, beamterm, and the ratatui-core pin are unchanged; older
transitive gloo and emoji versions remain where required by their parents.
Clippy and all 2,996 application tests pass without warnings on Rust
1.97.1 and 1.91.0 (34 existing native tests ignored on each compiler).
Native release and both WASM compiler builds pass. Fixture-controlled
browser checks cover rejected/successful HTTP authentication, emoji
search/categories/insertion, heartbeat intervals, asynchronous reconnect,
buffer switching, chat copy, and terminal paste. Existing E2E round trips
and SASL SCRAM RFC vectors pass with the upgraded base64 dependency.
Both documentation make targets pass; all 16 pages preserve semantic
structure, links, text, and code whitespace, render correctly, and have
valid local fragment links. Only block-element whitespace changed on
two pages. The dependency policy passes and all 681 registry entries
are non-yanked. Linux/FreeBSD and live IRC validation were not performed.

## PR 3 — Update SQLite and the Linux allocator

Branch: `chore/update-native-runtime-dependencies`

Status: completed on 2026-10-15 — [PR #199](https://github.com/outragedevs/repartee/pull/199).

Prerequisite: PR 1. Group these native integrations into one bounded PR,
with separate commits and validation for each dependency.

- [x] Update rusqlite 0.39.0 → 0.40.2, preserving `bundled-full`.
- [x] Inspect the bundled SQLite change and relevant release notes.
- [x] Validate opening an existing database copy, migrations, history
      read/write, transactions, search, and restart persistence. Never use
      the live user database as a migration test fixture.
- [x] Update tikv-jemallocator 0.6.1 → 0.7.0, retaining Linux-only selection.
- [x] Validate the allocator on Linux, including native linking, application
      startup, and a bounded allocation workload. macOS tests do not cover it.
- [x] Verify the existing Linux ARM64/AMD64 and FreeBSD release build paths;
      explicitly record any platform validation that cannot be performed.
- [x] Build native; complete review/fix and open the PR.

Completion evidence: rusqlite 0.40.2 keeps bundled-full and upgrades
SQLite 3.51.3 to 3.53.2; tikv-jemallocator 0.7.0 uses jemalloc 5.3.1
and remains Linux-only. The upgrades have separate commits. A fixed
synthetic SQLite 3.51.3 file tests legacy read-only reads, application
migrations, message-ID backfill, FTS, transaction commit/rollback, and
reopen persistence. No live user database was used.

Clippy and 2,997 application tests pass without warnings on macOS with
Rust 1.97.1 and 1.91.0; Linux ARM64 / Rust 1.98.1 passes Clippy and
2,998 tests, including the bounded threaded allocator workload. Each run
ignores the same 34 existing native tests. Native debug/release builds
and startup checks pass on Linux ARM64; the release daemon also starts
offline and migrates the fixture database. The build retains 64 KiB
jemalloc pages and runs on a 4 KiB-page kernel; larger-page kernels were
not tested. Linking to jemalloc was verified. macOS release also passes.

Target resolution and existing release workflow paths were checked for
Linux ARM64/AMD64 and FreeBSD AMD64, with bundled SQLite throughout and
no custom allocator on FreeBSD. AMD64 Linux and FreeBSD binaries were
not built or run here. Two pre-existing Clippy warnings on the newer
Linux compiler were fixed without behavior changes. The dependency
policy passes; all 681 registry entries are non-yanked and none declares
an MSRV above 1.91. Existing frontend assets are unchanged.

## PR 4 — Migrate the Lua integration

Branch: `chore/update-mlua`

Status: completed on 2026-10-16 — [PR #200](https://github.com/outragedevs/repartee/pull/200).

Prerequisite: PR 1. Isolate scripting API changes from the other migrations.

- [x] Update mlua 0.11.6 → 0.12.2.
- [x] Preserve Lua 5.4, vendored builds, and the current threading model;
      do not silently switch to Lua 5.5.
- [x] Review API, callback, ownership, and error propagation changes.
- [x] Validate script loading/reloading, command registration, callbacks,
      timers, error handling, and representative existing scripts.
- [x] Build native; complete review/fix and open the PR.

Completion evidence: mlua 0.12.2 and mlua-sys 0.13.0 keep the `lua54`,
`vendored`, and `send` features; the vendored interpreter moves from Lua
5.4.8 to 5.4.9, and a new test pins `_VERSION` to Lua 5.4. The code
compiles without API changes. mlua 0.12.2 rejects fractional numbers for
Rust integer arguments where 0.11 truncated them; a `TruncatedInt`
argument type restores truncation for the five integer parameters of the
script API, while NaN, infinities, out-of-range values, and non-numbers
are still rejected. The failing test for this was written before the fix.

New tests cover timers and their cleanup on unload, fractional and string
arguments, rejected arguments, failing timer callbacks, and reload through
`ScriptManager`; the bundled keepnick integration test passes. Clippy and
all application tests (158 web + 2,845 native, 34 existing native tests
ignored) pass without warnings on Rust 1.97.1 and 1.91.0. Native release
passes. A one-off A/B run of the seven complete documentation scripts on
mlua 0.11.6 and 0.12.2 produced identical API call traces, apart from one
example's `math.random` choice, which also varies between runs on main.
The dependency policy passes; the changed packages are not yanked and
declare MSRV 1.88 at most. Linux/FreeBSD builds and live IRC sessions with
user scripts were not exercised.

## PR 5 — Migrate the cryptographic dependency family

Branch: `chore/update-crypto-dependencies`

Status: completed on 2026-10-16 — [PR #201](https://github.com/outragedevs/repartee/pull/201).

Prerequisites: PRs 1 and 2. Keep mutually dependent digest, HMAC, and KDF
trait migrations together. Preserve the existing algorithms and parameters.

| Package | Baseline | Target |
| --- | --- | --- |
| aes-gcm | 0.10.3 | 0.11.1 |
| chacha20poly1305 | 0.10.1 | 0.11.0 |
| ed25519-dalek | 2.2.0 | 3.0.0 |
| x25519-dalek | 2.0.1 | 3.0.0 |
| p256 | 0.13.2 | 0.14.0 |
| sha1 | 0.10.7 | 0.11.0 |
| sha2 | 0.10.9 | 0.11.1 |
| hmac | 0.12.1 | 0.13.0 |
| hkdf | 0.12.4 | 0.13.0 |
| pbkdf2 | 0.12.2 | 0.13.0 |
| bip39 | 2.2.2 | 3.0.0 |

- [x] Remove PBKDF2's obsolete `simple` feature; select features from actual
      usage rather than enabling the replacement high-level API by default.
- [x] Adapt digest and random-source APIs without weakening randomness,
      zeroization, authentication checks, or error handling.
- [x] Use fixed pre-upgrade fixtures for stored keys, encrypted data,
      recovery phrases, and protocol messages; a new-version self-round-trip
      alone does not prove backward compatibility.
- [x] Test E2E encryption/decryption, signatures, tamper rejection, recovery,
      SASL SCRAM, and ECDSA challenge responses using known vectors.
- [x] Verify old/new client interoperability where the protocol permits it.
- [x] Inspect remaining old transitive crypto versions; do not force
      incompatible replacements into upstream libraries.
- [x] Build native; complete review/fix and open the PR.

Completion evidence: all eleven packages reached their targets with
unchanged algorithms, parameters, and formats. Fixtures were recorded with
the old crates and committed before the upgrade. They cover an encrypted
log line, a keyring secret blob, identity derivation with the six SAS
words, a KEYRSP wrap and signature, a REKEY plus a channel message from an
old peer (accepted through `handle_rekey` and `decrypt_incoming`), a
SCRAM-SHA-512 exchange, the deterministic ECDSA challenge signature (also
verified with OpenSSL), and the web session token hash. Published RFC 8032,
XChaCha20-Poly1305, RFC 5869, and RFC 4231 vectors are pinned alongside.

PBKDF2 now uses `default-features = false`, and ed25519-dalek drops
`rand_core`. Nonces still come from the OS CSPRNG. A malformed stored IV is
now an error instead of a panic. chacha20poly1305 0.11 no longer wipes its
key and keystream buffer by default, so `zeroize` and `cipher/zeroize` are
declared explicitly. The enabled zeroize features are a superset of the
previous set.

A one-off cross-version run between a main build and this branch passed:
handshakes, messages, and REKEY in both directions with both role
assignments. sha1/sha2/hmac/digest 0.10-era copies remain only through
cookie, axum, and tungstenite, whose latest compatible releases still use
them. Clippy and all application tests (158 web and 2,858 native, 34
existing native tests ignored) pass without warnings on Rust 1.97.1 and
1.91.0. Native release and the dependency policy pass. No new package is
yanked, and the highest declared MSRV is 1.89. Linux/FreeBSD builds and
live IRC, SASL, E2E, and web sessions were not exercised.

## PR 6 — Migrate the browser terminal renderer

Branch: `chore/update-beamterm`

Status: completed on 2026-10-16 — [PR #202](https://github.com/outragedevs/repartee/pull/202).

Prerequisites: PRs 1 and 2. Keep this separate from native TUI rendering.

- [x] Update beamterm-renderer 0.17.0 → 1.0.0 and its dependent packages.
- [x] Adapt renderer initialization, cells, font atlas, and resize APIs.
- [x] Validate WebGL initialization, font loading, resize, cursor rendering,
      colors, Unicode/wide glyphs, selection/copy, bracketed paste, and keys.
- [x] Exercise the shell with a real PTY/backend as well as deterministic
      browser fixtures; synthetic transport alone is insufficient here.
- [x] Rebuild WASM and embedded assets, then the native release.
- [x] Complete review/fix and open the PR.

Completion evidence: beamterm-renderer, beamterm-core, and beamterm-data
moved to 1.0.0, and beamterm-unicode 1.0.0 was added. The only API change
in use is `terminal_size()`, which now returns a struct. `is_double_width`
now relies on `unicode-width` alone. Comparing old and new over every
Unicode scalar value found a single difference (U+17D8, width 3). The old
`emojis` 0.8 copy left the tree, so Cargo.lock stays at 687 packages.

A one-off side-by-side run compared the main build and this branch. Each
used a real zsh PTY behind the web UI, and canvases were read back and
hashed. Both builds produced pixel-identical output for a rendering
fixture covering colors, styles, wide and emoji glyphs, Nerd Font glyphs,
box drawing, and the cursor. They also matched on an interaction scenario
covering bracketed paste, special keys, Alt, Ctrl+C, and font-zoom atlas
replacement, on mouse selection with copy, and on PTY sizes after window
resize and zoom.

The run found two issues that predate this change and also occur on main.
First, `/shell` typed in the browser does not start the web PTY until the
user switches buffers, because the first `ShellResize` is ignored. Second,
wide glyphs occupy three cells. Both are fixed separately in
[PR #203](https://github.com/outragedevs/repartee/pull/203). Clippy and all application tests pass on
Rust 1.97.1 and 1.91.0, and the WASM crate builds on 1.91.0. `make wasm`,
`make release`, and the dependency policy pass. The beamterm packages are
not yanked and declare MSRV 1.90.

## PR 7 — Migrate Ratatui and retire the rendering pin

Branch: `chore/update-ratatui-rendering`

Status: completed on 2026-10-16 — [PR #204](https://github.com/outragedevs/repartee/pull/204).

Prerequisite: PR 1. Highest regression risk; keep this independent and last.

- [x] Update ratatui 0.30.0 → 0.30.2 and ratatui-image 10.0.8 → 11.1.0.
- [x] Trial ratatui-core 0.1.0 → 0.1.2 together with those updates.
      ratatui 0.30.2 requires core ^0.1.2, so updating only the umbrella
      package while retaining =0.1.0 is not possible.
- [x] Inspect the upstream skip-cell / forced-width changes and adapt the
      image integration where necessary.
- [x] Reproduce the original blank-inline-image regression with a targeted
      test before removing or relaxing the protective pin.
- [x] Validate inline emotes, the emote picker, image previews, scrolling,
      resize, and redraw over wide glyphs in actual supported terminals.
- [x] Cover Kitty, iTerm2, Sixel, and fallback paths where available; clearly
      distinguish buffer/PTY simulations from real terminal validation.
- [x] Preserve the selected image decoders and the previous dependency
      reduction; inspect feature changes for reintroduced encoders.
- [x] Verify both locked source builds and dependency resolution without
      the repository lockfile, since cargo-install previously exposed this
      regression. Use `make package` for the packaging check.
- [x] Remove or relax the explicit core pin only after the regression is
      demonstrated fixed. If it remains broken, leave this PR unmerged and
      record the reproducer and upstream blocker.
- [x] Build native; complete review/fix and open the PR.

Progress: ratatui 0.30.2, ratatui-core 0.1.2, and ratatui-image 11.1.0 are
in place, and the explicit core pin is removed. A new test drives Kitty,
iTerm2, and Sixel images through the real output backend and checks the
`vt100` screen. It passes on main and fails with core 0.1.2 plus
ratatui-image 10, because text beside the image is never written. It passes
again with ratatui-image 11, which forces the escape cell to width 1.

ratatui-core 0.1.2 also queries cursor position and screen size during
`clear` and `resize` on fixed viewports, which failed in the tty-less
daemon and would have detached the session on every full redraw.
`GlyphBackend` now answers those queries for socket terminals.

Image codec features are unchanged. `make package` passes, and the packaged
crate resolved without a lockfile picks the fixed versions. Clippy and all
application tests pass on Rust 1.97.1 and 1.91.0. Everything so far is a
buffer, `vt100`, or PTY simulation. Inline emotes, the emote picker, image
previews, scrolling, resize, and reattach still needed a check in real
terminals. The operator ran the release build in Ghostty (Kitty graphics)
on 2026-10-16 and confirmed it works. iTerm2 and Sixel were covered by the
simulated backend test only, and Linux/FreeBSD builds were not exercised.

## Final reconciliation

- [ ] Re-audit direct and transitive versions after all approved PRs merge.
- [ ] Record remaining older versions with their parent dependency, MSRV,
      compatibility constraint, or rendering blocker.
- [ ] Keep prereleases and Rust-1.95-only transitives out of the Rust-1.91
      baseline unless a separate migration is approved.
- [ ] Confirm no retired macro packages or yanked versions were reintroduced.
- [ ] Confirm the final release contains the rebuilt frontend and that all
      required platform validations and review rounds are documented.

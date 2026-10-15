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

Prerequisite: none. Keep manifest requirements and the ratatui-core pin.

- [ ] Update Cargo.lock within existing requirements, including transitives.
- [ ] Update these direct dependencies:

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

- [ ] Verify removal of yanked chacha20 0.10.1 and spin 0.9.8; the resolver
      selected 0.10.2 and 0.9.9 respectively during the audit.
- [ ] Inspect transitive additions, removals, duplicate versions, and MSRV.
- [ ] Build native and WASM; smoke-test browser login, reconnect, buffer
      switching, chat copy, and terminal paste.
- [ ] Complete review/fix and open the PR.

## PR 2 — Update small utilities and browser dependencies

Branch: `chore/update-browser-utilities`

Prerequisite: PR 1. Keep Leptos at its latest stable 0.8 line; do not adopt
0.9 prereleases. Keep the terminal renderer unchanged in this PR.

- [ ] Update base64 0.22.1 → 0.23.1.
- [ ] Update gloo-net 0.6.0 → 0.7.0 and gloo-timers 0.3.0 → 0.4.0.
- [ ] Update emojis 0.8.2 → 0.9.0.
- [ ] Update marked 17.0.4 → 18.1.1 and docs/bun.lock.
- [ ] Validate base64 round trips and existing protocol fixtures.
- [ ] Exercise HTTP authentication, WebSocket reconnect, timers, emoji
      lookup, and the emoji picker in the compiled browser application.
- [ ] Run `make docs`, inspect generated HTML changes, then `make docs-check`;
      check headings, tables, code blocks, links, and blank-line handling.
- [ ] Build native and WASM; complete review/fix and open the PR.

## PR 3 — Update SQLite and the Linux allocator

Branch: `chore/update-native-runtime-dependencies`

Prerequisite: PR 1. Group these native integrations into one bounded PR,
with separate commits and validation for each dependency.

- [ ] Update rusqlite 0.39.0 → 0.40.2, preserving `bundled-full`.
- [ ] Inspect the bundled SQLite change and relevant release notes.
- [ ] Validate opening an existing database copy, migrations, history
      read/write, transactions, search, and restart persistence. Never use
      the live user database as a migration test fixture.
- [ ] Update tikv-jemallocator 0.6.1 → 0.7.0, retaining Linux-only selection.
- [ ] Validate the allocator on Linux, including native linking, application
      startup, and a bounded allocation workload. macOS tests do not cover it.
- [ ] Verify the existing Linux ARM64/AMD64 and FreeBSD release build paths;
      explicitly record any platform validation that cannot be performed.
- [ ] Build native; complete review/fix and open the PR.

## PR 4 — Migrate the Lua integration

Branch: `chore/update-mlua`

Prerequisite: PR 1. Isolate scripting API changes from the other migrations.

- [ ] Update mlua 0.11.6 → 0.12.2.
- [ ] Preserve Lua 5.4, vendored builds, and the current threading model;
      do not silently switch to Lua 5.5.
- [ ] Review API, callback, ownership, and error propagation changes.
- [ ] Validate script loading/reloading, command registration, callbacks,
      timers, error handling, and representative existing scripts.
- [ ] Build native; complete review/fix and open the PR.

## PR 5 — Migrate the cryptographic dependency family

Branch: `chore/update-crypto-dependencies`

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

- [ ] Remove PBKDF2's obsolete `simple` feature; select features from actual
      usage rather than enabling the replacement high-level API by default.
- [ ] Adapt digest and random-source APIs without weakening randomness,
      zeroization, authentication checks, or error handling.
- [ ] Use fixed pre-upgrade fixtures for stored keys, encrypted data,
      recovery phrases, and protocol messages; a new-version self-round-trip
      alone does not prove backward compatibility.
- [ ] Test E2E encryption/decryption, signatures, tamper rejection, recovery,
      SASL SCRAM, and ECDSA challenge responses using known vectors.
- [ ] Verify old/new client interoperability where the protocol permits it.
- [ ] Inspect remaining old transitive crypto versions; do not force
      incompatible replacements into upstream libraries.
- [ ] Build native; complete review/fix and open the PR.

## PR 6 — Migrate the browser terminal renderer

Branch: `chore/update-beamterm`

Prerequisites: PRs 1 and 2. Keep this separate from native TUI rendering.

- [ ] Update beamterm-renderer 0.17.0 → 1.0.0 and its dependent packages.
- [ ] Adapt renderer initialization, cells, font atlas, and resize APIs.
- [ ] Validate WebGL initialization, font loading, resize, cursor rendering,
      colors, Unicode/wide glyphs, selection/copy, bracketed paste, and keys.
- [ ] Exercise the shell with a real PTY/backend as well as deterministic
      browser fixtures; synthetic transport alone is insufficient here.
- [ ] Rebuild WASM and embedded assets, then the native release.
- [ ] Complete review/fix and open the PR.

## PR 7 — Migrate Ratatui and retire the rendering pin

Branch: `chore/update-ratatui-rendering`

Prerequisite: PR 1. Highest regression risk; keep this independent and last.

- [ ] Update ratatui 0.30.0 → 0.30.2 and ratatui-image 10.0.8 → 11.1.0.
- [ ] Trial ratatui-core 0.1.0 → 0.1.2 together with those updates.
      ratatui 0.30.2 requires core ^0.1.2, so updating only the umbrella
      package while retaining =0.1.0 is not possible.
- [ ] Inspect the upstream skip-cell / forced-width changes and adapt the
      image integration where necessary.
- [ ] Reproduce the original blank-inline-image regression with a targeted
      test before removing or relaxing the protective pin.
- [ ] Validate inline emotes, the emote picker, image previews, scrolling,
      resize, and redraw over wide glyphs in actual supported terminals.
- [ ] Cover Kitty, iTerm2, Sixel, and fallback paths where available; clearly
      distinguish buffer/PTY simulations from real terminal validation.
- [ ] Preserve the selected image decoders and the previous dependency
      reduction; inspect feature changes for reintroduced encoders.
- [ ] Verify both locked source builds and dependency resolution without
      the repository lockfile, since cargo-install previously exposed this
      regression. Use `make package` for the packaging check.
- [ ] Remove or relax the explicit core pin only after the regression is
      demonstrated fixed. If it remains broken, leave this PR unmerged and
      record the reproducer and upstream blocker.
- [ ] Build native; complete review/fix and open the PR.

## Final reconciliation

- [ ] Re-audit direct and transitive versions after all approved PRs merge.
- [ ] Record remaining older versions with their parent dependency, MSRV,
      compatibility constraint, or rendering blocker.
- [ ] Keep prereleases and Rust-1.95-only transitives out of the Rust-1.91
      baseline unless a separate migration is approved.
- [ ] Confirm no retired macro packages or yanked versions were reintroduced.
- [ ] Confirm the final release contains the rebuilt frontend and that all
      required platform validations and review rounds are documented.

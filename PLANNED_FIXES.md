# Planned Fixes

This document tracks confirmed bugs and deferred hardening work that still needs to be addressed.

## Priority 1

### Fix SASL EXTERNAL client certificate authentication

Status: completed on `fix/sasl-external-certificate`; Clippy, 2258 native tests, 127 web tests, and Sol medium review passed.

GitHub issue: [#43](https://github.com/outragedevs/repartee/issues/43)

The current implementation does not provide the TLS layer with all the client certificate material it requires. It also passes `client_cert_path` through without resolving relative paths against the certificates directory, despite the documented behavior.

The fix should:

- define and document the supported certificate and private-key format;
- resolve relative paths against the application certificates directory;
- pass both the certificate chain and private key to the TLS implementation;
- report actionable errors for missing or invalid certificate material;
- add tests covering absolute paths, relative paths, invalid files, and successful configuration.

## Priority 2

### Make `/ignore` changes effective immediately

Status: completed on `fix/ignore-runtime-sync`; Clippy, 2265 native tests, 127 web tests, and Sol medium review passed.

The `/ignore` and `/unignore` commands currently update the persisted configuration but do not synchronize the runtime ignore list. Rules therefore do not take effect until `/reload` or restart.

The fix should:

- synchronize runtime state after adding or removing a rule;
- treat an omitted level list as `ALL`, consistently with the command output;
- add regression tests for adding and removing rules without reloading;
- verify that CTCP messages carried by `NOTICE` are classified under the intended ignore level.

### Make scripted buffer switching observable immediately

GitHub issue: [#41](https://github.com/outragedevs/repartee/issues/41)

`api.ui.switch_buffer()` queues a state change, while `api.store.active_buffer()` reads a snapshot that is refreshed later. A script that switches buffers and immediately reads the active buffer therefore receives stale data.

The fix should define consistent read-after-write semantics for scripting actions and add a regression test that performs multiple switches and reads within one Lua callback.

### Merge and revalidate the mobile preview reflow fix

Pull request: [#39](https://github.com/outragedevs/repartee/pull/39)

The branch contains fixes for mobile image-preview layout shifts, tail-following behavior, scroll intent detection, and iOS text inflation, but it has not been merged into `main`.

Before merging:

- rebase or merge the current `main` branch;
- rerun the web build and project checks;
- manually verify buffer switching and delayed image loading on a mobile Safari viewport.

## Deferred Hardening

### Parse quoted shell arguments correctly

The embedded shell currently uses whitespace splitting. Commands containing quoted paths, escaped spaces, or other shell-style arguments are parsed incorrectly. Replace the simple split with a tested argument parser while preserving the current direct process execution model.

### Reduce the lifetime of E2E secrets in memory

Secret zeroization is only partial. Review private keys and exported secret material stored in ordinary arrays, strings, and vectors, and use zeroizing containers wherever ownership and external APIs permit it.

This is security hardening rather than a known functional failure.

## Feature Requests

These open requests are not bugs, but remain candidates for future work:

- [#40: Display images inline with chat](https://github.com/outragedevs/repartee/issues/40)
- [#26: Add an Alt+A binding for buffers with new activity](https://github.com/outragedevs/repartee/issues/26)


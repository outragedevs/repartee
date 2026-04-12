# Repartee

Rust IRC client — a port of kokoirc (~/dev/kokoirc) from TypeScript/OpenTUI/Bun to Rust/ratatui/tokio.

- **Website**: https://repart.ee
- **Repo**: https://github.com/outragedevs/repartee

## Naming

The app name is **Repartee** (binary: `repartee`, alias: `reptee`).

```rust
pub const APP_NAME: &str = "repartee";
```

- Config/data directory: `~/.repartee/`
- Binary installed at: `/usr/local/bin/repartee` (symlink to `target/release/repartee`)
- Alias: `/usr/local/bin/reptee` (symlink to same binary)
- All paths, config dirs, CTCP version strings, etc. must reference the `APP_NAME` constant — do NOT hardcode the name in strings.

## Build

- **Workspace**: `Cargo.toml` has two members: `.` (main binary) and `web-ui/` (Leptos WASM frontend)
- **Makefile**: All builds go through `make` targets — never use raw cargo/trunk commands
  - `make all` — clean + WASM + release
  - `make release` — native release binary
  - `make wasm` / `make web` — Leptos WASM frontend
  - `make test` / `make clippy` — testing and linting
- **CI**: GitHub Actions release workflow on tag push (`v*`) — macOS ARM64, Linux AMD64/ARM64, FreeBSD AMD64

## Release

Manual release — push the version-bump commit to `outrage/main`, then push a `vX.Y.Z` tag to trigger the GitHub release workflow, then publish the crate to crates.io as a separate manual step.

### Pre-flight (local only)

1. Bump `version` in `Cargo.toml` (`[package]` section — the only place that needs changing)
2. Add a `### vX.Y.Z` entry to the changelog in `README.md` (match the style of existing entries — user-facing bullet list)
3. `make build` — refreshes `Cargo.lock` and validates the crate still builds locally after the version bump
4. `make clippy`
5. `make test` — steps 4 and 5 must pass with 0 warnings (pedantic + nursery + perf=deny + redundant_clone=deny)
6. `make release` — verifies the LTO/codegen-units=1/strip release profile builds locally
7. `git add Cargo.toml Cargo.lock README.md && git commit -m "chore: bump version to X.Y.Z"`

Optional: if `web-ui/` changed, run `make wasm` before step 6 and include `static/web/` in the commit. CI also runs `build-wasm` as a dependency of all native build jobs, so CI-built releases always embed the latest web UI regardless.

### Publishing (destructive — requires explicit confirmation in agent sessions)

8. `cargo publish --dry-run -p repartee` — packaging sanity check, no network upload
9. `git push outrage main` — push pending commits (remote is `outrage`, not `origin` — verify with `git remote -v`)
10. `git tag vX.Y.Z && git push outrage vX.Y.Z` — lightweight tag, triggers `.github/workflows/release.yml` which builds macOS ARM + Linux AMD64/ARM64 + FreeBSD AMD64 tarballs and creates a GitHub Release with auto-generated notes
11. Wait for the GitHub Actions release workflow for tag `vX.Y.Z` to go green and verify the GitHub Release was created with artifacts attached
12. `cargo publish -p repartee` — publishes to crates.io. **Irreversible**: `cargo yank --version X.Y.Z` can mark a version unfetchable for new consumers but cannot delete it. Use `--allow-dirty` only if unavoidable generated artifacts require it.

### Notes

- CI does not run `cargo publish` — it is strictly manual.
- The `irc-repartee` and `irc-proto-repartee` crates are published separately from `~/dev/irc/` (a fork of the `irc` crate family), not from this workspace. Only publish those when the fork itself needs bumping.
- The release workflow runs `cargo build --release --locked`, so the version-bump commit must land on `outrage/main` **before** the tag is pushed — otherwise CI fails with a `Cargo.lock` mismatch.
- `~/.cargo/credentials.toml` must contain a valid crates.io token (set via `cargo login <token>` once per machine). Never commit or print this file.

## Architecture

- **Pattern**: TEA (Model → Message → Update → View)
- **TUI**: ratatui 0.30+ with crossterm backend
- **Async**: tokio with crossterm event-stream
- **IRC**: `irc-repartee` v1.5.0 on crates.io (published fork of `irc` crate with bind_address, rustls fix, immediate flush)
  - **Bind address**: `Config::bind_address` — bind to specific local IP (our config field: `bind_ip`)
  - **Immediate send flush**: outgoing messages flush immediately via spawned tokio task (not buffered until next poll)
- **Config**: TOML (`config.toml`), same format as kokoirc
- **Credentials**: `.env` file (never written to config.toml)
- **Theming**: TOML `.theme` files with irssi-compatible format strings
  - `%Z` RRGGBB = 24-bit foreground color
  - `%z` RRGGBB = 24-bit background color
  - `%X` single-letter irssi color codes
  - `{abstract args}` template expansion
  - `$0-$9`, `$*`, `$[N]0` variable substitution
  - mIRC control characters (\x02, \x03, \x04, \x0F, \x16, \x1D, \x1E, \x1F)

### Module Layout

```
src/
├── app/           # TEA controller — 13 domain submodules (backlog, dcc, image, input, irc, maintenance, mentions, scripting, session, shell, web, who, mod)
├── commands/      # Command parser + handler groups (IRC, UI, DCC, admin) + settings + registry
├── config/        # TOML config + .env credentials
├── dcc/           # DCC CHAT (active + passive/reverse)
├── image_preview/ # Kitty/iTerm2/Sixel image preview with async fetch + cache
├── irc/           # IRC protocol (IRCv3 caps, SASL, ISUPPORT, batch, extban, flood, netsplit, ignore, formatting)
├── scripting/     # Lua 5.4 engine (mlua), EventBus, ScriptAPI
├── session/       # Detach/reattach session persistence (postcard protocol)
├── shell/         # Embedded PTY terminal (/shell command)
├── spellcheck/    # Hunspell spell checking (spellbook crate)
├── state/         # UI-agnostic state (buffers, connections, events, sorting)
├── storage/       # SQLite + WAL + FTS5, optional AES-256-GCM, async batched writer
├── theme/         # irssi-compatible theme engine (loader, parser)
├── ui/            # ratatui rendering — 13 view components
├── web/           # axum HTTPS + WSS server, auth, broadcasting, snapshots
├── nick_color.rs  # Deterministic per-nick coloring (djb2 hash, HSL palettes)
└── main.rs        # tokio event loop + select! arms
web-ui/            # Leptos WASM frontend (separate workspace crate)
```

## Reference Projects

- **kokoirc** (`~/dev/kokoirc`): Primary reference for features, UI, theming, config format
- **erssi** (`~/dev/erssi`): Reference for irssi theme format and sidepanel rendering

## Conventions

- Use `color-eyre` for error handling
- Use `tracing` for logging (not `log` or `println!`)
- Follow Rust 2024 edition idioms
- Prefer `thiserror` for library error types
- Clippy: pedantic=warn, nursery=warn, perf=deny, redundant_clone=deny (0 warnings policy)
- Commands use function pointer handlers: `fn(&mut App, &[String])`
- State is UI-agnostic — no ratatui imports in `state/`

## MemPalace — long-term memory for this project

This project has a dedicated wing in MemPalace (palace at `~/.mempalace/palace`, shared between Claude Code and opencode). As of 2026-04-10 `wing=repartee` holds ~3577 drawers — past decisions, debugging sessions, design rationale, cross-references to `~/dev/kokoirc` (the reference project) and `~/dev/erssi` (theme format reference).

**How to access:**
- In Claude Code: 19 MCP tools under the `mempalace` plugin (`plugin:mempalace:mempalace`). Key ones: `mempalace_search`, `mempalace_kg_query`, `mempalace_list_rooms`, `mempalace_add_drawer`.
- In opencode: same tools via the MCP server at `python3 -m mempalace.mcp_server` (config in `~/.config/opencode/opencode.json`).
- Slash commands in Claude Code: `/mempalace:search`, `/mempalace:status`, `/mempalace:mine`.

**When to query the palace in this repo:**
1. **Before answering questions about past design decisions** — "why do we use ratatui over cursive", "what was the reason for the irc-repartee fork", "how did we decide on the theme format" — call `mempalace_search "<topic>" --wing repartee` first. Don't guess from memory.
2. **When looking for prior bug investigations** — if you're about to debug something that feels familiar, search `wing=repartee` before reading code. A previous session may have already traced the same issue.
3. **For entity/person facts** (Rust crate authors, collaborators, maintainers of `irc-repartee`) — use `mempalace_kg_query` which returns temporal entity relationships.
4. **Cross-reference to kokoirc or erssi** — if you need to check how the TypeScript/OpenTUI reference (kokoirc) or the irssi fork (erssi) implements something, search without a wing filter or use the specific wing (`wing=erssi` has ~6423 drawers including theme format details).

**When filing NEW memories:**
- Design decisions → `wing=repartee room=general` with verbatim user quotes where possible
- Code-level discoveries / bug fixes → `wing=repartee room=src`
- Documentation notes → `wing=repartee room=documentation`
- Use `mempalace_check_duplicate` (threshold 0.85) before `mempalace_add_drawer` to avoid bloat

**Auto-save hooks**: Stop (every 15 messages) and PreCompact hooks are provided by the mempalace plugin globally — when blocked, file drawers as classified above, don't skip.

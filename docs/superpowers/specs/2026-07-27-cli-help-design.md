# `repartee --help`

**Date:** 2026-07-27
**Branch:** `feat/cli-help`
**Scope:** `src/main.rs`, `docs/src/content/installation.md`

## Problem

repartee has six launch parameters and two subcommands, and no way to discover
them from the binary. `repartee --help` today is silently ignored: argv is
hand-scanned in `main()` for the flags it knows, anything unrecognised falls
through, and the process starts a normal foreground session. A user asking for
help gets a running IRC client instead of an answer.

The full surface, read off `src/main.rs`:

| Form | Effect |
|---|---|
| *(no arguments)* | fork; child is the headless backend, parent is the terminal shim |
| `-d`, `--detach` | start headless, no fork, no terminal |
| `a [PID]`, `attach [PID]` | attach to a running session |
| `l`, `logs` | read-only log browser |
| `-h <IP>`, `--bind <IP>`, `--bind=<IP>` | outgoing bind address for this session |
| `-v`, `--version` | print version |

`docs/src/content/installation.md:29-37` documents five of those six — the
bind flag is missing from that block and lives only in `README.md:160-180`.

## Constraint: `-h` is taken

`-h` is the bind-address override, modelled on `irssi -h` and shipped as a
documented flag (`README.md:166`). It cannot become help without breaking
released behaviour, so **help is long-form only**.

That leaves the likeliest discovery path broken in a second way: a user who
types `repartee -h` expecting help currently gets

```
repartee: -h requires an argument, e.g. -h 192.0.2.10
```

which explains a flag they did not mean to use and does not name the flag they
did. Both `-h` error paths in `parse_bind_override` gain a pointer to `--help`.

`-?` is rejected as an additional spelling: `?` is a glob metacharacter, so in
zsh — the user's shell — `repartee -?` fails at the shell with
`no matches found` before the binary is reached. `help` is accepted as a bare
subcommand, matching the existing `a` / `l` subcommand style.

## Design

### 1. An option table, not a heredoc

```rust
struct CliOption {
    /// Comma-joined spellings as the user types them, e.g. "-d, --detach".
    forms: &'static str,
    /// Value placeholder, empty for a switch.
    value: &'static str,
    /// Description lines — pre-split so the renderer never re-wraps.
    help: &'static [&'static str],
}

const SUBCOMMANDS: &[CliOption] = ...;
const OPTIONS: &[CliOption] = ...;
```

`help_text()` renders both tables into the usual `USAGE / SUBCOMMANDS /
OPTIONS / ENVIRONMENT / FILES` shape, aligning the description column to the
widest `forms` + `value` in either table so the two blocks line up with each
other. The table is the single source of truth; nothing is duplicated between
the renderer and the text.

Descriptions are stored pre-split into lines rather than wrapped at runtime. A
wrapper would be ~30 lines of code to re-derive a layout that is fixed at
compile time, and it would make the output depend on `$COLUMNS`.

**No `clap`.** Adding it would claim `-h` for help (the collision above),
start rejecting unrecognised argv, and pull a dependency tree in for one screen
of text. The hand-rolled table matches how `main()` already reads argv.

Per `CLAUDE.md`, the binary name in the output comes from
`constants::APP_NAME`, never a literal; paths come from the `constants`
accessors' known layout (`~/.<APP_NAME>/…`).

### 2. Dispatch position

Help is matched at the very top of `main()` — before `parse_bind_override`,
before `--version`, before `ensure_config_dir` — and prints to **stdout** with
exit status 0.

Ordering is load-bearing. `parse_bind_override` runs first today and exits 2 on
a malformed `-h`, so with help matched later, `repartee -h --help` would die
with a bind error while the user is explicitly asking for help. Help is the
escape hatch for a confused invocation; nothing may pre-empt it.

Matched forms: `--help` anywhere in argv, or `help` as the first argument.
`help` is position-restricted because a bare word later in argv is a value
(`repartee --bind help` is a malformed bind, not a help request), whereas
`--help` is unambiguous wherever it appears.

### 3. What stays unchanged

Unrecognised arguments keep falling through to a normal start. Rejecting them
is a separate behaviour change — it would turn today's silently-ignored junk
into exit 2 and could break wrapper scripts — and it is not what this work is
for. Noted here so the omission is a decision rather than an oversight.

`--version` keeps its current output and its current position after the bind
parse.

## Testing

Unit tests in `src/main.rs`:

1. `help_text()` contains a `USAGE:` line naming `APP_NAME`, and every section
   header.
2. Every flag and subcommand literal that `main()` matches on appears in the
   help output. The test derives the literal list from `main.rs`'s own source
   via `include_str!`, scanning for `== "…"` and `== Some("…")` comparisons, so
   adding a flag without a help entry fails the build rather than shipping an
   incomplete help screen. A hand-written list would go stale exactly when it
   matters.
3. The description column is aligned — every non-blank description line in the
   rendered `OPTIONS` and `SUBCOMMANDS` blocks starts at the same column.
4. `parse_bind_override` still errors on `-h` with no value, and the message
   now names `--help`.
5. `--help` is not mistaken for a bind value: `parse_bind_override` returns
   `None` for `["--help"]` (guards the case where argv reaches it anyway).

Verification: `make test` and `make clippy` clean — 0 warnings under pedantic +
nursery + perf=deny + redundant_clone=deny.

## Out of scope

- Man page and shell completions (neither exists in the repo today).
- Per-subcommand help (`repartee help attach`) — the whole surface fits on one
  screen, so a second level would be ceremony.
- Rejecting unrecognised argv (see §3).

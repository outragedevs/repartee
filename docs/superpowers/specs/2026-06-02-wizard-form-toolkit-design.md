# Wizard form toolkit + `/wizard server` — design

**Date:** 2026-06-02
**Status:** Approved
**Branch:** `feat/wizard-server-form`

## Summary

Build a **reusable wizard/form toolkit** for Repartee and ship **`/wizard server`** as its
first consumer: a popup, mouse-and-keyboard form for adding/editing an IRC server, in **both
the TUI and the web UI**. The existing manual `/server add …` command stays exactly as-is.

The toolkit is the real deliverable boundary. Future wizards — `/wizard connection`,
`/wizard user` — reuse the toolkit by supplying a different field *schema* and a *serializer*;
they add no new UI plumbing. We design the toolkit around the concrete needs of the server
wizard (YAGNI — no speculative widgets), but with clean boundaries so the next wizards drop in.

## Goals

- A friendly, discoverable way for "a mIRC user" to add a server without memorising flags.
- Full feature coverage of `ServerConfig` (everything `/server add` supports).
- Mouse support: click to focus a field, toggle a checkbox, switch pages, press a button.
- TUI and web parity: same fields, same structure, same validation, same persistence.
- Reusable: the form engine is generic; the server wizard is just a schema + serializer.

## Non-goals

- Replacing or deprecating manual `/server add` (it stays).
- "Save & Connect" (explicitly out — Save only).
- A gear/edit affordance on web connections (out for now; editing is via command).
- `/wizard connection` and `/wizard user` (future consumers; not built here).

## Triggers

- **TUI**
  - `/wizard server` → opens the **add** form (empty, sensible defaults).
  - `/wizard server <id>` → opens the **edit** form, pre-filled from `config.servers[<id>]`.
  - `/wizard` with no/unknown subcommand → prints short usage listing available wizards
    (currently just `server`).
- **Web**
  - `/wizard server[ <id>]` typed in the message input → opens the web modal (parity).
  - A discoverable **"+ Add network"** button opens the add modal with a click.

Manual `/server add|remove|list` is unchanged.

## Layout (TUI + web parity) — fixed-size modal, two pages

A centered modal that does **not** resize or scroll. Two pages switched with `◂ ▸` (TUI) or
tab clicks (web): **Basics** and **Advanced**.

**Basics page**

- Network Name (text) — friendly label; also seeds the server id.
- Server address / IP (text)
- Port (number)
- `[ ] Use TLS/SSL` (toggle)
- `[ ] Verify TLS certificate` (toggle, default on)
- Bind IP (text)

**Advanced page**

- Server id (text) — override of the auto-slug; *read-only in edit mode* (it is the map key).
- Nick (text)
- Username (text)
- Realname (text)
- Channels (text, comma-separated list)
- Server password (masked)
- SASL user (text)
- SASL pass (masked)
- SASL mechanism (select: `Auto` / `PLAIN` / `EXTERNAL`)
- Encoding (text)
- `[ ] Autoconnect` (toggle)
- `[ ] Auto-reconnect` (toggle)
- Reconnect delay, seconds (number)
- Reconnect max retries (number)
- Autosendcmd (text)
- Client cert path (text)

**Footer (both pages):** `[ Save ] [ Cancel ]` plus a key-hint line. Esc or Cancel discards
all input and closes with no changes.

## Toolkit field model

A wizard is a list of typed fields plus a current-value store, a focus index, a current page,
and a button row. Field kinds needed by the server wizard:

- `Text` — single-line editable string with a cursor.
- `Masked` — like `Text` but renders as `••••` (passwords).
- `Toggle` — boolean checkbox.
- `Select` — cycle through a fixed list of string options.
- `Number` — integer text validated against a target width (`u16`/`u32`/`u64`).

Each field carries: a stable key, a label, a kind, which page it belongs to, and optional
validation (required, numeric range). The toolkit owns:

- **Navigation:** Tab / Shift-Tab move focus across fields on the current page (wrapping into
  the button row); `◂ ▸` switch pages; Space toggles a `Toggle`; left/right cycles a `Select`;
  Enter on a button activates it; Esc cancels.
- **Mouse:** click a field to focus it; click a checkbox to toggle; click a tab to switch page;
  click a button to activate. Hit-testing uses per-field rects recorded at render time (the
  same pattern already used by `emote_picker`).
- **Rendering:** highlights the focused field; renders the active page's fields and the footer.

The toolkit is UI-mechanism only; it knows nothing about servers. The **server wizard** supplies
the field schema and two functions: `ServerConfig (+ creds) → field values` (for edit pre-fill)
and `field values → ServerConfig (+ creds)` (on Save).

## Data flow & persistence

Both front-ends converge on **one shared apply function**, conceptually:

```
apply_server_config(app, id: &str, config: ServerConfig, creds: ServerCreds, mode: Add|Edit)
```

It: inserts/overwrites `config.servers[id]`; writes credentials to `.env`
(`<ID>_PASSWORD`, `<ID>_SASL_PASS`, uppercased id); calls `save_config`; invalidates
`cached_config_toml`; emits a local confirmation event. The manual `/server add` path is
refactored to reuse the same persistence helper where practical (at minimum the `.env`
credential write is shared, since today's `/server add` keeps the password only in memory).

- **TUI** builds the `ServerConfig` in-process from field values and calls the helper directly.
- **Web** sends a **structured** `WebCommand::SaveServer { id, fields… }` — *not* a constructed
  `/server add` string. Building a command string would re-tokenise passwords and channel lists
  on whitespace and `-flag=value`, which breaks on spaces/special characters. The structured
  command is mirrored in `web-ui/src/protocol.rs`; `src/app/web.rs` decodes it and calls the
  same shared helper. The "+ Add network" button and the typed `/wizard server` both open the
  same Leptos modal, which emits `SaveServer`.

## Credentials & id rules

- Password and SASL pass are **masked** and only ever written to `.env`, never `config.toml`
  (existing project rule; `ServerConfig` already `skip_serializing`s them).
- **Edit mode:** masked fields render a sentinel placeholder meaning "unchanged"; `.env` is only
  rewritten for a masked field if the user actually changed it. Clearing the field explicitly
  removes the credential.
- **id derivation:** slugified from Network Name — lowercase, non-`[a-z0-9_]` runs collapsed to
  `_`, trimmed. Uniqueness enforced in **add** mode (suffix `_2`, `_3`, … or surface an inline
  error if the user typed an explicit duplicate id). In **edit** mode the id field is read-only.

## Validation

- Required: Network Name, Server address. Empty → inline error, Save blocked.
- Port and reconnect numbers must parse within their integer width (Port 1–65535).
- Add mode: id must be unique.
- TLS default port: if TLS is on and port is still the plaintext default 6667, bump to 6697
  (matches current `/server add` behaviour).
- Errors are shown inline near the offending field (or in the footer); Save stays disabled
  until the form validates.

## Module / file plan (approximate)

**TUI**

- `src/ui/wizard/mod.rs` — toolkit: `Field`, `FieldKind`, `WizardState`, render, navigation,
  mouse hit-testing.
- `src/ui/wizard/server.rs` — server schema + `ServerConfig` ↔ field-values serializers.
- `src/app/mod.rs` — `wizard: Option<WizardState>` overlay field + helpers.
- `src/app/input.rs` — route key/mouse events to the wizard when open (mirrors `emote_picker`).
- `src/commands/registry.rs` + a handler — register `/wizard`, open the overlay.

**Shared**

- A persistence helper (in `src/commands/handlers_admin.rs` or a `config` module) used by both
  the wizard and, where practical, the manual `/server add` path — including `.env` credential
  writes.

**Web**

- `web-ui/src/components/wizard.rs` — the modal (Basics/Advanced tabs, fields, Save/Cancel).
- web state signal for open/edit-id/field values; "+ Add network" button wiring.
- `WebCommand::SaveServer` added to **both** `src/web/protocol.rs` and `web-ui/src/protocol.rs`.
- `src/app/web.rs` — handle `SaveServer` → shared persistence helper.

**Tests**

- Schema ↔ `ServerConfig` round-trip (add and edit pre-fill).
- Slug derivation + uniqueness.
- Validation: required fields, port range, numeric fields.
- `.env` credential routing (written to env, absent from `config.toml`); edit "unchanged"
  sentinel leaves existing creds intact.
- Web `SaveServer` decodes and applies identically to the TUI path.
- Toolkit navigation: Tab/Shift-Tab wrap, page switch, toggle/select mutation, mouse hit-test.

## Out-of-scope reuse note

`/wizard connection` and `/wizard user` are future consumers. They will reuse
`src/ui/wizard/mod.rs` and the web modal shell unchanged, supplying their own schema +
serializer. Nothing in the toolkit may hard-code server semantics.

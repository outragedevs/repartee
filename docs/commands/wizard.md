---
category: Connection
description: Open settings or the guided server form
---

# /wizard

## Syntax

    /wizard
    /wizard server [id]

## Settings

Open Settings with `/wizard` or the gear in the top-left corner of the terminal
or web interface. The panel contains Networks & Connections, Appearance,
Messages, Notifications, History & Privacy, Keyboard, Extensions, E2E Encryption, Translation, and Advanced.
Search finds settings across all sections. Each field includes a description
and indicates whether a reconnect or restart is needed.

Changes remain in a draft until **Save**. **Cancel** discards the draft and
reverts browser appearance previews. **Section defaults** restores defaults for
the selected section, preserving existing network identities and credentials.
If an edited setting changed elsewhere, reopen the panel before saving it.

Browser text size, line spacing, image-preview visibility, and following the
terminal's active buffer remain local browser preferences. Other settings are
shared with the terminal. Complex collections (aliases, ignore rules, statusbar
items, translation models and per-buffer rules) use list and table editors.

Passwords are never prefilled. A configured indicator means an existing secret
is present; leaving the field untouched preserves it. Replacements are written
to `.env`, never to `config.toml`.

### Encryption and translation

**E2E Encryption** contains the master switch, default key-sharing mode and
replay-protection window. Enabling support does not encrypt every conversation:
use `/e2e on` in the relevant channel or query to set up encryption.

**Translation** contains the master switch, backend, your language, original-text
display and queue limits. Choose `ai`, configure models using API-key environment
variable names, and add channels or queries using `connection_id/#channel` or
`connection_id/nickname`. Set incoming/outgoing translation for each entry;
outgoing translation requires the channel language. Model routing lists and the
optional final stage are editable without writing JSON. The `stub` backend is
only for testing and reverses word order; it does not translate.

### Channel wizards

Choose **Channels** in the terminal footer or **Manage channels** on the web
from E2E Encryption or Translation. Save or cancel global settings drafts first.
Each wizard has separate tabs for your networks. Changes remain a draft until
Save; Cancel leaves the saved rules unchanged.

In the encryption wizard, add a channel such as `#chat`, toggle encryption and
select its key-sharing mode. Removing an entry disables encryption but keeps
its keys and trust records; the channel is subsequently listed as disabled.
Logging and E2E must be enabled and the main process restarted before this
wizard is available. Private-query key management remains under `/e2e`.

In the translation wizard, enter a channel or nickname without a network
prefix. Set incoming/outgoing translation, the channel language and optionally
your language. Outgoing translation needs an explicit channel language. Rules
on other networks are preserved, and encrypted conversations are excluded from
translation. Concurrent edits are rejected instead of silently overwritten.

### Terminal controls

- Click categories, fields, toggles, lists, or action buttons.
- **Tab / Shift-Tab** or **Down / Up** move focus; the field list scrolls.
- **Alt+Left / Alt+Right** switch categories; **Ctrl+F** focuses search.
- **Enter** activates a focused button, toggle or choice.
- **Ctrl+S** saves; **Esc** cancels.
- Text fields support arrows, Home/End, Delete/Backspace and paste.

The web panel supports desktop and phone layouts. Browser notifications use the
existing push controls and require browser permission and server support.

## Server shortcut

`/wizard server` opens the existing guided add-network form. In the terminal,
`/wizard server <id>` opens an existing network for editing. In the web client,
existing networks can be edited in Settings under Networks & Connections.

The server form has Basics and Advanced pages. Tab/Shift-Tab move focus,
Left/Right switch pages or choices, Space toggles checkboxes, and Enter on Save
persists changes. Mouse controls are available throughout.

## Examples

    /wizard
    /wizard server
    /wizard server libera

## See Also

/server, /connect, /set

---
category: Connection
description: Open settings or the guided server form
---

# /wizard

## Syntax

    /wizard
    /wizard server [id]

## Settings

Open Settings with `/wizard` or the gear in the top-right corner of the terminal
or web interface. The panel contains Networks & Connections, Appearance,
Messages, Notifications, History & Privacy, Keyboard, Extensions, and Advanced.
Search finds settings across all sections. Each field includes a description
and indicates whether a reconnect or restart is needed.

Changes remain in a draft until **Save**. **Cancel** discards the draft and
reverts browser appearance previews. **Section defaults** restores defaults for
the selected section, preserving existing network identities and credentials.
If an edited setting changed elsewhere, reopen the panel before saving it.

Browser text size, line spacing, image-preview visibility, and following the
terminal's active buffer remain local browser preferences. Other settings are
shared with the terminal. Complex collections (aliases, ignore rules, statusbar
items, translation models) use JSON fields with descriptions of their shape.

Passwords are never prefilled. A configured indicator means an existing secret
is present; leaving the field untouched preserves it. Replacements are written
to `.env`, never to `config.toml`.

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

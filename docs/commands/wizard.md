---
category: Connection
description: Open a guided add/edit form (server)
---

# /wizard

## Syntax

    /wizard server [id]

## Description

Open a guided popup form — a friendlier alternative to memorising `/server add`
flags. The form is a fixed-size modal with two pages, **Basics** and
**Advanced**, switched with the `◂ ▸` arrows or by clicking the tab row.

Both keyboard and mouse work throughout:

- **Tab** / **Shift-Tab** move between fields and the Save/Cancel buttons
- **Space** toggles the focused checkbox
- **← →** switch pages (or cycle the focused dropdown, e.g. SASL mechanism)
- **Enter** on Save commits; **Esc** (or Cancel) closes without saving
- **Click** a field to focus it, a checkbox to toggle, a tab to switch pages, or
  a button to press it

**Basics:** Network Name, server address/IP, port, Use TLS/SSL, Verify TLS
certificate, Bind IP.

**Advanced:** server id, nick, username, realname, channels, server password,
SASL user/pass/mechanism, encoding, autoconnect, auto-reconnect, reconnect delay
and max retries, autosendcmd, client cert path.

Passwords (server password, SASL pass) are stored in `.env`, never `config.toml`.
The manual `/server add` command is unchanged and still available.

## Subcommands

### server

Open the add-server form.

    /wizard server

With an id, open the form pre-filled to **edit** that existing server (the id
field is locked):

    /wizard server libera

On edit, leaving a password field blank keeps the stored credential unchanged.

## Web UI

The wizard is also available in the web UI: click **"+ Add network"** or type
`/wizard server`. The web form is add-only (editing an existing server is a TUI
feature).

## Examples

    /wizard server
    /wizard server libera

## See Also

/server, /connect, /set

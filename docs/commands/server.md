---
category: Connection
description: Manage server configurations
---

# /server

## Syntax

    /server [list|add|remove] [args...]

## Description

Manage IRC server configurations. Add, remove, and list servers.
Server credentials (passwords, SASL) are stored in `.env`.

## Subcommands

### list

List all configured servers with their connection status.

    /server list

This is the default when no subcommand is given.

### add

Add a new server to the configuration.

    /server add <id> <address>[:<port>] [flags...]

**Flags:**
- `-tls` — Enable TLS (auto-sets port to 6697)
- `-noauto` — Don't auto-connect on startup
- `-notlsverify` — Skip TLS certificate verification
- `-bind=<ip>` — Bind to local IP (vhost)
- `-nick=<nick>` — Server-specific nick override
- `-label=<name>` — Display name
- `-password=<pass>` — Server password (saved to .env)
- `-sasl=<user>:<pass>` — SASL auth (saved to .env)
- `-autosendcmd=<cmds>` — Commands to run on connect, before autojoin (must be last flag)

Autosendcmd uses erssi-style syntax: commands separated by `;`, with `WAIT <ms>`
for delays. `$N` is replaced with your nick.

    /server add libera irc.libera.chat:6697 -tls -autosendcmd=MSG NickServ identify pass; WAIT 2000; MODE $N +i

### remove

Remove a server and disconnect if connected.

    /server remove <id>

Aliases: del

## Examples

    /server list
    /server add libera irc.libera.chat:6697 -tls
    /server add libera irc.libera.chat:6697 -tls -sasl=user:pass
    /server add local 127.0.0.1:6667 -noauto -label=dev
    /server remove libera

## See Also

/connect, /disconnect, /set

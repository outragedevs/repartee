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
- `-notlsverify` — Skip TLS certificate verification
- `-noauto` — Don't auto-connect on startup
- `-label=<name>` — Display name
- `-nick=<nick>` — Use a different nick for this server
- `-password=<pass>` — Server password (PASS command)
- `-sasl=<user>:<pass>` — SASL PLAIN authentication credentials
- `-bind=<ip>` — Bind to a specific local IP address
- `-autosendcmd=<cmds>` — Commands to run on connect (semicolon-separated)

### remove

Remove a server and disconnect if connected.

    /server remove <id>

Aliases: del

## Examples

    /server list
    /server add libera irc.libera.chat 6697 -tls
    /server add local 127.0.0.1 6667 -noauto -label=dev
    /server add ircnet irc.ircnet.net 6697 -tls -nick=mynick -sasl=user:pass
    /server add bouncer bnc.example.com 6697 -tls -password=secret -bind=192.168.1.10
    /server remove libera

## See Also

/connect, /disconnect, /set

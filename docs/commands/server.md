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

    /server add <id> <address>[:<port>] [port] [flags...]

**Flags:**
- `-tls` — Enable TLS (auto-sets port to 6697)
- `-notls` — Disable TLS
- `-tlsverify` — Enable TLS certificate verification
- `-notlsverify` — Skip TLS certificate verification
- `-auto` — Auto-connect on startup
- `-noauto` — Don't auto-connect on startup
- `-label=<name>` — Display name
- `-nick=<nick>` — Use a different nick for this server
- `-username=<user>` — Use a different username for this server
- `-realname=<name>` — Use a different real name for this server
- `-password=<pass>` — Server password (PASS command)
- `-sasl=<user>:<pass>` — SASL PLAIN authentication credentials
- `-sasl-user=<user>` — SASL username
- `-sasl-pass=<pass>` — SASL password
- `-sasl-mechanism=<mechanism>` — SASL mechanism (`PLAIN`, `SCRAM-SHA-256`, `EXTERNAL`)
- `-channels=<ch1,ch2>` — Channels to join after registration
- `-bind=<ip>` — Bind to a specific local IP address
- `-encoding=<codec>` — IRC text encoding label
- `-autoreconnect=<bool>` — Enable or disable reconnects (`true`, `false`)
- `-reconnect-delay=<secs>` — Base reconnect delay in seconds
- `-reconnect-max-retries=<n>` — Maximum reconnect attempts
- `-autosendcmd=<cmds>` — Commands to run on connect (semicolon-separated)
- `-client-cert=<path>` — Client TLS certificate path for SASL EXTERNAL / CertFP

Flag values are parsed as single command arguments, so values with spaces should
be edited in `config.toml` or with `/set`.

### remove

Remove a server and disconnect if connected.

    /server remove <id>

Aliases: del

## Examples

    /server list
    /server add libera irc.libera.chat 6697 -tls
    /server add kakao kakao.ajalo.com 6697 -tls -notlsverify -noauto
    /server add local 127.0.0.1 6667 -noauto -label=dev
    /server add ircnet irc.ircnet.net:6697 -tls -nick=mynick -sasl=user:pass
    /server add bouncer bnc.example.com 6697 -tls -password=secret -bind=192.168.1.10
    /server add certfp irc.example.net:6697 -tls -sasl-mechanism=EXTERNAL -client-cert=/path/to/cert.pem
    /server remove libera

## See Also

/connect, /disconnect, /set

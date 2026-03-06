---
category: Connection
description: Connect to a server by id, label, or address
---

# /connect

## Syntax

    /connect <server-id|label|address>[:<port>] [-tls] [-bind=<ip>]

## Description

Connect to an IRC server. Accepts a configured server ID, label, or a
raw address for ad-hoc connections. If the server is already configured,
flags override its settings for this connection only.

## Examples

    /connect libera
    /connect irc.libera.chat:6697 -tls
    /connect mynet -bind=192.168.1.100

## See Also

/server, /disconnect, /quit

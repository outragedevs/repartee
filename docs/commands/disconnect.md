---
category: Connection
description: Disconnect from a server
---

# /disconnect

## Syntax

    /disconnect [server-id|label] [message]

## Description

Disconnect from an IRC server. With no arguments, disconnects from the
server associated with the current buffer. Optionally specify a server
by ID or label, and a quit message.

## Examples

    /disconnect
    /disconnect libera
    /disconnect libera Goodbye!

## See Also

/connect, /quit, /server

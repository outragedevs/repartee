---
category: Connection
description: Disconnect from a server
---

# /disconnect

## Syntax

    /disconnect [message]

## Description

Disconnect the IRC server associated with the current buffer and disable its
automatic reconnect for this session. Any argument is used as the quit
message; switch to a buffer on the intended network before running the command.

## Examples

    /disconnect
    /disconnect Goodbye!

## See Also

/connect, /quit, /server

---
category: Info
description: Query server or user client version
---

# /version

## Syntax

    /version [nick]

## Description

Without arguments, queries the IRC server version. With a nick, sends a CTCP VERSION request to that user to find out what client they are using. The reply is displayed in the active buffer.

## Examples

    /version                # query server version
    /version someone        # query someone's client version

## See Also

/whois

---
category: Channel
description: List users in a channel
---

# /names

## Syntax

    /names [channel]

## Description

Display the list of users in a channel. Without arguments, lists users in the current channel. Also sends a NAMES request to the server to refresh the nick list panel.

The output shows each user with their mode prefix (@, +, etc.) and a total user count.

## Examples

    /names              # list users in current channel
    /names #help        # list users in #help

## See Also

/who, /whois

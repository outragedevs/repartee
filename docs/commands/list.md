---
category: Info
description: List channels on server
---

# /list

## Syntax

    /list [pattern]

## Description

Request a list of channels from the server. Optionally filter by a pattern (e.g. `#rust*`).

Results are displayed in the server status buffer as they arrive.

Note: On large networks, `/list` without a filter may return thousands of channels.

## Examples

    /list
    /list #rust*
    /list *linux*

## See Also

/join, /names

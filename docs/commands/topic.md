---
category: Channel
description: Set or view channel topic
---

# /topic

## Syntax

    /topic
    /topic <text>
    /topic <channel>
    /topic <channel> <text>

## Description

View or set the channel topic.

Without arguments, displays the current topic for the active channel. With only a channel name, requests the topic from the server. With text, sets the topic on the active channel (or on the specified channel).

## Examples

    /topic                          # show current topic
    /topic #help                    # request topic for #help
    /topic Welcome to the channel   # set topic on current channel
    /topic #help Welcome!           # set topic on #help

## See Also

/mode, /names

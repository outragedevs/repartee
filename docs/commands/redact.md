---
category: Connection
description: Request deletion of a message through a bouncer
---

# /redact

## Syntax

    /redact <target> <msgid> [reason]

## Description

Ask the current network to delete a message in a channel or private conversation.
Use the exact IRC message ID supplied by the server; IDs are case-sensitive.
The optional reason may contain spaces. No reason is supplied automatically.

The connection must be a connected bouncer network that has negotiated
`draft/message-redaction` and `message-tags`. Control connections and connections
without this support report that redaction is unavailable. The pinned Lurker
version does not offer this capability.

The message remains visible until the server confirms deletion with REDACT.
The server decides which messages you may delete and whether a time limit
applies. Rejections appear in the originating command buffer when response
labels are available, otherwise on the originating network. A rejection does
not delete the message locally.

Confirmed deletion replaces the message with a notice identifying who deleted
it and any reason the server supplied. Once detailed deletion metadata expires,
older messages still remain deleted but may show only a generic deletion notice.
Recipients that do not support message
redaction may retain their original copy.

## Examples

    /redact #chat opaque-message-id
    /redact Alice another-message-id sent to the wrong conversation

## See Also

/msg, /bouncer

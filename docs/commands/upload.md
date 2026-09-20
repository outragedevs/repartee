---
category: Connection
description: Upload a local file through the current bouncer
---

# /upload

## Syntax

    /upload <path> [content-type]

## Description

Upload a local file through the FILEHOST service advertised by the current
connected bouncer network. Select a channel or private conversation first.
Paths containing spaces must be quoted. The optional MIME type overrides the
type inferred from the filename. Files must be regular, nonempty, and at most
64 MiB; the server may impose a lower limit or reject the content type.

The returned URL appears in the originating conversation. If that conversation
is still active and the input line is empty, the URL is placed there for you to
send. It is never sent to IRC automatically. One upload may run at a time.

The command reads a path on the machine running the daemon. It is restricted to
native input; browser commands and scripts cannot use it to read daemon files.
HTTPS is required when the IRC connection uses TLS. Upload redirects are refused.

## Examples

    /upload /tmp/picture.png
    /upload "/tmp/my document.pdf" application/pdf

## See Also

/bouncer, /msg

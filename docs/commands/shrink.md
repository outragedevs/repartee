---
category: Other
description: Shorten a URL via the configured shrink API (default `shr.al`)
---

# /shrink

## Syntax

    /shrink <url>

## Description

Send a single URL to the configured shrink service and print the
shortened form into the current buffer as a local event line.

Repartee can also shorten URLs automatically as you type them
(`shrink.outgoing_enabled`) and when others post long URLs you
receive (`shrink.incoming_enabled`). `/shrink` is the manual escape
hatch for the one-off case, or when you want a copy-pasteable short
URL without sending anything to a channel.

URLs hit the in-memory cache first, so calling `/shrink` on a URL
that was already shortened in this session returns instantly with
`(cached)` appended.

The API key is read from `.env` (`SHRINK_API_KEY=…`) — see
`SHRINK_API.md` for the API spec. If the key is missing or
`shrink.enabled = false`, `/shrink` prints an error instead of
calling the API.

## Examples

    /shrink https://example.com/very/long/path/to/article-2026-05-24
    /shrink https://x.com/foo/status/1234567890

## Configuration

    /set shrink.enabled                true
    /set shrink.api_url                https://shr.al
    /set shrink.outgoing_enabled       true
    /set shrink.incoming_enabled       true
    /set shrink.min_url_length         50      (≥ 25)
    /set shrink.outgoing_timeout_ms    2000
    /set shrink.incoming_timeout_ms    2000
    /set shrink.cache_max_entries      500

`SHRINK_API_KEY` is loaded from `.env` only; it is never written to
`config.toml`.

## See Also

/set, /preview

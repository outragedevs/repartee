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

## Known limitations (v1)

- **Mentions buffer (`_mentions`)** records the **original** URL even
  when the chat buffer shows the shortened form. The mention-buffer
  push runs synchronously inside the IRC handler before the deferred
  shrink worker substitutes the text. Look at the chat buffer for the
  shortened form; the mentions buffer is canonical for full URLs.
- **Multi-chunk outgoing messages** (over ~510 bytes) fall through to
  the non-shrink path and ship with original URLs. Per-chunk
  substitution accounting is out of scope for v1; threshold of 50
  chars keeps this rare in practice.
- **`/set shrink.{api_url,outgoing_timeout_ms,incoming_timeout_ms,cache_max_entries}`**
  changes config but the running workers captured these at startup —
  `/set` emits a `restart required` notice for these keys.
- **`/set shrink.enabled true`** has no effect if `SHRINK_API_KEY`
  was missing at startup (no client was ever built). `/set` emits a
  diagnostic and the operator must restart with the key set in `.env`.
- **Multi-URL latency budget is per chunk, not per message.** A
  message with N URLs that all miss the cache makes
  `ceil(N / 4)` sequential round-trips to the API (4 is the
  per-message concurrency cap chosen to keep shr.al rate limits
  happy). A 12-URL paste with a 2 s per-call timeout can therefore
  stall the outgoing pipeline for up to 6 s. The pipeline is
  sequential per direction, so a message typed during that stall
  falls through to the unshrunk path (visible in the buffer, but
  with original URLs).

## See Also

/set, /preview

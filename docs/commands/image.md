---
category: Media
description: Manage image preview cache
---

# /image

## Syntax

    /image [stats|clear]

## Description

Manage the image preview cache. Without arguments, shows current status.

All image preview settings are persistent via `/set`:

    /set image_preview.enabled true|false
    /set image_preview.protocol auto|kitty|iterm2|sixel|symbols
    /set image_preview.max_width 0
    /set image_preview.cache_max_mb 100

## Subcommands

### stats

Show cache file count and disk usage.

    /image stats

### clear

Delete all cached images.

    /image clear

## Examples

    /image
    /image stats
    /image clear
    /set image_preview.enabled false
    /set image_preview.protocol kitty

## See Also

/preview, /set

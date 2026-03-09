---
category: Media
description: Manage image preview cache
---

# /image

## Syntax

    /image [stats|clear|cleanup|debug]

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

### cleanup

Remove cached images that exceed the configured size or age limits
(`image_preview.cache_max_mb` and `image_preview.cache_max_days`).
Reports the number of files removed and disk space freed.

    /image cleanup

### debug

Show detailed image preview diagnostics including detected protocol,
terminal capabilities, font size, environment variables, and tmux
passthrough status.

    /image debug

## Examples

    /image
    /image stats
    /image clear
    /image cleanup
    /image debug
    /set image_preview.enabled false
    /set image_preview.protocol kitty

## See Also

/preview, /set

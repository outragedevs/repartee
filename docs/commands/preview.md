---
category: Media
description: Preview an image URL in the terminal
---

# /preview

## Syntax

    /preview <url>

## Description

Fetches an image URL and displays it as a popup overlay in the terminal.
Supports direct image links (jpg, png, gif, webp) and pages with og:image
metadata (imgur, imgbb, etc.).

The display protocol is auto-detected based on your terminal:
kitty, iTerm2, sixel, or Unicode half-block fallback.
Works through tmux with DCS passthrough.

Press any key or click to dismiss the preview.

## Examples

    /preview https://i.imgur.com/abc123.jpg
    /preview https://imgur.com/gallery/xyz

## See Also

/image

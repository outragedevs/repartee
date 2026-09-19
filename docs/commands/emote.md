---
category: Other
description: Open the emote picker, or insert/search :name: emotes
---

# /emote

## Syntax

    /emote [name|search]

## Description

Open the built-in emote picker. With an exact Polish or English emote name,
insert its `:name:` token at the cursor. With a partial name, list up to ten
matching emotes in the active buffer.

The command is available when `[emotes] enabled = true` and `render` is not
`"off"`. Names are matched case-insensitively, with or without surrounding
colons. The picker can also be opened with `Ctrl+G`.

## Examples

    /emote
    /emote smile
    /emote :usmiech:
    /emote smi

## See Also

/set

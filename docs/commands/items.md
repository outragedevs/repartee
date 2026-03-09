---
category: Statusbar
description: Manage statusbar items and formats
---

# /items

## Syntax

    /items [list|add|remove|move|format|separator|available|reset] [args...]

## Description

Manage the statusbar layout. Add, remove, reorder, and format statusbar items.

## Subcommands

### list

Show current statusbar items with their positions.

    /items list

This is the default when no subcommand is given.

### add

Add an item to the statusbar.

    /items add <item>

### remove

Remove an item from the statusbar.

    /items remove <item>

### move

Move an item to a different position in the statusbar.

    /items move <item> <position>

Position is 1-based (1 = first item).

### format

Set or view the format string for a statusbar item.

    /items format <item> [format_string]

Without a format string, shows the current format.

### separator

Set or view the separator between statusbar items.

    /items separator [string]

Without a string, shows the current separator.

### available

List all available statusbar item types.

    /items available

### reset

Reset statusbar to default items, formats, and separator.

    /items reset

## Available Items

- `time` — Current time
- `nick_info` — Current nick and user modes
- `channel_info` — Channel/buffer name and modes
- `lag` — Server lag measurement
- `active_windows` — Windows with unread activity

## Examples

    /items list
    /items add time
    /items remove lag
    /items move time 1
    /items format time %H:%M
    /items separator  -
    /items available
    /items reset

## See Also

/set

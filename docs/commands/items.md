---
category: Statusbar
description: Manage statusbar items and formats
---

# /items

## Syntax

    /items [list|add|remove|move|format|separator|available|reset] [args...]

## Description

Manage the statusbar layout. Add, remove, reorder items and customize
their format strings. Use `/items available` to see all possible items
and their format variables.

## Subcommands

### list

Show current statusbar items with their positions and formats.

    /items list

This is the default when no subcommand is given.

### add

Add an item to the statusbar.

    /items add <item>

### remove

Remove an item from the statusbar.

    /items remove <item>

Aliases: del

### move

Move an item to a new position.

    /items move <item> <position>

Position is 1-based.

### format

View or set the format string for an item.

    /items format <item> [format_string]

Format variables: `$win`, `$activity`, `$nick`, `$modes`, `$name`, `$lag`, `$time`.

Aliases: fmt

### separator

View or set the separator string between items.

    /items separator [string]

Aliases: sep

### available

List all available statusbar items and their default formats.

    /items available

Shows which items are currently active and which are unused.

### reset

Reset statusbar to default items, formats, and separator.

    /items reset

## Examples

    /items list
    /items add clock
    /items remove lag
    /items move clock 1
    /items format window_name [$win] $name
    /items separator  |
    /items available
    /items reset

## See Also

/set

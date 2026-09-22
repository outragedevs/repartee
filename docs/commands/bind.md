---
category: Configuration
description: List and configure keyboard bindings and key sequences
---

# /bind

## Syntax

    /bind [<key> [<action> [<data>]]]
    /bind -list
    /bind -delete <key>
    /bind -reset <key>

## Description

List current bindings, filter by key, or assign an action. `-list` lists supported
actions. `-delete` removes a binding, including a default; `-reset` restores its
default. Successful changes are saved immediately. A failed save leaves the
previous configuration active. Customizations and deleted defaults survive
restart and `/reload`.

Keys are case-sensitive. Use `meta-` for Alt or Escape followed by another key,
and uppercase caret notation for Control (`^W^C`). `key` gives a sequence a name;
combine names with hyphens. Recursive definitions are rejected. `/command`
is shorthand for `command command`. Command arguments retain internal spaces.

The default navigation bindings are `meta-a` and `meta-A` for unread activity,
`meta-0` through `meta-9` for numbered windows, and `meta-left`/`meta-right` for
adjacent windows. These are editable bindings, not hardcoded fallbacks.
Numbers match the sidebar; 0 is Status. Unlike irssi, Alt+0 does not select 10
unless explicitly rebound. Activity uses the highest unread priority, breaking
ties by oldest unread activity, not by last visited window.

`change_window`, `active_window`, `previous_window`, and `next_window` navigate.
`command` executes a command in the current window/network. `multi` runs action
names with arguments separated by semicolons; `nothing` consumes a key.
Editing, history, completion, scrolling, `insert_text`, and `refresh_screen`
actions are available through `-list`. Unsupported actions produce an error.

A complete sequence that is also a prefix waits for more input. Configure
`/set keyboard.key_timeout 1000` for a one-second timeout; 0 disables that
sequence timeout, as in irssi. A lone Escape has a separate 500 ms prefix
window. On timeout an exact match executes. Unmatched printable input is
preserved instead of silently discarded. Paste bypasses bindings and ends
an incomplete sequence. Key release events do not execute bindings.

Settings, wizard and emote modals receive input first. Escape dismisses spell
suggestions and image previews first. In shell input mode only navigation
bindings apply; other input goes to the PTY. A standalone Escape goes to the
PTY immediately, so use Alt navigation while a shell owns input. The terminal shim reserves
Ctrl+backslash, Ctrl+4 and Ctrl+Z for detach. Detach clears incomplete sequences.

The initial integration applies to TUI input. Browser execution is implemented
in the subsequent integration stage; configuring bindings from a browser still
updates the shared saved configuration.

## Examples

    /bind meta-0 change_window 10
    /bind meta-c /clear
    /bind ^W^C command query someone
    /bind meta-q key win1
    /bind meta-w key win2
    /bind win1-win2 change_window 12
    /bind meta-x multi erase_line;insert_text hello
    /bind -delete meta-a
    /bind -reset meta-a

The full two-digit scheme is described at
https://redlegion.org/posts/2012-05-03-100-windows-in-irssi/.
The implementation was compared with official irssi commit
`43f1727ef9fe1cb51d5031ee2dd0e1965217e4cb`, particularly `keyboard.c`,
`gui-readline.c`, and the bind help page.

## See Also

/set, /alias, /help

# Theming

rustirc uses irssi-compatible format strings with 24-bit color support.

## Theme files

Themes are TOML files stored in `~/.rustirc/themes/`. Set the active theme in your config:

```toml
[general]
theme = "mytheme"
```

This loads `~/.rustirc/themes/mytheme.theme`.

## Theme structure

A theme file has two sections: `colors` and `abstracts`.

```toml
[colors]
bg = "1a1b26"
bg_alt = "24283b"
fg = "a9b1d6"
fg_alt = "565f89"
highlight = "e0af68"
nick_self = "7aa2f7"
timestamp = "565f89"
separator = "3b4261"

[abstracts]
line_start = "{timestamp $Z}{sb_background}"
timestamp = "%Z565f89$*"
own_msg = "{ownmsgnick $0}$1"
pubmsg = "{pubmsgnick $0}$1"
```

## Colors

The `[colors]` section defines hex RGB values (without `#`) for UI elements:

| Key | Description |
|---|---|
| `bg` | Main background color |
| `bg_alt` | Alternate background (topic bar, status line) |
| `fg` | Main text color |
| `fg_alt` | Muted text color |
| `highlight` | Highlight/mention color |
| `nick_self` | Your own nick color |
| `timestamp` | Timestamp color |
| `separator` | Border/separator color |

## Abstracts

Abstracts are named format string templates that can reference each other. They control how every UI element is rendered — from message lines to the status bar.

See [Format Strings](theming-format-strings.html) for the full format string syntax.

## Default theme

If no theme is set, rustirc uses built-in defaults with a dark color scheme inspired by Tokyo Night.

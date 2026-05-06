# Theming

repartee uses irssi-compatible format strings with 24-bit color support.

## Theme files

Themes are TOML files stored in `~/.repartee/themes/`. Set the active theme in your config:

```toml
[general]
theme = "mytheme"
```

This loads `~/.repartee/themes/mytheme.theme`.

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
date_separator = "%Z3b4261─── $* ───"
backlog_end = "%Z565f89─── End of backlog ($* lines) ───"
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

## Event formats

System IRC lines use `[formats.events]`. Each entry receives positional
arguments from the IRC event. For example, WHOIS replies can be themed with:

```toml
[formats.events]
whois = "%Zc0caf5$0%Z565f89 ($1@$2)%N %Za9b1d6$3%N"
whois_server = "%Z565f89  server: %Za9b1d6$1%N%Z565f89$3%N"
whois_channels = "%Z565f89  channels: %Za9b1d6$1%N"
end_of_whois = "%Z7aa2f7─────────────────────────────────────────────%N"
```

WHOIS event keys are `whois_header`, `whois`, `whois_server`, `whois_oper`,
`whois_idle`, `whois_idle_signon`, `whois_channels`, `whois_away`,
`whois_account`, `whois_secure`, `whois_certfp`, `whois_keyvalue`, and
`end_of_whois`.

WHOIS parameters follow IRC reply structure. Common examples: `whois` receives
nick, user, host, realname; `whois_server` receives nick, server, server info,
formatted server info; `whois_idle_signon` receives nick, idle duration, signon
time; `whois_secure` receives nick, display value, server text.

## Default theme

If no theme is set, repartee uses built-in defaults with a dark color scheme inspired by Tokyo Night.

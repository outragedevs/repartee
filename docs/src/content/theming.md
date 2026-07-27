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
whois_server = "{whois server: {whois_value $1}%Z565f89$3}"
whois_channels = "{whois channels: {whois_value $1}}"
end_of_whois = "%Z7aa2f7─────────────────────────────────────────────%N"
```

WHOIS event keys are `whois_header`, `whois`, `whois_server`, `whois_oper`,
`whois_idle`, `whois_idle_signon`, `whois_channels`, `whois_away`,
`whois_account`, `whois_secure`, `whois_certfp`, `whois_keyvalue`,
`whois_special`, `whois_registered`, `whois_help`, `whois_bot`,
`whois_actually`, `whois_host`, `whois_modes`, and `end_of_whois`.

For every `whois_*` key, `$0` is the nick and `$1` is the line's primary value;
further parameters carry detail. `whois` receives nick, user, host, realname;
`whois_server` receives nick, server, server info, formatted server info;
`whois_idle_signon` receives nick, idle duration, signon time; `whois_secure`
receives nick, display value (`TLS`), and the server's own wording.

Numerics with no dedicated key render through `whois_special`. That includes
IRCnet's 320, which carries both the cloak line and the TLS line with text
configured by the server admin — indistinguishable by numeric, so repartee
shows them verbatim rather than guessing which is which.

### Restyling the whole block

The block's indent and base colour come from the `whois` and `whois_value`
abstracts, so changing how every WHOIS line looks means editing those two
entries rather than all twenty formats:

```toml
[abstracts]
whois = "%Z565f89  $*%N"
whois_value = "%Za9b1d6$*%N"
```

`whois_header`, `whois`, `whois_oper` and `end_of_whois` stay literal — the
first two and the last are the block's frame rather than indented body lines.

### WHOIS errors

A WHOIS can also be answered with an error. Those keys are deliberately not
whois-prefixed, because the same numerics answer other commands too:
`no_such_nick` (401, also a failed `/msg` or `/invite`), `no_such_server` (402,
returned by `/whois nick nick` against an unknown server), and `try_again`
(263, rate limiting). Each receives the subject in `$0` and the server's reason
text in `$1`, and is styled through the `error` abstract rather than `whois`.

## Default theme

If no theme is set, repartee uses built-in defaults with a dark color scheme inspired by Tokyo Night.

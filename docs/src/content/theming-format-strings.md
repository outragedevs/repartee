# Format Strings

repartee implements a full irssi-compatible format string engine with extensions for 24-bit color.

## Color codes

### 24-bit foreground: `%Z` + RRGGBB

```
%Z7aa2f7Hello    → "Hello" in blue (#7aa2f7)
```

### 24-bit background: `%z` + RRGGBB

```
%z1a1b26%Za9b1d6Text    → Light text on dark background
```

### irssi single-letter codes: `%X`

| Code | Color |
|---|---|
| `%k` / `%K` | Black / Dark gray |
| `%r` / `%R` | Red / Light red |
| `%g` / `%G` | Green / Light green |
| `%y` / `%Y` | Yellow / Light yellow |
| `%b` / `%B` | Blue / Light blue |
| `%m` / `%M` | Magenta / Light magenta |
| `%c` / `%C` | Cyan / Light cyan |
| `%w` / `%W` | White / Bright white |

### Style codes

| Code | Effect |
|---|---|
| `%_` | Bold |
| `%/` | Italic |
| `%-` | Strikethrough |
| `%U` | Underline |
| `%n` / `%N` | Reset all formatting |

## Abstracts: `{name args}`

Abstracts are named templates that expand recursively:

```toml
[abstracts]
sb_background = "%z24283b"
timestamp = "%Z565f89$*"
line_start = "{timestamp $Z}{sb_background}"
```

Usage in another abstract: `{timestamp 12:34}` expands `$*` to `12:34`.

## Variable substitution

| Syntax | Meaning |
|---|---|
| `$0` – `$9` | Positional argument |
| `$*` | All arguments joined |
| `$[N]0` | Argument padded/truncated to N characters |

Example: `$[8]0` pads argument 0 to 8 characters (right-aligned by default).

## mIRC control characters

repartee also parses mIRC-style control characters in incoming messages:

| Char | Hex | Effect |
|---|---|---|
| Bold | `\x02` | Toggle bold |
| Color | `\x03` | mIRC color code (fg,bg) |
| Hex color | `\x04` | Hex color code |
| Reset | `\x0F` | Reset all formatting |
| Reverse | `\x16` | Swap fg/bg |
| Italic | `\x1D` | Toggle italic |
| Strikethrough | `\x1E` | Toggle strikethrough |
| Underline | `\x1F` | Toggle underline |

## Abstraction depth

Abstracts can reference other abstracts up to 10 levels deep to prevent infinite recursion.

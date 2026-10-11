---
category: Other
description: Show the current window as plain text to select and click
---

# /bare

## Syntax

    /bare [<seconds>]

## Description

Toggle the bare display, as in weechat. The terminal shows only the current
window's messages as plain text: no sidebar, nick list, topic, status or input
line. Mouse reporting is switched off, so the terminal selects text with the
mouse as usual. Each message is printed as one line that the terminal wraps
itself, so a long URL split across rows is still recognised and clickable as
one link.

The default key is `meta-l` (Alt+L or Escape then L); see `/bind` to change it.
Press it again to return. With `<seconds>`, the normal layout returns on its
own after that long.

While the bare display is on:

- Page Up/Page Down scroll by a screen, Up/Down by one message, and Home/End
  jump to the oldest loaded message or the live end. The mouse wheel scrolls
  in terminals that turn it into arrow keys. Bound `scroll_start`,
  `scroll_end` and highlight seek actions work too.
- Following the live end, new messages appear as they arrive. A scrolled view
  stays where it is.
- Typing returns to the normal layout, and the typed text is kept in the
  input line. So does switching windows, from the terminal or anywhere else.

Messages show their timestamp, nick and text without colors. Inline images
and emotes are hidden until you return. Only messages already loaded in the
window are shown. Shell windows have no bare display.

The web interface does not need this mode: the browser selects text and opens
links itself.

## Examples

    /bare
    /bare 10

## See Also

/bind, /set

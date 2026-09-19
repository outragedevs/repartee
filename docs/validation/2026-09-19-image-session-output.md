# Image output and attached sessions

A 2000x2000 RGBA Kitty image at 20x40 pixel cell size produced more than
21 MiB of terminal output. The previous 16 MiB writer limit rejected it even
with an empty queue, and the app detached on the resulting `WouldBlock`.

Socket rendering now stages a complete frame before publishing it, including
flushes performed internally by the terminal backend and graphics helpers.
The staging buffer and queued output each have a 64 MiB limit. Rendering waits for outstanding
output to drain; IRC and input handling remain in the event loop. Failed frames
are discarded before publication. An oversized popup becomes an error popup
without dropping the terminal, and the next frame performs a full redraw.
If an ordinary chat frame is too large, inline images and graphical emotes are
suppressed for this attachment; text and links remain usable. Reattaching resets
this fallback, and the saved configuration is unchanged.

The output task sends at most 1 MiB per protocol message, preserving byte order
and keeping framing overhead below the existing 64 MiB receiver limit. The shim
holds at most two downstream messages, so a slow stdout applies backpressure
instead of accumulating hundreds of large output messages.

Validation covers:

- The real application layout, Kitty encoder and socket writer with a
  2000x2000 image: output exceeds the old limit without detaching.
- A stalled receiver defers subsequent frames until the previous output drains.
- A reduced-budget failure publishes no partial frame, retains the terminal,
  and successfully draws the error on the next frame.
- An oversized inline-image frame falls back to text, including after closing
  its warning; the saved inline-image setting remains enabled.
- Protocol chunks round-trip through a 128-byte duplex buffer with exact byte
  equality and a subsequent detach control message still readable.

No original failing URL or physical terminal trace was supplied. These tests
reproduce and fix the confirmed output-limit mechanism; they do not claim a
physical Kitty/iTerm2/Sixel terminal matrix or rule out other image failures.

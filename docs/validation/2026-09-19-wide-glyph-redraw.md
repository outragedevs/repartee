# Residual characters after wide emoji

The pinned ratatui-core 0.1.0 diff emits an extra update for a trailing cell
inside a wide grapheme containing VS16 (U+FE0F), when that cell previously held
content. CrosstermBackend treats consecutive update coordinates as single-column
cursor advances. After printing a two-column emoji, its synthetic trailing-cell
write therefore lands one column too far right. A contiguous run of subsequent
updates, including a distant character, is shifted with it.

A following frame clears the character's logical position, while its shifted
physical position remains untouched. This is a frame-diff defect affecting
scrolling and buffer changes, not only buffer-switch invalidation.

Reproduction: render `abcdefghijklmnopqrstuvwx`, then a VS16 emoji followed by
18 spaces and `Z`, then `x`. The middle frame incorrectly emits 19 spaces after
the emoji without cursor repositioning. The backend adapter moves the
synthetic trailing-cell clears before the wide VS16 grapheme. This retains the
library's explicit clearing for terminals that need it, without overwriting
the new glyph or shifting the following text. It preserves trailing-column
clears when a wide glyph is removed and leaves escape-bearing image cells untouched.
Both local and socket-backed application terminals use the adapter.

The regression test replays actual emitted ANSI through vt100. Since that
parser does not give VS16 presentation sequences their two-column terminal
width, the fixture substitutes a known two-column emoji for each tested glyph
before replay; escape sequences and spacing are unchanged. This explicitly
models terminals where the reported emoji occupies two columns. Controls cover
ordinary emoji and CJK. The unadapted backend fails; the adapted backend passes.
This is not a physical terminal/font compatibility matrix.

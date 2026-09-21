# Local event history

SQLite previously stored an event's fallback text and key, but omitted its
structured parameters. Reopening history could not reproduce the live theme.
The literal-percent change additionally treated preformatted WHOIS fallback
text as user text, exposing `%Z...` and `%N` sequences in native and web history.

New rows persist the complete parameter vector. Native history passes it back
to the same theme renderer as live events, including custom event templates.
Parameters are independent per row, including QUIT/NICK fan-out references.
Encrypted logs encrypt parameter JSON separately with a fresh random IV; they
do not leave a plaintext copy of event arguments in a new column.

The migration adds nullable parameter and IV columns without rewriting old
messages. Read-only history accepts databases without these columns. Legacy
WHOIS and the associated preformatted error events retain their inline color
codes as formatting. Plain event fallbacks retain literal percent characters.
New structured WHOIS web output substitutes literal arguments into its default
formatted template, so percent-looking user data is not interpreted as theme
syntax. No browser protocol or frontend rebuild is required.

Old plain JOIN/PART/QUIT/KICK rows do not contain the original parameter vector.
Their stored text remains available, but the exact old live theme cannot be
reconstructed from data that was never persisted. New rows preserve that data.

Validation includes every event template in both bundled themes through the real asynchronous
SQLite writer and a separately opened read-only connection, both encrypted and
plain. The tests compare native live/history spans, compare web live/history
output, inspect ciphertext storage, preserve literal percent data, and exercise
legacy colored WHOIS. The existing fan-out logging test checks that reference
rows retain parameters even when their stored text is empty.

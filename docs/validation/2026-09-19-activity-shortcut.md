# Activity shortcut validation

Alt+A selects the highest pending activity level and then the oldest buffer within that level. Equal or lower activity does not move a buffer back in the queue; escalation starts a new position at the higher level. Reading, closing and renaming buffers update the ordering metadata. Activity is ordered when it becomes visible in the local state, independently of server timestamps.

Keyboard tests cover Alt+A, Shift+Alt+A, the Esc+A fallback, preservation of the input draft, no-op behavior without activity, shell input, and exclusion of Ctrl+Alt+A. State tests cover priority, stable ordering, escalation, aggregated mentions, DCC/server-label renames and both automatic fallback paths after closing a buffer. A web-handler test verifies that web reads and silent switches remove candidates.

Validation: `make clippy` without project warnings and `make test` with 2299 native tests and 138 web tests. `make build` passed, and the final Sol medium review reported no actionable findings.

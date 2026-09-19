# Mobile preview revalidation

Validated PR #39 after merging the current main branch on 2026-09-19. Generated web assets were rebuilt from the merged source to resolve asset-name conflicts.

## Checks

- `make wasm` passed with `NO_COLOR=true` (Trunk requires a boolean value).
- `make clippy` passed without project warnings.
- `make test` passed: 2268 native tests and 138 web tests.
- Cargo reported the existing future-compatibility notice in the third-party `proc-macro-error2` dependency.

## Browser exercise

The rebuilt WASM application ran in Playwright WebKit 26.6 on macOS, using a 390 × 844 CSS-pixel mobile viewport. A local WebSocket fixture supplied two channels with 30 messages and ten image previews each. Preview responses were held until more than 3.2 seconds after buffer switching. No live IRC connection or user data was used.

| Measurement | Before image load | After image load |
| --- | --- | --- |
| Preview box | 380 × 237.5 px | 380 × 237.5 px |
| Scroll height | 3033 px | 3033 px |
| Distance from bottom | 0 px | 0 px |

Switching to the other channel and revisiting the first channel both retained a zero-pixel distance from the bottom. While reading 500 pixels above the tail, an incoming message increased content height without changing scroll position. Jump-to-latest restored tail following.

The extended exercise also passed:

- unchanged rounded scroll positions preserving an upward gesture near the tail;
- zoom and multi-touch events preserving tail-following mode;
- a failed preview above the viewport preserving history mode and the 20-pixel tail gap;
- native keyboard Shift+Space with focus on the chat scroller;
- a synthetic mouse press at the overlay-scrollbar edge followed by scrolling;
- a viewport shrink while reading, followed by a single Home-style scroll that fetched older history;
- a viewport shrink while following, followed by the first upward history fetch.

Screenshots of tail following and history reading were inspected. No JavaScript errors occurred. This checks the Safari engine at a mobile viewport; it does not claim validation of physical iPhone touch momentum or the iOS software keyboard. Gesture thresholds and directions are additionally covered by the web unit tests.

## Corrections found during revalidation

The review and browser exercise identified overlay-scrollbar detection, Shift+Space handling, focus theft by the global type-to-chat listener, stale height after resize, and premature tail-following during the first pixels of an animated upward scroll. The final implementation preserves focus for scrolling keys, updates cached height through the resize observer, and requires movement toward the tail before resuming near-bottom following.

Failed-preview collapse now uses the explicit follow-tail mode and the pre-collapse scroll position, avoiding both unwanted tail jumps and duplicate browser height compensation.

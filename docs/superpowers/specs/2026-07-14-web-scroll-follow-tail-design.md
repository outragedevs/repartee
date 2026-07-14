# Web UI scroll: follow-tail as an intent, not a geometry guess

**Date:** 2026-07-14
**Status:** approved (design reviewed independently)
**Files:** `web-ui/src/components/chat_view.rs`, `web-ui/src/state.rs`

## Problem

On iOS, switching into a buffer leaves the chat off its last line. The view is often
correct for a moment, then drifts a few lines up and the scroll-to-bottom arrow
appears — i.e. the client decided the reader had scrolled away when they had not.

Broken on iPhone 12 mini and 16 Pro under Safari, Brave and "Chrome" (every browser on
iOS is WebKit; Apple permits no other engine). Not reproducible on Android Chrome
(Blink) or on desktop.

Four previous attempts failed. Each targeted a *source of layout growth* (preview
images, iOS font inflation) or a *symptom* (forcing the flag back on inside `do_pin`).
None touched the *mechanism* that is supposed to absorb that growth. That is the
architectural defect this design fixes.

## Root cause

`is_at_bottom` conflates two different things:

1. **The reader's intent** — "keep me at the end of the conversation".
2. **The current geometry** — "the viewport happens to sit at the bottom right now".

Every pin in the component is gated on that single flag, and the *only* thing allowed
to clear it is `on_scroll` — which cannot tell a real gesture from a scroll event the
browser synthesised while we replaced the DOM.

The failure path:

1. Buffer switch replaces the message list; `scrollHeight` changes and the browser
   clamps `scrollTop`, queueing a synthetic `scroll` event.
2. Per the HTML rendering steps, scroll events are dispatched **before**
   `requestAnimationFrame` callbacks. So `on_scroll` runs before our pin, reads
   (new `scrollHeight`, stale `scrollTop`), concludes "not at bottom", and clears
   `is_at_bottom`. It may also fire a needless backlog fetch (`scrollTop < 400px`).
3. The RAF pin then places the viewport correctly — but the flag is already false.
4. Everything that lands afterwards (preview images at 0.5–3s, the webfont swap, iOS
   Text Autosizing, emote GIFs, backlog prepends) grows the content. The ResizeObserver
   that should re-pin is gated on `is_at_bottom` — so it does nothing, and the view is
   left a few lines short.

(Note: ResizeObserver notifications are delivered *after* RAF callbacks, not during the
"run the resize steps" phase — those are window `resize` events. The scroll-before-RAF
ordering, which is what this bug turns on, holds.)

## What the reference implementations do

Neither The Lounge nor lurker uses `flex-direction: column-reverse`. Both run the same
architecture we do. The differences that matter:

- **The Lounge** re-pins directly from each preview image's `@load` (`LinkPreview.vue` →
  `keepScrollPosition()`), and guards its scroll handler with a `skipNextScrollEvent`
  flag set around every programmatic write.
- **lurker** refuses to let a synthetic scroll change intent: if `clientHeight` differs
  from the last seen value, `onScroll` returns without touching `stickToBottom`
  (pattern from `stackblitz-labs/use-stick-to-bottom`). It also disengages
  synchronously on an upward `wheel`.

Our `onload` only adds a CSS class. We have no `clientHeight` guard. Those are the gaps.

## Design

Replace `is_at_bottom` with an explicit, **persistent** mode:

```rust
pub enum ScrollMode {
    FollowingTail,   // keep me at the end of the conversation
    ReadingHistory,  // I am reading; do not move my viewport
}
```

**`FollowingTail` has no timeout.** It is not a window, a tick, or a settle timer — a
deliberate rejection of the reviewed proposal, because "the height has not changed for
250ms" only means nothing happened for 250ms, not that the layout is done. An image
landing at 700ms must still be absorbed. Only the reader ends `FollowingTail`.

### Entering FollowingTail

- buffer switch
- the scroll-to-bottom button
- in `ReadingHistory`, the reader scrolling back near the bottom (within
  `SCROLL_THRESHOLD`)

### While in FollowingTail

- Every content-height change re-pins: the message-list mutation Effect, both
  ResizeObservers (container **and** content wrapper), and the appearance signals.
  **Unconditionally** — no geometry check, because the geometry check is the bug.
- `on_scroll` does nothing: it must not change the mode and must not fetch backlog.
  Every scroll event in this mode is the browser's, not the reader's.
- A pin runs only if the callback still belongs to the buffer it was scheduled for
  (generation guard) **and** the mode is still `FollowingTail`.

### Leaving FollowingTail — only on real reader intent

`touchstart` alone is **not** intent: it also fires when tapping a link, a nick, a
preview's dismiss button, or when selecting text. The signals are:

- `wheel` with `deltaY < 0`
- `touchmove` past a vertical threshold (`TOUCH_DRAG_PX = 8.0`) from the `touchstart`
  point — no `preventDefault`, so native momentum scrolling is untouched
- `PageUp` / `Home` / `ArrowUp`

A downward drag while already at the bottom flips to `ReadingHistory` too; the very next
scroll event finds the viewport near the bottom and flips it straight back. That is
acceptable and self-correcting.

### While in ReadingHistory

- `on_scroll` computes geometry normally, fetches backlog near the top, and returns to
  `FollowingTail` when the reader comes back to the bottom (collapsing the loaded
  backlog window, as today).
- Guard from lurker: if `clientHeight` changed since the last scroll event, that event
  came from a resize (keyboard, URL bar, textarea growth), not the reader — return
  without touching the mode. The resize path owns the correction.
- Nothing re-pins the viewport. A late image or a new message must not move the reader.

### Kept as-is

`pending_scroll_top` stays. Replacing it with a `skipNextScrollEvent` bool would be a
regression: a programmatic write that produces no scroll event (position already
correct, or clamped) leaves the bool armed, and it then swallows the reader's next real
gesture. It remains what it is — a marker for our own writes, notably the backlog-restore
path — and is no longer the primary way we infer intent.

Also kept: the reserved preview box (measured: 3×2px → 412×258px before the fix, i.e.
+2527px of post-pin growth for ten previews), `overflow-anchor: none`, both
ResizeObservers, the 30px threshold, no `content-visibility`, and
`text-size-adjust: 100%` (defensive — Text Autosizing was never confirmed as the cause,
and the comment claiming otherwise gets toned down).

### Bugs found in review, fixed here

- `pin_scheduled` and `resize_throttle` are set *before* `web_sys::window()` is
  obtained; if that ever returns `None` the flag stays latched and **every subsequent
  pin is dead**. Acquire the window first.
- RAF callbacks carry no buffer identity: one scheduled for buffer A can run after the
  reader has switched to B. Capture a generation counter and bail when stale.
- `do_pin` must only scroll. It must not decide the reader's state (the forced
  `is_at_bottom = true` added in `e51988c` is removed).

## Diagnostics

The failure has never been observed on real iOS — every conclusion so far is inference
from code plus a Playwright harness that does **not** reproduce it (Linux WebKitGTK has
no Text Autosizing and no iOS visual viewport). So ship an opt-in overlay, enabled with
`localStorage['repartee-scroll-debug'] = '1'`, showing live `scrollTop`, `scrollHeight`,
`clientHeight`, distance from bottom, the active buffer, the current `ScrollMode`, and a
rolling log of mode transitions with their trigger. If this design still fails on the
device, that log — not another hypothesis — is what tells us why.

## Acceptance criteria

1. Buffer switch: a synthetic scroll before the RAF neither leaves `FollowingTail` nor
   fires a backlog fetch.
2. A height change **more than 3 seconds** after the switch still re-pins, provided the
   reader has not scrolled.
3. Tapping a link, a nick, or a preview's dismiss button does not leave `FollowingTail`.
4. A real upward scroll gesture leaves `FollowingTail` immediately.
5. In `ReadingHistory`, a late image, font swap, or new message does not move the
   reader's position.
6. Scrolling back to the bottom re-enters `FollowingTail`.
7. Rapid A → B → C switching: callbacks from A and B cannot move C.
8. The backlog-prepend scroll restore is not mistaken for a gesture or for a return to
   the live tail.
9. A `clientHeight` change (textarea growth, orientation, URL bar) does not leave
   `FollowingTail`.
10. A preview with a reserved box has identical `scrollHeight` before and after its
    image loads.

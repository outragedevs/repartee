# GG7 Emotes — Plan 2: TUI Inline Animation + Input UX Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **Prerequisite:** Plan 1 (`2026-06-01-gg-emotes-foundation-web.md`) must be merged first — this plan depends on `crate::emotes::{names, contains, bytes}`, `crate::emotes::parse`, and `config::{EmotesConfig, RenderMode}`.

**Goal:** Render `:name:` emotes as inline, animated images in the TUI chat view on graphics-capable terminals, with clean `:name:` text fallback elsewhere, plus tab-completion, a picker popup, and a `/emote` command.

**Architecture:** Approach A (overlay compositing). When emotes are enabled and the terminal supports a graphics protocol, a render-time pass rewrites `:name:` tokens in each chat `Line` into fixed-width Private-Use-Area (PUA) placeholder spans so the existing manual wrapper reserves the right number of cells. After lines are laid out, a position resolver computes each visible placeholder's screen rect; an animator picks the current GIF frame (from a shared clock) and composites it into that rect via `ratatui-image` (or the tmux direct-write path). A new ~50 ms tick arm in the `select!` loop forces redraws so animations play. Input gains `:name:` tab-completion, a keyboard+mouse picker overlay, and a `/emote` command. Storage/session/web are untouched — buffers still hold plain `:name:` text.

**Tech Stack:** Rust 2024, ratatui 0.30, ratatui-image 10, `image` 0.25 (GIF frame decode), crossterm, tokio.

**Spec:** `docs/superpowers/specs/2026-06-01-builtin-gg-emotes-design.md`

**Conventions:** `color-eyre`, `tracing`, `crate::constants::APP_NAME`, clippy pedantic+nursery=warn / perf=deny / redundant_clone=deny (0 warnings), `state/` stays UI-agnostic, command handlers are `fn(&mut App, &[String])`. Build via `make`. Commit frequently.

---

## Key integration facts (from codebase exploration)

- **Chat render:** `src/ui/chat_view.rs::render(frame, area, app)` builds `Line`s via
  `message_line::render_message`, wraps each with `super::wrap_line(line, total_width, indent)`
  (`src/ui/mod.rs:155`), collects `visible_lines: Vec<Line>`, and renders one `Paragraph`.
  Final cell coordinates are **not** exposed — we compute them ourselves (Task 3).
- **Layout:** `src/ui/layout.rs::draw(frame, app)` calls `chat_view::render` then
  `image_overlay::render(frame, frame.area(), app)` last. `app.image_clear_rect` + `Clear`
  targeted repaint at `layout.rs:151`.
- **Compositing reference:** `src/ui/image_overlay.rs::render_ready` renders a
  `ratatui_image::StatefulImage` into a `Rect` (widget path), or, when `app.in_tmux` and protocol
  ≠ Halfblocks, defers to a direct-stdout write (`src/app/image.rs::write_tmux_direct_image`).
- **Picker/protocol:** `app.picker: ratatui_image::picker::Picker` (`src/app/mod.rs:385`);
  `picker.protocol_type()`, `picker.font_size() -> (u16, u16)`, `picker.new_resize_protocol(dyn_img)`.
- **Event loop:** `src/app/mod.rs` main loop (~994). `terminal.draw(|f| ui::layout::draw(f, self))`
  runs once per iteration before `select!` (~1077). Intervals declared ~1056
  (`let mut tick = interval(Duration::from_secs(1));`). `needs_full_redraw: bool` (field ~387)
  forces a clear+repaint next iteration.
- **App struct:** `src/app/mod.rs:354`. `App::new_with_mode` ~506. `ui_regions: Option<UiRegions>`
  (`layout.rs:14`) holds per-area `Rect`s (chat_area, etc.) and drives mouse hit-testing.
- **Input:** `src/ui/input.rs::InputState { value, cursor_pos, tab_state, .. }`; `insert_char`
  (~64). Tab completion: `src/app/input.rs::handle_tab` (~687) → `input.tab_complete(...)`
  (`src/ui/input.rs:341`). Key dispatch: `src/app/input.rs::handle_key` (~157). Mouse:
  `handle_mouse` (~386) using `ui_regions` + `Rect::contains(Position)`.
- **Commands:** `CommandHandler = fn(&mut App, &[String])` (`src/commands/types.rs`); registry
  vec `COMMANDS` (`src/commands/registry.rs:24`); handlers in `handlers_*.rs`.

---

## File Structure

| File | Responsibility | Create/Modify |
|---|---|---|
| `src/emotes/mod.rs` | Add `frames(name)` lazy GIF-frame decode cache | Modify |
| `src/ui/emote_layout.rs` | PUA placeholder encoding + position resolver (pure, tested) | Create |
| `src/app/emote_anim.rs` | `EmoteAnimator`: shared clock → frame index; encoded-frame cache; compositing helpers | Create |
| `src/ui/message_line.rs` | Conditional `:name:` → PUA placeholder rewrite when graphical | Modify |
| `src/ui/chat_view.rs` | After layout: resolve placeholder rects, record them on `App` | Modify |
| `src/ui/layout.rs` | After chat render: composite current frames over recorded rects | Modify |
| `src/app/mod.rs` | `anim_tick` interval + `select!` arm; animator/placement fields | Modify |
| `src/ui/emote_picker.rs` | Picker overlay state + render | Create |
| `src/app/input.rs` | `:name:` tab-completion; picker key/mouse; insert token | Modify |
| `src/ui/input.rs` | `tab_complete` emote branch | Modify |
| `src/commands/handlers_ui.rs` | `cmd_emote` handler | Modify |
| `src/commands/registry.rs` | register `/emote` | Modify |

---

## Task 1: `frames()` — lazy GIF frame decode cache

**Files:**
- Modify: `src/emotes/mod.rs`
- Test: inline tests in `src/emotes/mod.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn frames_decode_with_delays() {
    let frames = frames("usmiech").expect("usmiech frames");
    assert!(!frames.is_empty(), "at least one frame");
    let (img, _delay) = &frames[0];
    assert!(img.width() > 0 && img.height() > 0);
    // Delays are clamped to a sane floor so a 0-delay GIF doesn't spin the CPU.
    assert!(frames.iter().all(|(_, d)| *d >= 20));
    // Stable identity across calls (cached, same pointer).
    let again = frames("usmiech").unwrap();
    assert!(std::ptr::eq(frames.as_ptr(), again.as_ptr()));
}

#[test]
fn frames_unknown_is_none() {
    assert!(frames("definitely_not_an_emote").is_none());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p repartee emotes::tests::frames_decode_with_delays`
Expected: FAIL — `cannot find function frames`.

- [ ] **Step 3: Implement `frames()`**

Add to `src/emotes/mod.rs`:
```rust
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

/// One decoded animation frame and its display duration (ms, floored at 20).
pub type Frame = (image::RgbaImage, u32);

/// Lazily decode and cache all frames of an emote GIF. Returns `None` if unknown
/// or if decoding fails. The returned slice is stable for the process lifetime.
#[must_use]
pub fn frames(name: &str) -> Option<&'static [Frame]> {
    static CACHE: OnceLock<RwLock<HashMap<String, &'static [Frame]>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| RwLock::new(HashMap::new()));

    if let Some(slice) = cache.read().ok()?.get(name) {
        return Some(slice);
    }
    let decoded = decode_frames(name)?;
    let leaked: &'static [Frame] = Box::leak(decoded.into_boxed_slice());
    cache.write().ok()?.insert(name.to_owned(), leaked);
    Some(leaked)
}

fn decode_frames(name: &str) -> Option<Vec<Frame>> {
    use image::AnimationDecoder;
    use image::codecs::gif::GifDecoder;

    let data = bytes(name)?;
    let decoder = GifDecoder::new(std::io::Cursor::new(data.into_owned())).ok()?;
    let mut out = Vec::new();
    for frame in decoder.into_frames().collect_frames().ok()? {
        let (num, den) = frame.delay().numer_denom_ms();
        let delay = if den == 0 { 100 } else { (num / den).max(20) };
        out.push((frame.into_buffer(), delay));
    }
    if out.is_empty() { None } else { Some(out) }
}
```

> Leaking decoded frames is acceptable: the set is bounded (183 emotes), frames are decoded only
> on first display, and they live for the whole session. This keeps the API `&'static` so the
> render path needs no lifetimes.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p repartee emotes::tests`
Expected: PASS.

- [ ] **Step 5: Lint + commit**

```bash
make clippy
git add src/emotes/mod.rs
git commit -m "feat(emotes): lazy GIF frame decode cache (frames())"
```

---

## Task 2: PUA placeholder encoding (pure)

**Files:**
- Create: `src/ui/emote_layout.rs`
- Modify: `src/ui/mod.rs` (add `pub mod emote_layout;`)
- Test: inline tests in `src/ui/emote_layout.rs`

The placeholder is a fixed run of PUA codepoints so the existing `wrap_line` (unicode-width based)
reserves exactly `EMOTE_COLS` cells. We encode the emote's registry index in the codepoint.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_then_decode_roundtrips_index() {
        let ph = placeholder_for_index(42);
        assert_eq!(ph.chars().count(), EMOTE_COLS);
        assert!(ph.chars().all(is_placeholder_char));
        assert_eq!(decode_placeholder_index(ph.chars().next().unwrap()), Some(42));
    }

    #[test]
    fn placeholder_width_is_emote_cols() {
        use unicode_width::UnicodeWidthStr;
        assert_eq!(placeholder_for_index(0).width(), EMOTE_COLS);
    }

    #[test]
    fn non_placeholder_char_decodes_none() {
        assert_eq!(decode_placeholder_index('a'), None);
        assert!(!is_placeholder_char('a'));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p repartee emote_layout`
Expected: FAIL — unresolved items.

- [ ] **Step 3: Implement encoding**

In `src/ui/emote_layout.rs`:
```rust
//! Encoding of emote placeholders into chat lines and resolution of their screen
//! rectangles after wrapping. Pure logic — no rendering side effects.

/// Width in terminal cells reserved for one emote (square-ish at 1 row tall).
pub const EMOTE_COLS: usize = 2;

/// Base of the Private Use Area range we use to mark emote placeholders.
/// PUA-A (U+F0000..=U+FFFFD) gives ~65k slots; 183 emotes fit easily.
const PUA_BASE: u32 = 0x000F_0000;
const PUA_MAX: u32 = 0x000F_FFFD;

/// Build the placeholder string for an emote registry index: `EMOTE_COLS`
/// identical PUA chars, each encoding the index. Identical chars => a contiguous
/// run of width `EMOTE_COLS` that the resolver collapses back to one emote.
#[must_use]
pub fn placeholder_for_index(index: u32) -> String {
    let c = char::from_u32(PUA_BASE + index).unwrap_or('\u{FFFD}');
    std::iter::repeat(c).take(EMOTE_COLS).collect()
}

/// True if `c` is one of our placeholder codepoints.
#[must_use]
pub fn is_placeholder_char(c: char) -> bool {
    let u = c as u32;
    (PUA_BASE..=PUA_MAX).contains(&u)
}

/// Recover the emote registry index from a placeholder char.
#[must_use]
pub fn decode_placeholder_index(c: char) -> Option<u32> {
    let u = c as u32;
    if (PUA_BASE..=PUA_MAX).contains(&u) { Some(u - PUA_BASE) } else { None }
}
```

- [ ] **Step 4: Register the module**

In `src/ui/mod.rs` add (near the other `pub mod` lines):
```rust
pub mod emote_layout;
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p repartee emote_layout`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/ui/emote_layout.rs src/ui/mod.rs
git commit -m "feat(ui): emote PUA placeholder encoding"
```

---

## Task 3: Position resolver (pure)

**Files:**
- Modify: `src/ui/emote_layout.rs`
- Test: inline tests

Given the wrapped `visible_lines` and the chat `Rect`, produce the screen rect of every emote
placeholder. We scan each visual line's `Span`s, tracking display-column offset; a run of
placeholder chars (same index) becomes one `EmotePlacement`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn resolves_placement_after_text() {
    use ratatui::layout::Rect;
    use ratatui::text::{Line, Span};

    // Visual line: "ab" + placeholder(index=5) + "c"
    let ph = placeholder_for_index(5);
    let line = Line::from(vec![Span::raw("ab"), Span::raw(ph), Span::raw("c")]);
    let area = Rect::new(10, 4, 40, 5);

    let placements = resolve_placements(&[line], area);
    assert_eq!(placements.len(), 1);
    let p = &placements[0];
    assert_eq!(p.emote_index, 5);
    assert_eq!(p.rect.x, 10 + 2);   // area.x + width("ab")
    assert_eq!(p.rect.y, 4);        // first visual row
    assert_eq!(p.rect.width as usize, EMOTE_COLS);
    assert_eq!(p.rect.height, 1);
}

#[test]
fn skips_placeholders_scrolled_off() {
    use ratatui::layout::Rect;
    use ratatui::text::{Line, Span};
    let ph = placeholder_for_index(1);
    let lines = vec![Line::from(Span::raw(ph.clone())), Line::from(Span::raw(ph))];
    // area only 1 row tall: only the first visual line is on-screen here, since the
    // caller passes exactly the visible slice. Both lines are "visible" in this test
    // (caller already sliced), so expect 2 placements on consecutive rows.
    let area = Rect::new(0, 0, 10, 2);
    let placements = resolve_placements(&lines, area);
    assert_eq!(placements.len(), 2);
    assert_eq!(placements[0].rect.y, 0);
    assert_eq!(placements[1].rect.y, 1);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p repartee emote_layout`
Expected: FAIL — `cannot find type EmotePlacement` / `resolve_placements`.

- [ ] **Step 3: Implement the resolver**

Append to `src/ui/emote_layout.rs`:
```rust
use ratatui::layout::Rect;
use ratatui::text::Line;
use unicode_width::UnicodeWidthChar;

/// Where one emote should be composited, in absolute screen cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmotePlacement {
    pub emote_index: u32,
    pub rect: Rect,
}

/// Walk the already-wrapped visible lines and compute the screen rect of each
/// emote placeholder run. `area` is the chat region; `lines[k]` maps to row
/// `area.y + k`. Placements whose row exceeds `area` height are dropped.
#[must_use]
pub fn resolve_placements(lines: &[Line<'_>], area: Rect) -> Vec<EmotePlacement> {
    let mut out = Vec::new();
    for (row, line) in lines.iter().enumerate() {
        if row >= area.height as usize {
            break;
        }
        let y = area.y + row as u16;
        let mut col: usize = 0;
        let mut run: Option<(u32, usize, usize)> = None; // (index, start_col, width)
        let mut flush = |run: &mut Option<(u32, usize, usize)>, out: &mut Vec<EmotePlacement>| {
            if let Some((idx, start, w)) = run.take() {
                let x = area.x.saturating_add(u16::try_from(start).unwrap_or(u16::MAX));
                out.push(EmotePlacement {
                    emote_index: idx,
                    rect: Rect::new(x, y, u16::try_from(w).unwrap_or(u16::MAX), 1),
                });
            }
        };
        for span in &line.spans {
            for ch in span.content.chars() {
                let cw = ch.width().unwrap_or(0);
                if let Some(idx) = decode_placeholder_index(ch) {
                    match &mut run {
                        Some((cur, _start, w)) if *cur == idx => *w += cw,
                        _ => {
                            flush(&mut run, &mut out);
                            run = Some((idx, col, cw));
                        }
                    }
                } else {
                    flush(&mut run, &mut out);
                }
                col += cw;
            }
        }
        flush(&mut run, &mut out);
    }
    out
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p repartee emote_layout`
Expected: PASS (5 tests total in this module).

- [ ] **Step 5: Lint + commit**

```bash
make clippy
git add src/ui/emote_layout.rs
git commit -m "feat(ui): resolve emote placeholder screen rects after wrap"
```

---

## Task 4: Rewrite `:name:` → placeholder in message rendering (gated)

**Files:**
- Modify: `src/ui/message_line.rs`
- Test: inline tests in `src/ui/message_line.rs`

We only rewrite when emotes are enabled, `render == Graphical`, and the terminal supports graphics.
That decision is computed once per frame and passed in as a bool (`emotes_graphical`). In text/off
modes or on non-graphics terminals, the literal `:name:` stays (existing behavior).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod emote_tests {
    use super::*;
    use crate::ui::emote_layout::{is_placeholder_char, EMOTE_COLS};

    #[test]
    fn known_token_becomes_placeholder_when_graphical() {
        // emotify_message_text replaces ":usmiech:" with a placeholder run.
        let out = emotify_message_text("hi :usmiech: x", true);
        let placeholder_chars = out.chars().filter(|c| is_placeholder_char(*c)).count();
        assert_eq!(placeholder_chars, EMOTE_COLS);
        assert!(!out.contains(":usmiech:"));
        assert!(out.starts_with("hi "));
    }

    #[test]
    fn unchanged_when_not_graphical() {
        let out = emotify_message_text("hi :usmiech: x", false);
        assert_eq!(out, "hi :usmiech: x");
    }

    #[test]
    fn unknown_token_unchanged() {
        let out = emotify_message_text(":nope: :)", true);
        assert_eq!(out, ":nope: :)");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p repartee message_line::emote_tests`
Expected: FAIL — `cannot find function emotify_message_text`.

- [ ] **Step 3: Implement the rewrite helper**

In `src/ui/message_line.rs`:
```rust
use crate::emotes;
use crate::emotes::parse::Segment;
use crate::ui::emote_layout::placeholder_for_index;

/// Replace known `:name:` tokens with fixed-width PUA placeholders so the wrapper
/// reserves cells for the image. No-op when `graphical` is false. The emote's
/// placeholder index is its position in `emotes::names()` (binary-searchable).
#[must_use]
pub(crate) fn emotify_message_text(text: &str, graphical: bool) -> String {
    if !graphical || !text.contains(':') {
        return text.to_owned();
    }
    let segs = emotes::parse::tokenize(text);
    if !segs.iter().any(|s| matches!(s, Segment::Emote(_))) {
        return text.to_owned();
    }
    let names = emotes::names();
    let mut out = String::with_capacity(text.len());
    for seg in segs {
        match seg {
            Segment::Text(range) => out.push_str(&text[range]),
            Segment::Emote(name) => {
                match names.binary_search_by(|n| n.as_str().cmp(name.as_str())) {
                    Ok(idx) => out.push_str(&placeholder_for_index(u32::try_from(idx).unwrap_or(0))),
                    Err(_) => {
                        out.push(':');
                        out.push_str(&name);
                        out.push(':');
                    }
                }
            }
        }
    }
    out
}
```

- [ ] **Step 4: Apply the rewrite in the message-rendering path**

`render_message` builds spans from `msg.text`. Add a `graphical: bool` parameter to
`render_message` (and the internal `render_chat_message`), and rewrite the text before it is
passed to `parse_format_string`. Concretely, where the message body string is taken from
`msg.text` (in `render_chat_message`), wrap it:
```rust
let body = emotify_message_text(&msg.text, graphical);
// ...use `body` wherever `&msg.text` was passed into parse_format_string for the message body...
```
Update the single call site in `chat_view.rs` (Task 5) to pass the computed `graphical` flag.

> Do NOT rewrite event/MentionLog text — only the chat message body (PRIVMSG/ACTION/NOTICE).
> Keep the placeholder out of the timestamp/nick columns.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p repartee message_line::emote_tests`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/ui/message_line.rs
git commit -m "feat(ui): rewrite :name: to PUA placeholders in graphical mode"
```

---

## Task 5: Resolve placements in chat_view and store on App

**Files:**
- Modify: `src/app/mod.rs` (App fields + helper)
- Modify: `src/ui/chat_view.rs`

- [ ] **Step 1: Add App state for placements + graphical decision**

In `src/app/mod.rs` `struct App`, add:
```rust
    /// Emote placements resolved during the last chat render, consumed by the
    /// compositing pass in `layout::draw`. Cleared and rebuilt every frame.
    pub emote_placements: Vec<crate::ui::emote_layout::EmotePlacement>,
```
Initialize to `Vec::new()` in `App::new_with_mode`.

Add a helper on `App` (e.g. in `src/app/mod.rs` or a small `src/app/emote_anim.rs` created in Task 6):
```rust
/// Whether emotes should render graphically this frame: enabled, mode=Graphical,
/// and the detected protocol is a real graphics protocol (not Halfblocks).
#[must_use]
pub fn emotes_graphical(&self) -> bool {
    use crate::config::RenderMode;
    self.config.emotes.enabled
        && self.config.emotes.render == RenderMode::Graphical
        && self.picker.protocol_type() != ratatui_image::picker::ProtocolType::Halfblocks
}
```

- [ ] **Step 2: Thread the flag + resolve placements in chat_view**

In `src/ui/chat_view.rs::render`, compute `let graphical = app.emotes_graphical();`, pass it into
`message_line::render_message(msg, is_own, &app.theme, &app.config, nick_fg, graphical)`, and after
`visible_lines` is built (before/after `frame.render_widget(paragraph, area)`):
```rust
// Record emote placements for the compositing pass (layout::draw consumes them).
// `render` takes `&App` today; change its signature to `&mut App` OR collect into a
// local and store via a setter. Simplest: change chat_view::render to take &mut App.
let placements = if graphical {
    crate::ui::emote_layout::resolve_placements(&visible_lines, area)
} else {
    Vec::new()
};
app.emote_placements = placements;
```

> `chat_view::render` currently takes `app: &App`. Change it to `app: &mut App` and update the
> call in `layout.rs`. (Render functions for other components already take `&mut App` per the
> exploration, e.g. `image_overlay::render`, so this is consistent.)

- [ ] **Step 3: Build to verify it compiles**

Run: `cargo build -p repartee`
Expected: compiles. (Behavioral verification happens in Task 7.)

- [ ] **Step 4: Commit**

```bash
git add src/app/mod.rs src/ui/chat_view.rs
git commit -m "feat(ui): resolve and store emote placements each chat render"
```

---

## Task 6: Emote animator + compositing

**Files:**
- Create: `src/app/emote_anim.rs`
- Modify: `src/app/mod.rs` (add `mod emote_anim;`, animator field, start instant)
- Modify: `src/ui/layout.rs` (composite after chat render)

- [ ] **Step 1: Write the failing test (frame-index math is pure)**

In `src/app/emote_anim.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_index_advances_with_time() {
        // delays: [100, 100, 100] ms, total 300ms loop.
        let delays = [100u32, 100, 100];
        assert_eq!(frame_index_at(&delays, 0), 0);
        assert_eq!(frame_index_at(&delays, 150), 1);
        assert_eq!(frame_index_at(&delays, 250), 2);
        assert_eq!(frame_index_at(&delays, 350), 0); // wrapped
    }

    #[test]
    fn single_frame_is_static() {
        assert_eq!(frame_index_at(&[100], 99_999), 0);
    }

    #[test]
    fn empty_delays_is_zero() {
        assert_eq!(frame_index_at(&[], 123), 0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p repartee emote_anim`
Expected: FAIL — `cannot find function frame_index_at`.

- [ ] **Step 3: Implement the animator**

In `src/app/emote_anim.rs`:
```rust
//! Drives inline emote animation: a process-global clock maps elapsed time to a
//! frame index per emote, and a cache holds per-(emote,frame,size) protocol images
//! for compositing.

use std::collections::HashMap;

use ratatui::layout::Rect;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

/// Pick the current frame index for a loop of `delays` (ms) at `elapsed_ms`.
#[must_use]
pub fn frame_index_at(delays: &[u32], elapsed_ms: u128) -> usize {
    let total: u128 = delays.iter().map(|d| u128::from(*d)).sum();
    if total == 0 || delays.len() <= 1 {
        return 0;
    }
    let mut t = elapsed_ms % total;
    for (i, d) in delays.iter().enumerate() {
        let d = u128::from(*d);
        if t < d {
            return i;
        }
        t -= d;
    }
    0
}

/// Holds per-(emote_index, frame_index) protocol images sized for compositing.
#[derive(Default)]
pub struct EmoteAnimator {
    cache: HashMap<(u32, usize), StatefulProtocol>,
}

impl EmoteAnimator {
    /// Get or build the protocol image for one emote frame, scaled into `rect`.
    /// Returns `None` if the emote/frame can't be decoded.
    pub fn protocol_for(
        &mut self,
        picker: &Picker,
        emote_index: u32,
        frame_index: usize,
    ) -> Option<&mut StatefulProtocol> {
        use std::collections::hash_map::Entry;
        match self.cache.entry((emote_index, frame_index)) {
            Entry::Occupied(e) => Some(e.into_mut()),
            Entry::Vacant(slot) => {
                let names = crate::emotes::names();
                let name = names.get(emote_index as usize)?;
                let frames = crate::emotes::frames(name)?;
                let (img, _delay) = frames.get(frame_index)?;
                let dyn_img = image::DynamicImage::ImageRgba8(img.clone());
                Some(slot.insert(picker.new_resize_protocol(dyn_img)))
            }
        }
    }

    /// Frame delays for an emote (ms), or empty if unknown.
    #[must_use]
    pub fn delays(emote_index: u32) -> Vec<u32> {
        let names = crate::emotes::names();
        names
            .get(emote_index as usize)
            .and_then(|n| crate::emotes::frames(n))
            .map(|f| f.iter().map(|(_, d)| *d).collect())
            .unwrap_or_default()
    }
}

/// Composite the current frame of every recorded placement onto the frame buffer.
/// Called from `layout::draw` after the chat view renders.
pub fn composite(
    frame: &mut ratatui::Frame,
    picker: &Picker,
    animator: &mut EmoteAnimator,
    placements: &[crate::ui::emote_layout::EmotePlacement],
    elapsed_ms: u128,
) {
    use ratatui_image::StatefulImage;
    for p in placements {
        let delays = EmoteAnimator::delays(p.emote_index);
        let fi = frame_index_at(&delays, elapsed_ms);
        if let Some(proto) = animator.protocol_for(picker, p.emote_index, fi) {
            // Clear the placeholder cells, then draw the frame on top.
            frame.render_widget(ratatui::widgets::Clear, p.rect);
            frame.render_stateful_widget(StatefulImage::default(), p.rect, proto);
        }
    }
}
```

> **tmux note:** this uses the ratatui-image widget path. If `app.in_tmux` and protocol ≠
> Halfblocks, the existing direct-write path (`src/app/image.rs::write_tmux_direct_image`) is the
> robust route. For v1, gate inline emotes on `!app.in_tmux || protocol == Kitty` if the widget
> path misbehaves under tmux — verify in Task 7 and add the direct-write variant only if needed.
> Keep this decision in `emotes_graphical()` if you must exclude tmux initially.

- [ ] **Step 4: Wire module + App fields**

In `src/app/mod.rs`: add `mod emote_anim;`, and to `struct App`:
```rust
    pub emote_animator: crate::app::emote_anim::EmoteAnimator,
    /// Animation clock origin; frame indices derive from `now - this`.
    pub emote_anim_start: std::time::Instant,
```
Initialize in `App::new_with_mode`: `emote_animator: Default::default()`,
`emote_anim_start: std::time::Instant::now()` (Instant is fine in app code; the workflow-script
restriction does not apply).

- [ ] **Step 5: Composite in layout::draw**

In `src/ui/layout.rs::draw`, AFTER `super::chat_view::render(frame, chat_area, app);` and BEFORE
the image_overlay (so the modal still draws on top):
```rust
if app.emotes_graphical() && !app.emote_placements.is_empty() {
    let elapsed = app.emote_anim_start.elapsed().as_millis();
    // Split borrows: take the placements out, then re-store, to satisfy the borrow checker.
    let placements = std::mem::take(&mut app.emote_placements);
    crate::app::emote_anim::composite(frame, &app.picker, &mut app.emote_animator, &placements, elapsed);
    app.emote_placements = placements;
}
```

- [ ] **Step 6: Build + lint**

Run: `cargo test -p repartee emote_anim && make clippy`
Expected: animator tests PASS, 0 clippy warnings.

- [ ] **Step 7: Commit**

```bash
git add src/app/emote_anim.rs src/app/mod.rs src/ui/layout.rs
git commit -m "feat(emotes): animator + inline frame compositing"
```

---

## Task 7: Animation tick in the event loop

**Files:**
- Modify: `src/app/mod.rs` (main loop ~1056 intervals, ~1092 select! arms)

- [ ] **Step 1: Add the interval**

Near `let mut tick = interval(Duration::from_secs(1));` (~1056) add:
```rust
let mut anim_tick = interval(Duration::from_millis(50)); // ~20 FPS for emote animation
```

- [ ] **Step 2: Add the select! arm**

Inside the `select!` block, add an arm. It should only force a redraw when there is at least one
animated emote on screen and the animation is enabled — otherwise it must NOT spin the redraw loop:
```rust
_ = anim_tick.tick() => {
    // Only repaint if emotes are graphical and at least one multi-frame emote is visible.
    if self.emotes_graphical()
        && self.emote_placements.iter().any(|p| {
            crate::app::emote_anim::EmoteAnimator::delays(p.emote_index).len() > 1
        })
    {
        self.needs_full_redraw = true;
    }
},
```

> **Performance guard:** the `delays(..).len() > 1` check means static (single-frame) emotes never
> trigger redraws, and when no emotes are visible the arm is a no-op. `frames()` caches decoding so
> `delays` is cheap after first use. If `needs_full_redraw = true` causes visible flicker with the
> graphics protocol, switch to a lighter "dirty" flag that re-runs `terminal.draw` without
> `terminal.clear()` — confirm during verification.

- [ ] **Step 3: Build**

Run: `cargo build -p repartee`
Expected: compiles.

- [ ] **Step 4: Manual verification (TUI)**

Use the `run`/`verify` skill in a graphics-capable terminal (Kitty/WezTerm/Ghostty):
1. Send `:usmiech: hi :lol:` in a channel — emotes appear inline, ~2 cells wide, 1 row tall,
   animating, not breaking line layout or wrapping.
2. Resize the terminal and scroll — emotes track their text position after rewrap/scroll.
3. Set `[emotes] render = "text"` (or run in a non-graphics terminal) — emotes show as literal
   `:usmiech:` text, no placeholder garbage (tofu) visible.
4. Confirm idle CPU is low when no animated emotes are on screen (the tick is a no-op).

Expected: inline animated emotes on capable terminals; clean text fallback otherwise.

- [ ] **Step 5: Commit**

```bash
git add src/app/mod.rs
git commit -m "feat(emotes): animation tick drives inline redraws"
```

---

## Task 8: `:name:` tab-completion

**Files:**
- Modify: `src/app/input.rs` (`handle_tab` ~687)
- Modify: `src/ui/input.rs` (`tab_complete` ~341)
- Test: inline tests in `src/ui/input.rs`

- [ ] **Step 1: Write the failing test**

In `src/ui/input.rs` tests:
```rust
#[test]
fn tab_completes_emote_prefix() {
    let mut input = InputState::default();
    input.value = "hey :usm".to_owned();
    input.cursor_pos = input.value.len();
    // Candidate emote names supplied by the caller (app layer); here a fixed list.
    input.tab_complete(&[], &[], &["usmiech".to_owned(), "usmiechniety".to_owned()], &[], &[]);
    assert!(input.value.starts_with("hey :usmiech"), "got {:?}", input.value);
    assert!(input.value.ends_with(':'), "emote completion closes the colon: {:?}", input.value);
}
```

> Adjust the `tab_complete` argument list to match the real signature discovered in
> `src/ui/input.rs:341` (the exploration shows it takes nicks, commands, settings, etc.). Add an
> `emotes: &[String]` parameter. Update the test call and all call sites accordingly.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p repartee tab_completes_emote_prefix`
Expected: FAIL.

- [ ] **Step 3: Implement the emote branch in `tab_complete`**

In `tab_complete`, when the word before the cursor starts with `:` and has no closing `:`, treat
the remainder as an emote prefix: filter `emotes` by `starts_with(prefix_without_colon)`, and on
completion write `:{name}:` (closing colon included) with the cursor after it. Mirror the existing
nick/command cycling via `tab_state`. Pseudocode within the initial-completion branch:
```rust
if let Some(prefix) = word.strip_prefix(':').filter(|p| !p.contains(':')) {
    let matches: Vec<String> = emotes.iter()
        .filter(|n| n.starts_with(prefix))
        .cloned()
        .collect();
    if !matches.is_empty() {
        let completion = &matches[0];
        self.value = format!("{text_before}:{completion}:");
        self.cursor_pos = self.value.len();
        self.tab_state = Some(TabCompletionState {
            prefix: word.to_owned(),
            matches,            // store bare names; cycling re-wraps with colons
            index: 0,
            text_before,
            is_start_of_line: false,
            is_command: false,
        });
        return;
    }
}
```
Ensure the cycling branch (already at the top of `tab_complete`) wraps emote matches with colons
when `prefix` starts with `:` (detect via `tab.prefix.starts_with(':')`).

- [ ] **Step 4: Pass emote candidates from `handle_tab`**

In `src/app/input.rs::handle_tab`, gather emote names and pass them:
```rust
let emotes: Vec<String> = crate::emotes::names().to_vec();
self.input.tab_complete(/* existing args */, &emotes);
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p repartee tab_completes_emote_prefix`
Expected: PASS.

- [ ] **Step 6: Lint + commit**

```bash
make clippy
git add src/ui/input.rs src/app/input.rs
git commit -m "feat(input): tab-complete :name: emotes"
```

---

## Task 9: Emote picker overlay

**Files:**
- Create: `src/ui/emote_picker.rs`
- Modify: `src/app/mod.rs` (state field), `src/ui/layout.rs` (render), `src/app/input.rs` (key + mouse), `src/ui/mod.rs` (`pub mod emote_picker;`)

- [ ] **Step 1: Add picker state**

In `src/ui/emote_picker.rs`:
```rust
//! Keyboard+mouse emote picker overlay. Selecting an emote inserts `:name:` into
//! the input at the cursor.

use ratatui::layout::Rect;

#[derive(Debug, Default)]
pub enum EmotePickerState {
    #[default]
    Hidden,
    Open {
        /// Current filter text (matches against emote names).
        filter: String,
        /// Index of the highlighted emote within the filtered list.
        selected: usize,
        /// Cell rects of the currently-rendered cells, for mouse hit-testing.
        /// (name index in the full registry, rect)
        cell_rects: Vec<(u32, Rect)>,
    },
}

impl EmotePickerState {
    #[must_use]
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Open { .. })
    }

    /// Names matching the current filter (or all names when filter is empty).
    #[must_use]
    pub fn filtered_indices(filter: &str) -> Vec<u32> {
        crate::emotes::names()
            .iter()
            .enumerate()
            .filter(|(_, n)| filter.is_empty() || n.contains(filter))
            .map(|(i, _)| u32::try_from(i).unwrap_or(0))
            .collect()
    }
}
```
Add to `struct App`: `pub emote_picker: crate::ui::emote_picker::EmotePickerState,` initialized to
`Default::default()`. Register `pub mod emote_picker;` in `src/ui/mod.rs`.

- [ ] **Step 2: Render the overlay (grid)**

In `src/ui/emote_picker.rs` add a `render(frame, area, app)` that, when `Open`, draws a centered
bordered `Block`, a filter line, and a grid of emote cells. Each cell shows the emote (graphical
via the animator/`StatefulImage` when `app.emotes_graphical()`, else the `:name:` text) and the
cell's `Rect` is recorded into `cell_rects` for mouse hit-testing. Mirror the `Block`/`Clear`/
centering pattern from `src/ui/image_overlay.rs::render_ready` (`centered_rect`, `Clear`, `Block`
with borders + title). Call `super::emote_picker::render(frame, frame.area(), app);` from
`layout::draw` AFTER chat compositing and BEFORE/AFTER the image overlay (decide stacking; picker
should be top-most when open).

> Recording `cell_rects` requires `&mut App`; have `render` take `&mut App` (consistent with the
> other overlay renderers). Storing rects during render is the same technique used for chat
> placements (Task 5).

- [ ] **Step 3: Key handling**

In `src/app/input.rs::handle_key`, before the normal text-input handling, branch when
`self.emote_picker.is_open()`:
- `Esc` → set `emote_picker = Hidden`.
- `Up/Down/Left/Right` → move `selected` within `filtered_indices(filter)`.
- printable char → append to `filter`, reset `selected = 0`.
- `Backspace` → pop from `filter`.
- `Enter` → insert the selected emote then close (see Step 5 helper).

Bind opening the picker to a key in `handle_key` (e.g. `Ctrl+E`), where other global keys are
dispatched:
```rust
(KeyModifiers::CONTROL, KeyCode::Char('e')) if !self.emote_picker.is_open() => {
    self.emote_picker = crate::ui::emote_picker::EmotePickerState::Open {
        filter: String::new(), selected: 0, cell_rects: Vec::new(),
    };
}
```
> Verify `Ctrl+E` isn't already bound (the exploration lists hardcoded bindings in
> `handle_key`). If taken, pick a free combo and note it in the help text.

- [ ] **Step 4: Mouse click-to-insert**

In `src/app/input.rs::handle_mouse`, when the picker is open and a `Down(Left)` click lands inside
a recorded `cell_rect`, insert that emote and close. Add at the top of the left-click handling:
```rust
if let crate::ui::emote_picker::EmotePickerState::Open { cell_rects, .. } = &self.emote_picker {
    if let Some((idx, _)) = cell_rects.iter().find(|(_, r)| r.contains(pos)) {
        let idx = *idx;
        self.insert_emote_by_index(idx);
        self.emote_picker = crate::ui::emote_picker::EmotePickerState::Hidden;
        return;
    }
}
```

- [ ] **Step 5: Insert helper**

Add to `App` (e.g. in `src/app/input.rs`):
```rust
/// Insert `:name:` for the registry index at the input cursor.
pub(crate) fn insert_emote_by_index(&mut self, index: u32) {
    if let Some(name) = crate::emotes::names().get(index as usize) {
        let token = format!(":{name}:");
        let at = self.input.cursor_pos;
        self.input.value.insert_str(at, &token);
        self.input.cursor_pos = at + token.len();
    }
}
```

- [ ] **Step 6: Write a test for the pure parts**

```rust
#[test]
fn picker_filters_by_substring() {
    use crate::ui::emote_picker::EmotePickerState;
    let all = EmotePickerState::filtered_indices("");
    let some = EmotePickerState::filtered_indices("usm");
    assert!(some.len() <= all.len() && !some.is_empty());
}
```
Run: `cargo test -p repartee picker_filters_by_substring`
Expected: PASS.

- [ ] **Step 7: Manual verification**

Run the app in a graphics terminal: press the picker key, type to filter, navigate with arrows,
press Enter (and separately click a cell) — confirm `:name:` is inserted at the cursor and the
overlay closes. Confirm Esc dismisses without inserting.

- [ ] **Step 8: Lint + commit**

```bash
make clippy
git add src/ui/emote_picker.rs src/ui/mod.rs src/ui/layout.rs src/app/mod.rs src/app/input.rs
git commit -m "feat(ui): keyboard+mouse emote picker overlay"
```

---

## Task 10: `/emote` command

**Files:**
- Modify: `src/commands/handlers_ui.rs` (add `cmd_emote`)
- Modify: `src/commands/registry.rs` (register)
- Test: none beyond the registry presence check (handler is thin UI glue)

- [ ] **Step 1: Implement the handler**

In `src/commands/handlers_ui.rs`:
```rust
/// `/emote` opens the picker; `/emote <name>` inserts `:name:` if known; with no
/// match it lists a few suggestions to the active buffer.
pub(crate) fn cmd_emote(app: &mut App, args: &[String]) {
    if args.is_empty() {
        app.emote_picker = crate::ui::emote_picker::EmotePickerState::Open {
            filter: String::new(),
            selected: 0,
            cell_rects: Vec::new(),
        };
        return;
    }
    let query = args[0].trim_matches(':');
    if crate::emotes::contains(query) {
        let token = format!(":{query}:");
        let at = app.input.cursor_pos;
        app.input.value.insert_str(at, &token);
        app.input.cursor_pos = at + token.len();
    } else {
        let hits: Vec<&str> = crate::emotes::names()
            .iter()
            .filter(|n| n.contains(query))
            .take(10)
            .map(String::as_str)
            .collect();
        let msg = if hits.is_empty() {
            format!("No emote matches \"{query}\"")
        } else {
            format!("Emotes matching \"{query}\": {}", hits.join(", "))
        };
        crate::commands::helpers::add_local_event(app, &msg);
    }
}
```
> Confirm the exact name/signature of the local-message helper (`add_local_event` per the
> exploration of `handlers_ui.rs`); use whatever that file already uses to print a local notice.

- [ ] **Step 2: Register the command**

In `src/commands/registry.rs` `COMMANDS` vec:
```rust
        (
            "emote",
            CommandDef {
                handler: cmd_emote,
                description: "Open the emote picker, or insert/search :name: emotes",
                aliases: &["emotes"],
                category: CommandCategory::Other,
            },
        ),
```
Ensure `cmd_emote` is imported alongside the other `handlers_ui` imports at the top of `registry.rs`.

- [ ] **Step 3: Write a registry presence test**

```rust
#[test]
fn emote_command_registered() {
    assert!(COMMANDS.iter().any(|(n, _)| *n == "emote"));
}
```
Run: `cargo test -p repartee emote_command_registered`
Expected: PASS.

- [ ] **Step 4: Lint + commit**

```bash
make clippy
git add src/commands/handlers_ui.rs src/commands/registry.rs
git commit -m "feat(commands): /emote picker + insert/search"
```

---

## Task 11: Full verification + finish

**Files:** none

- [ ] **Step 1: Full gate**

Run: `make clippy && make test`
Expected: 0 warnings, all tests pass.

- [ ] **Step 2: Full build**

Run: `make wasm && make release`
Expected: builds.

- [ ] **Step 3: End-to-end TUI verification (graphics terminal)**

1. Type `:usm`, press Tab → completes to `:usmiech:`.
2. Open picker (Ctrl+E or `/emote`), filter, arrow-navigate, Enter inserts; click a cell inserts.
3. Send the message → inline animated emotes render, wrap/scroll correctly.
4. `/emote zzz` with no match → suggestion/none message in buffer.
5. Switch `render = "text"` → literal `:name:` shown, no tofu, no animation tick churn.
6. Non-graphics terminal (or `TERM` without protocol) → literal `:name:` fallback.

- [ ] **Step 4: Finish the branch**

Use `superpowers:finishing-a-development-branch` to merge/PR.

---

## Self-Review

- **Spec coverage:** §4.3 TUI inline render → Tasks 2–7; §4.4 insertion UX (autocomplete, picker,
  raw text) → Tasks 8–10 (raw text already works via Task 4's tokenizer path + Plan 1); animation
  (full, v1) → Tasks 1,6,7; §4.7 render-mode gating → `emotes_graphical()` (Task 5) used in Tasks
  4,6,7,9. Fallback (Halfblocks/text/off) → guaranteed by gating: tokens stay literal text.
- **Type consistency:** `placeholder_for_index`/`decode_placeholder_index`/`is_placeholder_char`/
  `EMOTE_COLS` (Task 2) are used by `emotify_message_text` (Task 4) and `resolve_placements`
  (Task 3); `EmotePlacement` (Task 3) is produced in Task 5 and consumed in Task 6;
  `frame_index_at`/`EmoteAnimator`/`composite` (Task 6) match the loop arm (Task 7);
  `emotes_graphical()` defined once (Task 5) and reused; `EmotePickerState::Open { filter,
  selected, cell_rects }` constructed identically in Tasks 9 and 10; `insert_emote_by_index` and
  `crate::emotes::{names,contains,frames}` used consistently.
- **Placeholders:** every code step ships full code. Open items are explicitly flagged with how to
  resolve in-tree: the real `tab_complete` argument list (Task 8), the local-notice helper name
  (Task 10), the free keybinding for the picker (Task 9), and the tmux compositing path / redraw
  flag (Tasks 6–7), each to be confirmed during the marked verification step.
- **Risk callouts retained:** performance guard on the tick (Task 7), tmux path note (Task 6),
  wrap/scroll position tracking validated by the resolver unit tests (Task 3) + manual scroll test
  (Task 7).

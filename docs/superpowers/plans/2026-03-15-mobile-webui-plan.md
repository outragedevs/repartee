# Mobile Web UI Redevelopment Plan

> **For agentic workers:** REQUIRED: Use superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the mobile web UI to match the approved design spec — full viewport fit, working slide-out panels, proper top bar, inline chat layout, and touch gestures.

**Architecture:** CSS-only responsive changes (media queries) + minimal Leptos component updates. No protocol or server changes needed.

**Tech Stack:** Leptos 0.7 CSR (WASM), CSS custom properties, web-sys touch events

**Branch:** `feat/web-frontend`

---

## Project Context Feed

This plan continues work on the Repartee IRC client's web frontend. Here is everything an agent needs to know:

### Codebase Structure
- **Server-side** (Rust/tokio/axum): `src/web/` — protocol.rs, server.rs, ws.rs, snapshot.rs, broadcast.rs, auth.rs, tls.rs
- **Client-side** (Leptos 0.7 WASM): `web-ui/src/` — app.rs, state.rs, ws.rs, format.rs, protocol.rs
- **Components**: `web-ui/src/components/` — layout.rs, buffer_list.rs, chat_view.rs, input.rs, nick_list.rs, status_line.rs, topic_bar.rs, login.rs
- **Styles**: `web-ui/styles/base.css` (layout + mobile media queries), `web-ui/styles/themes.css` (5 themes)
- **Build**: `make wasm` (trunk build --release), `make release` (cargo build --release), `make clippy`, `make test`

### Key Patterns
- `AppState` derives `Copy` (all fields are `RwSignal<T>`)
- `ws::send_command()` uses thread_local CMD_TX (not Leptos context)
- `ws::connect()` creates fresh mpsc channel per connection
- Buffer list renders directly (no `<For>`) — recomputes all items on signal change
- Chat uses `display: flex; align-items: baseline;` with inline-block timestamp/nick/text
- Format parser in `format.rs` handles irssi %Z/%N colors + mIRC codes
- Tab completion in input.rs covers nicks, /commands, /set paths

### Current Mobile State (BROKEN)
The mobile layout (`<768px`) has these issues:
1. **Not fitting viewport** — `100vh` doesn't account for mobile browser chrome (URL bar, bottom bar)
2. **Slide-out panels don't work** — CSS animations exist but the Leptos conditionals (`left_open.get().then(...)`) may not render properly, or the z-index/position is wrong
3. **Top bar missing** — The `.mobile-topbar` div exists in layout.rs but the `@media` rules may be conflicting with `.desktop-only`/`.mobile-only` visibility

### Design Spec (APPROVED)
From `docs/superpowers/specs/2026-03-14-web-frontend-design.md` lines 188-236:

**Default View — Full-Width Chat:**
```
┌──────────────────────────┐
│ ☰  #rust (+nt) — Welc… 2 👥│  top bar
├──────────────────────────┤
│ 14:23 @ferris❯ Has any…  │  inline nicks
│ 14:24 alice❯ Yeah, it's… │
├──────────────────────────┤
│ [kofany|Act: 3,4,7]      │  compact status
│ [Message...          ] ⏎  │  input
└──────────────────────────┘
```

- Nicks inline (no right-aligned column)
- Topic merged into top bar as single line
- Top bar: ☰ hamburger | channel name (+modes) — topic snippet | mentions badge | 👥 nick list button
- Compact status: [nick|Act: 3,4,7] (no time, no lag, no channel modes)
- Slide-out buffer list (left): 220px, tap buffer → switch + auto-close
- Slide-out nick list (right): 180px, grouped by mode
- Swipe gestures: right edge → buffer list, left edge → nick list

### Build Commands
```bash
make wasm      # cd web-ui && trunk build --release
make release   # cargo build --release (embeds WASM)
make clippy    # cargo clippy -p repartee --all-targets
make test      # cargo test -p repartee
```

### Coding Standards
- Follow `/rust-best-practices` — `#[expect]` over `#[allow]`, no `unwrap()` in production, clippy pedantic clean
- Leptos: `use_context().unwrap()` is OK (programmer error if missing)
- CSS: use custom properties from themes.css, no hardcoded colors
- After ALL changes: `make wasm && make clippy && make test && make release`

---

## Task 1: Fix viewport fitting

The core issue — `100vh` on iOS/Android doesn't account for browser chrome (URL bar, bottom navigation). The app overflows the visible area.

**Files:**
- Modify: `web-ui/styles/base.css`
- Modify: `web-ui/index.html`

- [ ] **Step 1: Add viewport meta + CSS fix**

In `web-ui/index.html`, verify the viewport meta tag exists:
```html
<meta name="viewport" content="width=device-width, initial-scale=1.0, viewport-fit=cover" />
```

In `web-ui/styles/base.css`, replace `100vh` with `100dvh` (dynamic viewport height) which accounts for mobile browser chrome. Add fallback for older browsers:

```css
.app {
    display: flex;
    flex-direction: column;
    height: 100vh; /* fallback */
    height: 100dvh; /* dynamic viewport height — accounts for mobile browser chrome */
    overflow: hidden;
}
```

Also update `.mobile-only`:
```css
@media (max-width: 767px) {
    .mobile-only {
        display: flex;
        flex-direction: column;
        height: 100vh;
        height: 100dvh;
        overflow: hidden;
    }
}
```

- [ ] **Step 2: Verify**

Build with `make wasm` and test on mobile viewport (Chrome DevTools responsive mode or actual device). The app should fill exactly the visible area with no overflow.

- [ ] **Step 3: Commit**

```bash
git add web-ui/styles/base.css web-ui/index.html web-ui/dist/
git commit -m "fix(mobile): use 100dvh for proper viewport fitting on mobile"
```

---

## Task 2: Fix desktop/mobile visibility toggling

The `.desktop-only` and `.mobile-only` divs need clean mutual exclusion. Currently there may be conflicting rules.

**Files:**
- Modify: `web-ui/styles/base.css`

- [ ] **Step 1: Consolidate media queries**

Ensure there's exactly ONE block for each breakpoint. Remove duplicate `@media` blocks. The structure should be:

```css
/* Desktop: hide mobile elements */
@media (min-width: 768px) {
    .mobile-only { display: none !important; }
}

/* Mobile: hide desktop, show mobile */
@media (max-width: 767px) {
    .desktop-only { display: none !important; }
    .mobile-only {
        display: flex;
        flex-direction: column;
        height: 100vh;
        height: 100dvh;
        overflow: hidden;
    }
}
```

Remove any separate rules that hide `.mobile-topbar`, `.slide-overlay`, `.slide-panel-left`, `.slide-panel-right` on desktop — those elements only exist inside `.mobile-only` which is already hidden.

- [ ] **Step 2: Verify**

Test at 768px+ (desktop layout visible, mobile hidden) and <768px (mobile visible, desktop hidden). The top bar (☰ + channel name + 👥) should appear on mobile.

- [ ] **Step 3: Commit**

```bash
git add web-ui/styles/base.css web-ui/dist/
git commit -m "fix(mobile): consolidate media queries, clean desktop/mobile visibility"
```

---

## Task 3: Fix mobile top bar

The top bar should show: ☰ hamburger | channel name (+modes) — topic snippet | mentions badge | 👥

**Files:**
- Modify: `web-ui/src/components/layout.rs`
- Modify: `web-ui/styles/base.css`

- [ ] **Step 1: Update top bar content**

In `layout.rs`, the mobile topbar should show channel modes and topic:
```rust
<div class="mobile-topbar">
    <span class="hamburger" on:click=move |_| set_left_open.set(true)>"\u{2630}"</span>
    <div class="mobile-topbar-center">
        {move || active_buf().map(|b| {
            let modes = b.modes.as_deref()
                .filter(|m| !m.is_empty())
                .map(|m| format!(" (+{m})"))
                .unwrap_or_default();
            let topic = b.topic.as_deref().unwrap_or("");
            let topic_short = if topic.len() > 30 { &topic[..30] } else { topic };
            view! {
                <span class="mobile-chan">{b.name}{modes}</span>
                {(!topic.is_empty()).then(|| view! {
                    <span class="mobile-topic">" — "{topic_short}</span>
                })}
            }
        })}
    </div>
    <div class="mobile-topbar-right">
        {move || {
            let count = mention_count();
            (count > 0).then(|| view! {
                <span class="mention-badge">{count.to_string()}</span>
            })
        }}
        <span class="nicklist-btn" on:click=move |_| set_right_open.set(true)>
            "\u{1F465}"
        </span>
    </div>
</div>
```

- [ ] **Step 2: Style the top bar**

```css
.mobile-topbar {
    display: flex;
    background: var(--bg-alt);
    padding: 4px 8px;
    align-items: center;
    border-bottom: 1px solid var(--border);
    min-height: 28px;
    flex-shrink: 0;
}

.mobile-topbar .hamburger,
.mobile-topbar .nicklist-btn {
    color: var(--fg-muted);
    cursor: pointer;
    font-size: 16px;
    padding: 2px 4px;
    -webkit-tap-highlight-color: transparent;
}

.mobile-topbar-center {
    flex: 1;
    text-align: center;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    padding: 0 8px;
}

.mobile-topbar .mobile-chan {
    color: var(--accent);
    font-weight: bold;
    font-size: 13px;
}

.mobile-topbar .mobile-topic {
    color: var(--fg-muted);
    font-size: 11px;
}

.mobile-topbar-right {
    display: flex;
    gap: 6px;
    align-items: center;
    flex-shrink: 0;
}
```

- [ ] **Step 3: Build and verify**

`make wasm` — top bar should show on mobile with hamburger, channel+modes, topic, mentions, nick button.

- [ ] **Step 4: Commit**

```bash
git add web-ui/src/components/layout.rs web-ui/styles/base.css web-ui/dist/
git commit -m "fix(mobile): top bar with channel modes, topic, mentions badge"
```

---

## Task 4: Fix slide-out panels

The slide-out panels exist in the Leptos template but don't work. The issues are likely:
1. CSS `transform: translateX(-100%)` not animating because the panel is conditionally rendered (destroyed/recreated on each open)
2. The buffer list inside the panel may have conflicting CSS from the desktop `display: none` media query

**Files:**
- Modify: `web-ui/src/components/layout.rs`
- Modify: `web-ui/styles/base.css`

- [ ] **Step 1: Always render panels, toggle via CSS class**

Instead of conditional rendering (`left_open.get().then(...)`), always render both panels and toggle visibility via a CSS class. This allows CSS transitions to work:

```rust
// Left panel — always rendered, toggled via class
<div class="slide-overlay" class:visible=left_open on:click=move |_| set_left_open.set(false)></div>
<div class="slide-panel-left" class:open=left_open>
    <div class="slide-panel-header">
        <span style="color: var(--accent); font-weight: bold;">"Buffers"</span>
        {move || {
            let count = mention_count();
            (count > 0).then(|| view! {
                <span class="mention-badge">{format!("{count} mentions")}</span>
            })
        }}
    </div>
    <BufferList />
    <ThemePicker />
</div>

// Right panel
<div class="slide-overlay" class:visible=right_open on:click=move |_| set_right_open.set(false)></div>
<div class="slide-panel-right" class:open=right_open>
    <div class="slide-panel-header">
        {move || active_buf().map(|b| {
            view! {
                <span style="color: var(--accent); font-weight: bold;">{b.name}</span>
                <span style="color: var(--fg-muted); font-size: 10px; margin-left: 6px;">
                    {format!("{} users", b.nick_count)}
                </span>
            }
        })}
    </div>
    <NickList />
</div>
```

- [ ] **Step 2: Update CSS for always-rendered panels**

```css
/* Slide-out panels — always in DOM on mobile, toggled via .open class */
@media (max-width: 767px) {
    .slide-overlay {
        position: fixed;
        inset: 0;
        background: rgba(0, 0, 0, 0.5);
        z-index: 10;
        opacity: 0;
        pointer-events: none;
        transition: opacity 0.2s ease;
    }
    .slide-overlay.visible {
        opacity: 1;
        pointer-events: auto;
    }

    .slide-panel-left {
        position: fixed;
        left: 0; top: 0; bottom: 0;
        width: 220px;
        background: var(--bg-alt);
        border-right: 1px solid var(--border);
        z-index: 11;
        overflow-y: auto;
        transform: translateX(-100%);
        transition: transform 0.2s ease;
        display: flex;
        flex-direction: column;
    }
    .slide-panel-left.open { transform: translateX(0); }

    .slide-panel-right {
        position: fixed;
        right: 0; top: 0; bottom: 0;
        width: 180px;
        background: var(--bg-alt);
        border-left: 1px solid var(--border);
        z-index: 11;
        overflow-y: auto;
        transform: translateX(100%);
        transition: transform 0.2s ease;
    }
    .slide-panel-right.open { transform: translateX(0); }

    .slide-panel-header {
        padding: 8px 10px;
        border-bottom: 1px solid var(--border);
        display: flex;
        align-items: center;
        justify-content: space-between;
        flex-shrink: 0;
    }

    /* Override desktop buffer-list/nick-list styles inside panels */
    .slide-panel-left .buffer-list,
    .slide-panel-right .nick-list {
        display: block;
        width: auto;
        border: none;
    }
}
```

- [ ] **Step 3: Auto-close panel on buffer select**

In `buffer_list.rs`, after switching buffer, close the left panel. Since BufferList doesn't know about the panel signal, use a CSS/JS approach: add a data attribute or dispatch a custom event. The simplest approach: wrap the buffer click to also close the panel.

Actually, the cleanest approach: in layout.rs, watch `active_buffer` changes and close the left panel:
```rust
Effect::new(move || {
    let _ = state.active_buffer.get(); // track changes
    set_left_open.set(false);
});
```

- [ ] **Step 4: Build and verify**

`make wasm` — test hamburger opens left panel with slide animation, tap overlay closes it, tap buffer switches and closes panel. Test nick list button opens right panel.

- [ ] **Step 5: Commit**

```bash
git add web-ui/src/components/layout.rs web-ui/styles/base.css web-ui/dist/
git commit -m "fix(mobile): working slide-out panels with CSS transitions"
```

---

## Task 5: Fix mobile chat layout

On mobile, chat lines should be inline (no right-aligned nick column) to maximize horizontal space.

**Files:**
- Modify: `web-ui/styles/base.css`

- [ ] **Step 1: Override desktop chat layout for mobile**

```css
@media (max-width: 767px) {
    .chat-line {
        display: block;
        padding: 0 6px;
    }

    .chat-line .ts {
        display: inline;
        width: auto;
        margin-right: 4px;
    }

    .chat-line .nick {
        display: inline;
        width: auto;
        text-align: left;
    }

    .chat-line .text {
        display: inline;
        padding-left: 0;
    }

    /* Compact status bar on mobile — nick + activity only */
    .status-line { font-size: 11px; }
}
```

- [ ] **Step 2: Build and verify**

Messages should render as `14:23 @ferris❯ Has anyone tried...` all inline, wrapping naturally.

- [ ] **Step 3: Commit**

```bash
git add web-ui/styles/base.css web-ui/dist/
git commit -m "fix(mobile): inline chat layout, no nick column on small screens"
```

---

## Task 6: Add swipe gestures

Swipe from left edge opens buffer list, swipe from right edge opens nick list.

**Files:**
- Modify: `web-ui/src/components/layout.rs`

- [ ] **Step 1: Add touch event handlers**

In the mobile layout div, add touch start/move/end handlers:

```rust
let (touch_start_x, set_touch_start_x) = signal(0i32);
let (touch_start_y, set_touch_start_y) = signal(0i32);

let on_touch_start = move |ev: web_sys::TouchEvent| {
    if let Some(touch) = ev.touches().get(0) {
        set_touch_start_x.set(touch.client_x());
        set_touch_start_y.set(touch.client_y());
    }
};

let on_touch_end = move |ev: web_sys::TouchEvent| {
    let Some(touch) = ev.changed_touches().get(0) else { return };
    let dx = touch.client_x() - touch_start_x.get_untracked();
    let dy = touch.client_y() - touch_start_y.get_untracked();

    // Only horizontal swipes (dx > dy) with minimum 50px distance.
    if dx.abs() < 50 || dy.abs() > dx.abs() {
        return;
    }

    let start_x = touch_start_x.get_untracked();
    let screen_width = web_sys::window()
        .map(|w| w.inner_width().ok().and_then(|v| v.as_f64()).unwrap_or(800.0) as i32)
        .unwrap_or(800);

    if dx > 0 && start_x < 30 {
        // Swipe right from left edge → open buffer list
        set_left_open.set(true);
    } else if dx < 0 && start_x > screen_width - 30 {
        // Swipe left from right edge → open nick list
        set_right_open.set(true);
    }
};
```

Apply to the mobile-only div:
```rust
<div class="mobile-only"
    on:touchstart=on_touch_start
    on:touchend=on_touch_end
>
```

Add `"TouchEvent", "Touch", "TouchList"` to web-sys features in `web-ui/Cargo.toml`.

- [ ] **Step 2: Build and verify**

`make wasm` — swipe from left edge opens buffer list, swipe from right edge opens nick list.

- [ ] **Step 3: Commit**

```bash
git add web-ui/src/components/layout.rs web-ui/Cargo.toml web-ui/dist/
git commit -m "feat(mobile): swipe gestures for slide-out panels"
```

---

## Task 7: Final polish and testing

- [ ] **Step 1: Run full build pipeline**

```bash
make wasm && make clippy && make test && make release
```

- [ ] **Step 2: Test on real devices or emulators**

Test checklist:
- [ ] iPhone Safari: viewport fits, no scroll bounce
- [ ] Android Chrome: viewport fits, URL bar doesn't overlap
- [ ] ☰ hamburger opens left panel with smooth slide
- [ ] Tap buffer in left panel → switches + panel closes
- [ ] 👥 button opens right panel with smooth slide
- [ ] Tap overlay closes any open panel
- [ ] Swipe right from left edge → buffer list
- [ ] Swipe left from right edge → nick list
- [ ] Chat lines are inline (timestamp nick❯ text on one line)
- [ ] Top bar shows channel (+modes) — topic
- [ ] Status bar is compact (nick | Act: N)
- [ ] Input textarea works, no password popup
- [ ] Theme picker accessible in left panel
- [ ] Mentions badge shows in top bar

- [ ] **Step 3: Commit and push**

```bash
git add -A -- web-ui/ src/
git commit -m "fix(mobile): complete mobile web UI redevelopment"
git push outrage feat/web-frontend
```

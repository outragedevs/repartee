use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::state::{AppState, ScrollMode, is_history_drag, is_resize, mode_after_reader_scroll};

/// Distance in pixels from the absolute bottom that still counts as
/// "user is at bottom". Mirrors thelounge's value — generous enough
/// to absorb sub-pixel measurement noise but tight enough that the
/// user has to actively scroll up before stickiness flips off.
const SCROLL_THRESHOLD: f64 = 30.0;

/// `localStorage` key that turns on the scroll-state overlay.
///
/// This bug has never been observed on a real iPhone — every diagnosis so far,
/// including this module's, is inference from code plus a headless harness that
/// does not reproduce it (Linux WebKit has neither iOS text inflation nor its
/// visual viewport). If following the tail still fails on the device, the way to
/// find out why is to read what the device did, not to guess a fifth time.
fn scroll_debug_enabled() -> bool {
    // "<APP_NAME>-scroll-debug" — set it to "1" on the device to show the overlay.
    let key = crate::constants::storage_key("scroll-debug");
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|s| s.get_item(&key).ok().flatten())
        .is_some_and(|v| v == "1")
}

/// Record a scroll-mode transition, with the geometry at the moment it happened.
/// A no-op unless the overlay is switched on.
fn debug_log(state: AppState, why: &str) {
    if !scroll_debug_enabled() {
        return;
    }
    let geom = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.query_selector(".chat-messages").ok().flatten())
        .map_or_else(
            || "no scroller".to_string(),
            |el| {
                format!(
                    "top={} h={} ch={} fromBottom={}",
                    el.scroll_top(),
                    el.scroll_height(),
                    el.client_height(),
                    el.scroll_height() - el.scroll_top() - el.client_height(),
                )
            },
        );
    state.scroll_debug.update(|log| {
        log.push(format!("{why} | {geom}"));
        // Keep the tail: on a phone the interesting moments are the last few.
        if log.len() > 40 {
            log.remove(0);
        }
    });
}

/// Distance from the top (px) at which scrolling up triggers an on-demand
/// fetch of an older backlog page from the server. Generous so the page is
/// usually loaded before the user reaches the very top.
const BACKLOG_TRIGGER_PX: f64 = 400.0;

fn is_near_bottom(el: &web_sys::Element) -> bool {
    el.scroll_height() as f64 - el.scroll_top() as f64 - el.client_height() as f64
        <= SCROLL_THRESHOLD
}

fn viewport_needs_backlog(scroll_height: i32, client_height: i32) -> bool {
    client_height > 0 && scroll_height <= client_height
}

fn restored_scroll_top(current: i32, delta: f64) -> Option<i32> {
    #[allow(clippy::cast_possible_truncation)]
    let target = (f64::from(current) + delta).max(0.0) as i32;
    (target != current).then_some(target)
}

/// Hard-pin the scroller to the bottom.
fn pin_to_bottom(el: &web_sys::Element) {
    el.set_scroll_top(el.scroll_height());
}

/// First real (non-separator) chat line currently visible in `container`,
/// returned as `(data-mid, pixel offset from the container's top edge)`.
/// Used to anchor a backlog-prepend restore to a concrete element instead of
/// a distance from the bottom (the latter is corrupted by live appends).
///
/// `data-mid` is the `(id, log_id)` composite the `<For>` keys on — NOT the
/// transport id alone, which isn't unique across sources (a live message and a
/// backlog DB row can share a number), so an id-only selector could re-find the
/// wrong element after a prepend. Date separators are skipped by their
/// `date-separator` class, not by id: server-side (backlog-path) separators
/// carry nonzero ids, and a same-day prepend removes the seam separator — both
/// of which would break an id-based skip.
fn first_visible_anchor(container: &web_sys::Element) -> Option<(String, f64)> {
    let ctop = container.get_bounding_client_rect().top();
    let lines = container
        .query_selector_all(".chat-line[data-mid]:not(.date-separator)")
        .ok()?;
    for i in 0..lines.length() {
        let Some(node) = lines.item(i) else { continue };
        let Ok(elem) = node.dyn_into::<web_sys::Element>() else {
            continue;
        };
        let Some(mid) = elem.get_attribute("data-mid") else {
            continue;
        };
        let rect = elem.get_bounding_client_rect();
        // First line whose bottom edge is below the container's top is the
        // first (partially) visible one.
        if rect.bottom() > ctop {
            return Some((mid, rect.top() - ctop));
        }
    }
    None
}

/// Current pixel offset of the chat line with `data-mid == mid` from the
/// container's top edge, or `None` if that line is no longer rendered.
fn anchor_offset(container: &web_sys::Element, mid: &str) -> Option<f64> {
    let selector = format!(".chat-line[data-mid=\"{mid}\"]");
    let node = container.query_selector(&selector).ok()??;
    let elem = node.dyn_into::<web_sys::Element>().ok()?;
    let ctop = container.get_bounding_client_rect().top();
    Some(elem.get_bounding_client_rect().top() - ctop)
}

#[component]
pub fn ChatView() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();

    // Memoized so the chat-area branch closure below only re-runs when the
    // shell/non-shell verdict actually flips — NOT on every `state.buffers`
    // mutation (unread counts, activity, nick counts churn constantly from
    // traffic on other channels). A plain closure here subscribed the whole
    // chat subtree to `state.buffers`, recreating the entire `<For>` (and
    // every preview `<img>`) several times a second — the image flicker /
    // scroll-jump bug.
    let is_shell = Memo::new(move |_| {
        let Some(active_id) = state.active_buffer.get() else {
            return false;
        };
        state.buffers.with(|bufs| {
            bufs.iter()
                .find(|b| b.id == active_id)
                .is_some_and(|b| b.buffer_type == "shell")
        })
    });

    let messages = move || {
        let active_id = state.active_buffer.get()?;
        state.messages.with(|msgs| msgs.get(&active_id).cloned())
    };

    let chat_ref = NodeRef::<leptos::html::Div>::new();

    // Track previous buffer ID to detect buffer switches.
    let prev_buffer_id = StoredValue::new(None::<String>);

    // Identifies the scroll position requested by the latest programmatic
    // move. A later user gesture is ignored only when it lands at this exact
    // position, so a clamped write that emits no event cannot consume the
    // user's next real scroll.
    let pending_scroll_top = StoredValue::new(None::<i32>);

    // Coalesces multiple message appends in the same microtask into a
    // single RAF-scheduled pin. Without this, a burst of incoming
    // messages would queue N pins per tick.
    let pin_scheduled = StoredValue::new(false);

    // While scrolling up, holds `(buffer_id, anchor_mid, on-screen-offset)`
    // captured when a backlog fetch was sent: a concrete chat line (its
    // `data-mid`) and its pixel offset from the viewport top. After the older
    // page prepends (content added above), an Effect re-finds that same line
    // and restores `scrollTop` so it sits at the same on-screen offset. Keying
    // on a real element — not a distance from the bottom — keeps the view put
    // even when a live message appends at the bottom mid-fetch (a bottom append
    // grows `scrollHeight`, which a distance-from-bottom anchor would wrongly
    // absorb into the restore and jump the reader). Keyed by buffer so a fetch
    // that completes after the user switched away can't move the new buffer's
    // view. `None` = no fetch pending.
    let pending_anchor = StoredValue::new(None::<(String, String, f64)>);

    // Bumped on every buffer switch. A RAF or observer callback scheduled while
    // buffer A was active must not move the viewport after the reader has moved
    // on to B — it would scroll B to a position measured in A.
    let switch_generation = StoredValue::new(0_u64);

    // Armed by a mousedown in the scrollbar gutter (a classic scrollbar sits
    // between `clientWidth` and the border edge). Dragging the thumb or clicking
    // the track emits only scroll events — no wheel, touch, or key — so without
    // this flag those scrolls are indistinguishable from the browser's own and
    // `on_scroll` would ignore the one input path a mouse user has left.
    // Consumed by the first scroll it explains, cleared on mouseup and on buffer
    // switch (a release outside the window can leak one stale flag; the switch
    // reset and the consume-once semantics bound the damage to a single scroll).
    // Overlay scrollbars (macOS, mobile) have no gutter, so this never arms
    // there — behaviour is unchanged where the gutter doesn't exist.
    let scrollbar_grab = StoredValue::new(false);

    // Scrolls. That is all it does. It deliberately does NOT decide whether the
    // reader wants to be at the bottom — an earlier fix had it force
    // `is_at_bottom = true` from in here, which meant a function whose job is to
    // move the viewport was quietly overwriting the reader's intent. Intent lives
    // in `state.scroll_mode` and only a gesture changes it.
    let do_pin = move |el: &web_sys::Element| {
        let target = (el.scroll_height() - el.client_height()).max(0);
        if el.scroll_top() == target {
            pending_scroll_top.set_value(None);
            return;
        }
        pending_scroll_top.set_value(Some(target));
        pin_to_bottom(el);
    };

    // Every deferred pin funnels through here: it runs only if we are still
    // following the tail AND still in the buffer the callback was scheduled for.
    let pin_if_following = move |generation: u64| {
        if !state.scroll_mode.get_untracked().is_following_tail() {
            return;
        }
        if switch_generation.get_value() != generation {
            return;
        }
        let Some(el) = chat_ref.get_untracked() else {
            return;
        };
        do_pin(&web_sys::Element::from(el));
    };

    // Send an older-history `FetchMessages` for the active buffer using the
    // oldest loaded real message as the keyset cursor. Shared by the scroll-up
    // handler (`on_scroll`) and the viewport-fill Effect below. Always captures
    // a scroll anchor: when the response arrives with the user still at the
    // bottom the restore Effect drops it (its `is_at_bottom` guard), so it costs
    // nothing in the common fill case — but if a live append or preview-image
    // load grows the viewport mid-fetch and the user scrolls up before the page
    // lands, the anchor is what keeps the view on the line being read instead of
    // jumping. Returns `true` if a fetch was actually dispatched; `false` when a
    // guard (already fetching / no more history / window full / no cursor)
    // short-circuited it. Reads everything untracked so it is safe to call from
    // inside a RAF callback without creating reactive subscriptions.
    let trigger_backlog_fetch = move |el: &web_sys::Element| -> bool {
        let Some(id) = state.active_buffer.get_untracked() else {
            return false;
        };
        if state.backlog_fetching.with_untracked(|s| s.contains(&id)) {
            return false;
        }
        if state
            .backlog_has_more
            .with_untracked(|m| m.get(&id) == Some(&false))
        {
            return false;
        }
        // Window already full: stop paging until the user returns to the bottom
        // (which collapses + frees it). Without this, the prepend would be
        // trimmed straight back off the front and the same page re-requested.
        let len = state
            .messages
            .with_untracked(|m| m.get(&id).map_or(0, Vec::len));
        if len >= crate::state::PINNED_WEB_CAP {
            return false;
        }
        // Cursor = oldest real (non-separator) message's full-millisecond `@time`
        // (`ts_ms`), plus its `log_id` (the SQLite rowid) when it came from the
        // log. The server runs the subsecond keyset, so same-second
        // CHATHISTORY-backfilled rows are ordered by real time, not insertion id,
        // and are no longer skipped. At the live/DB boundary `log_id` is None, so
        // the server treats it as "strictly older millisecond". Never the
        // in-memory `id` counter (unrelated to rowids — sending it would corrupt
        // the keyset comparison). `ts_ms == 0` (field absent) falls back to
        // whole-seconds × 1000.
        let cursor = state.messages.with_untracked(|m| {
            m.get(&id).and_then(|v| {
                v.iter()
                    .find(|msg| {
                        msg.id != 0
                            && msg.event_key.as_deref() != Some("date_separator")
                            && msg.event_key.as_deref() != Some("backlog_end")
                    })
                    .map(|msg| {
                        let ms = if msg.ts_ms != 0 {
                            msg.ts_ms
                        } else {
                            msg.timestamp * 1000
                        };
                        (ms, msg.log_id)
                    })
            })
        });
        let Some((before, before_id)) = cursor else {
            return false;
        };
        // Remember the first visible real line and its on-screen offset so the
        // restore Effect can keep the view put once the page prepends — immune
        // to live appends at the bottom. If no real line is visible yet (e.g.
        // only separators), skip anchoring rather than fall back to a
        // bottom-relative measure.
        if let Some((mid, off)) = first_visible_anchor(el) {
            pending_anchor.set_value(Some((id.clone(), mid, off)));
        } else {
            pending_anchor.set_value(None);
        }
        state.backlog_fetching.update(|s| {
            s.insert(id.clone());
        });
        crate::ws::send_command(&crate::protocol::WebCommand::FetchMessages {
            buffer_id: id,
            limit: 100,
            before: Some(before),
            before_id,
        });
        true
    };

    // Buffer-switch: opening a buffer means "show me the end of this
    // conversation", so it re-enters `FollowingTail` and snaps to the bottom on
    // the next animation frame (giving the `<For>` a chance to render the new
    // buffer's messages first).
    //
    // Following the tail then persists with NO timeout. Everything that arrives
    // afterwards — a preview image three seconds later, the webfont swapping, iOS
    // inflating the font and re-wrapping every line — grows the content, and each
    // of those growths re-pins through the ResizeObserver below. A settle timer
    // ("stop pinning once the height has been stable for 250ms") was considered
    // and rejected: a stable height only means nothing has happened yet, not that
    // the layout is finished, and the late arrivals are exactly the ones that
    // knocked the view off the last line.
    Effect::new(move || {
        let active_id = state.active_buffer.get();
        let old_id = prev_buffer_id.get_value();
        let is_switch = old_id.as_deref() != active_id.as_deref();
        if let Some(ref id) = active_id {
            prev_buffer_id.set_value(Some(id.clone()));
        }
        if !is_switch {
            return;
        }
        // Collapse the buffer we just left so it doesn't keep a pinned (up to
        // PINNED_WEB_CAP) backlog window in memory forever.
        if let Some(old_id) = old_id {
            state.collapse_backlog(&old_id);
        }
        let generation = switch_generation.get_value().wrapping_add(1);
        switch_generation.set_value(generation);
        scrollbar_grab.set_value(false);
        state.scroll_mode.set(ScrollMode::FollowingTail);
        debug_log(state, "switch → FollowingTail");
        let Some(window) = web_sys::window() else {
            return;
        };
        let cb = wasm_bindgen::prelude::Closure::once(move || pin_if_following(generation));
        let _ = window.request_animation_frame(cb.as_ref().unchecked_ref());
        cb.forget();
    });

    // Re-pin on every message-list mutation while following the tail. Subscribes
    // to `state.messages` so any append (or backlog batch) triggers;
    // `pin_scheduled` debounces bursts so we pin at most once per animation
    // frame. RAF lets Leptos commit the DOM patch first, so we measure against
    // the final scrollHeight. Also subscribes to the appearance signals —
    // changing font size / line spacing reflows every line and would otherwise
    // leave the viewport mid-history (a visible snap on the next append).
    Effect::new(move || {
        state.messages.with(|_| ());
        let _ = state.active_buffer.get();
        let _ = state.font_size_override.get();
        let _ = state.line_height_override.get();
        let _ = state.line_height.get();
        if !state.scroll_mode.get().is_following_tail() {
            return;
        }
        if pin_scheduled.get_value() {
            return;
        }
        // Claim the debounce slot only once the frame is actually booked: setting
        // it before this `else` branch would latch it forever if scheduling ever
        // failed, and every future pin would be silently dropped.
        let Some(window) = web_sys::window() else {
            return;
        };
        pin_scheduled.set_value(true);
        let generation = switch_generation.get_value();
        let cb = wasm_bindgen::prelude::Closure::once(move || {
            pin_scheduled.set_value(false);
            pin_if_following(generation);
        });
        let _ = window.request_animation_frame(cb.as_ref().unchecked_ref());
        cb.forget();
    });

    // After a scroll-back prepend lands, restore the scroll position so the
    // view stays anchored on the same content (older lines were added above).
    // Subscribes to `backlog_fetching` (NOT `messages`): it fires once when the
    // fetch starts (still fetching → skip) and again when the response clears the
    // guard (restore). Keying on the guard — not the message list — means a live
    // NewMessage arriving mid-fetch can't consume the anchor early.
    Effect::new(move || {
        let active = state.active_buffer.get();
        let still_fetching = state
            .backlog_fetching
            .with(|s| active.as_deref().is_some_and(|id| s.contains(id)));
        if still_fetching {
            return;
        }
        let Some((anchor_buf, anchor_mid, anchor_off)) = pending_anchor.get_value() else {
            return;
        };
        // If the reader returned to the tail while the fetch was in flight (the
        // scroll-to-bottom button, or a buffer switch), the anchor is stale —
        // restoring it would yank them back up into the backlog. Drop it.
        if state.scroll_mode.get_untracked().is_following_tail() {
            pending_anchor.set_value(None);
            return;
        }
        // Only restore for the buffer the anchor was captured in. If the user
        // switched away before the fetch landed, drop it — restoring would move
        // the *new* buffer's view to a stale position.
        if active.as_deref() != Some(anchor_buf.as_str()) {
            pending_anchor.set_value(None);
            return;
        }
        pending_anchor.set_value(None);
        let Some(window) = web_sys::window() else {
            return;
        };
        let cb = wasm_bindgen::prelude::Closure::once(move || {
            let Some(el) = chat_ref.get() else { return };
            let el_dom: web_sys::Element = el.into();
            // Re-find the anchored line and shift scrollTop so it returns to the
            // same on-screen offset. The prepend pushed it down by the page's
            // height; any live append landed below it and so doesn't move it.
            let Some(new_off) = anchor_offset(&el_dom, &anchor_mid) else {
                return; // anchor trimmed away — leave the view as-is, don't jump
            };
            let delta = new_off - anchor_off;
            let Some(target) = restored_scroll_top(el_dom.scroll_top(), delta) else {
                return;
            };
            pending_scroll_top.set_value(Some(target));
            el_dom.set_scroll_top(target);
        });
        let _ = window.request_animation_frame(cb.as_ref().unchecked_ref());
        cb.forget();
    });

    // Viewport-fill: when the loaded page is shorter than the viewport, the
    // container never overflows, so no `scroll` event ever fires and the
    // scroll-up fetch in `on_scroll` can never be reached — the user is stuck
    // with whatever the initial load returned (the TUI seeds only
    // `display.backlog_lines`, default 20, which rarely fills a browser
    // window). This Effect closes that gap: after each page lands, if the
    // container still isn't scrollable and the server says more history exists,
    // it pulls the next older page. Subscribes to `messages` + `active_buffer`
    // so it re-checks after every prepend, looping (one page per render) until
    // the viewport overflows, `has_more` turns false, or the window cap is hit.
    // Gated on `FollowingTail`: once the reader scrolls up, the container is
    // overflowing by definition and `on_scroll` takes over.
    Effect::new(move || {
        state.messages.with(|_| ());
        let active = state.active_buffer.get();
        // Re-runs when a fetch completes (guard cleared) — skip while one is in
        // flight so we don't stack duplicate requests for the same page.
        let still_fetching = state
            .backlog_fetching
            .with(|s| active.as_deref().is_some_and(|id| s.contains(id)));
        if still_fetching {
            return;
        }
        if !state.scroll_mode.get_untracked().is_following_tail() {
            return;
        }
        let Some(id) = active else { return };
        if state
            .backlog_has_more
            .with_untracked(|m| m.get(&id) == Some(&false))
        {
            return;
        }
        let Some(window) = web_sys::window() else {
            return;
        };
        // RAF so Leptos has committed the latest prepend before we measure
        // scrollHeight against the viewport.
        let cb = wasm_bindgen::prelude::Closure::once(move || {
            if !state.scroll_mode.get_untracked().is_following_tail() {
                return;
            }
            let Some(el) = chat_ref.get() else { return };
            let el_dom: web_sys::Element = el.into();
            if !viewport_needs_backlog(el_dom.scroll_height(), el_dom.client_height()) {
                return;
            }
            trigger_backlog_fetch(&el_dom);
        });
        let _ = window.request_animation_frame(cb.as_ref().unchecked_ref());
        cb.forget();
    });

    // ResizeObserver on BOTH the scroll container and its content wrapper
    // (.chat-messages-inner), coalesced into the next RAF.
    //
    // The container's own box changes on:
    //   - desktop browser resize / mobile orientation change
    //   - Android Chrome keyboard open/close (layout viewport resizes)
    //   - mobile URL-bar collapse/expand
    //   - the input textarea auto-growing while the user types a long
    //     message (the bottom bar grows, this container shrinks) — the
    //     tester's "last line hides while typing on the phone" bug; a
    //     window.resize listener never sees this one.
    //
    // The container is `flex:1` + `overflow-y:auto` inside a bounded flex
    // column, so CONTENT growth never changes its box — only scrollHeight.
    // Async image-preview decode grows the content after the pin, which is
    // invisible to a container-only observer: that was the "switching
    // channels / loading previews doesn't stick to the last line" bug. The
    // inner wrapper's height IS the content height, so observing it too
    // catches every post-pin growth (image decode, font swap, late DOM).
    // iOS Safari's keyboard overlays the visual viewport without resizing
    // the layout viewport, so nothing fires here — and nothing needs to:
    // the container geometry is unchanged and the browser pans the focused
    // input into view itself. (A VisualViewport listener was tried and
    // reverted: it fired mid-animation and produced visible hops.)
    //
    // This is the mechanism that absorbs everything that lands AFTER the
    // buffer-switch pin — a preview image seconds later, the webfont swapping,
    // iOS re-wrapping every line — for as long as the reader is following the
    // tail. It never re-measures geometry to decide whether to act: geometry is
    // exactly what a browser-initiated scroll corrupts. It acts on intent.
    type ObserverHandle = Option<(
        web_sys::ResizeObserver,
        wasm_bindgen::prelude::Closure<dyn Fn()>,
        web_sys::Element,
    )>;
    let observer_handle: StoredValue<ObserverHandle, leptos::prelude::LocalStorage> =
        StoredValue::new_local(None);
    let resize_throttle = StoredValue::new(false);
    Effect::new(move || {
        let Some(el) = chat_ref.get() else { return };
        let el_dom: web_sys::Element = el.into();
        // Keyed on the OBSERVED ELEMENT, not a "registered" boolean: the
        // shell branch swaps this whole subtree out and back, re-creating
        // the chat div — a boolean flag would leave the observer bound to
        // the detached old node and silently kill resize re-pinning after
        // the first /shell round-trip.
        let already_observing = observer_handle.with_value(|h| {
            h.as_ref()
                .is_some_and(|(_, _, observed)| observed == &el_dom)
        });
        if already_observing {
            return;
        }
        if let Some((old_observer, old_cb, _)) =
            observer_handle.try_update_value(Option::take).flatten()
        {
            old_observer.disconnect();
            drop(old_cb);
        }
        let cb = wasm_bindgen::prelude::Closure::<dyn Fn()>::new(move || {
            if resize_throttle.get_value() {
                return;
            }
            // Same reasoning as `pin_scheduled`: claim the throttle only once the
            // frame is booked, or a failure here latches it and kills every
            // future resize re-pin.
            let Some(window) = web_sys::window() else {
                return;
            };
            resize_throttle.set_value(true);
            let generation = switch_generation.get_value();
            let raf_cb = wasm_bindgen::prelude::Closure::once(move || {
                resize_throttle.set_value(false);
                if !state.scroll_mode.get_untracked().is_following_tail() {
                    return;
                }
                if switch_generation.get_value() != generation {
                    return;
                }
                let Some(el) = chat_ref.get_untracked() else {
                    return;
                };
                let el_dom: web_sys::Element = el.into();
                do_pin(&el_dom);
                if viewport_needs_backlog(el_dom.scroll_height(), el_dom.client_height()) {
                    trigger_backlog_fetch(&el_dom);
                }
            });
            let _ = window.request_animation_frame(raf_cb.as_ref().unchecked_ref());
            raf_cb.forget();
        });
        let Ok(observer) = web_sys::ResizeObserver::new(cb.as_ref().unchecked_ref()) else {
            return;
        };
        observer.observe(&el_dom);
        // The content wrapper is part of the same template as the container,
        // so it exists by the time the node_ref resolves. Select by class,
        // not positionally — a future first child (sticky header, sentinel)
        // must not silently steal the content-growth observation.
        if let Some(inner) = el_dom.query_selector(".chat-messages-inner").ok().flatten() {
            observer.observe(&inner);
        }
        observer_handle.set_value(Some((observer, cb, el_dom)));
    });

    on_cleanup(move || {
        let handle = observer_handle.try_update_value(Option::take).flatten();
        let Some((observer, cb, _)) = handle else {
            return;
        };
        observer.disconnect();
        drop(cb);
    });

    // Last `clientHeight` seen by a scroll event. A container that shrinks (the
    // composer growing as you type, the keyboard sliding up, the URL bar
    // collapsing, an orientation change) makes the browser clamp `scrollTop` and
    // emit a scroll event that arrives BEFORE the ResizeObserver notification.
    // Running the usual distance-from-bottom maths on those values — new
    // clientHeight against a stale scrollTop — crosses the threshold and reports
    // a reader who has scrolled away when nobody touched anything. The resize
    // path owns the correction in that case. (Same guard lurker uses, from
    // stackblitz-labs/use-stick-to-bottom.)
    let last_client_height = StoredValue::new(None::<i32>);

    // A scroll event is evidence of nothing. The browser emits them when it
    // clamps a scroll position after we swap a buffer's DOM, when the container
    // resizes, when content is prepended — none of which is the reader deciding
    // to read back. So while we are following the tail this handler does not
    // touch the mode and does not fetch backlog; the gesture handlers below are
    // the only things that can end `FollowingTail`.
    let on_scroll = move |ev: web_sys::Event| {
        let target = ev.target().unwrap();
        let el: &web_sys::Element = target.unchecked_ref();
        let requested = pending_scroll_top.get_value();
        pending_scroll_top.set_value(None);
        if requested == Some(el.scroll_top()) {
            return;
        }

        let client_height = el.client_height();
        let resized = is_resize(last_client_height.get_value(), client_height);
        last_client_height.set_value(Some(client_height));

        if state.scroll_mode.get_untracked().is_following_tail() {
            // One exception to "scroll events mean nothing here": a scroll that
            // arrives under an armed scrollbar grab IS the reader — thumb drags
            // and track clicks produce no wheel/touch/key event, only this.
            if !scrollbar_grab.get_value() {
                return;
            }
            scrollbar_grab.set_value(false);
            state.scroll_mode.set(ScrollMode::ReadingHistory);
            debug_log(state, "scrollbar drag → ReadingHistory");
            // Fall through: the geometry below decides whether the drag actually
            // left the bottom (a track click that lands back near the tail flips
            // straight back to FollowingTail, same as any reader scroll).
        }
        if resized {
            return;
        }

        // ReadingHistory: the reader owns the viewport, so geometry decides
        // whether they have come back to the live tail.
        let next = mode_after_reader_scroll(is_near_bottom(el));
        if next.is_following_tail() {
            state.scroll_mode.set(next);
            debug_log(state, "scrolled back to bottom → FollowingTail");
            // Collapse the loaded backlog window (free the older lines, re-arm
            // scroll-up).
            if let Some(id) = state.active_buffer.get_untracked() {
                state.collapse_backlog(&id);
            }
        }
        // Near the top: pull an older page from the server (the shared helper
        // applies the in-flight / no-more-history / window-full guards, and
        // anchors the restore to the first visible line so the prepend keeps the
        // view put).
        if f64::from(el.scroll_top()) < BACKLOG_TRIGGER_PX {
            trigger_backlog_fetch(el);
        }
    };

    // ---- The only things that may end `FollowingTail`: the reader's own hands.

    let start_reading = move |why: &'static str| {
        if !state.scroll_mode.get_untracked().is_following_tail() {
            return;
        }
        // A gesture only means "reading history" if there is history to move
        // into. On a buffer too short to overflow nothing can scroll, so no
        // scroll event would ever run the near-bottom correction that restores
        // `FollowingTail` — the mode would stick (arrow up, pinning dead) until
        // the reader pressed the jump button.
        let Some(el) = chat_ref.get_untracked() else {
            return;
        };
        let el: web_sys::Element = el.into();
        if el.scroll_height() <= el.client_height() {
            return;
        }
        state.scroll_mode.set(ScrollMode::ReadingHistory);
        debug_log(state, why);
    };

    // Upward wheel/trackpad. Handled synchronously rather than waiting for the
    // scroll event it produces: a message arriving in between would otherwise be
    // pinned on top of a reader who has already started scrolling away.
    let on_wheel = move |ev: web_sys::WheelEvent| {
        if ev.delta_y() < 0.0 {
            start_reading("wheel up → ReadingHistory");
        }
    };

    // Touch. `touchstart` is NOT intent — it also fires on tapping a link, a
    // nick, a preview's dismiss button, or starting a text selection. Only a
    // drag that travels far enough *and toward history* (the finger moving down,
    // revealing older lines) is the reader taking the viewport; a drag toward the
    // already-pinned bottom is ignored — see `is_history_drag`. Nothing here calls
    // `preventDefault`, so native momentum scrolling is untouched.
    let touch_origin = StoredValue::new(None::<f64>);
    let on_touch_start = move |ev: web_sys::TouchEvent| {
        touch_origin.set_value(ev.touches().get(0).map(|t| f64::from(t.client_y())));
    };
    let on_touch_move = move |ev: web_sys::TouchEvent| {
        let (Some(start_y), Some(touch)) = (touch_origin.get_value(), ev.touches().get(0)) else {
            return;
        };
        if is_history_drag(start_y, f64::from(touch.client_y())) {
            touch_origin.set_value(None);
            start_reading("touch drag → ReadingHistory");
        }
    };

    // Mouse, for the scrollbar only. A press whose x falls past `clientWidth`
    // (measured from the padding edge) is on the classic scrollbar gutter; arm
    // the flag `on_scroll` consumes above. A press on the content arms nothing —
    // text selection and link clicks stay inert, exactly like a tap. `offsetX`
    // can't be used directly: it is relative to the event *target*, which for
    // content clicks is some inner span, not the scroller.
    let on_mouse_down = move |ev: web_sys::MouseEvent| {
        let Some(el) = chat_ref.get_untracked() else {
            return;
        };
        let el: web_sys::Element = el.into();
        let x = f64::from(ev.client_x()) - el.get_bounding_client_rect().left();
        scrollbar_grab.set_value(x >= f64::from(el.client_left() + el.client_width()));
    };
    let on_mouse_up = move |_: web_sys::MouseEvent| {
        scrollbar_grab.set_value(false);
    };

    // Keyboard scrolling, when the container has focus.
    let on_key_down = move |ev: web_sys::KeyboardEvent| {
        if matches!(ev.key().as_str(), "PageUp" | "Home" | "ArrowUp") {
            start_reading("key up → ReadingHistory");
        }
    };

    // Custom copy handler: the browser's default copy uses `innerText`,
    // which inserts a `\n` between every block-level box — and CSS Flex
    // promotes each flex item to block-level. Our `.chat-line` is
    // `display: flex` with three child spans (ts, nick, text), so a
    // selection that crosses those spans pastes as three lines split
    // by `\n` instead of `ts nick text` on one line. The TUI doesn't
    // hit this because terminal text is literally one line per row.
    // We intercept and rebuild each affected `.chat-line` as
    // space-separated text; lines stay separated by `\n` as expected.
    // The guard `if !raw.contains('\n')` skips the override for
    // partial selections within a single span (where the default is
    // already correct).
    let on_copy = move |ev: web_sys::Event| {
        let Some(clip_ev) = ev.dyn_ref::<web_sys::ClipboardEvent>() else {
            return;
        };
        let Some(window) = web_sys::window() else {
            return;
        };
        let Ok(Some(selection)) = window.get_selection() else {
            return;
        };
        if selection.is_collapsed() {
            return;
        }
        let raw_js = selection.to_string();
        let raw: String = raw_js.into();
        if !raw.contains('\n') {
            return;
        }
        let Some(doc) = window.document() else { return };
        let Ok(chat_lines) = doc.query_selector_all(".chat-line") else {
            return;
        };
        let mut out: Vec<String> = Vec::with_capacity(chat_lines.length() as usize);
        for i in 0..chat_lines.length() {
            let Some(node) = chat_lines.item(i) else {
                continue;
            };
            let in_selection = selection
                .contains_node_with_allow_partial_containment(&node, true)
                .unwrap_or(false);
            if !in_selection {
                continue;
            }
            if let Some(line) = format_chat_line_for_copy(&node)
                && !line.is_empty()
            {
                out.push(line);
            }
        }
        if out.is_empty() {
            return;
        }
        let formatted = out.join("\n");
        let Some(clipboard) = clip_ev.clipboard_data() else {
            return;
        };
        if clipboard.set_data("text/plain", &formatted).is_ok() {
            clip_ev.prevent_default();
        }
    };

    view! {
        <div class="chat-area">
            {move || {
                if is_shell.get() {
                    return view! { <super::shell_view::ShellView /> }.into_any();
                }
                view! {
            <div class="chat-messages-outer">
                <div
                    class="chat-messages"
                    node_ref=chat_ref
                    // Focusable so PageUp/Home/ArrowUp reach `on_key_down`: a plain
                    // overflow container takes no keyboard focus, and since
                    // `on_scroll` ignores geometry while following the tail, this
                    // handler is the only way a keyboard user leaves it. Touch/mouse
                    // focus shows no ring (`:focus-visible` gates it to keyboard).
                    tabindex="0"
                    on:scroll=on_scroll
                    on:copy=on_copy
                    on:wheel=on_wheel
                    on:touchstart=on_touch_start
                    on:touchmove=on_touch_move
                    on:mousedown=on_mouse_down
                    on:mouseup=on_mouse_up
                    on:keydown=on_key_down
                >
                    <div class="chat-messages-inner">
                        <For
                            each=move || messages().unwrap_or_default()
                            // Date-separator rows use `id == 0` (see
                            // state.rs:168 — "reserved for date separators
                            // and is not unique"), so keying by `msg.id`
                            // alone would collide across every separator
                            // and let Leptos reuse one DOM node for all of
                            // them. Use the timestamp as the discriminator
                            // for separators. Real messages key by
                            // `(id, log_id)`: the transport `id` alone is not
                            // unique across sources — a live message
                            // (log_id None) and a backlog DB row (log_id Some)
                            // can share a numeric id, and keying on id alone
                            // would collide them into one reused DOM node.
                            key=|msg| (
                                msg.id,
                                msg.log_id,
                                if msg.id == 0 { msg.timestamp } else { 0 },
                            )
                            children=move |msg| render_message(state, msg)
                        />
                    </div>
                </div>
                <button type="button" class="scroll-bottom-btn" aria-label="Jump to latest message"
                    class:hidden=move || state.scroll_mode.get().is_following_tail()
                    on:click=move |_| {
                        state.scroll_mode.set(ScrollMode::FollowingTail);
                        debug_log(state, "jump-to-bottom → FollowingTail");
                        // Mirror the `on_scroll` return-to-bottom path: collapse
                        // the pinned backlog window. `do_pin` sets
                        // `pending_scroll_top`, so the resulting scroll event
                        // returns early and never reaches that collapse — without
                        // this, a buffer loaded to PINNED_WEB_CAP stays full and
                        // the scroll-up fetch guard blocks deeper history.
                        if let Some(id) = state.active_buffer.get_untracked() {
                            state.collapse_backlog(&id);
                        }
                        if let Some(el) = chat_ref.get() {
                            let el_dom: web_sys::Element = el.into();
                            do_pin(&el_dom);
                        }
                    }
                >
                    "\u{25BC}"
                </button>
                <Show when=scroll_debug_enabled>
                    <div class="scroll-debug">
                        <div class="scroll-debug-head">
                            {move || {
                                let mode = if state.scroll_mode.get().is_following_tail() {
                                    "FollowingTail"
                                } else {
                                    "ReadingHistory"
                                };
                                let buf = state.active_buffer.get().unwrap_or_default();
                                format!("{mode} | {buf}")
                            }}
                        </div>
                        <For
                            // Newest first: the overlay is capped in height and
                            // `pointer-events: none`, so entries past the fold are
                            // unreachable. The freshest transition — the whole point
                            // on a phone with no debugger — must sit at the top.
                            each=move || {
                                state
                                    .scroll_debug
                                    .get()
                                    .into_iter()
                                    .enumerate()
                                    .rev()
                                    .collect::<Vec<_>>()
                            }
                            key=|(i, line)| (*i, line.clone())
                            children=|(_, line)| view! { <div>{line}</div> }
                        />
                    </div>
                </Show>
            </div>
                }.into_any()
            }}
        </div>
    }
}

/// Look up the local user's nick for the currently active buffer.
/// Reads signals untracked — called from inside the `<For>` children
/// closure where re-running on connection-meta changes would defeat the
/// keyed render. Buffer switches/SyncInits already recreate everything.
fn current_nick(state: AppState) -> Option<String> {
    let active_id = state.active_buffer.get_untracked()?;
    let bufs = state.buffers.get_untracked();
    let buf = bufs.iter().find(|b| b.id == active_id)?;
    let conns = state.connections.get_untracked();
    let conn = conns.iter().find(|c| c.id == buf.connection_id)?;
    Some(conn.nick.clone())
}

/// Render one chat line.
///
/// Static (snapshot at first render): msg-type-derived `line_class`,
/// `is_own` (would change only on /nick), event arrow, styled text.
///
/// Reactive (wrapped in `move ||` so the specific DOM node updates
/// in-place when the underlying signal fires):
///   - timestamp text (depends on `timestamp_format`)
///   - nick truncation (depends on `nick_max_length`)
///   - nick column width style (depends on `nick_column_width`)
///   - nick color style (depends on `nick_colors_enabled` +
///     `nick_color_saturation` + `nick_color_lightness`)
///   - preview block (depends on `dismissed_previews` — so dismissing
///     a thumbnail makes it disappear without rebuilding the line)
///
/// All signal subscriptions are scoped to this one message's elements,
/// so an attribute change updates only the elements it touches, not
/// the 1000-line list. New-message appends create exactly one new
/// child (via the keyed `<For>`) — that's the headline win over the
/// old `.iter().map().collect()` pattern.
#[expect(
    clippy::too_many_lines,
    reason = "linear per-message branch dispatch; splitting per branch would obscure the shared layout"
)]
fn render_message(state: AppState, msg: crate::protocol::WireMessage) -> AnyView {
    let nick_self = current_nick(state);
    let emotes_on = state.emotes_enabled.get();

    // `data-mid` lets the backlog-prepend restore re-find this exact line. It
    // must be the same `(id, log_id)` identity the `<For>` keys on — the
    // transport id alone isn't unique (a live message and a backlog DB row can
    // share a number), so an id-only attribute could match the wrong element.
    // Separators are excluded from anchoring by their class, not this value.
    let mid = match msg.log_id {
        Some(log) => format!("{}:{log}", msg.id),
        None => format!("{}:n", msg.id),
    };

    let is_mention_log = msg.msg_type == "mention_log";
    let is_event = msg.msg_type == "event";
    let is_action = msg.msg_type == "action";
    let is_notice = msg.msg_type == "notice";
    let is_separator = is_event && msg.nick.is_none() && msg.text.starts_with('\u{2500}');

    let is_own = nick_self
        .as_ref()
        .is_some_and(|our| msg.nick.as_deref() == Some(our.as_str()));

    let line_class = if is_separator {
        "chat-line date-separator"
    } else if is_mention_log {
        "chat-line mention-log"
    } else if msg.highlight && msg.nick.is_some() {
        if is_own {
            "chat-line mention own"
        } else {
            "chat-line mention"
        }
    } else if is_event {
        match msg.event_key.as_deref() {
            Some("join" | "connected") => "chat-line event join-event",
            Some("part" | "quit" | "disconnected") => "chat-line event part-event",
            Some("kick") => "chat-line event kick-event",
            Some("kicked") => "chat-line event kicked-event",
            Some("nick_change" | "chghost" | "account") => "chat-line event nick-event",
            Some("topic_changed") => "chat-line event topic-event",
            Some("mode") => "chat-line event mode-event",
            _ => "chat-line event",
        }
    } else if is_notice {
        "chat-line notice"
    } else if is_action {
        "chat-line event action"
    } else if is_own {
        "chat-line own"
    } else {
        "chat-line"
    };

    if is_separator {
        return view! {
            <div class=line_class data-mid=mid>
                <span class="separator-text">{msg.text}</span>
            </div>
        }
        .into_any();
    }

    // Reactive timestamp: re-runs only when `timestamp_format` changes.
    let timestamp = msg.timestamp;
    let ts_fn = move || {
        let fmt = state.timestamp_format.get();
        format_timestamp(timestamp, &fmt)
    };

    // Reactive previews subtree: re-runs only when `dismissed_previews`
    // changes, so clicking the × on one thumbnail visibly removes that
    // thumbnail (and only re-renders this one message's preview list).
    let msg_id = msg.id;
    let preview_data = msg.previews.clone();
    let previews_view = move || render_previews(state, msg_id, preview_data.clone());

    if is_mention_log {
        let styled = render_styled_text(&msg.text, emotes_on);
        return view! {
            <>
                <div class=line_class data-mid=mid>
                    <span class="mention-log-text">{styled}</span>
                </div>
                {previews_view}
            </>
        }
        .into_any();
    }

    if is_action {
        let nick_text = msg.nick.unwrap_or_default();
        let styled = render_styled_text(&msg.text, emotes_on);
        let nick_color_style = {
            let nick = nick_text.clone();
            move || nick_color_or_empty(state, &nick, !is_own)
        };
        let on_nick_click = mention_on_click(state, nick_text.clone());
        let on_nick_keydown = mention_on_keydown(state, nick_text.clone());
        let nick_label = format!("Mention {nick_text}");
        view! {
            <>
                <div class=line_class data-mid=mid>
                    <span class="ts">{ts_fn}</span>
                    <span class="action-body">
                        "* "
                        <span class="action-nick" style=nick_color_style role="button" tabindex="0"
                            aria-label=nick_label
                            on:keydown=on_nick_keydown
                            on:click=on_nick_click>{nick_text}</span>
                        " "
                        {styled}
                    </span>
                </div>
                {previews_view}
            </>
        }
        .into_any()
    } else if is_notice {
        let nick_text = msg.nick.unwrap_or_default();
        let styled = render_styled_text(&msg.text, emotes_on);
        view! {
            <>
                <div class=line_class data-mid=mid>
                    <span class="ts">{ts_fn}</span>
                    <span class="notice-body">
                        "-"
                        <span class="notice-nick">{nick_text}</span>
                        "- "
                        {styled}
                    </span>
                </div>
                {previews_view}
            </>
        }
        .into_any()
    } else if is_event {
        let arrow = event_icon(msg.event_key.as_deref(), &msg.text);
        let styled = render_styled_text(&msg.text, emotes_on);
        view! {
            <div class=line_class data-mid=mid>
                <span class="ts">{ts_fn}</span>
                <span>
                    {arrow.map(|(symbol, css_class)| view! {
                        <span class=css_class>{symbol}</span>
                    })}
                    {styled}
                </span>
            </div>
        }
        .into_any()
    } else {
        let nick_text = msg.nick.unwrap_or_default();
        let mode = msg.nick_mode.unwrap_or_default();
        let styled = render_styled_text(&msg.text, emotes_on);
        let highlight = msg.highlight;

        let nick_truncated = {
            let nick = nick_text.clone();
            let mode = mode.clone();
            move || {
                let max_len = state.nick_max_length.get() as usize;
                truncate_nick(&nick, max_len, &mode)
            }
        };
        let nick_style = move || format!("width: {}ch;", state.nick_column_width.get());
        let nick_color_style = {
            let nick = nick_text.clone();
            move || nick_color_or_empty(state, &nick, !is_own && !highlight)
        };
        let on_nick_click = mention_on_click(state, nick_text.clone());
        let on_nick_keydown = mention_on_keydown(state, nick_text.clone());

        view! {
            <>
                <div class=line_class data-mid=mid>
                    <span class="ts">{ts_fn}</span>
                    <span class="nick" style=nick_style>
                        <span class="mode">{mode}</span>
                        <span class="name" style=nick_color_style role="button" tabindex="0"
                            aria-label=format!("Mention {nick_text}")
                            on:keydown=on_nick_keydown
                            on:click=on_nick_click>{nick_truncated}</span>
                        <span class="sep">"❯"</span>
                    </span>
                    <span class="text">{styled}</span>
                </div>
                {previews_view}
            </>
        }
        .into_any()
    }
}

/// Click handler for a nick in the chat log: queue the nick as a mention for
/// the input (which picks the `nick: ` / `nick ` delimiter by caret
/// context). Selecting the nick to copy it must NOT also insert it, and a
/// double-click's FIRST click still fires with a collapsed selection — so the
/// insert is deferred past the double-click window and the selection is
/// re-checked at commit time (later clicks of a multi-click bail immediately
/// via `detail()`).
fn mention_on_click(state: AppState, nick: String) -> impl Fn(web_sys::MouseEvent) + Clone {
    move |ev: web_sys::MouseEvent| {
        if ev.detail() > 1 || nick.is_empty() {
            return;
        }
        let nick = nick.clone();
        let tapped_in = state.active_buffer.get_untracked();
        leptos::task::spawn_local(async move {
            gloo_timers::future::sleep(std::time::Duration::from_millis(300)).await;
            // The user may have switched buffers inside the defer window
            // (rapid two-tap on mobile) — a mention captured in #foo must
            // not land in #bar's draft.
            if state.active_buffer.get_untracked() != tapped_in {
                return;
            }
            let selecting = web_sys::window()
                .and_then(|w| w.get_selection().ok().flatten())
                .is_some_and(|s| !s.is_collapsed());
            if !selecting {
                state.pending_mention.set(Some(nick));
            }
        });
    }
}

fn mention_on_keydown(state: AppState, nick: String) -> impl Fn(web_sys::KeyboardEvent) + Clone {
    move |event: web_sys::KeyboardEvent| {
        if matches!(event.key().as_str(), "Enter" | " ") && !nick.is_empty() {
            event.prevent_default();
            state.pending_mention.set(Some(nick.clone()));
        }
    }
}

/// Compute the per-nick CSS color string (`color: #rrggbb;`) when
/// `colors_apply` and nick colors are enabled, or `""` otherwise.
/// Reads `nick_colors_enabled`, `nick_color_saturation`, and
/// `nick_color_lightness` tracked — the calling closure should be
/// invoked from a reactive position so changes update the DOM.
fn nick_color_or_empty(state: AppState, nick: &str, colors_apply: bool) -> String {
    if state.nick_colors_enabled.get() && colors_apply {
        let sat = state.nick_color_saturation.get();
        let lit = state.nick_color_lightness.get();
        let css_color = crate::nick_color::nick_color_css(nick, sat, lit);
        format!("color: {css_color};")
    } else {
        String::new()
    }
}

/// LocalStorage key that mirrors the server's `web.image_previews` setting
/// for individual browsers. When set to `"false"`, this client suppresses
/// previews even if the server has them enabled. Any other value (missing,
/// `"true"`, etc.) means "show them". No UI toggle yet — power users flip
/// it from devtools; a Settings panel toggle is the obvious follow-up.
const IMAGE_PREVIEWS_TOGGLE_KEY: &str = "web_image_previews_enabled";

/// Read the per-browser image-previews override. Returns `true` (show) when
/// the key is absent or any value other than the literal `"false"`.
fn previews_enabled_in_browser() -> bool {
    let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) else {
        return true;
    };
    !matches!(
        storage.get_item(IMAGE_PREVIEWS_TOGGLE_KEY),
        Ok(Some(ref v)) if v == "false"
    )
}

/// Render the per-message preview block, if there are previews to show.
///
/// Returns `None` (which leptos renders as nothing) when:
/// - the message has no server-extracted previews,
/// - every preview is in the dismissed-previews localStorage set, or
/// - the user has previews disabled in their browser via the
///   `web_image_previews_enabled = "false"` localStorage key.
fn render_previews(
    state: AppState,
    msg_id: u64,
    previews: Vec<crate::protocol::LinkPreview>,
) -> Option<leptos::prelude::AnyView> {
    if previews.is_empty() || !previews_enabled_in_browser() {
        return None;
    }
    let dismissed = state.dismissed_previews.get();
    let visible: Vec<_> = previews
        .into_iter()
        .filter(|p| !dismissed.contains(&(msg_id, p.link.clone())))
        .filter(|p| p.thumb_url.is_some())
        .collect();
    if visible.is_empty() {
        return None;
    }
    let nodes: Vec<leptos::prelude::AnyView> = visible
        .into_iter()
        .map(|preview| {
            let link = preview.link.clone();
            let thumb = preview.thumb_url.unwrap_or_default();
            let dismiss_link = preview.link.clone();
            let on_dismiss = move |_| {
                state.dismissed_previews.update(|set| {
                    set.insert((msg_id, dismiss_link.clone()));
                });
                crate::state::save_dismissed_previews(&state.dismissed_previews.get());
            };
            // Reserve-upfront: the card renders its fixed 320×200
            // (aspect-ratio on mobile) box IMMEDIATELY, so an async
            // image load never changes layout — the load only fades
            // the pixels in (`.loaded` flips opacity). This is what
            // keeps the log rock-steady while a just-switched-to
            // buffer's previews stream in; the previous
            // reveal-on-load design (display:none until onload) grew
            // the scroller at each decode and visibly floated the
            // text.
            //
            // The rare failure path (dead link, /api/preview 502)
            // collapses the box with scroll compensation — that logic
            // lives as a named, formatted function in `index.html`
            // (`__rpPreviewError`); the attribute here only delegates.
            //
            // Inline HTML attributes rather than Leptos closures
            // because `render_message` has no access to `ChatView`'s
            // `chat_ref`, and the handlers need only the DOM they're
            // attached to.
            const ON_IMG_LOAD: &str = "this.classList.add('loaded');";
            const ON_IMG_ERROR: &str = "__rpPreviewError(this);";
            view! {
                <span class="msg-preview-card">
                    <a
                        href=link
                        target="_blank"
                        rel="noopener noreferrer"
                        class="msg-preview-link"
                    >
                        // Eager loading is deliberate (no `loading="lazy"`):
                        // a dead preview must error out EARLY — while its
                        // card is still off-screen and the collapse is
                        // invisible or compensated — not the moment the
                        // reader scrolls it into view (lazy would time the
                        // error exactly for the worst reader-visible jump,
                        // after a long stare at an empty box). The server
                        // thumbnail cache + browser HTTP cache absorb the
                        // eager cost, same as before this branch.
                        <img
                            src=thumb
                            class="msg-preview-thumb"
                            alt="link preview"
                            onload=ON_IMG_LOAD
                            onerror=ON_IMG_ERROR
                        />
                    </a>
                    <button
                        type="button"
                        class="msg-preview-dismiss"
                        title="Hide this preview"
                        on:click=on_dismiss
                    >"\u{00D7}"</button>
                </span>
            }
            .into_any()
        })
        .collect();
    Some(view! { <div class="msg-previews">{nodes}</div> }.into_any())
}

/// Render text with irssi/mIRC format codes as styled HTML spans.
///
/// `parse_format` produces colour/bold spans; `linkify_spans` then carves
/// URLs out of plain-text fragments; `emotify_spans` rewrites known `:name:`
/// tokens into emote spans. Spans with `link = Some(url)` are wrapped in
/// `<a target="_blank" rel="noopener noreferrer">`; emote spans render as an
/// inline `<img class="emote">` (with the `:name:` token as alt/title for
/// accessibility and copy/paste fallback).
fn render_styled_text(text: &str, emotes_on: bool) -> Vec<leptos::prelude::AnyView> {
    crate::components::styled::render_message_text(text, emotes_on)
}

/// Rebuild a `.chat-line` as space-joined plain text for the copy
/// handler — concatenates each direct child span's `textContent` with
/// a single space. Mirrors what users actually see (ts, nick, text),
/// and matches the TUI's one-line-per-message copy semantics.
///
/// Children:
///   - regular line: `<span ts><span nick><span text>` → `ts nick text`
///   - action      : `<span ts><span action-body>`     → `ts * nick text`
///   - event/notice: `<span ts><span text>`            → `ts text`
///   - separator   : `<span separator-text>`           → just the text
///
/// `textContent` on the nick span flattens its nested mode/name/sep
/// children to e.g. `snieg❯`, which is exactly the visual form.
fn format_chat_line_for_copy(node: &web_sys::Node) -> Option<String> {
    let el = node.dyn_ref::<web_sys::Element>()?;
    let children = el.children();
    let mut parts: Vec<String> = Vec::with_capacity(children.length() as usize);
    for i in 0..children.length() {
        let Some(child) = children.item(i) else {
            continue;
        };
        let text = child.text_content().unwrap_or_default();
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    Some(parts.join(" "))
}

fn format_timestamp(ts: i64, fmt: &str) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| {
            use chrono::TimeZone;
            let local = chrono::Local.from_utc_datetime(&dt.naive_utc());
            local.format(fmt).to_string()
        })
        .unwrap_or_default()
}

/// Map an event_key to a (symbol, css_class) pair for rendering.
/// Falls back to text heuristic for backlog messages without event_key.
fn event_icon(event_key: Option<&str>, text: &str) -> Option<(&'static str, &'static str)> {
    if let Some(key) = event_key {
        match key {
            "join" => Some(("\u{2192} ", "join-arrow")),
            "part" => Some(("\u{2190} ", "part-arrow")),
            "quit" => Some(("\u{2190} ", "quit-arrow")),
            "kick" => Some(("\u{2190} ", "kick-arrow")),
            "kicked" => Some(("\u{2190} ", "kicked-arrow")),
            "nick_change" => Some(("\u{2194} ", "nick-arrow")),
            "topic_changed" => Some(("\u{2192} ", "topic-arrow")),
            "mode" => Some(("\u{25CB} ", "mode-arrow")),
            "connected" => Some(("\u{25CF} ", "connect-arrow")),
            "disconnected" => Some(("\u{25CB} ", "disconnect-arrow")),
            "chghost" => Some(("\u{2194} ", "chghost-arrow")),
            "account" => Some(("\u{2194} ", "account-arrow")),
            _ => None,
        }
    } else if text.contains("has joined") {
        Some(("\u{2192} ", "join-arrow"))
    } else if text.contains("has left") {
        Some(("\u{2190} ", "part-arrow"))
    } else if text.contains("has quit") {
        Some(("\u{2190} ", "quit-arrow"))
    } else if text.contains("is now known as") {
        Some(("\u{2194} ", "nick-arrow"))
    } else {
        None
    }
}

/// Truncate nick to fit max_len columns, accounting for mode prefix width.
/// TUI subtracts mode width from the nick budget; web must match.
fn truncate_nick(nick: &str, max_len: usize, mode: &str) -> String {
    let mode_width = mode.len();
    let nick_budget = max_len.saturating_sub(mode_width);
    let char_count = nick.chars().count();
    if char_count <= nick_budget {
        nick.to_string()
    } else {
        let mut result = String::with_capacity(nick_budget);
        for (i, ch) in nick.chars().enumerate() {
            if i >= nick_budget - 1 {
                break;
            }
            result.push(ch);
        }
        result.push('+');
        result
    }
}

#[cfg(test)]
mod tests {
    use super::{restored_scroll_top, viewport_needs_backlog};

    #[test]
    fn viewport_fill_is_disabled_without_a_rendered_viewport() {
        assert!(!viewport_needs_backlog(0, 0));
    }

    #[test]
    fn viewport_fill_is_enabled_when_content_does_not_overflow() {
        assert!(viewport_needs_backlog(200, 300));
    }

    #[test]
    fn viewport_fill_is_disabled_when_content_overflows() {
        assert!(!viewport_needs_backlog(301, 300));
    }

    #[test]
    fn zero_offset_does_not_schedule_a_scroll_restore() {
        assert_eq!(restored_scroll_top(120, 0.0), None);
    }

    #[test]
    fn subpixel_offset_that_keeps_scroll_top_does_not_schedule_a_restore() {
        assert_eq!(restored_scroll_top(120, 0.75), None);
    }

    #[test]
    fn changed_scroll_top_is_returned_for_restore() {
        assert_eq!(restored_scroll_top(120, 15.0), Some(135));
    }
}

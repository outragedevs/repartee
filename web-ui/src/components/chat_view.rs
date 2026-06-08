use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::state::AppState;

/// Distance in pixels from the absolute bottom that still counts as
/// "user is at bottom". Mirrors thelounge's value — generous enough
/// to absorb sub-pixel measurement noise but tight enough that the
/// user has to actively scroll up before stickiness flips off.
const SCROLL_THRESHOLD: f64 = 30.0;

/// Distance from the top (px) at which scrolling up triggers an on-demand
/// fetch of an older backlog page from the server. Generous so the page is
/// usually loaded before the user reaches the very top.
const BACKLOG_TRIGGER_PX: f64 = 400.0;

fn is_near_bottom(el: &web_sys::Element) -> bool {
    el.scroll_height() as f64 - el.scroll_top() as f64 - el.client_height() as f64
        <= SCROLL_THRESHOLD
}

/// Hard-pin the scroller to the bottom. Callers MUST set
/// `skip_next_scroll` first so the resulting `scroll` event does not
/// re-enter `on_scroll` and re-measure mid-paint.
fn pin_to_bottom(el: &web_sys::Element) {
    el.set_scroll_top(el.scroll_height());
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

    // Suppresses the next `scroll` event handler. We set `scrollTop`
    // programmatically in `pin_to_bottom`, the browser fires `scroll`
    // anyway, and without this flag the handler would re-measure
    // mid-paint and could briefly flip `is_at_bottom` to false. The
    // same trick is what keeps thelounge's MessageList stable.
    let skip_next_scroll = StoredValue::new(false);

    // Coalesces multiple message appends in the same microtask into a
    // single RAF-scheduled pin. Without this, a burst of incoming
    // messages would queue N pins per tick.
    let pin_scheduled = StoredValue::new(false);

    // While scrolling up, holds the viewport's distance-from-bottom captured
    // when a backlog fetch was sent. After the older page prepends (content
    // added above), an Effect restores `scrollTop` to keep that same distance,
    // so the view stays anchored instead of jumping. `None` = no fetch pending.
    let pending_anchor = StoredValue::new(None::<f64>);

    let do_pin = move |el: &web_sys::Element| {
        skip_next_scroll.set_value(true);
        pin_to_bottom(el);
    };

    // Buffer-switch: always reset `is_at_bottom = true` and snap to
    // the bottom on the next animation frame (so the `<For>` has had a
    // chance to render the new buffer's messages).
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
        state.is_at_bottom.set(true);
        let Some(el) = chat_ref.get() else { return };
        let el_dom: web_sys::Element = el.into();
        let Some(window) = web_sys::window() else {
            return;
        };
        let cb = wasm_bindgen::prelude::Closure::once(move || do_pin(&el_dom));
        let _ = window.request_animation_frame(cb.as_ref().unchecked_ref());
        cb.forget();
    });

    // Re-pin on every message-list mutation, but only if the user is
    // already at the bottom. Subscribes to `state.messages` so any
    // append (or backlog batch) triggers; `pin_scheduled` debounces
    // bursts so we pin at most once per animation frame. RAF lets
    // Leptos commit the DOM patch first, so we measure against the
    // final scrollHeight.
    Effect::new(move || {
        state.messages.with(|_| ());
        let _ = state.active_buffer.get();
        if !state.is_at_bottom.get_untracked() {
            return;
        }
        if pin_scheduled.get_value() {
            return;
        }
        pin_scheduled.set_value(true);
        let Some(window) = web_sys::window() else {
            return;
        };
        let cb = wasm_bindgen::prelude::Closure::once(move || {
            pin_scheduled.set_value(false);
            if !state.is_at_bottom.get_untracked() {
                return;
            }
            let Some(el) = chat_ref.get() else { return };
            let el_dom: web_sys::Element = el.into();
            do_pin(&el_dom);
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
        let Some(dist) = pending_anchor.get_value() else {
            return;
        };
        pending_anchor.set_value(None);
        let Some(window) = web_sys::window() else {
            return;
        };
        let cb = wasm_bindgen::prelude::Closure::once(move || {
            let Some(el) = chat_ref.get() else { return };
            let el_dom: web_sys::Element = el.into();
            // Keep the same distance from the bottom across the prepend.
            skip_next_scroll.set_value(true);
            let target = (f64::from(el_dom.scroll_height()) - dist).max(0.0);
            #[allow(clippy::cast_possible_truncation)]
            el_dom.set_scroll_top(target as i32);
        });
        let _ = window.request_animation_frame(cb.as_ref().unchecked_ref());
        cb.forget();
    });

    // Single window.resize listener, coalesced into the next RAF.
    // Fires on:
    //   - desktop browser resize
    //   - Android Chrome keyboard open/close (OSK overlays a smaller
    //     area, browser resizes the layout viewport)
    //   - mobile URL-bar collapse/expand
    // iOS Safari does NOT fire `window.resize` on keyboard open and
    // does NOT resize the layout viewport; the OSK overlays the visual
    // viewport. We accept that — the browser auto-scrolls the focused
    // textarea into view on focus, and on blur the visual viewport
    // returns to the layout viewport with chat unchanged. Adding a
    // VisualViewport listener was a net negative: it fired
    // mid-animation and re-pinned partway through, producing visible
    // hops at both keyboard open and close.
    type ResizeHandle = Option<(wasm_bindgen::prelude::Closure<dyn Fn()>, js_sys::Function)>;
    let resize_cleanup: StoredValue<ResizeHandle, leptos::prelude::LocalStorage> =
        StoredValue::new_local(None);
    let resize_registered = StoredValue::new(false);
    let resize_throttle = StoredValue::new(false);
    Effect::new(move || {
        if chat_ref.get().is_none() {
            return;
        }
        if resize_registered.get_value() {
            return;
        }
        resize_registered.set_value(true);
        let cb = wasm_bindgen::prelude::Closure::<dyn Fn()>::new(move || {
            if resize_throttle.get_value() {
                return;
            }
            resize_throttle.set_value(true);
            let Some(window) = web_sys::window() else {
                return;
            };
            let raf_cb = wasm_bindgen::prelude::Closure::once(move || {
                resize_throttle.set_value(false);
                if !state.is_at_bottom.get_untracked() {
                    return;
                }
                let Some(el) = chat_ref.get() else { return };
                let el_dom: web_sys::Element = el.into();
                do_pin(&el_dom);
            });
            let _ = window.request_animation_frame(raf_cb.as_ref().unchecked_ref());
            raf_cb.forget();
        });
        let cb_fn: js_sys::Function = cb.as_ref().unchecked_ref::<js_sys::Function>().clone();
        if let Some(window) = web_sys::window() {
            let _ = window.add_event_listener_with_callback("resize", &cb_fn);
        }
        resize_cleanup.set_value(Some((cb, cb_fn)));
    });

    on_cleanup(move || {
        let handle = resize_cleanup.try_update_value(Option::take).flatten();
        let Some((cb, cb_fn)) = handle else { return };
        if let Some(window) = web_sys::window() {
            let _ = window.remove_event_listener_with_callback("resize", &cb_fn);
        }
        drop(cb);
    });

    // The scroll handler is the ONLY place that flips `is_at_bottom`
    // off. Programmatic pins set `skip_next_scroll` so the resulting
    // scroll event is ignored — without that guard, the synchronous
    // measurement during a mid-paint scroll callback could read a
    // stale scrollTop and incorrectly mark us as "not at bottom".
    let on_scroll = move |ev: web_sys::Event| {
        if skip_next_scroll.get_value() {
            skip_next_scroll.set_value(false);
            return;
        }
        let target = ev.target().unwrap();
        let el: &web_sys::Element = target.unchecked_ref();
        let next = is_near_bottom(el);
        if state.is_at_bottom.get_untracked() != next {
            state.is_at_bottom.set(next);
            // Returned to the live bottom: collapse the loaded backlog window
            // (free the older lines, re-arm scroll-up).
            if next && let Some(id) = state.active_buffer.get_untracked() {
                state.collapse_backlog(&id);
            }
        }
        // Near the top: pull an older page from the server, unless one is already
        // in flight or the server told us there's nothing older.
        if f64::from(el.scroll_top()) < BACKLOG_TRIGGER_PX {
            let Some(id) = state.active_buffer.get_untracked() else {
                return;
            };
            if state.backlog_fetching.with_untracked(|s| s.contains(&id)) {
                return;
            }
            if state
                .backlog_has_more
                .with_untracked(|m| m.get(&id) == Some(&false))
            {
                return;
            }
            // Oldest real (non-separator) message's timestamp = the cursor.
            let before = state.messages.with_untracked(|m| {
                m.get(&id)
                    .and_then(|v| v.iter().find(|msg| msg.id != 0).map(|msg| msg.timestamp))
            });
            let Some(before) = before else {
                return;
            };
            // Anchor: remember the distance from the bottom so the restore Effect
            // can keep the view put once the older page prepends.
            pending_anchor
                .set_value(Some(f64::from(el.scroll_height()) - f64::from(el.scroll_top())));
            state.backlog_fetching.update(|s| {
                s.insert(id.clone());
            });
            crate::ws::send_command(&crate::protocol::WebCommand::FetchMessages {
                buffer_id: id,
                limit: 100,
                before: Some(before),
            });
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
                <div class="chat-messages" node_ref=chat_ref on:scroll=on_scroll on:copy=on_copy>
                    <div class="chat-messages-inner">
                        <For
                            each=move || messages().unwrap_or_default()
                            // Date-separator rows use `id == 0` (see
                            // state.rs:168 — "reserved for date separators
                            // and is not unique"), so keying by `msg.id`
                            // alone would collide across every separator
                            // and let Leptos reuse one DOM node for all of
                            // them. Use the timestamp as the discriminator
                            // for separators; real messages still key by
                            // their unique `msg.id` so backlog timestamp
                            // re-stamps cannot force a re-mount.
                            key=|msg| (msg.id, if msg.id == 0 { msg.timestamp } else { 0 })
                            children=move |msg| render_message(state, msg)
                        />
                    </div>
                </div>
                <div class="scroll-bottom-btn"
                    class:hidden=move || state.is_at_bottom.get()
                    on:click=move |_| {
                        state.is_at_bottom.set(true);
                        if let Some(el) = chat_ref.get() {
                            let el_dom: web_sys::Element = el.into();
                            do_pin(&el_dom);
                        }
                    }
                >
                    "\u{25BC}"
                </div>
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
            <div class=line_class>
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
                <div class=line_class>
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
        view! {
            <>
                <div class=line_class>
                    <span class="ts">{ts_fn}</span>
                    <span class="action-body">
                        "* "
                        <span class="action-nick" style=nick_color_style>{nick_text}</span>
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
                <div class=line_class>
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
            <div class=line_class>
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

        view! {
            <>
                <div class=line_class>
                    <span class="ts">{ts_fn}</span>
                    <span class="nick" style=nick_style>
                        <span class="mode">{mode}</span>
                        <span class="name" style=nick_color_style>{nick_truncated}</span>
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
            // Reveal-on-load: the card is `display:none` by default
            // (see `.msg-preview-card` in base.css). On successful
            // image load we add the `.loaded` class, which switches
            // it to `display:inline-block` and reserves its 320×200
            // (or aspect-ratio on mobile) box. On error we do
            // nothing, so failed previews never flash a placeholder
            // and never trigger reserve-then-collapse reflow.
            //
            // The trailing scroll re-anchor handles the case where
            // the user was at the bottom of chat when the new card
            // appeared — without it the freshly-revealed 200 px box
            // pushes live messages off-screen. Threshold (40 px)
            // matches `SCROLL_THRESHOLD` so the re-anchor logic
            // tracks the same "near bottom" semantics used elsewhere.
            //
            // Inline HTML attribute rather than a Leptos closure
            // because `render_message` has no access to `ChatView`'s
            // `chat_ref` and threading it through every per-message
            // child would be ceremony for no functional gain. The
            // `loading="lazy"` attribute is intentionally absent —
            // it's a no-op while the parent is display:none, so all
            // preview images for in-DOM messages fetch eagerly.
            // Re-pinning on image decode falls out of the messages
            // Effect in ChatView: when the preview's `loaded` class
            // flips and the card reveals, the resulting reflow does
            // not affect scrollHeight any differently than the
            // initial message append — and we already pin on append.
            // Reveal-on-load plus an inline scroll-pin: previews go
            // from display:none to display:inline-block, which grows
            // the message-list scrollHeight. We measure whether the
            // user was at the bottom BEFORE flipping the class, and
            // if so, snap to the new bottom afterwards. The previous
            // implementation relied on a ResizeObserver in ChatView
            // to catch this; we dropped that observer because it also
            // fired on every keyboard-animation frame and produced
            // visible jitter. The threshold (30) mirrors
            // `SCROLL_THRESHOLD`.
            const ON_IMG_LOAD: &str = "var c=this.closest('.msg-preview-card');var s=c&&c.closest('.chat-messages');var atBottom=s&&(s.scrollHeight-s.scrollTop-s.clientHeight<=30);c.classList.add('loaded');if(atBottom){s.scrollTop=s.scrollHeight;}";
            view! {
                <span class="msg-preview-card">
                    <a
                        href=link
                        target="_blank"
                        rel="noopener noreferrer"
                        class="msg-preview-link"
                    >
                        <img
                            src=thumb
                            class="msg-preview-thumb"
                            alt="link preview"
                            onload=ON_IMG_LOAD
                        />
                    </a>
                    <button
                        class="msg-preview-dismiss"
                        type="button"
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

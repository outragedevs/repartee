use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::format;
use crate::state::AppState;

const SCROLL_THRESHOLD: f64 = 40.0;

fn is_near_bottom(el: &web_sys::Element) -> bool {
    el.scroll_height() as f64 - el.scroll_top() as f64 - el.client_height() as f64
        <= SCROLL_THRESHOLD
}

#[component]
pub fn ChatView() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();

    let is_shell = move || {
        let active_id = state.active_buffer.get()?;
        let bufs = state.buffers.get();
        bufs.iter()
            .find(|b| b.id == active_id)
            .map(|b| b.buffer_type == "shell")
    };

    let messages = move || {
        let active_id = state.active_buffer.get()?;
        state.messages.with(|msgs| msgs.get(&active_id).cloned())
    };

    let chat_ref = NodeRef::<leptos::html::Div>::new();

    // Track previous buffer ID to detect buffer switches.
    let prev_buffer_id = StoredValue::new(None::<String>);

    // Auto-scroll: scroll to bottom on new messages (only if at bottom)
    // or on buffer switch (always).
    Effect::new(move || {
        let active_id = state.active_buffer.get();
        let _ = messages();

        let is_switch = prev_buffer_id.get_value().as_deref() != active_id.as_deref();
        if let Some(ref id) = active_id {
            prev_buffer_id.set_value(Some(id.clone()));
        }

        if is_switch {
            state.is_at_bottom.set(true);
        }

        if is_switch || state.is_at_bottom.get() {
            scroll_to_bottom(&chat_ref);
        }
    });

    // Scroll to bottom on resize only if user was at bottom. The
    // listener is removed on unmount via on_cleanup, so component
    // remounts (e.g. login→logout→login) don't leak forgotten Closures.
    // wasm_bindgen Closure and js_sys::Function are !Send, so the
    // cleanup handle has to use LocalStorage rather than the default
    // SyncStorage of StoredValue::new.
    let resize_registered = StoredValue::new(false);
    type ResizeHandle = Option<(wasm_bindgen::prelude::Closure<dyn Fn()>, js_sys::Function)>;
    let resize_cleanup: StoredValue<ResizeHandle, leptos::prelude::LocalStorage> =
        StoredValue::new_local(None);
    Effect::new(move || {
        let Some(el) = chat_ref.get() else { return };
        if resize_registered.get_value() {
            return;
        }
        resize_registered.set_value(true);
        let el_clone: web_sys::Element = el.clone().into();
        let is_at_bottom = state.is_at_bottom;
        let cb = wasm_bindgen::prelude::Closure::<dyn Fn()>::new(move || {
            if is_at_bottom.get() {
                el_clone.set_scroll_top(el_clone.scroll_height());
            }
        });
        if let Some(window) = web_sys::window() {
            let cb_fn: js_sys::Function = cb.as_ref().unchecked_ref::<js_sys::Function>().clone();
            let _ = window.add_event_listener_with_callback("resize", &cb_fn);
            resize_cleanup.set_value(Some((cb, cb_fn)));
        }
    });
    on_cleanup(move || {
        let handle = resize_cleanup.try_update_value(Option::take).flatten();
        let Some((cb, cb_fn)) = handle else { return };
        if let Some(window) = web_sys::window() {
            let _ = window.remove_event_listener_with_callback("resize", &cb_fn);
        }
        drop(cb);
    });

    // Update is_at_bottom on every scroll event. Equality-guard the
    // write: Leptos 0.7 fires subscribers regardless of value equality,
    // so an unguarded set on every scroll tick was retriggering the
    // auto-scroll Effect and contributing to the visible jitter.
    let on_scroll = move |ev: web_sys::Event| {
        let target = ev.target().unwrap();
        let el: &web_sys::Element = target.unchecked_ref();
        let next = is_near_bottom(el);
        if state.is_at_bottom.get_untracked() != next {
            state.is_at_bottom.set(next);
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
        let Some(clip_ev) = ev.dyn_ref::<web_sys::ClipboardEvent>() else { return };
        let Some(window) = web_sys::window() else { return };
        let Ok(Some(selection)) = window.get_selection() else { return };
        if selection.is_collapsed() {
            return;
        }
        let raw_js = selection.to_string();
        let raw: String = raw_js.into();
        if !raw.contains('\n') {
            return;
        }
        let Some(doc) = window.document() else { return };
        let Ok(chat_lines) = doc.query_selector_all(".chat-line") else { return };
        let mut out: Vec<String> = Vec::with_capacity(chat_lines.length() as usize);
        for i in 0..chat_lines.length() {
            let Some(node) = chat_lines.item(i) else { continue };
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
        let Some(clipboard) = clip_ev.clipboard_data() else { return };
        if clipboard.set_data("text/plain", &formatted).is_ok() {
            clip_ev.prevent_default();
        }
    };

    view! {
        <div class="chat-area">
            {move || {
                if is_shell() == Some(true) {
                    return view! { <super::shell_view::ShellView /> }.into_any();
                }
                view! {
            <div class="chat-messages-outer">
                <div class="chat-messages" node_ref=chat_ref on:scroll=on_scroll on:copy=on_copy>
                    <For
                        each=move || messages().unwrap_or_default()
                        key=|msg| (msg.id, msg.timestamp)
                        children=move |msg| render_message(state, msg)
                    />
                </div>
                <div class="scroll-bottom-btn"
                    class:hidden=move || state.is_at_bottom.get()
                    on:click=move |_| {
                        scroll_to_bottom(&chat_ref);
                        state.is_at_bottom.set(true);
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

    let is_mention_log = msg.msg_type == "mention_log";
    let is_event = msg.msg_type == "event";
    let is_action = msg.msg_type == "action";
    let is_notice = msg.msg_type == "notice";
    let is_separator =
        is_event && msg.nick.is_none() && msg.text.starts_with('\u{2500}');

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
        let styled = render_styled_text(&msg.text);
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
        let styled = render_styled_text(&msg.text);
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
        let styled = render_styled_text(&msg.text);
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
        let styled = render_styled_text(&msg.text);
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
        let styled = render_styled_text(&msg.text);
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
            const ON_IMG_LOAD: &str = "var c=this.closest('.msg-preview-card');\
                var m=this.closest('.chat-messages');\
                var b=m&&(m.scrollHeight-m.scrollTop-m.clientHeight<40);\
                c.classList.add('loaded');\
                if(b)m.scrollTop=m.scrollHeight;";
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
/// URLs out of plain-text fragments. Spans with `link = Some(url)` are
/// wrapped in `<a target="_blank" rel="noopener noreferrer">` so a left
/// click opens a new tab and a right click yields the browser's standard
/// "Open in New Window" context menu.
fn render_styled_text(text: &str) -> Vec<leptos::prelude::AnyView> {
    let spans = format::linkify_spans(format::parse_format(text));
    spans
        .into_iter()
        .map(|span| {
            let css = span.css();
            if let Some(url) = span.link {
                let style = if css.is_empty() { String::new() } else { css };
                view! {
                    <a
                        href=url
                        target="_blank"
                        rel="noopener noreferrer"
                        class="msg-link"
                        style=style
                    >{span.text}</a>
                }
                .into_any()
            } else if span.has_style() {
                view! { <span style=css>{span.text}</span> }.into_any()
            } else {
                view! { <span>{span.text}</span> }.into_any()
            }
        })
        .collect::<Vec<_>>()
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
        let Some(child) = children.item(i) else { continue };
        let text = child.text_content().unwrap_or_default();
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    Some(parts.join(" "))
}

/// Scroll chat container to bottom via requestAnimationFrame.
fn scroll_to_bottom(chat_ref: &NodeRef<leptos::html::Div>) {
    let Some(el) = chat_ref.get() else { return };
    let scroll_fn = wasm_bindgen::prelude::Closure::once(move || {
        el.set_scroll_top(el.scroll_height());
    });
    if let Some(window) = web_sys::window() {
        let _ = window.request_animation_frame(scroll_fn.as_ref().unchecked_ref());
        scroll_fn.forget();
    }
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

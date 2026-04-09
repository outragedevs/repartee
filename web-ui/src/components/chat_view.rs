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
        let msgs = state.messages.get();
        msgs.get(&active_id).cloned()
    };

    let our_nick = move || -> Option<String> {
        let active_id = state.active_buffer.get()?;
        let bufs = state.buffers.get();
        let buf = bufs.iter().find(|b| b.id == active_id)?;
        let conns = state.connections.get();
        let conn = conns.iter().find(|c| c.id == buf.connection_id)?;
        Some(conn.nick.clone())
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

    // Scroll to bottom on resize only if user was at bottom.
    let resize_registered = StoredValue::new(false);
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
            let _ = window.add_event_listener_with_callback("resize", cb.as_ref().unchecked_ref());
            cb.forget();
        }
    });

    // Update is_at_bottom on every scroll event.
    let on_scroll = {
        let state = state;
        move |ev: web_sys::Event| {
            let target = ev.target().unwrap();
            let el: &web_sys::Element = target.unchecked_ref();
            state.is_at_bottom.set(is_near_bottom(el));
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
                <div class="chat-messages" node_ref=chat_ref on:scroll=on_scroll>
                    {move || {
                        let nick_self = our_nick();
                        let ts_fmt = state.timestamp_format.get();
                        messages().unwrap_or_default().into_iter().map(|msg| {
                            let is_mention_log = msg.msg_type == "mention_log";
                            let is_event = msg.msg_type == "event";
                            let is_action = msg.msg_type == "action";
                            let is_notice = msg.msg_type == "notice";
                            let is_separator = is_event && msg.nick.is_none() && msg.text.starts_with('\u{2500}');

                            let is_own = nick_self.as_ref().is_some_and(|our| {
                                msg.nick.as_deref() == Some(our.as_str())
                            });

                            let line_class = if is_separator {
                                "chat-line date-separator"
                            } else if is_mention_log {
                                "chat-line mention-log"
                            } else if msg.highlight && msg.nick.is_some() {
                                if is_own { "chat-line mention own" } else { "chat-line mention" }
                            } else if is_event {
                                match msg.event_key.as_deref() {
                                    Some("join") | Some("connected") => "chat-line event join-event",
                                    Some("part") | Some("quit") | Some("disconnected") => "chat-line event part-event",
                                    Some("kick") => "chat-line event kick-event",
                                    Some("kicked") => "chat-line event kicked-event",
                                    Some("nick_change") | Some("chghost") | Some("account") => "chat-line event nick-event",
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

                            let ts = format_timestamp(msg.timestamp, &ts_fmt);

                            if is_separator {
                                return view! {
                                    <div class=line_class>
                                        <span class="separator-text">{msg.text}</span>
                                    </div>
                                }.into_any();
                            }

                            if is_mention_log {
                                let styled = render_styled_text(&msg.text);
                                return view! {
                                    <div class=line_class>
                                        <span class="mention-log-text">{styled}</span>
                                    </div>
                                }.into_any();
                            }

                            if is_action {
                                let nick_text = msg.nick.unwrap_or_default();
                                let nick_color_style = if state.nick_colors_enabled.get() && !is_own {
                                    let sat = state.nick_color_saturation.get();
                                    let lit = state.nick_color_lightness.get();
                                    let css_color = crate::nick_color::nick_color_css(&nick_text, sat, lit);
                                    format!("color: {css_color};")
                                } else {
                                    String::new()
                                };
                                let styled = render_styled_text(&msg.text);
                                view! {
                                    <div class=line_class>
                                        <span class="ts">{ts}</span>
                                        <span class="action-body">
                                            "* "
                                            <span class="action-nick" style=nick_color_style>{nick_text}</span>
                                            " "
                                            {styled}
                                        </span>
                                    </div>
                                }.into_any()
                            } else if is_notice {
                                // Notice: -nick- text
                                let nick_text = msg.nick.unwrap_or_default();
                                let styled = render_styled_text(&msg.text);
                                view! {
                                    <div class=line_class>
                                        <span class="ts">{ts}</span>
                                        <span class="notice-body">
                                            "-"
                                            <span class="notice-nick">{nick_text}</span>
                                            "- "
                                            {styled}
                                        </span>
                                    </div>
                                }.into_any()
                            } else if is_event {
                                let arrow = event_icon(msg.event_key.as_deref(), &msg.text);
                                let styled = render_styled_text(&msg.text);
                                view! {
                                    <div class=line_class>
                                        <span class="ts">{ts}</span>
                                        <span>
                                            {arrow.map(|(symbol, css_class)| view! {
                                                <span class=css_class>{symbol}</span>
                                            })}
                                            {styled}
                                        </span>
                                    </div>
                                }.into_any()
                            } else {
                                // Regular message: timestamp | right-aligned nick❯ | text
                                let max_len = state.nick_max_length.get() as usize;
                                let col_width = state.nick_column_width.get();
                                let nick_text = msg.nick.unwrap_or_default();
                                let mode = msg.nick_mode.unwrap_or_default();
                                let nick = truncate_nick(&nick_text, max_len, &mode);
                                let styled = render_styled_text(&msg.text);

                                // Per-nick color: skip for own messages (use --green via CSS) and highlights/mentions
                                let nick_color_style = if state.nick_colors_enabled.get() && !is_own && !msg.highlight {
                                    let sat = state.nick_color_saturation.get();
                                    let lit = state.nick_color_lightness.get();
                                    let css_color = crate::nick_color::nick_color_css(&nick_text, sat, lit);
                                    format!("color: {css_color};")
                                } else {
                                    String::new()
                                };

                                let nick_style = format!("width: {col_width}ch;");
                                view! {
                                    <div class=line_class>
                                        <span class="ts">{ts}</span>
                                        <span class="nick" style=nick_style>
                                            <span class="mode">{mode}</span>
                                            <span class="name" style=nick_color_style>{nick}</span>
                                            <span class="sep">"❯"</span>
                                        </span>
                                        <span class="text">{styled}</span>
                                    </div>
                                }.into_any()
                            }
                        }).collect::<Vec<_>>()
                    }}
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

/// Render text with irssi/mIRC format codes as styled HTML spans.
fn render_styled_text(text: &str) -> Vec<leptos::prelude::AnyView> {
    let spans = format::parse_format(text);
    spans
        .into_iter()
        .map(|span| {
            if span.has_style() {
                let css = span.css();
                view! { <span style=css>{span.text}</span> }.into_any()
            } else {
                view! { <span>{span.text}</span> }.into_any()
            }
        })
        .collect::<Vec<_>>()
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

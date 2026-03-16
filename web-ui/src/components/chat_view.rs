use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::format;
use crate::state::AppState;

#[component]
pub fn ChatView() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();

    let messages = move || {
        let active_id = state.active_buffer.get()?;
        let msgs = state.messages.get();
        msgs.get(&active_id).cloned()
    };

    // Derive our own nick for the active buffer's connection.
    let our_nick = move || -> Option<String> {
        let active_id = state.active_buffer.get()?;
        let bufs = state.buffers.get();
        let buf = bufs.iter().find(|b| b.id == active_id)?;
        let conns = state.connections.get();
        let conn = conns.iter().find(|c| c.id == buf.connection_id)?;
        Some(conn.nick.clone())
    };

    let chat_ref = NodeRef::<leptos::html::Div>::new();

    // Scroll to bottom on new messages or buffer switch.
    Effect::new(move || {
        let _ = messages(); // track message changes
        scroll_to_bottom(&chat_ref);
    });

    // Scroll to bottom on resize (browser window resize).
    let resize_registered = StoredValue::new(false);
    Effect::new(move || {
        let Some(el) = chat_ref.get() else { return };
        if resize_registered.get_value() {
            return;
        }
        resize_registered.set_value(true);
        let el_clone: web_sys::Element = el.clone().into();
        let cb = wasm_bindgen::prelude::Closure::<dyn Fn()>::new(move || {
            el_clone.set_scroll_top(el_clone.scroll_height());
        });
        if let Some(window) = web_sys::window() {
            let _ = window.add_event_listener_with_callback("resize", cb.as_ref().unchecked_ref());
            cb.forget();
        }
    });

    view! {
        <div class="chat-area">
            <div class="chat-messages" node_ref=chat_ref>
                {move || {
                    let nick_self = our_nick();
                    let ts_fmt = state.timestamp_format.get();
                    messages().unwrap_or_default().into_iter().map(|msg| {
                        let is_event = msg.msg_type == "event";
                        let is_action = msg.msg_type == "action";
                        let is_notice = msg.msg_type == "notice";

                        // Detect own message.
                        let is_own = nick_self.as_ref().is_some_and(|our| {
                            msg.nick.as_deref() == Some(our.as_str())
                        });

                        let class = if msg.highlight && msg.nick.is_some() {
                            if is_own { "chat-line mention own" } else { "chat-line mention" }
                        } else if is_event {
                            "chat-line event"
                        } else if is_action {
                            "chat-line event action"
                        } else if is_own {
                            "chat-line own"
                        } else {
                            "chat-line"
                        };

                        let ts = format_timestamp(msg.timestamp, &ts_fmt);

                        if is_action {
                            // Action (/me): render as "* nick text"
                            let nick = msg.nick.unwrap_or_default();
                            let styled = render_styled_text(&msg.text);
                            view! {
                                <div class=class>
                                    <span class="ts">{ts}</span>
                                    <span class="action-body">
                                        "* "
                                        <span class="action-nick">{nick}</span>
                                        " "
                                        {styled}
                                    </span>
                                </div>
                            }.into_any()
                        } else if is_event || is_notice {
                            // Detect event arrow from text content.
                            let arrow = if msg.text.contains("has joined") {
                                Some(("\u{2192} ", "join-arrow"))
                            } else if msg.text.contains("has left") {
                                Some(("\u{2190} ", "part-arrow"))
                            } else if msg.text.contains("has quit") {
                                Some(("\u{2190} ", "quit-arrow"))
                            } else if msg.text.contains("is now known as") {
                                Some(("\u{2194} ", "nick-arrow"))
                            } else {
                                None
                            };

                            let styled = render_styled_text(&msg.text);
                            view! {
                                <div class=class>
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
                            let nick = truncate_nick(&msg.nick.unwrap_or_default(), max_len);
                            let mode = msg.nick_mode.unwrap_or_default();
                            let styled = render_styled_text(&msg.text);
                            let nick_style = format!("width: {col_width}ch;");
                            view! {
                                <div class=class>
                                    <span class="ts">{ts}</span>
                                    <span class="nick" style=nick_style>
                                        <span class="mode">{mode}</span>
                                        <span class="name">{nick}</span>
                                        <span class="sep">"❯"</span>
                                    </span>
                                    <span class="text">{styled}</span>
                                </div>
                            }.into_any()
                        }
                    }).collect::<Vec<_>>()
                }}
            </div>
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

/// Truncate nick to max_len chars. If longer, cut to max_len-1 and append `+`.
fn truncate_nick(nick: &str, max_len: usize) -> String {
    let char_count = nick.chars().count();
    if char_count <= max_len {
        nick.to_string()
    } else {
        let mut result = String::with_capacity(max_len);
        for (i, ch) in nick.chars().enumerate() {
            if i >= max_len - 1 {
                break;
            }
            result.push(ch);
        }
        result.push('+');
        result
    }
}

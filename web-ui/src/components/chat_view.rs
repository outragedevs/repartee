use leptos::prelude::*;

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

    Effect::new(move || {
        let _ = messages(); // track message changes
        // Scroll to bottom after DOM update.
        if let Some(el) = chat_ref.get() {
            el.set_scroll_top(el.scroll_height());
        }
    });

    view! {
        <div class="chat-area">
            <div class="chat-messages" node_ref=chat_ref>
                {move || {
                    let nick_self = our_nick();
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

                        let ts = format_timestamp(msg.timestamp);

                        if is_event || is_action || is_notice {
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
                            let nick = msg.nick.unwrap_or_default();
                            let mode = msg.nick_mode.unwrap_or_default();
                            let styled = render_styled_text(&msg.text);
                            view! {
                                <div class=class>
                                    <span class="ts">{ts}</span>
                                    <span class="nick">
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

fn format_timestamp(ts: i64) -> String {
    let hours = (ts / 3600) % 24;
    let minutes = (ts % 3600) / 60;
    format!("{hours:02}:{minutes:02}")
}

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
                    messages().unwrap_or_default().into_iter().map(|msg| {
                        let is_event = msg.msg_type == "event";
                        let is_action = msg.msg_type == "action";
                        let is_notice = msg.msg_type == "notice";

                        let class = if msg.highlight && msg.nick.is_some() {
                            "chat-line mention"
                        } else if is_event {
                            "chat-line event"
                        } else if is_action {
                            "chat-line event action"
                        } else {
                            "chat-line"
                        };

                        let ts = format_timestamp(msg.timestamp);

                        if is_event || is_action || is_notice {
                            // Events/actions: no nick column, span full width.
                            let styled = render_styled_text(&msg.text);
                            view! {
                                <div class=class>
                                    <span class="ts">{ts}</span>
                                    <span>{styled}</span>
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
    // Simple HH:MM format.
    let secs = ts;
    let hours = (secs / 3600) % 24;
    let minutes = (secs % 3600) / 60;
    format!("{hours:02}:{minutes:02}")
}

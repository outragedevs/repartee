use leptos::prelude::*;

use crate::state::AppState;

#[component]
pub fn ChatView() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();

    let messages = move || {
        let active_id = state.active_buffer.get()?;
        let msgs = state.messages.get();
        msgs.get(&active_id).cloned()
    };

    view! {
        <div class="chat-area">
            <div class="chat-messages">
                {move || {
                    messages().unwrap_or_default().into_iter().map(|msg| {
                        let is_event = matches!(msg.msg_type.as_str(), "event");
                        let is_action = matches!(msg.msg_type.as_str(), "action");
                        let is_notice = matches!(msg.msg_type.as_str(), "notice");

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
                            view! {
                                <div class=class>
                                    <span class="ts">{ts}</span>
                                    <span>{msg.text}</span>
                                </div>
                            }.into_any()
                        } else {
                            // Regular message: timestamp | right-aligned nick❯ | text
                            let nick = msg.nick.unwrap_or_default();
                            let mode = msg.nick_mode.unwrap_or_default();
                            view! {
                                <div class=class>
                                    <span class="ts">{ts}</span>
                                    <span class="nick">
                                        <span class="mode">{mode}</span>
                                        <span class="name">{nick}</span>
                                        <span class="sep">"❯"</span>
                                    </span>
                                    <span class="text">{msg.text}</span>
                                </div>
                            }.into_any()
                        }
                    }).collect::<Vec<_>>()
                }}
            </div>
        </div>
    }
}

fn format_timestamp(ts: i64) -> String {
    // Simple HH:MM format. Full configurability comes in Phase 3.
    let secs = ts;
    let hours = (secs / 3600) % 24;
    let minutes = (secs % 3600) / 60;
    format!("{hours:02}:{minutes:02}")
}

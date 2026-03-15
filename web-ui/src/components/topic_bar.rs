use leptos::prelude::*;

use crate::state::AppState;

#[component]
pub fn TopicBar() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();

    let active_buf = move || {
        let active_id = state.active_buffer.get()?;
        state.buffers.get().into_iter().find(|b| b.id == active_id)
    };

    view! {
        <div class="topic-bar">
            {move || {
                if let Some(buf) = active_buf() {
                    let topic = buf.topic.unwrap_or_default();
                    view! {
                        <span style="color: var(--accent); font-weight: bold;">{buf.name.clone()}</span>
                        <span style="color: var(--fg-muted); margin: 0 6px;">" \u{2014} "</span>
                        <span>{topic}</span>
                    }.into_any()
                } else {
                    view! { <span>"repartee"</span> }.into_any()
                }
            }}
        </div>
    }
}

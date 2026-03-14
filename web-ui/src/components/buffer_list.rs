use leptos::prelude::*;

use crate::protocol::WebCommand;
use crate::state::AppState;

#[component]
pub fn BufferList() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();

    let grouped = move || {
        let buffers = state.buffers.get();
        let active_id = state.active_buffer.get();

        // Group by connection_id, maintaining order.
        let mut groups: Vec<(String, Vec<BufferView>)> = Vec::new();
        let mut current_conn = String::new();
        let mut num = 1u32;

        for buf in &buffers {
            if buf.connection_id != current_conn {
                current_conn = buf.connection_id.clone();
                // Find connection label.
                let label = state
                    .connections
                    .get_untracked()
                    .iter()
                    .find(|c| c.id == buf.connection_id)
                    .map_or_else(|| buf.connection_id.clone(), |c| c.label.clone());
                groups.push((label, Vec::new()));
            }

            // Skip server buffers from numbered list.
            if buf.buffer_type == "server" {
                continue;
            }

            let is_active = active_id.as_deref() == Some(&buf.id);
            if let Some((_, items)) = groups.last_mut() {
                items.push(BufferView {
                    id: buf.id.clone(),
                    name: buf.name.clone(),
                    num,
                    activity: buf.activity,
                    is_active,
                });
            }
            num += 1;
        }
        groups
    };

    view! {
        <div class="buffer-list">
            <For
                each=grouped
                key=|(label, _)| label.clone()
                let:group
            >
                {
                    let (label, items) = group;
                    view! {
                        <div class="network-header">{label}</div>
                        <For
                            each=move || items.clone()
                            key=|item| item.id.clone()
                            let:item
                        >
                            <BufferItem item=item />
                        </For>
                    }
                }
            </For>
        </div>
    }
}

#[derive(Clone)]
struct BufferView {
    id: String,
    name: String,
    num: u32,
    activity: u8,
    is_active: bool,
}

#[component]
fn BufferItem(item: BufferView) -> impl IntoView {
    let state = use_context::<AppState>().unwrap();
    let id = item.id.clone();
    let class = format!(
        "buffer-item{}{}",
        if item.is_active { " active" } else { "" },
        if item.activity > 0 {
            format!(" activity-{}", item.activity)
        } else {
            String::new()
        }
    );

    let on_click = move |_| {
        state.active_buffer.set(Some(id.clone()));
        // Request messages for the buffer.
        crate::ws::send_command(&WebCommand::FetchMessages {
            buffer_id: id.clone(),
            limit: 50,
            before: None,
        });
        // Mark as read.
        crate::ws::send_command(&WebCommand::MarkRead {
            buffer_id: id.clone(),
            up_to: chrono::Utc::now().timestamp(),
        });
    };

    view! {
        <div class=class on:click=on_click>
            <span class="num">{item.num}"."</span>
            " "
            <span class="name">{item.name}</span>
        </div>
    }
}

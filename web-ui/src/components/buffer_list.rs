use leptos::prelude::*;

use crate::protocol::WebCommand;
use crate::state::AppState;

#[component]
pub fn BufferList() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();

    let grouped = move || {
        let buffers = state.buffers.get();

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

            if let Some((_, items)) = groups.last_mut() {
                items.push(BufferView {
                    id: buf.id.clone(),
                    name: buf.name.clone(),
                    buffer_type: buf.buffer_type.clone(),
                    num,
                });
            }
            // Server buffers don't consume a number.
            if buf.buffer_type != "server" {
                num += 1;
            }
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
                            {
                                let id = item.id.clone();
                                let id2 = item.id.clone();
                                let name = item.name.clone();
                                let num = item.num;
                                let buf_type = item.buffer_type.clone();

                                let class = move || {
                                    let is_active = state.active_buffer.get().as_deref() == Some(&id);
                                    let activity = state.buffers.get().iter()
                                        .find(|b| b.id == id)
                                        .map_or(0u8, |b| b.activity);
                                    let type_class = match buf_type.as_str() {
                                        "server" => " type-server",
                                        "query" => " type-query",
                                        "dcc_chat" => " type-dcc",
                                        _ => "",
                                    };
                                    format!(
                                        "buffer-item{}{}{type_class}",
                                        if is_active { " active" } else { "" },
                                        if activity > 0 { format!(" activity-{activity}") } else { String::new() }
                                    )
                                };

                                let on_click = move |_| {
                                    state.active_buffer.set(Some(id2.clone()));
                                    // Sync to TUI — server changes active buffer for both.
                                    crate::ws::send_command(&WebCommand::SwitchBuffer {
                                        buffer_id: id2.clone(),
                                    });
                                    crate::ws::send_command(&WebCommand::MarkRead {
                                        buffer_id: id2.clone(),
                                        up_to: chrono::Utc::now().timestamp(),
                                    });
                                };

                                let is_server = item.buffer_type == "server";
                                view! {
                                    <div class=class on:click=on_click>
                                        {if is_server {
                                            view! { <span class="name status-name">"(status)"</span> }.into_any()
                                        } else {
                                            view! {
                                                <span class="num">{num}"."</span>
                                                " "
                                                <span class="name">{name.clone()}</span>
                                            }.into_any()
                                        }}
                                    </div>
                                }
                            }
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
    buffer_type: String,
    num: u32,
}

use leptos::prelude::*;

use crate::protocol::WebCommand;
use crate::state::AppState;

/// A flat list entry — either a network header or a buffer item.
#[derive(Clone)]
enum ListEntry {
    Header { label: String, conn_id: String },
    Buffer(BufferView),
}

impl ListEntry {
    fn key(&self) -> String {
        match self {
            Self::Header { conn_id, .. } => format!("h:{conn_id}"),
            Self::Buffer(b) => b.id.clone(),
        }
    }
}

#[derive(Clone)]
struct BufferView {
    id: String,
    name: String,
    buffer_type: String,
    num: u32,
}

#[component]
pub fn BufferList() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();

    // Flat list of headers + buffers — single <For> avoids nested reuse bugs.
    let flat_items = move || {
        let buffers = state.buffers.get();
        let connections = state.connections.get_untracked();
        let mut items = Vec::new();
        let mut current_conn = String::new();
        let mut num = 1u32;

        for buf in &buffers {
            if buf.connection_id != current_conn {
                current_conn = buf.connection_id.clone();
                let label = connections
                    .iter()
                    .find(|c| c.id == buf.connection_id)
                    .map_or_else(|| buf.connection_id.clone(), |c| c.label.clone());
                items.push(ListEntry::Header {
                    label,
                    conn_id: buf.connection_id.clone(),
                });
            }

            items.push(ListEntry::Buffer(BufferView {
                id: buf.id.clone(),
                name: buf.name.clone(),
                buffer_type: buf.buffer_type.clone(),
                num,
            }));

            // Server buffers don't consume a number.
            if buf.buffer_type != "server" {
                num += 1;
            }
        }
        items
    };

    view! {
        <div class="buffer-list">
            <For
                each=flat_items
                key=|item| item.key()
                let:item
            >
                {match item {
                    ListEntry::Header { label, .. } => {
                        view! { <div class="network-header">{label}</div> }.into_any()
                    }
                    ListEntry::Buffer(buf) => {
                        let id = buf.id;
                        let name = buf.name;
                        let num = buf.num;
                        let buf_type = buf.buffer_type;
                        let is_server = buf_type == "server";
                        let id_for_click = id.clone();

                        let class = move || {
                            let is_active = state.active_buffer.get().as_deref() == Some(id.as_str());
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
                            state.active_buffer.set(Some(id_for_click.clone()));
                            crate::ws::send_command(&WebCommand::SwitchBuffer {
                                buffer_id: id_for_click.clone(),
                            });
                            crate::ws::send_command(&WebCommand::MarkRead {
                                buffer_id: id_for_click.clone(),
                                up_to: chrono::Utc::now().timestamp(),
                            });
                        };

                        if is_server {
                            view! {
                                <div class=class on:click=on_click>
                                    <span class="name status-name">"(status)"</span>
                                </div>
                            }.into_any()
                        } else {
                            view! {
                                <div class=class on:click=on_click>
                                    <span class="num">{num}"."</span>
                                    " "
                                    <span class="name">{name}</span>
                                </div>
                            }.into_any()
                        }
                    }
                }}
            </For>
        </div>
    }
}

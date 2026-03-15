use leptos::prelude::*;

use crate::protocol::WebCommand;
use crate::state::AppState;

#[component]
pub fn BufferList() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();

    view! {
        <div class="buffer-list">
            {move || {
                let buffers = state.buffers.get();
                let connections = state.connections.get_untracked();
                let active_id = state.active_buffer.get();
                let mut views: Vec<leptos::prelude::AnyView> = Vec::new();
                let mut current_conn = String::new();
                let mut num = 1u32;

                for buf in &buffers {
                    // Network header when connection changes.
                    if buf.connection_id != current_conn {
                        current_conn = buf.connection_id.clone();
                        let label = connections
                            .iter()
                            .find(|c| c.id == buf.connection_id)
                            .map_or_else(|| buf.connection_id.clone(), |c| c.label.clone());
                        views.push(
                            view! { <div class="network-header">{label}</div> }.into_any(),
                        );
                    }

                    let is_server = buf.buffer_type == "server";
                    let is_active = active_id.as_deref() == Some(buf.id.as_str());
                    let type_class = match buf.buffer_type.as_str() {
                        "server" => " type-server",
                        "query" => " type-query",
                        "dcc_chat" => " type-dcc",
                        _ => "",
                    };
                    let activity_class = match buf.activity {
                        0 => "",
                        1 => " activity-1",
                        2 => " activity-2",
                        3 => " activity-3",
                        4 => " activity-4",
                        _ => " activity-4",
                    };
                    let class = format!(
                        "buffer-item{}{activity_class}{type_class}",
                        if is_active { " active" } else { "" },
                    );

                    let id = buf.id.clone();
                    let name = buf.name.clone();
                    let current_num = num;

                    let on_click = move |_| {
                        state.active_buffer.set(Some(id.clone()));
                        crate::ws::send_command(&WebCommand::SwitchBuffer {
                            buffer_id: id.clone(),
                        });
                        crate::ws::send_command(&WebCommand::MarkRead {
                            buffer_id: id.clone(),
                            up_to: chrono::Utc::now().timestamp(),
                        });
                    };

                    if is_server {
                        views.push(
                            view! {
                                <div class=class on:click=on_click>
                                    <span class="name status-name">"(status)"</span>
                                </div>
                            }
                            .into_any(),
                        );
                    } else {
                        views.push(
                            view! {
                                <div class=class on:click=on_click>
                                    <span class="num">{current_num}"."</span>
                                    " "
                                    <span class="name">{name}</span>
                                </div>
                            }
                            .into_any(),
                        );
                        num += 1;
                    }
                }
                views
            }}
        </div>
    }
}

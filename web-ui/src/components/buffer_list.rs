use leptos::prelude::*;

use crate::protocol::WebCommand;
use crate::state::AppState;

#[component]
pub fn BufferList() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();

    view! {
        <div class="buffer-list">
            <button
                class="add-network-btn"
                title="Add a new server"
                on:click=move |_| state.wizard_open.set(true)
            >"+ Add network"</button>
            {move || {
                let buffers = state.buffers.get();
                let connections = state.connections.get();
                let active_id = state.active_buffer.get();
                let mut views: Vec<leptos::prelude::AnyView> = Vec::new();

                for (idx, buf) in buffers.iter().enumerate() {
                    let is_server = buf.buffer_type == "server";
                    let is_active = active_id.as_deref() == Some(buf.id.as_str());
                    let type_class = match buf.buffer_type.as_str() {
                        "server" => " type-server",
                        "query" => " type-query",
                        "dcc_chat" => " type-dcc",
                        "mentions" => " type-mentions",
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
                    let current_num = u32::try_from(idx + 1).unwrap_or(0);

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

                    // Server buffers display the connection label —
                    // they serve as both the network grouping and status window.
                    let display_name = if is_server {
                        connections
                            .iter()
                            .find(|c| c.id == buf.connection_id)
                            .map_or_else(|| name.clone(), |c| c.label.clone())
                    } else {
                        name
                    };
                    views.push(
                        view! {
                            <div class=class on:click=on_click>
                                <span class="num">{current_num}"."</span>
                                " "
                                <span class="name">{display_name}</span>
                            </div>
                        }
                        .into_any(),
                    );
                }
                views
            }}
        </div>
    }
}

use leptos::prelude::*;

use crate::state::AppState;

#[component]
pub fn BufferList() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();

    view! {
        <div class="buffer-list">
            <button
                type="button"
                class="add-network-btn"
                title="Add a new server"
                on:click=move |_| state.wizard_open.set(true)
            >"+ Add network"</button>
            {move || {
                let buffers = state.buffers.get();
                let connections = state.connections.get();
                let active_id = state.active_buffer.get();
                let mut views: Vec<leptos::prelude::AnyView> = Vec::new();

                for (current_num, buf) in crate::state::numbered_buffers(&buffers) {
                    let is_server = buf.buffer_type == "server";
                    let is_active = active_id.as_deref() == Some(buf.id.as_str());
                    let conn = connections.iter().find(|c| c.id == buf.connection_id);
                    // Dim every buffer of a disconnected network — at a
                    // glance the user sees which side of a netsplit or
                    // dropped link each window belongs to.
                    let is_offline = conn.is_some_and(|c| !c.connected);
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
                        _ => " activity-4",
                    };
                    let class = format!(
                        "buffer-item{}{activity_class}{type_class}{}",
                        if is_active { " active" } else { "" },
                        if is_offline { " offline" } else { "" },
                    );

                    let id = buf.id.clone();
                    let name = buf.name.clone();

                    let on_click = move |_| state.switch_to_buffer(&id);

                    // Unread badge — hidden for the active buffer (its
                    // content is on screen) and for zero counts; capped so a
                    // flooded channel doesn't blow the row width.
                    let unread = buf.unread_count;
                    let badge = (!is_active && unread > 0).then(|| {
                        let label = if unread > 99 {
                            "99+".to_string()
                        } else {
                            unread.to_string()
                        };
                        view! { <span class="unread-badge">{label}</span> }
                    });

                    // Server buffers display the connection label —
                    // they serve as both the network grouping and status window.
                    let display_name = if is_server {
                        conn.map_or_else(|| name.clone(), |c| c.label.clone())
                    } else {
                        name
                    };
                    views.push(
                        view! {
                            <button type="button" class=class on:click=on_click
                                aria-current=is_active.then_some("page")>
                                <span class="num">{current_num}"."</span>
                                " "
                                <span class="name">{display_name}</span>
                                {badge}
                            </button>
                        }
                        .into_any(),
                    );
                }
                views
            }}
        </div>
    }
}

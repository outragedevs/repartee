use leptos::prelude::*;

use crate::state::AppState;

fn current_time() -> String {
    let date = js_sys::Date::new_0();
    let h = date.get_hours();
    let m = date.get_minutes();
    format!("{h:02}:{m:02}")
}

#[component]
pub fn StatusLine() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();

    let (time_str, set_time_str) = signal(current_time());

    // Update the clock every 30 seconds.
    leptos::task::spawn_local(async move {
        loop {
            gloo_timers::future::sleep(std::time::Duration::from_secs(30)).await;
            set_time_str.set(current_time());
        }
    });

    let active_buf = move || {
        let active_id = state.active_buffer.get()?;
        state.buffers.get().into_iter().find(|b| b.id == active_id)
    };

    let active_conn = move || {
        let buf = active_buf()?;
        state
            .connections
            .get()
            .into_iter()
            .find(|c| c.id == buf.connection_id)
    };

    let activity_items = move || {
        state
            .buffers
            .get()
            .iter()
            .enumerate()
            .filter(|(_, b)| {
                b.activity > 0
                    && state
                        .active_buffer
                        .get()
                        .as_deref() != Some(&b.id)
            })
            .map(|(i, b)| (i + 1, b.activity))
            .collect::<Vec<_>>()
    };

    view! {
        <div class="status-line">
            <span class="bracket">"["</span>
            // Time
            <span class="muted">{time_str}</span>
            <span class="sep">"|"</span>
            // Nick
            {move || active_conn().map(|c| view! {
                <span class="nick">{c.nick}</span>
            })}
            <span class="sep">"|"</span>
            // Channel
            {move || active_buf().map(|b| view! {
                <span class="nick">{b.name}</span>
            })}
            // Activity
            {move || {
                let items = activity_items();
                if items.is_empty() {
                    return None;
                }
                Some(view! {
                    <span class="sep">"|"</span>
                    <span class="muted">"Act: "</span>
                    {items.iter().enumerate().map(|(i, (num, level))| {
                        let class = match level {
                            1 => "act-green",
                            2 => "act-red",
                            3 => "act-yellow",
                            4 => "act-purple",
                            _ => "muted",
                        };
                        let sep = if i > 0 { "," } else { "" };
                        view! {
                            <span class="sep">{sep}</span>
                            <span class=class>{num.to_string()}</span>
                        }
                    }).collect::<Vec<_>>()}
                })
            }}
            <span class="bracket">"]"</span>
        </div>
    }
}

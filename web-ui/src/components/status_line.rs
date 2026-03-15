use leptos::prelude::*;

use crate::state::AppState;

fn current_time() -> String {
    let date = js_sys::Date::new_0();
    let h = date.get_hours();
    let m = date.get_minutes();
    let s = date.get_seconds();
    format!("{h:02}:{m:02}:{s:02}")
}

#[component]
pub fn StatusLine() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();

    let (time_str, set_time_str) = signal(current_time());

    // Update the clock every second. Cancelled via on_cleanup on unmount.
    let clock_alive = StoredValue::new(true);
    on_cleanup(move || clock_alive.set_value(false));
    leptos::task::spawn_local(async move {
        loop {
            gloo_timers::future::sleep(std::time::Duration::from_secs(1)).await;
            if !clock_alive.get_value() {
                break;
            }
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

    // Activity numbers — skip server buffers, use global sequential numbering.
    let activity_items = move || {
        let active_id = state.active_buffer.get();
        let buffers = state.buffers.get();
        let mut num = 1u32;
        let mut items = Vec::new();
        for b in &buffers {
            if b.buffer_type == "server" {
                continue;
            }
            let current_num = num;
            num += 1;
            if active_id.as_deref() == Some(b.id.as_str()) {
                continue;
            }
            if b.activity == 0 {
                continue;
            }
            items.push((current_num, b.activity));
        }
        items
    };

    view! {
        <div class="status-line">
            <span class="bracket">"["</span>
            <span class="muted">{time_str}</span>
            <span class="sep">"|"</span>
            // Nick (+modes)
            {move || active_conn().map(|c| {
                let modes = if c.user_modes.is_empty() {
                    String::new()
                } else {
                    format!("(+{})", c.user_modes)
                };
                view! {
                    <span class="nick">{c.nick}</span>
                    <span class="muted">{modes}</span>
                }
            })}
            <span class="sep">"|"</span>
            // Channel (+modes)
            {move || active_buf().map(|b| {
                let modes = b.modes.as_deref()
                    .filter(|m| !m.is_empty())
                    .map(|m| format!("(+{m})"))
                    .unwrap_or_default();
                view! {
                    <span class="nick">{b.name}</span>
                    <span class="muted">{modes}</span>
                }
            })}
            // Lag
            {move || {
                let conn = active_conn()?;
                let lag = conn.lag?;
                #[expect(clippy::cast_precision_loss, reason = "u64 lag ms to f64 seconds, precision loss acceptable")]
                let secs = lag as f64 / 1000.0;
                Some(view! {
                    <span class="sep">"|"</span>
                    <span class="muted">"Lag: "</span>
                    <span class="nick">{format!("{secs:.1}s")}</span>
                })
            }}
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

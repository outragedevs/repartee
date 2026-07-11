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

    // Activity numbers — must use the exact numbering the buffer list
    // displays (position in the same sorted vec, 1-based, nothing skipped).
    // The old version skipped server buffers while counting, so with one
    // network every Act number was off by one ("[Act:3]" while the traffic
    // was on window 4), and off by N with N networks.
    let activity_items = move || {
        let active_id = state.active_buffer.get();
        state
            .buffers
            .with(|bufs| activity_numbers(bufs, active_id.as_deref()))
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
                    {items.into_iter().enumerate().map(|(i, (num, level, id))| {
                        // Clamp unknown levels to the highest tier, matching
                        // the buffer list's `activity-4` fallback.
                        let class = match level {
                            1 => "act-green",
                            2 => "act-red",
                            3 => "act-yellow",
                            _ => "act-purple",
                        };
                        let sep = if i > 0 { "," } else { "" };
                        let on_click = move |_| state.switch_to_buffer(&id);
                        view! {
                            <span class="sep">{sep}</span>
                            <span
                                class=format!("act-num {class}")
                                title="Jump to this window"
                                on:click=on_click
                            >{num.to_string()}</span>
                        }
                    }).collect::<Vec<_>>()}
                })
            }}
            <span class="bracket">"]"</span>
        </div>
    }
}

/// `(display_number, activity_level, buffer_id)` for every non-active buffer
/// with pending activity. `display_number` is the buffer's 1-based position in
/// the sorted buffer vec — identical to the number the buffer list renders
/// next to it, which is the whole point: "Act: 4" must mean "window 4".
fn activity_numbers(
    buffers: &[crate::protocol::BufferMeta],
    active_id: Option<&str>,
) -> Vec<(u32, u8, String)> {
    crate::state::numbered_buffers(buffers)
        .filter(|(_, b)| b.activity != 0 && active_id != Some(b.id.as_str()))
        .map(|(num, b)| (num, b.activity, b.id.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::BufferMeta;

    fn buf(id: &str, buffer_type: &str, activity: u8) -> BufferMeta {
        BufferMeta {
            id: id.to_string(),
            connection_id: "net".to_string(),
            name: id.to_string(),
            buffer_type: buffer_type.to_string(),
            topic: None,
            unread_count: 0,
            activity,
            nick_count: 0,
            modes: None,
        }
    }

    #[test]
    fn numbers_match_buffer_list_positions() {
        // The tester's bug: activity on the 4th listed window reported as
        // "Act: 3" because server buffers were skipped while counting. The
        // number must be the 1-based position in the same vec the buffer
        // list renders.
        let buffers = vec![
            buf("mentions", "mentions", 0),
            buf("srv", "server", 0),
            buf("#a", "channel", 0),
            buf("#b", "channel", 2),
        ];
        let items = activity_numbers(&buffers, Some("#a"));
        assert_eq!(items, vec![(4, 2, "#b".to_string())]);
    }

    #[test]
    fn server_buffers_with_activity_are_listed() {
        let buffers = vec![buf("srv", "server", 1), buf("#a", "channel", 0)];
        let items = activity_numbers(&buffers, Some("#a"));
        assert_eq!(items, vec![(1, 1, "srv".to_string())]);
    }

    #[test]
    fn active_buffer_and_idle_buffers_are_excluded() {
        let buffers = vec![
            buf("#a", "channel", 3), // active — excluded even with activity
            buf("#b", "channel", 0), // idle — excluded
            buf("#c", "channel", 4),
        ];
        let items = activity_numbers(&buffers, Some("#a"));
        assert_eq!(items, vec![(3, 4, "#c".to_string())]);
    }
}

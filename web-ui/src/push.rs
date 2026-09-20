use leptos::prelude::*;
use wasm_bindgen::prelude::*;

use crate::{protocol::WebCommand, state::AppState};

#[wasm_bindgen(module = "/push/client.js")]
extern "C" {
    #[wasm_bindgen(js_name = openSettings)]
    fn open_settings();
    #[wasm_bindgen(js_name = update)]
    fn update_snapshot(value: &str);
}

#[component]
pub fn PushButton() -> impl IntoView {
    view! { <button type="button" class="appearance-btn" title="Notifications" on:click=move |_| open_settings()>"Notifications"</button> }
}

#[derive(serde::Deserialize)]
struct OpenTarget {
    buffer_id: String,
    target: String,
    channel: bool,
}

pub fn install(state: AppState) {
    Effect::new(move || {
        let snapshot = serde_json::json!({
            "appName": crate::constants::APP_NAME,
            "authenticated": state.authenticated.get() && state.connected.get(),
            "sessionHint": state.session_hint.get(),
            "connections": state.connections.get(), "buffers": state.buffers.get(),
        });
        update_snapshot(&snapshot.to_string());
    });
    let listener = leptos::leptos_dom::helpers::window_event_listener(leptos::ev::Custom::<web_sys::CustomEvent>::new("push-open"), move |event| {
        if !state.authenticated.get_untracked() { return; }
        let Some(value) = event.detail().as_string() else { return; };
        let Ok(target) = serde_json::from_str::<OpenTarget>(&value) else { return; };
        if !state.buffers.with_untracked(|buffers| buffers.iter().any(|buffer| buffer.id == target.buffer_id)) { return; }
        if target.target.len() > 512 || target.target.contains(',') || target.target.chars().any(|ch| ch.is_whitespace() || ch.is_control()) { return; }
        state.switch_to_buffer(&target.buffer_id);
        if !target.target.is_empty() {
            let command = if target.channel { "join" } else { "query" };
            crate::ws::send_command(&WebCommand::RunCommand { buffer_id: target.buffer_id, text: format!("/{command} {}", target.target) });
        }
    });
    on_cleanup(move || listener.remove());
}

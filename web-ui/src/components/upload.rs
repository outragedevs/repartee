use leptos::prelude::*;

use crate::state::AppState;

#[component]
pub fn UploadButton(on_url: Callback<String>) -> impl IntoView {
    let state = use_context::<AppState>().unwrap();
    let picker = NodeRef::<leptos::html::Input>::new();
    let busy = RwSignal::new(false);
    let status = RwSignal::new(String::new());
    let on_change = move |event: web_sys::Event| {
        let element = event_target::<web_sys::HtmlInputElement>(&event);
        let file = element.files().and_then(|files| files.get(0));
        element.set_value("");
        let Some(file) = file else { return; };
        if busy.get_untracked() { return; }
        let Some(buffer_id) = state.active_buffer.get_untracked() else { return; };
        if file.size() <= 0.0 || file.size() > 67_108_864.0 {
            status.set("Choose a file between 1 byte and 64 MiB".into());
            return;
        }
        busy.set(true);
        status.set("Uploading…".into());
        wasm_bindgen_futures::spawn_local(async move {
            let result = upload(&buffer_id, file).await;
            busy.set(false);
            match result {
                Ok(url) => {
                    status.set("Uploaded; link saved in the original conversation".into());
                    if state.active_buffer.get_untracked().as_deref() == Some(&buffer_id) {
                        on_url.run(url);
                    }
                }
                Err(error) => status.set(error),
            }
        });
    };
    view! {
        <input type="file" node_ref=picker style="display:none" on:change=on_change />
        <button type="button" class="input-emote-btn" aria-label="Upload file"
            title="Upload a file through the bouncer" disabled=move || busy.get()
            on:click=move |_| { if let Some(input) = picker.get() { input.click(); } }>
            {move || if busy.get() { "…" } else { "+" }}
        </button>
        <span role="status" class="upload-status" style="max-width:18em;font-size:0.8em;overflow-wrap:anywhere">
            {move || status.get()}
        </span>
    }
}

async fn upload(buffer_id: &str, file: web_sys::File) -> Result<String, String> {
    let filename: String = js_sys::encode_uri_component(&file.name()).into();
    let buffer: String = js_sys::encode_uri_component(buffer_id).into();
    let content_type = file.type_();
    let mime = if content_type.is_empty() { "application/octet-stream" } else { &content_type };
    let response = gloo_net::http::Request::post(&format!("/api/upload?buffer_id={buffer}&filename={filename}"))
        .header("X-Upload-Intent", "1")
        .header("Content-Type", mime)
        .body(file).map_err(|_| "Cannot prepare file upload".to_string())?
        .send().await.map_err(|_| "Upload connection lost; check the original conversation before retrying".to_string())?;
    let status = response.status();
    let text = response.text().await.map_err(|_| "Cannot read upload result".to_string())?;
    if status == 201 { Ok(text) } else if text.is_empty() {
        Err(format!("Upload failed (HTTP {status})"))
    } else { Err(text) }
}

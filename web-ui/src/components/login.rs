use leptos::prelude::*;

use crate::state::AppState;

#[component]
pub fn Login() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();
    let (password, set_password) = signal(String::new());
    let (error, set_error) = signal(Option::<String>::None);
    let (loading, set_loading) = signal(false);

    let do_submit = Callback::new(move |_: ()| {
        let pw = password.get();
        if pw.is_empty() {
            return;
        }
        set_loading.set(true);
        set_error.set(None);

        let state = state.clone();
        leptos::task::spawn_local(async move {
            match do_login(&pw).await {
                Ok(token) => {
                    state.token.set(Some(token));
                    crate::ws::connect(&state);
                }
                Err(e) => {
                    set_error.set(Some(e));
                }
            }
            set_loading.set(false);
        });
    });

    view! {
        <div class="login-page">
            <h1 style="color: var(--accent); font-size: 24px;">"repartee"</h1>
            <p style="color: var(--fg-muted); font-size: 14px;">"web frontend"</p>
            <div class="login-box">
                <input
                    type="password"
                    placeholder="Password"
                    prop:value=password
                    on:input=move |ev| set_password.set(event_target_value(&ev))
                    on:keydown=move |ev| {
                        if ev.key() == "Enter" { do_submit.run(()) }
                    }
                />
                <button
                    on:click=move |_| do_submit.run(())
                    disabled=loading
                >
                    {move || if loading.get() { "Connecting..." } else { "Login" }}
                </button>
                {move || error.get().map(|e| view! { <p class="error">{e}</p> })}
            </div>
        </div>
    }
}

async fn do_login(password: &str) -> Result<String, String> {
    let window = web_sys::window().unwrap();
    let location = window.location();
    let origin = location.origin().unwrap();
    let url = format!("{origin}/api/login");

    let body = serde_json::json!({ "password": password });

    let resp = gloo_net::http::Request::post(&url)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .map_err(|e| format!("request error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("network error: {e}"))?;

    if resp.status() == 429 {
        return Err("Rate limited — try again later".to_string());
    }

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("parse error: {e}"))?;

    if resp.ok() {
        json["token"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| "no token in response".to_string())
    } else {
        Err(json["error"]
            .as_str()
            .unwrap_or("login failed")
            .to_string())
    }
}

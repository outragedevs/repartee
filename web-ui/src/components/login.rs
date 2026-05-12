use leptos::prelude::*;

use crate::state::AppState;

#[component]
pub fn Login() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();
    let (username, set_username) = signal(String::from("repartee"));
    let (password, set_password) = signal(String::new());
    let (error, set_error) = signal(Option::<String>::None);
    let (loading, set_loading) = signal(false);

    // Pre-fill the username from the server's configured value (so users
    // who set `web.username` see their preferred login appear in
    // password-manager prompts).
    Effect::new(move |_| {
        leptos::task::spawn_local(async move {
            if let Some(name) = fetch_login_info().await {
                set_username.set(name);
            }
        });
    });

    let do_submit = Callback::new(move |_: ()| {
        let pw = password.get();
        if pw.is_empty() {
            return;
        }
        let u = username.get();
        set_loading.set(true);
        set_error.set(None);

        leptos::task::spawn_local(async move {
            match do_login(&u, &pw).await {
                Ok(()) => {
                    state.session_hint.set(true);
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
            <form
                class="login-box"
                on:submit=move |ev| {
                    ev.prevent_default();
                    do_submit.run(());
                }
            >
                <input
                    type="text"
                    name="username"
                    placeholder="Username"
                    autocomplete="username"
                    prop:value=username
                    on:input=move |ev| set_username.set(event_target_value(&ev))
                />
                <input
                    type="password"
                    name="password"
                    placeholder="Password"
                    autocomplete="current-password"
                    prop:value=password
                    on:input=move |ev| set_password.set(event_target_value(&ev))
                />
                <button type="submit" disabled=loading>
                    {move || if loading.get() { "Connecting..." } else { "Login" }}
                </button>
                {move || error.get().map(|e| view! { <p class="error">{e}</p> })}
            </form>
        </div>
    }
}

async fn do_login(username: &str, password: &str) -> Result<(), String> {
    let window = web_sys::window().ok_or("no window object")?;
    let location = window.location();
    let origin = location.origin().map_err(|_| "failed to get origin")?;
    let url = format!("{origin}/api/login");

    let body = serde_json::json!({ "username": username, "password": password });

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

    let json: serde_json::Value = resp.json().await.map_err(|e| format!("parse error: {e}"))?;

    if resp.ok() {
        Ok(())
    } else {
        Err(json["error"].as_str().unwrap_or("login failed").to_string())
    }
}

/// Best-effort fetch of `/api/login_info` to pre-fill the username field.
/// Returns `None` if the server is unreachable or the response is malformed —
/// the form keeps its hard-coded "repartee" default in that case.
async fn fetch_login_info() -> Option<String> {
    let window = web_sys::window()?;
    let origin = window.location().origin().ok()?;
    let url = format!("{origin}/api/login_info");
    let resp = gloo_net::http::Request::get(&url).send().await.ok()?;
    if !resp.ok() {
        return None;
    }
    let json: serde_json::Value = resp.json().await.ok()?;
    json["username"].as_str().map(str::to_owned)
}

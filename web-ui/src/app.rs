use leptos::prelude::*;

use crate::components::layout::Layout;
use crate::components::login::Login;
use crate::state::AppState;

#[component]
pub fn App() -> impl IntoView {
    let state = AppState::new();
    provide_context(state.clone());

    // Save token to localStorage whenever it changes.
    Effect::new({
        let state = state.clone();
        move || {
            let token = state.token.get();
            if let Some(storage) = web_sys::window()
                .and_then(|w| w.local_storage().ok().flatten())
            {
                if let Some(ref t) = token {
                    let _ = storage.set_item("repartee-token", t);
                } else {
                    let _ = storage.remove_item("repartee-token");
                }
            }
        }
    });

    // Auto-connect if we have a saved token from a previous session.
    {
        let state = state.clone();
        let saved_token = web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .and_then(|s| s.get_item("repartee-token").ok().flatten());
        if let Some(token) = saved_token {
            state.token.set(Some(token));
            crate::ws::connect(&state);
        }
    }

    // Apply theme.
    Effect::new(move || {
        let theme = state.theme.get();
        if let Some(doc) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.document_element())
        {
            let _ = doc.set_attribute("data-theme", &theme);
        }
        if let Some(storage) = web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
        {
            let _ = storage.set_item("repartee-theme", &theme);
        }
    });

    let has_token = move || state.token.get().is_some();

    view! {
        <Show when=has_token fallback=Login>
            <Layout />
        </Show>
    }
}

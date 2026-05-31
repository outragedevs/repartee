use leptos::prelude::*;

use crate::components::layout::Layout;
use crate::components::login::Login;
use crate::state::AppState;

#[component]
pub fn App() -> impl IntoView {
    let state = AppState::new();
    provide_context(state);

    // Save the non-secret session hint to localStorage whenever it changes.
    Effect::new({
        move || {
            let session_hint = state.session_hint.get();
            if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten())
            {
                if session_hint {
                    let _ = storage.set_item("repartee-session", "1");
                } else {
                    let _ = storage.remove_item("repartee-session");
                }
            }
        }
    });

    // Auto-connect if we have a saved session hint from a previous session.
    {
        let saved_session = web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .and_then(|s| s.get_item("repartee-session").ok().flatten());
        if saved_session.is_some() {
            state.session_hint.set(true);
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
        if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
            let _ = storage.set_item("repartee-theme", &theme);
        }
    });

    view! {
        <Show when=move || state.authenticated.get() fallback=Login>
            <Layout />
        </Show>
    }
}

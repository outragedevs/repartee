use leptos::prelude::*;

use crate::components::layout::Layout;
use crate::components::login::Login;
use crate::state::AppState;

/// Root application component.
///
/// Shows login screen until authenticated, then renders the full IRC layout.
#[component]
pub fn App() -> impl IntoView {
    let state = AppState::new();
    provide_context(state.clone());

    // Apply theme from state to the document.
    Effect::new(move || {
        let theme = state.theme.get();
        if let Some(doc) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.document_element())
        {
            let _ = doc.set_attribute("data-theme", &theme);
        }
        // Persist to localStorage.
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

use leptos::prelude::*;

use crate::components::layout::Layout;
use crate::components::login::Login;
use crate::state::AppState;
use crate::ws::CommandSender;

#[component]
pub fn App() -> impl IntoView {
    let state = AppState::new();
    provide_context(state.clone());

    // Create the command channel — sender goes into context, receiver into WS loop on login.
    let (cmd_tx, cmd_rx) = futures::channel::mpsc::unbounded::<String>();
    provide_context(CommandSender(cmd_tx));
    // Store receiver in a signal so login can take it.
    let cmd_rx_cell = StoredValue::new(Some(cmd_rx));
    provide_context(cmd_rx_cell);

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

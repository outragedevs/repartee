use leptos::prelude::*;
use wasm_bindgen::JsCast;

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
        crate::state::store_or_remove("repartee-theme", Some(&theme));
    });

    // Apply + persist the appearance vars. The stylesheet reads
    // `var(--font-size, …)` / `var(--line-height, 1.35)`, so REMOVING the
    // inline property is what restores the stylesheet defaults — never write
    // a hardcoded fallback here. Line height without a local override follows
    // the server's `web.line_height` (this Effect is what makes that setting
    // actually take effect; it previously went nowhere).
    Effect::new(move || {
        let font_px = state.font_size_override.get();
        let line_h = state.line_height_override.get();
        let server_line_h = state.line_height.get();

        if let Some(root) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.document_element())
            .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
        {
            let style = root.style();
            match font_px {
                Some(px) => {
                    let _ = style.set_property("--font-size", &format!("{px}px"));
                }
                None => {
                    let _ = style.remove_property("--font-size");
                }
            }
            let effective_lh = line_h.unwrap_or(server_line_h);
            // Two decimals: the stepper works in 0.05 increments, and f32
            // arithmetic noise ("1.4000001") must not leak into the CSS.
            let _ = style.set_property("--line-height", &format!("{effective_lh:.2}"));
        }

        crate::state::store_or_remove(
            crate::state::FONT_SIZE_KEY,
            font_px.map(|v| v.to_string()).as_deref(),
        );
        crate::state::store_or_remove(
            crate::state::LINE_HEIGHT_KEY,
            line_h.map(|v| v.to_string()).as_deref(),
        );
    });

    view! {
        <Show when=move || state.authenticated.get() fallback=Login>
            <Layout />
        </Show>
    }
}

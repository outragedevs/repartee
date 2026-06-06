//! GG emote picker modal. Shows the built-in `:name:` GIF emotes as a filtered
//! thumbnail grid; selecting one splices `:name:` into the input at the caret.

use leptos::prelude::*;

use crate::state::AppState;

/// Indices into [`crate::emotes::EMOTE_NAMES`] whose name contains the
/// (case-insensitive) needle. Empty needle returns every index.
pub fn filter_emotes(needle: &str) -> Vec<usize> {
    let n = needle.trim().to_ascii_lowercase();
    crate::emotes::EMOTE_NAMES
        .iter()
        .enumerate()
        .filter(|(_, name)| n.is_empty() || name.to_ascii_lowercase().contains(&n))
        .map(|(i, _)| i)
        .collect()
}

#[component]
pub fn EmotePicker() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();
    let open = state.emote_picker_open;
    let filter = RwSignal::new(String::new());

    let close = move || {
        open.set(false);
        filter.set(String::new());
    };
    let pick = move |idx: usize| {
        if let Some(name) = crate::emotes::EMOTE_NAMES.get(idx) {
            state.pending_insert.set(Some(format!(":{name}: ")));
        }
        close();
    };

    view! {
        <Show when=move || open.get() fallback=|| ()>
            <div class="wizard-backdrop" on:click=move |_| close()></div>
            <div class="emote-picker-modal">
                <input
                    class="emote-picker-filter"
                    placeholder="filter emotes…"
                    autofocus=true
                    prop:value=move || filter.get()
                    on:input=move |ev| filter.set(event_target_value(&ev))
                    on:keydown=move |ev| {
                        match ev.key().as_str() {
                            "Escape" => close(),
                            "Enter" => {
                                ev.prevent_default();
                                if let Some(&idx) = filter_emotes(&filter.get()).first() {
                                    pick(idx);
                                }
                            }
                            _ => {}
                        }
                    }
                />
                <div class="emote-picker-grid">
                    {move || {
                        filter_emotes(&filter.get())
                            .into_iter()
                            .map(|idx| {
                                let name = crate::emotes::EMOTE_NAMES[idx];
                                let stem = crate::emotes::stem_for(name);
                                let stem = if stem.is_empty() { name } else { stem };
                                let src = format!("/emotes/{stem}.gif");
                                view! {
                                    <button
                                        class="emote-picker-cell"
                                        title=name
                                        on:click=move |_| pick(idx)
                                    >
                                        <img src=src loading="lazy" alt=name />
                                        <span class="emote-picker-name">{name}</span>
                                    </button>
                                }
                            })
                            .collect::<Vec<_>>()
                    }}
                </div>
            </div>
        </Show>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_empty_returns_all() {
        assert_eq!(filter_emotes("").len(), crate::emotes::EMOTE_NAMES.len());
    }

    #[test]
    fn filter_substring_narrows() {
        let all = filter_emotes("").len();
        let some = filter_emotes("usm").len();
        assert!(some <= all);
        assert!(some >= 1, "expected at least :usmiech:");
    }

    #[test]
    fn filter_is_case_insensitive() {
        assert_eq!(filter_emotes("USM").len(), filter_emotes("usm").len());
    }
}

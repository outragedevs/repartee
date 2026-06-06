//! UTF-8 Unicode emoji picker modal (desktop only). Category tabs + search;
//! selecting an emoji splices the literal character into the input at the caret.

use leptos::prelude::*;

use crate::state::AppState;

#[component]
pub fn EmojiPicker() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();
    let open = state.emoji_picker_open;
    let filter = RwSignal::new(String::new());
    let group = RwSignal::new(0usize);

    let close = move || {
        open.set(false);
        filter.set(String::new());
        group.set(0);
    };
    let pick = move |ch: &'static str| {
        state.pending_insert.set(Some(ch.to_string()));
        close();
    };

    view! {
        <Show when=move || open.get() fallback=|| ()>
            <div class="wizard-backdrop" on:click=move |_| close()></div>
            <div class="emoji-picker-modal">
                <input
                    class="emote-picker-filter"
                    placeholder="search emoji…"
                    autofocus=true
                    prop:value=move || filter.get()
                    on:input=move |ev| filter.set(event_target_value(&ev))
                    on:keydown=move |ev| {
                        match ev.key().as_str() {
                            "Escape" => close(),
                            "Enter" => {
                                ev.prevent_default();
                                let f = filter.get();
                                // Insert the first cell currently shown — search
                                // results when filtering, else the active group.
                                let first = if f.trim().is_empty() {
                                    crate::emoji::in_group(crate::emoji::GROUPS[group.get()].1)
                                        .first()
                                        .copied()
                                } else {
                                    crate::emoji::search(&f).first().copied()
                                };
                                if let Some(ch) = first {
                                    pick(ch);
                                }
                            }
                            _ => {}
                        }
                    }
                />
                <div class="emoji-picker-tabs">
                    {crate::emoji::GROUPS.iter().enumerate().map(|(i, (label, _))| {
                        view! {
                            <button
                                class="emoji-tab"
                                title=*label
                                on:click=move |_| { filter.set(String::new()); group.set(i); }
                            >{*label}</button>
                        }
                    }).collect::<Vec<_>>()}
                </div>
                <div class="emoji-picker-grid">
                    {move || {
                        let f = filter.get();
                        let items = if f.trim().is_empty() {
                            crate::emoji::in_group(crate::emoji::GROUPS[group.get()].1)
                        } else {
                            crate::emoji::search(&f)
                        };
                        items.into_iter().map(|ch| {
                            view! {
                                <button class="emoji-cell" on:click=move |_| pick(ch)>{ch}</button>
                            }
                        }).collect::<Vec<_>>()
                    }}
                </div>
            </div>
        </Show>
    }
}

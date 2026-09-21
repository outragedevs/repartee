use super::DraftField;
use crate::settings_model::collection::{CellKind, Collection};
use leptos::prelude::*;

#[component]
pub(super) fn CollectionControl(
    field: DraftField,
    collection_open: RwSignal<bool>,
) -> impl IntoView {
    let open = RwSignal::new(false);
    on_cleanup(move || {
        if open.get_untracked() {
            collection_open.set(false);
        }
    });
    Effect::new(move || {
        if !open.get() {
            collection_open.set(false);
        }
    });
    let value = field.value;
    let path = field.spec.path.clone();
    view! {
        <button class="wizard-btn s" type="button" disabled=move || collection_open.get() && !open.get() on:click=move |_| { let next = !open.get(); open.set(next); collection_open.set(next); }>
            {move || if open.get() { "Close editor".to_string() } else {
                let count = Collection::open(&path, &value.get()).map(|c| c.rows.len()).unwrap_or(0);
                format!("Edit list · {count} {}", if count == 1 { "entry" } else { "entries" })
            }}
        </button>
        <Show when=move || open.get()><CollectionEditor field=field.clone() open /></Show>
    }
}

#[component]
fn CollectionEditor(field: DraftField, open: RwSignal<bool>) -> impl IntoView {
    let parsed = Collection::open(&field.spec.path, &field.value.get_untracked());
    let error = RwSignal::new(parsed.as_ref().err().cloned());
    let collection = RwSignal::new(parsed.ok());
    let editing = RwSignal::new(None::<Option<usize>>);
    let key = RwSignal::new(String::new());
    let cells = RwSignal::new(Vec::<RwSignal<String>>::new());
    let start = move |index: Option<usize>| {
        collection.with_untracked(|collection| {
            if let Some(collection) = collection {
                let row = index
                    .and_then(|i| collection.rows.get(i))
                    .cloned()
                    .unwrap_or_else(|| collection.new_row());
                key.set(row.key.clone());
                cells.set(
                    collection
                        .cells(&row)
                        .into_iter()
                        .map(RwSignal::new)
                        .collect(),
                );
                editing.set(Some(index));
                error.set(None);
            }
        });
    };
    view! {
        <div class="settings-collection">
            <Show when=move || editing.get().is_none()>
                <Show when=move || collection.get().is_some_and(|c| c.optional)>
                    <label class="wizard-check"><input type="checkbox" prop:checked=move || collection.get().is_some_and(|c| c.inherited) on:change=move |ev| collection.update(|c| { if let Some(c) = c { c.inherited = event_target_checked(&ev); } }) />"Automatic selection (overrides the list)"</label>
                </Show>
                <div class="settings-collection-list">
                    <For each=move || collection.get().map(|c| c.rows.iter().enumerate().map(|(i,row)| (i,c.summary(row),c.ordered(),c.rows.len())).collect::<Vec<_>>()).unwrap_or_default()
                        key=|(i,label,_,count)| (*i,label.clone(),*count) children=move |(i, label, ordered, count)| view! {
                        <div class="settings-collection-row"><span>{label}</span><div>
                            <Show when=move || ordered>
                                <button type="button" class="wizard-btn s" aria-label="Move entry up" disabled=i==0 on:click=move |_| collection.update(|c| { if let Some(c) = c { c.rows.swap(i,i-1); } })>"↑"</button>
                                <button type="button" class="wizard-btn s" aria-label="Move entry down" disabled=i+1==count on:click=move |_| collection.update(|c| { if let Some(c) = c { c.rows.swap(i,i+1); } })>"↓"</button>
                            </Show>
                            <button type="button" class="wizard-btn s" on:click=move |_| start(Some(i))>"Edit"</button>
                            <button type="button" class="wizard-btn s" on:click=move |_| collection.update(|c| { if let Some(c) = c { c.rows.remove(i); } })>"Remove"</button>
                        </div></div>
                    } />
                </div>
                <div class="settings-collection-actions">
                    <button type="button" class="wizard-btn s" disabled=move || collection.get().is_none() on:click=move |_| start(None)>"Add entry"</button>
                    <button type="button" class="wizard-btn p" disabled=move || collection.get().is_none() on:click=move |_| {
                        if let Some(c) = collection.get_untracked() { field.value.set(c.serialize()); field.touched.set(true); open.set(false); }
                    }>"Apply list"</button>
                    <button type="button" class="wizard-btn s" on:click=move |_| open.set(false)>"Cancel"</button>
                </div>
            </Show>
            <Show when=move || editing.get().is_some()>
                {move || collection.get().and_then(|c| c.key_label).map(|label| view! { <label class="wizard-row"><span class="wizard-label">{label}</span><input type="text" prop:value=move || key.get() on:input=move |ev| key.set(event_target_value(&ev)) /></label> })}
                {move || collection.get().map(|c| c.columns.into_iter().zip(cells.get()).map(|(column, value)| {
                    let control = match column.kind {
                        CellKind::Toggle => view! { <input type="checkbox" prop:checked=move || value.get() == "true" on:change=move |ev| value.set(event_target_checked(&ev).to_string()) /> }.into_any(),
                        CellKind::List => view! { <textarea rows="3" prop:value=move || value.get() on:input=move |ev| value.set(event_target_value(&ev)) /> }.into_any(),
                        kind => view! { <input type=if kind == CellKind::Number { "number" } else { "text" } min="0" prop:value=move || value.get() on:input=move |ev| value.set(event_target_value(&ev)) /> }.into_any(),
                    };
                    view! { <label class="wizard-row"><span class="wizard-label">{column.label}</span>{control}</label> }
                }).collect_view())}
                <div class="settings-collection-actions">
                    <button type="button" class="wizard-btn p" on:click=move |_| {
                        let values = cells.get_untracked().iter().map(|v| v.get_untracked()).collect::<Vec<_>>();
                        collection.update(|c| { if let Some(c) = c {
                            match c.update_row(editing.get_untracked().flatten(), key.get_untracked(), &values) {
                                Ok(()) => { editing.set(None); error.set(None); }
                                Err(message) => error.set(Some(message)),
                            }
                        } });
                    }>"Apply entry"</button>
                    <button type="button" class="wizard-btn s" on:click=move |_| { editing.set(None); error.set(None); }>"Cancel entry"</button>
                </div>
            </Show>
            {move || error.get().map(|message| view! { <p class="settings-error" role="alert">{message}</p> })}
        </div>
    }
}

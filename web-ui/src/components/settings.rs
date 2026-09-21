#[path = "settings_collection.rs"]
mod collection;

use crate::protocol::WebCommand;
use crate::settings_model::{SECTIONS, SettingChange, SettingField, SettingKind};
use crate::state::AppState;
use leptos::prelude::*;

#[derive(Clone)]
struct DraftField {
    spec: SettingField,
    value: RwSignal<String>,
    touched: RwSignal<bool>,
}

pub fn open_settings(state: AppState) {
    state.settings_fields.set(Vec::new());
    state.settings_error.set(None);
    state.settings_saving.set(false);
    state.settings_open.set(true);
}

#[component]
pub fn SettingsButton() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();
    view! { <button type="button" class="settings-gear" title="Settings" aria-label="Open settings" on:click=move |_| open_settings(state)>"⚙"</button> }
}

#[component]
pub fn SettingsPanel() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();
    view! { <Show when=move || state.settings_open.get()><SettingsDialog /></Show> }
}

#[component]
fn SettingsDialog() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();
    Effect::new(move || {
        if state.connected.get() && state.settings_fields.get_untracked().is_empty() {
            crate::ws::send_command(&WebCommand::GetSettings);
        }
    });
    let section = RwSignal::new(0usize);
    let network = RwSignal::new(None::<String>);
    let search = RwSignal::new(String::new());
    let fields = RwSignal::new(Vec::<DraftField>::new());
    let help = RwSignal::new(None::<SettingField>);
    let collection_open = RwSignal::new(false);
    let committed = StoredValue::new(false);
    let previews_original = super::chat_view::previews_enabled_in_browser();
    let follow_original = crate::state::follow_tui_active_buffer();
    let local_previews = RwSignal::new(previews_original);
    let local_follow = RwSignal::new(follow_original);
    let font_original = state.font_size_override.get_untracked();
    let line_original = state.line_height_override.get_untracked();
    let saved_at_open = state.settings_saved.get_untracked();
    on_cleanup(move || {
        if !committed.get_value() {
            state.font_size_override.set(font_original);
            state.line_height_override.set(line_original);
        }
    });
    Effect::new(move || {
        fields.set(
            state
                .settings_fields
                .get()
                .into_iter()
                .map(|spec| DraftField {
                    value: RwSignal::new(spec.value.clone()),
                    touched: RwSignal::new(false),
                    spec,
                })
                .collect(),
        );
    });
    Effect::new(move || {
        if state.settings_saved.get() > saved_at_open {
            crate::state::store_or_remove(
                super::chat_view::IMAGE_PREVIEWS_TOGGLE_KEY,
                Some(if local_previews.get_untracked() {
                    "true"
                } else {
                    "false"
                }),
            );
            crate::state::store_or_remove(
                crate::state::FOLLOW_TUI_BUFFER_KEY,
                Some(if local_follow.get_untracked() {
                    "true"
                } else {
                    "false"
                }),
            );
            state.browser_preferences_revision.update(|v| *v += 1);
            committed.set_value(true);
            state.settings_open.set(false);
        }
    });
    let changes = move || {
        fields.with_untracked(|fields| {
            fields
                .iter()
                .filter_map(|f| {
                    let value = f.value.get_untracked();
                    (value != f.spec.value
                        || (f.spec.kind == SettingKind::Secret && f.touched.get_untracked()))
                    .then(|| SettingChange {
                        path: f.spec.path.clone(),
                        original: f.spec.value.clone(),
                        value,
                    })
                })
                .collect::<Vec<_>>()
        })
    };
    let save = move |_| {
        if !state.connected.get_untracked() {
            state
                .settings_error
                .set(Some("Reconnect before saving settings.".into()));
            return;
        }
        state.settings_error.set(None);
        state.settings_saving.set(true);
        crate::ws::send_command(&WebCommand::SaveSettings { changes: changes() });
    };
    let defaults = move |_| {
        let current = section.get_untracked();
        fields.with_untracked(|fields| {
            for f in fields.iter().filter(|f| f.spec.section == current) {
                if let Some(value) = &f.spec.default_value {
                    f.value.set(value.clone());
                    f.touched.set(true);
                }
            }
        });
        if current == 2 {
            local_previews.set(true);
        }
        if current == 9 {
            local_follow.set(true);
        }
        if current == 1 {
            state.font_size_override.set(None);
            state.line_height_override.set(None);
        }
    };
    let add_network = move |_| {
        if !changes().is_empty()
            || state.font_size_override.get_untracked() != font_original
            || state.line_height_override.get_untracked() != line_original
            || local_previews.get_untracked() != previews_original
            || local_follow.get_untracked() != follow_original
        {
            state.settings_error.set(Some(
                "Save or cancel your changes before adding a network.".into(),
            ));
            return;
        }
        state.settings_open.set(false);
        state.wizard_open.set(true);
    };
    view! {
        <div class="settings-overlay">
            <section class="wizard-modal settings-dialog" role="dialog" aria-modal="true" aria-label="Settings">
                <header class="wizard-head"><h3>"Settings"</h3><button class="wizard-x" type="button" disabled=move || state.settings_saving.get() on:click=move |_| state.settings_open.set(false) aria-label="Close settings">"×"</button></header>
                <input class="settings-search" type="search" disabled=move || collection_open.get() placeholder="Search settings" aria-label="Search settings" prop:value=move || search.get() on:input=move |ev| search.set(event_target_value(&ev)) />
                <div class="settings-body">
                    <nav aria-label="Settings categories">
                        {SECTIONS.iter().enumerate().map(|(i, label)| view! { <button type="button" disabled=move || collection_open.get() class:active=move || section.get() == i on:click=move |_| { section.set(i); search.set(String::new()); help.set(None); }>{*label}</button> }).collect_view()}
                    </nav>
                    <div class="settings-content">
                    <Show when=move || section.get() == 0 && search.get().is_empty()>
                        <nav class="settings-network-tabs" aria-label="Networks">
                            <button type="button" disabled=move || collection_open.get() class:active=move || network.get().is_none() on:click=move |_| { network.set(None); help.set(None); }>"General"</button>
                            <For each=move || {
                                let mut networks = Vec::new();
                                for f in fields.get() {
                                    if let Some(id) = f.spec.network()
                                        && !networks.iter().any(|(existing, _)| existing == id) {
                                            networks.push((id.to_string(), f.spec.group().to_string()));
                                    }
                                }
                                networks
                            } key=|(id, _)| id.clone() children=move |(id, label)| {
                                let active_id = id.clone();
                                view! { <button type="button" disabled=move || collection_open.get() class:active=move || network.get().as_ref() == Some(&active_id) on:click=move |_| { network.set(Some(id.clone())); help.set(None); }>{label}</button> }
                            } />
                            <button type="button" disabled=move || collection_open.get() on:click=add_network>"+ Add network"</button>
                        </nav>
                    </Show>
                    <fieldset class="settings-fields" disabled=move || state.settings_saving.get()>
                        <div class="settings-section-heading"><h3>{move || if search.get().is_empty() { SECTIONS[section.get()].to_string() } else { "Search results".into() }}</h3><span>"* Unsaved change"</span></div>
                        <Show when=move || fields.get().is_empty()><p>"Loading settings…"</p></Show>

                        <Show when=move || section.get() == 1 && search.get().is_empty()>
                            <fieldset class="settings-local"><legend>"This browser only — live preview"</legend>
                                <label>"Text size (px; empty follows default)"<input type="number" min="10" max="24" prop:value=move || state.font_size_override.get().map(|v| v.to_string()).unwrap_or_default() on:input=move |ev| {
                                    let value = event_target_value(&ev);
                                    if value.is_empty() { state.font_size_override.set(None); }
                                    else if let Ok(px) = value.parse::<i64>() { state.font_size_override.set(Some(super::appearance::clamp_font(px))); }
                                } /></label>
                                <label>"Line height (empty follows server)"<input type="number" min="1" max="2.2" step="0.05" prop:value=move || state.line_height_override.get().map(|v| v.to_string()).unwrap_or_default() on:input=move |ev| {
                                    let value = event_target_value(&ev);
                                    if value.is_empty() { state.line_height_override.set(None); }
                                    else if let Ok(height) = value.parse::<f32>() { state.line_height_override.set(Some(super::appearance::clamp_line_h(height))); }
                                } /></label>
                            </fieldset>
                        </Show>
                        <Show when=move || section.get() == 2 && search.get().is_empty()><label class="settings-local"><input type="checkbox" prop:checked=move || local_previews.get() on:change=move |ev| local_previews.set(event_target_checked(&ev)) />" Show image previews in this browser"</label></Show>
                        <Show when=move || section.get() == 9 && search.get().is_empty()><label class="settings-local"><input type="checkbox" prop:checked=move || local_follow.get() on:change=move |ev| local_follow.set(event_target_checked(&ev)) />" Follow terminal buffer changes in this browser"</label></Show>
                        <Show when=move || section.get() == 3 && search.get().is_empty()><p>"Browser notifications require permission and support from your connection."</p><crate::push::PushButton /></Show>
                        <Show when=move || section.get() == 5 && search.get().is_empty()><p>"Tab completes input. Up/Down recalls input history. Shift+Enter adds a new line. Command aliases can be edited below."</p></Show>
                        <Show when=move || {
                            let query = search.get().to_lowercase();
                            !query.is_empty() && !fields.with(|fields| fields.iter().any(|f| format!("{} {} {}", f.spec.path, f.spec.label, f.spec.description).to_lowercase().contains(&query)))
                        }><p>"No settings match your search."</p></Show>
                        <For each=move || {
                            let query = search.get().to_lowercase();
                            let mut groups: Vec<(String, Vec<DraftField>)> = Vec::new();
                            for field in fields.get().into_iter().filter(|f| if query.is_empty() { f.spec.section == section.get() && (section.get() != 0 || f.spec.network() == network.get().as_deref()) } else { format!("{} {} {}", f.spec.path, f.spec.label, f.spec.description).to_lowercase().contains(&query) }) {
                                let group = field.spec.group().to_string();
                                if let Some((_, items)) = groups.iter_mut().find(|(name, _)| *name == group) {
                                    items.push(field);
                                } else {
                                    groups.push((group, vec![field]));
                                }
                            }
                            groups
                        } key=|(group, items)| (group.clone(), items.iter().map(|f| f.spec.path.clone()).collect::<Vec<_>>()) children=move |(group, items)| view! {
                            <section class="settings-group"><h4>{group}</h4>{items.into_iter().map(|field| view! { <SettingsField field help collection_open /> }).collect_view()}</section>
                        } />
                    </fieldset>
                    </div>
                </div>
                <aside class="settings-help" aria-live="polite">{move || help.get().map_or_else(
                    || view! { <strong>"Choose a setting"</strong><p>"Focus a field to see its description. Changes are applied when you save."</p> }.into_any(),
                    |field| view! { <strong>{field.label}</strong><p>{field.description}</p><small>{field.effect}</small> }.into_any()
                )}</aside>
                {move || state.settings_error.get().map(|error| view! { <p class="settings-error" role="alert">{error}</p> })}
                <footer class="wizard-foot">
                    <button class="wizard-btn p" type="button" disabled=move || state.settings_saving.get() || fields.get().is_empty() || collection_open.get() on:click=save>{move || if state.settings_saving.get() { "Saving…" } else { "Save" }}</button>
                    <button class="wizard-btn s" type="button" disabled=move || state.settings_saving.get() on:click=move |_| state.settings_open.set(false)>"Cancel"</button>
                    <button class="wizard-btn s settings-defaults" type="button" disabled=move || state.settings_saving.get() || !search.get().is_empty() || collection_open.get() title="Choose a category without an active search to restore its defaults" on:click=defaults>"Section defaults"</button>
                </footer>
            </section>
        </div>
    }
}

#[component]
fn SettingsField(
    field: DraftField,
    help: RwSignal<Option<SettingField>>,
    collection_open: RwSignal<bool>,
) -> impl IntoView {
    let value = field.value;
    let touched = field.touched;
    let id = format!("setting-{}", field.spec.path);
    let input_id = id.clone();
    let label = field.spec.short_label().to_string();
    let description = field.spec.clone();
    let description_click = description.clone();
    let original = field.spec.value.clone();
    let secret = field.spec.kind == SettingKind::Secret;
    let is_collection = field.spec.is_collection();
    let input = if is_collection {
        view! { <collection::CollectionControl field=field.clone() collection_open /> }.into_any()
    } else {
        match field.spec.kind {
        SettingKind::Toggle => view! { <input id=input_id type="checkbox" prop:checked=move || value.get() == "true" on:change=move |ev| { value.set(event_target_checked(&ev).to_string()); touched.set(true); } /> }.into_any(),
        SettingKind::Select(options) => view! { <select id=input_id prop:value=move || value.get() on:change=move |ev| { value.set(event_target_value(&ev)); touched.set(true); }>{options.into_iter().map(|option| { let label = if option.is_empty() { "Inherit default".to_string() } else { option.clone() }; let selected = option.clone(); view! { <option value=option prop:selected=move || value.get() == selected>{label}</option> } }).collect_view()}</select> }.into_any(),
        SettingKind::Json => view! { <textarea id=input_id rows="3" spellcheck="false" prop:value=move || value.get() on:input=move |ev| { value.set(event_target_value(&ev)); touched.set(true); } /> }.into_any(),
        kind => {
            let input_type = match kind { SettingKind::Secret => "password", SettingKind::Number => "number", _ => "text" };
            let placeholder = if kind == SettingKind::Secret { if field.spec.configured { "Configured — leave untouched to keep" } else { "Not configured" } } else { "" };
            view! { <input id=input_id type=input_type autocomplete="off" placeholder=placeholder prop:value=move || value.get() on:input=move |ev| { value.set(event_target_value(&ev)); touched.set(true); } /> }.into_any()
        }
    }
    };
    view! { <div class="settings-field" class:collection-field=is_collection
        class:modified=move || value.get() != original || secret && touched.get()
        on:focusin=move |_| help.set(Some(description.clone()))
        on:click=move |_| help.set(Some(description_click.clone()))>
        <label for=id>{label}</label><div class="settings-control">{input}</div>
    </div> }
}

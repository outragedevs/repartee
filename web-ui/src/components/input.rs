use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::protocol::WebCommand;
use crate::state::AppState;

#[component]
pub fn InputLine() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();
    let (value, set_value) = signal(String::new());

    // Tab completion state.
    let (tab_prefix, set_tab_prefix) = signal(Option::<String>::None);
    let (tab_matches, set_tab_matches) = signal(Vec::<String>::new());
    let (tab_index, set_tab_index) = signal(0usize);
    let (tab_word_start, set_tab_word_start) = signal(0usize);
    let (tab_cursor_end, set_tab_cursor_end) = signal(0usize);

    let input_ref = NodeRef::<leptos::html::Input>::new();

    // Global keydown listener: refocus input when user types anywhere.
    Effect::new(move || {
        let cb = wasm_bindgen::prelude::Closure::<dyn Fn(web_sys::KeyboardEvent)>::new(
            move |ev: web_sys::KeyboardEvent| {
                // Don't capture if a modifier is held (Ctrl+C, etc.)
                if ev.ctrl_key() || ev.alt_key() || ev.meta_key() {
                    return;
                }
                // Don't capture keys with special handling.
                let key = ev.key();
                if key == "Tab"
                    || key == "Enter"
                    || key == "Escape"
                    || key == "F1"
                    || key.starts_with("Arrow")
                {
                    return;
                }
                // Focus the input if it's not already focused.
                if let Some(el) = input_ref.get_untracked() {
                    let html_el: &web_sys::HtmlInputElement = el.as_ref();
                    let _ = html_el.focus();
                }
            },
        );
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            let _ =
                doc.add_event_listener_with_callback("keydown", cb.as_ref().unchecked_ref());
            cb.forget(); // keep alive for the lifetime of the page
        }
    });

    let submit = Callback::new(move |_: ()| {
        let text = value.get();
        if text.is_empty() {
            return;
        }
        let Some(buffer_id) = state.active_buffer.get() else {
            return;
        };

        if text.starts_with('/') {
            crate::ws::send_command(&WebCommand::RunCommand {
                buffer_id,
                text,
            });
        } else {
            crate::ws::send_command(&WebCommand::SendMessage {
                buffer_id,
                text,
            });
        }

        set_value.set(String::new());
    });

    let on_keydown = move |ev: web_sys::KeyboardEvent| {
        if ev.key() == "Enter" {
            // Reset tab state on Enter.
            set_tab_prefix.set(None);
            submit.run(());
            return;
        }

        if ev.key() == "Tab" {
            ev.prevent_default();

            let text = value.get_untracked();

            // Get cursor position from the input element.
            let cursor = input_ref
                .get_untracked()
                .and_then(|el| {
                    let html_input: &web_sys::HtmlInputElement = el.as_ref();
                    html_input.selection_start().ok().flatten()
                })
                .map_or(text.len(), |p| p as usize);

            // Clamp cursor to text length (safety).
            let cursor = cursor.min(text.len());
            let before_cursor = &text[..cursor];

            // Find the word being typed (from previous space to cursor).
            let word_start = before_cursor.rfind(' ').map_or(0, |i| i + 1);
            let typed = &before_cursor[word_start..];

            if typed.is_empty() {
                return;
            }

            // Check if we're continuing a previous tab cycle.
            let prev_prefix = tab_prefix.get_untracked();
            let continuing = prev_prefix.as_deref() == Some(typed)
                || (prev_prefix.is_some()
                    && tab_word_start.get_untracked() == word_start);

            if continuing {
                // Cycle to next match.
                let matches = tab_matches.get_untracked();
                if matches.is_empty() {
                    return;
                }
                let idx = (tab_index.get_untracked() + 1) % matches.len();
                set_tab_index.set(idx);

                let replacement = &matches[idx];
                let suffix = if word_start == 0 { ": " } else { " " };
                let after = &text[tab_cursor_end.get_untracked()..];
                let new_text = format!(
                    "{}{replacement}{suffix}{after}",
                    &text[..word_start]
                );
                let new_cursor = word_start + replacement.len() + suffix.len();
                set_tab_cursor_end.set(new_cursor);
                set_value.set(new_text);

                // Set cursor position.
                if let Some(el) = input_ref.get_untracked() {
                    let html_input: &web_sys::HtmlInputElement = el.as_ref();
                    let _ = html_input.set_selection_start(Some(new_cursor as u32));
                    let _ = html_input.set_selection_end(Some(new_cursor as u32));
                }
            } else {
                // New tab completion — build matches.
                let nicks = state.nick_lists.get_untracked();
                let active_id = state.active_buffer.get_untracked();
                let typed_lower = typed.to_lowercase();

                let mut matches: Vec<String> = active_id
                    .and_then(|id| nicks.get(&id))
                    .map_or(Vec::new(), |list| {
                        list.iter()
                            .filter(|n| n.nick.to_lowercase().starts_with(&typed_lower))
                            .map(|n| n.nick.clone())
                            .collect()
                    });

                if matches.is_empty() {
                    return;
                }

                // Sort alphabetically (case-insensitive).
                matches.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));

                let replacement = &matches[0];
                let suffix = if word_start == 0 { ": " } else { " " };
                let after = &text[cursor..];
                let new_text = format!(
                    "{}{replacement}{suffix}{after}",
                    &text[..word_start]
                );
                let new_cursor = word_start + replacement.len() + suffix.len();

                set_tab_prefix.set(Some(typed.to_string()));
                set_tab_matches.set(matches);
                set_tab_index.set(0);
                set_tab_word_start.set(word_start);
                set_tab_cursor_end.set(new_cursor);
                set_value.set(new_text);

                // Set cursor position.
                if let Some(el) = input_ref.get_untracked() {
                    let html_input: &web_sys::HtmlInputElement = el.as_ref();
                    let _ = html_input.set_selection_start(Some(new_cursor as u32));
                    let _ = html_input.set_selection_end(Some(new_cursor as u32));
                }
            }
            return;
        }

        // Any non-Tab key resets tab completion state.
        if tab_prefix.get_untracked().is_some() {
            set_tab_prefix.set(None);
        }
    };

    // Handle paste — split multiline content and send each line separately.
    let on_paste = move |ev: web_sys::Event| {
        use wasm_bindgen::JsCast;
        let Some(clip_ev) = ev.dyn_ref::<web_sys::ClipboardEvent>() else { return };
        let Some(data) = clip_ev.clipboard_data() else { return };
        let Ok(text) = data.get_data("text/plain") else { return };
        if !text.contains('\n') {
            return; // single-line paste — let default browser behavior handle it
        }
        ev.prevent_default();
        let Some(buffer_id) = state.active_buffer.get_untracked() else { return };
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with('/') {
                crate::ws::send_command(&WebCommand::RunCommand {
                    buffer_id: buffer_id.clone(),
                    text: trimmed.to_string(),
                });
            } else {
                crate::ws::send_command(&WebCommand::SendMessage {
                    buffer_id: buffer_id.clone(),
                    text: trimmed.to_string(),
                });
            }
        }
        set_value.set(String::new());
    };

    view! {
        <div class="input-line">
            <span class="prompt">"❯"</span>
            <input
                type="text"
                placeholder="Type a message..."
                autofocus=true
                prop:value=value
                node_ref=input_ref
                on:input=move |ev| set_value.set(event_target_value(&ev))
                on:keydown=on_keydown
                on:paste=on_paste
            />
            <button class="send-btn" on:click=move |_| submit.run(())>"Send"</button>
        </div>
    }
}

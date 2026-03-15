use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::protocol::WebCommand;
use crate::state::AppState;

/// Known IRC commands for tab completion.
const COMMANDS: &[&str] = &[
    "action", "admin", "ban", "clear", "close", "connect", "ctcp", "cycle",
    "dcc", "dehalfop", "deop", "detach", "devoice", "disconnect",
    "halfop", "help", "ignore",
    "info", "invite", "join", "kick", "links", "list", "log", "lusers",
    "me", "mentions", "mode", "msg", "names", "nick", "notice",
    "op", "part", "ping", "query", "quit", "raw", "reconnect", "rejoin",
    "script", "server", "set", "spell", "spellcheck", "stats",
    "time", "topic", "unban", "unexcept", "unignore", "uninvex", "unreop",
    "voice", "who", "whois", "window",
];

/// Known /set setting paths for tab completion.
const SETTING_PATHS: &[&str] = &[
    "dcc.autoaccept_lowports",
    "dcc.autochat_masks",
    "dcc.max_connections",
    "dcc.own_ip",
    "dcc.port_range",
    "dcc.timeout",
    "display.backlog_lines",
    "display.nick_alignment",
    "display.nick_column_width",
    "display.nick_max_length",
    "display.nick_truncation",
    "display.scrollback_lines",
    "display.show_timestamps",
    "general.ctcp_version",
    "general.flood_protection",
    "general.nick",
    "general.realname",
    "general.theme",
    "general.timestamp_format",
    "general.username",
    "image_preview.cache_max_days",
    "image_preview.cache_max_mb",
    "image_preview.enabled",
    "image_preview.fetch_timeout",
    "image_preview.kitty_format",
    "image_preview.max_file_size",
    "image_preview.max_height",
    "image_preview.max_width",
    "image_preview.protocol",
    "sidepanel.left.visible",
    "sidepanel.left.width",
    "sidepanel.right.visible",
    "sidepanel.right.width",
    "spellcheck.dictionary_dir",
    "spellcheck.enabled",
    "spellcheck.languages",
    "statusbar.accent_color",
    "statusbar.background",
    "statusbar.cursor_color",
    "statusbar.dim_color",
    "statusbar.enabled",
    "statusbar.input_color",
    "statusbar.muted_color",
    "statusbar.prompt",
    "statusbar.prompt_color",
    "statusbar.separator",
    "statusbar.text_color",
    "web.bind_address",
    "web.cloudflare_tunnel_name",
    "web.enabled",
    "web.line_height",
    "web.nick_column_width",
    "web.nick_max_length",
    "web.password",
    "web.port",
    "web.session_hours",
    "web.theme",
    "web.timestamp_format",
    "web.tls_cert",
    "web.tls_key",
];

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

    let state_paste = state.clone();
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
                let is_full_line = replacement.starts_with('/');
                let suffix = if is_full_line {
                    " "
                } else if word_start == 0 {
                    ": "
                } else {
                    " "
                };
                let start = if is_full_line { 0 } else { word_start };
                let after = &text[tab_cursor_end.get_untracked()..];
                let new_text = format!(
                    "{}{replacement}{suffix}{after}",
                    &text[..start]
                );
                let new_cursor = start + replacement.len() + suffix.len();
                set_tab_cursor_end.set(new_cursor);
                set_value.set(new_text);

                // Set cursor position.
                if let Some(el) = input_ref.get_untracked() {
                    let html_input: &web_sys::HtmlInputElement = el.as_ref();
                    let _ = html_input.set_selection_start(Some(new_cursor as u32));
                    let _ = html_input.set_selection_end(Some(new_cursor as u32));
                }
            } else {
                // New tab completion — build matches based on context.
                let mut matches: Vec<String> =
                    build_tab_matches(&text, word_start, typed, &state);

                if matches.is_empty() {
                    return;
                }

                // Sort alphabetically (case-insensitive).
                matches.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));

                let replacement = &matches[0];
                // For /command and "/set path" completions, no trailing suffix
                // because the replacement already includes the full text.
                let is_full_line = replacement.starts_with('/');
                let suffix = if is_full_line {
                    " "
                } else if word_start == 0 {
                    ": "
                } else {
                    " "
                };
                let start = if is_full_line { 0 } else { word_start };
                let after = &text[cursor..];
                let new_text = format!(
                    "{}{replacement}{suffix}{after}",
                    &text[..start]
                );
                let new_cursor = start + replacement.len() + suffix.len();

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
        let Some(buffer_id) = state_paste.active_buffer.get_untracked() else { return };
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

/// Build tab completion matches based on input context.
///
/// 1. `/` at start with no spaces: complete /command names.
/// 2. `/set ` prefix: complete setting paths.
/// 3. Otherwise: complete nicks from the active channel.
fn build_tab_matches(
    text: &str,
    word_start: usize,
    typed: &str,
    state: &AppState,
) -> Vec<String> {
    // Case 1: /command completion — input starts with / and cursor is in the first word.
    if text.starts_with('/') && !text[..word_start.max(1)].contains(' ') {
        let prefix = typed.strip_prefix('/').unwrap_or(typed).to_lowercase();
        return COMMANDS
            .iter()
            .filter(|c| c.starts_with(&prefix))
            .map(|c| format!("/{c}"))
            .collect();
    }

    // Case 2: /set path completion.
    if text.starts_with("/set ") && word_start >= 5 {
        let after_set = typed;
        return SETTING_PATHS
            .iter()
            .filter(|p| p.starts_with(after_set))
            .map(|p| format!("/set {p}"))
            .collect();
    }

    // Case 3: Nick completion (default).
    let nicks = state.nick_lists.get_untracked();
    let active_id = state.active_buffer.get_untracked();
    let typed_lower = typed.to_lowercase();

    active_id
        .and_then(|id| nicks.get(&id))
        .map_or(Vec::new(), |list| {
            list.iter()
                .filter(|n| n.nick.to_lowercase().starts_with(&typed_lower))
                .map(|n| n.nick.clone())
                .collect()
        })
}

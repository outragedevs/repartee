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
    let (tab_matches, set_tab_matches) = signal(Vec::<String>::new());
    let (tab_index, set_tab_index) = signal(0usize);
    // The cursor position where the last completion ended — used to detect continuation.
    let (tab_cursor_end, set_tab_cursor_end) = signal(0usize);
    // The start position of the replaced text — used to rebuild on cycle.
    let (tab_replace_start, set_tab_replace_start) = signal(0usize);
    // Whether we're in an active tab cycle.
    let (tab_active, set_tab_active) = signal(false);

    let input_ref = NodeRef::<leptos::html::Input>::new();

    // Global keydown listener — focus input when user types anywhere.
    Effect::new(move || {
        let cb = wasm_bindgen::prelude::Closure::<dyn Fn(web_sys::KeyboardEvent)>::new(
            move |ev: web_sys::KeyboardEvent| {
                if ev.ctrl_key() || ev.alt_key() || ev.meta_key() {
                    return;
                }
                let key = ev.key();
                if key == "Tab"
                    || key == "Enter"
                    || key == "Escape"
                    || key == "F1"
                    || key.starts_with("Arrow")
                {
                    return;
                }
                if let Some(el) = input_ref.get_untracked() {
                    let html_el: &web_sys::HtmlInputElement = el.as_ref();
                    let _ = html_el.focus();
                }
            },
        );
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            let _ =
                doc.add_event_listener_with_callback("keydown", cb.as_ref().unchecked_ref());
            cb.forget();
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
            set_tab_active.set(false);
            submit.run(());
            return;
        }

        if ev.key() == "Tab" {
            ev.prevent_default();

            let text = value.get_untracked();
            let cursor = get_cursor(&input_ref, text.len());

            if tab_active.get_untracked() && cursor == tab_cursor_end.get_untracked() {
                // Continue cycling through existing matches.
                let matches = tab_matches.get_untracked();
                if matches.is_empty() {
                    return;
                }
                let idx = (tab_index.get_untracked() + 1) % matches.len();
                set_tab_index.set(idx);

                let replacement = &matches[idx];
                let start = tab_replace_start.get_untracked();
                let old_end = tab_cursor_end.get_untracked();
                let after = &text[old_end..];
                let new_text = format!("{}{replacement}{after}", &text[..start]);
                let new_cursor = start + replacement.len();

                set_tab_cursor_end.set(new_cursor);
                set_value.set(new_text);
                set_cursor(&input_ref, new_cursor);
            } else {
                // New tab completion.
                let before_cursor = &text[..cursor];
                let word_start = before_cursor.rfind(' ').map_or(0, |i| i + 1);
                let typed = &before_cursor[word_start..];

                if typed.is_empty() && !text.starts_with('/') {
                    return;
                }

                let mut matches = build_tab_matches(&text, word_start, typed, &state);
                if matches.is_empty() {
                    return;
                }
                matches.sort_by_key(|a| a.to_lowercase());

                // Build the replacement with appropriate suffix.
                let completions: Vec<String> = matches
                    .iter()
                    .map(|m| {
                        if m.starts_with('/') {
                            format!("{m} ")
                        } else if word_start == 0 {
                            format!("{m}: ")
                        } else {
                            format!("{m} ")
                        }
                    })
                    .collect();

                let replace_start = if matches[0].starts_with('/') { 0 } else { word_start };
                let first = &completions[0];
                let after = &text[cursor..];
                let new_text = format!("{}{first}{after}", &text[..replace_start]);
                let new_cursor = replace_start + first.len();

                set_tab_matches.set(completions);
                set_tab_index.set(0);
                set_tab_replace_start.set(replace_start);
                set_tab_cursor_end.set(new_cursor);
                set_tab_active.set(true);
                set_value.set(new_text);
                set_cursor(&input_ref, new_cursor);
            }
            return;
        }

        // Any non-Tab key resets tab state.
        if tab_active.get_untracked() {
            set_tab_active.set(false);
        }
    };

    // Handle paste — split multiline content and send each line separately.
    let on_paste = move |ev: web_sys::Event| {
        let Some(clip_ev) = ev.dyn_ref::<web_sys::ClipboardEvent>() else { return };
        let Some(data) = clip_ev.clipboard_data() else { return };
        let Ok(text) = data.get_data("text/plain") else { return };
        if !text.contains('\n') {
            return;
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
                autocomplete="off"
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

/// Get cursor position from the input element.
fn get_cursor(input_ref: &NodeRef<leptos::html::Input>, text_len: usize) -> usize {
    input_ref
        .get_untracked()
        .and_then(|el| {
            let html_input: &web_sys::HtmlInputElement = el.as_ref();
            html_input.selection_start().ok().flatten()
        })
        .map_or(text_len, |p| (p as usize).min(text_len))
}

/// Set cursor position on the input element.
fn set_cursor(input_ref: &NodeRef<leptos::html::Input>, pos: usize) {
    if let Some(el) = input_ref.get_untracked() {
        let html_input: &web_sys::HtmlInputElement = el.as_ref();
        let _ = html_input.set_selection_start(Some(pos as u32));
        let _ = html_input.set_selection_end(Some(pos as u32));
    }
}

/// Build tab completion matches based on input context.
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
        return SETTING_PATHS
            .iter()
            .filter(|p| p.starts_with(typed))
            .map(|p| format!("/set {p}"))
            .collect();
    }

    // Case 3: Nick completion (default).
    let nicks = state.nick_lists.get_untracked();
    let active_id = state.active_buffer.get_untracked();
    let typed_lower = typed.to_lowercase();

    active_id
        .and_then(|id| nicks.get(&id))
        .map_or_else(Vec::new, |list| {
            list.iter()
                .filter(|n| n.nick.to_lowercase().starts_with(&typed_lower))
                .map(|n| n.nick.clone())
                .collect()
        })
}

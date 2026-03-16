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
    "dcc.autoaccept_lowports", "dcc.autochat_masks", "dcc.max_connections",
    "dcc.own_ip", "dcc.port_range", "dcc.timeout",
    "display.backlog_lines", "display.nick_alignment", "display.nick_column_width",
    "display.nick_max_length", "display.nick_truncation", "display.scrollback_lines",
    "display.show_timestamps",
    "general.ctcp_version", "general.flood_protection", "general.nick",
    "general.realname", "general.theme", "general.timestamp_format", "general.username",
    "logging.event_retention_hours", "logging.retention_days",
    "image_preview.cache_max_days", "image_preview.cache_max_mb", "image_preview.enabled",
    "image_preview.fetch_timeout", "image_preview.kitty_format", "image_preview.max_file_size",
    "image_preview.max_height", "image_preview.max_width", "image_preview.protocol",
    "sidepanel.left.visible", "sidepanel.left.width",
    "sidepanel.right.visible", "sidepanel.right.width",
    "spellcheck.dictionary_dir", "spellcheck.enabled", "spellcheck.languages",
    "statusbar.accent_color", "statusbar.background", "statusbar.cursor_color",
    "statusbar.dim_color", "statusbar.enabled", "statusbar.input_color",
    "statusbar.muted_color", "statusbar.prompt", "statusbar.prompt_color",
    "statusbar.separator", "statusbar.text_color",
    "web.bind_address", "web.cloudflare_tunnel_name", "web.enabled", "web.line_height",
    "web.nick_column_width", "web.nick_max_length", "web.password", "web.port",
    "web.session_hours", "web.theme", "web.timestamp_format", "web.tls_cert", "web.tls_key",
];

#[component]
pub fn InputLine() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();
    let (value, set_value) = signal(String::new());

    // Tab completion state.
    let (tab_matches, set_tab_matches) = signal(Vec::<String>::new());
    let (tab_index, set_tab_index) = signal(0usize);
    let (tab_cursor_end, set_tab_cursor_end) = signal(0usize);
    let (tab_replace_start, set_tab_replace_start) = signal(0usize);
    let (tab_active, set_tab_active) = signal(false);

    let input_ref = NodeRef::<leptos::html::Textarea>::new();

    // Set enterkeyhint for mobile keyboard "Send" button.
    Effect::new(move || {
        if let Some(el) = input_ref.get() {
            let html_el: &web_sys::HtmlTextAreaElement = el.as_ref();
            let _ = html_el.set_attribute("enterkeyhint", "send");
        }
    });

    // Global keydown listener — focus textarea when user types anywhere.
    // Skipped when active buffer is a shell (shell_view captures input instead).
    let keydown_registered = StoredValue::new(false);
    Effect::new(move || {
        if keydown_registered.get_value() {
            return;
        }
        keydown_registered.set_value(true);
        let cb = wasm_bindgen::prelude::Closure::<dyn Fn(web_sys::KeyboardEvent)>::new(
            move |ev: web_sys::KeyboardEvent| {
                if ev.ctrl_key() || ev.alt_key() || ev.meta_key() {
                    return;
                }
                // Don't steal focus from shell terminal.
                let is_shell = state.active_buffer.get_untracked()
                    .and_then(|id| {
                        state.buffers.get_untracked().iter()
                            .find(|b| b.id == id)
                            .map(|b| b.buffer_type == "shell")
                    })
                    .unwrap_or(false);
                if is_shell {
                    return;
                }
                let key = ev.key();
                if key == "Tab" || key == "Enter" || key == "Escape"
                    || key == "F1" || key.starts_with("Arrow")
                {
                    return;
                }
                if let Some(el) = input_ref.get_untracked() {
                    let html_el: &web_sys::HtmlTextAreaElement = el.as_ref();
                    let _ = html_el.focus();
                }
            },
        );
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            let _ = doc.add_event_listener_with_callback("keydown", cb.as_ref().unchecked_ref());
            cb.forget();
        }
    });

    let send_text = move |text: String| {
        if text.is_empty() {
            return;
        }
        let Some(buffer_id) = state.active_buffer.get() else {
            return;
        };
        // Split multiline input and send each line.
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
    };

    let on_keydown = move |ev: web_sys::KeyboardEvent| {
        if ev.key() == "Enter" && !ev.shift_key() {
            ev.prevent_default();
            set_tab_active.set(false);
            let text = value.get();
            send_text(text);
            set_value.set(String::new());
            // Reset textarea height.
            if let Some(el) = input_ref.get_untracked() {
                let html_el: &web_sys::HtmlTextAreaElement = el.as_ref();
                let el_html: &web_sys::HtmlElement = html_el.unchecked_ref();
                    el_html.style().set_property("height", "").ok();
            }
            return;
        }

        if ev.key() == "Tab" {
            ev.prevent_default();

            let text = value.get_untracked();
            let cursor = get_textarea_cursor(&input_ref, text.len());

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
                set_textarea_cursor(&input_ref, new_cursor);
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
                set_textarea_cursor(&input_ref, new_cursor);
            }
            return;
        }

        // Any non-Tab key resets tab state.
        if tab_active.get_untracked() {
            set_tab_active.set(false);
        }
    };

    // Auto-resize textarea to content height.
    let on_input = move |ev: web_sys::Event| {
        let target = event_target_value(&ev);
        set_value.set(target);
        // Resize textarea to fit content.
        if let Some(el) = input_ref.get_untracked() {
            let html_el: &web_sys::HtmlTextAreaElement = el.as_ref();
            let style: &web_sys::HtmlElement = html_el.unchecked_ref();
            style.style().set_property("height", "auto").ok();
            let scroll_h = html_el.scroll_height();
            let max_h = 120; // max ~6 lines
            let h = scroll_h.min(max_h);
            style
                .style()
                .set_property("height", &format!("{h}px"))
                .ok();
        }
    };

    view! {
        <div class="input-line">
            <span class="prompt">"❯"</span>
            <textarea
                id="chat-input"
                rows="1"
                placeholder="Type a message..."
                autofocus=true
                autocomplete="off"
                prop:value=value
                node_ref=input_ref
                on:input=on_input
                on:keydown=on_keydown
            ></textarea>
            <button class="send-btn" on:click=move |_| {
                let text = value.get();
                send_text(text);
                set_value.set(String::new());
                // Reset textarea height.
                if let Some(el) = input_ref.get_untracked() {
                    let html_el: &web_sys::HtmlTextAreaElement = el.as_ref();
                    let el_html: &web_sys::HtmlElement = html_el.unchecked_ref();
                    el_html.style().set_property("height", "").ok();
                }
            }
                inner_html="<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' width='16' height='16' fill='currentColor'><path d='M2.01 21L23 12 2.01 3 2 10l15 2-15 2z'/></svg>"
            ></button>
        </div>
    }
}

/// Get cursor position from the textarea element.
fn get_textarea_cursor(input_ref: &NodeRef<leptos::html::Textarea>, text_len: usize) -> usize {
    input_ref
        .get_untracked()
        .and_then(|el| {
            let html_el: &web_sys::HtmlTextAreaElement = el.as_ref();
            html_el.selection_start().ok().flatten()
        })
        .map_or(text_len, |p| (p as usize).min(text_len))
}

/// Set cursor position on the textarea element.
fn set_textarea_cursor(input_ref: &NodeRef<leptos::html::Textarea>, pos: usize) {
    if let Some(el) = input_ref.get_untracked() {
        let html_el: &web_sys::HtmlTextAreaElement = el.as_ref();
        let _ = html_el.set_selection_start(Some(pos as u32));
        let _ = html_el.set_selection_end(Some(pos as u32));
    }
}

/// Build tab completion matches based on input context.
fn build_tab_matches(text: &str, word_start: usize, typed: &str, state: &AppState) -> Vec<String> {
    // Case 1: /command completion.
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

    // Case 3: Nick completion.
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

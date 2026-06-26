use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::protocol::WebCommand;
use crate::state::AppState;

/// Known IRC commands for tab completion.
const COMMANDS: &[&str] = &[
    "action",
    "admin",
    "ban",
    "clear",
    "close",
    "connect",
    "ctcp",
    "cycle",
    "dcc",
    "dehalfop",
    "deop",
    "detach",
    "devoice",
    "disconnect",
    "halfop",
    "help",
    "ignore",
    "info",
    "invite",
    "join",
    "kick",
    "links",
    "list",
    "log",
    "lusers",
    "me",
    "mentions",
    "mode",
    "msg",
    "names",
    "nick",
    "notice",
    "op",
    "part",
    "ping",
    "query",
    "quit",
    "raw",
    "reconnect",
    "rejoin",
    "script",
    "server",
    "set",
    "spell",
    "spellcheck",
    "stats",
    "time",
    "topic",
    "unban",
    "unexcept",
    "unignore",
    "uninvex",
    "unreop",
    "voice",
    "who",
    "whois",
    "window",
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
    "general.flood_exemptions",
    "general.flood_protection",
    "general.nick",
    "general.realname",
    "general.theme",
    "general.timestamp_format",
    "general.username",
    "logging.event_retention_hours",
    "logging.retention_days",
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
    "web.image_previews",
    "web.image_previews_max_per_msg",
    "web.line_height",
    "web.nick_column_width",
    "web.nick_max_length",
    "web.password",
    "web.port",
    "web.session_days",
    "web.theme",
    "web.thumbnail_cache_mb",
    "web.timestamp_format",
    "web.tls_cert",
    "web.tls_key",
    "web.username",
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

    // Apply a picker-requested insertion (`:name:` or a Unicode emoji) at the
    // caret, then clear the signal. Keeps the `value` signal authoritative.
    //
    // Two InputLine instances are mounted at once (desktop + mobile layouts,
    // toggled by CSS `display`), so both subscribe to the shared
    // `pending_insert`. Only the *visible* textarea may consume the token —
    // otherwise the hidden desktop instance swallows it on mobile and the
    // visible field never updates. `offset_parent()` is `None` when an ancestor
    // is `display:none`, which is exactly the hidden layout.
    Effect::new(move |_| {
        let Some(token) = state.pending_insert.get() else {
            return;
        };
        let Some(el) = input_ref.get_untracked() else {
            return;
        };
        let html_el: &web_sys::HtmlTextAreaElement = el.as_ref();
        let visible = {
            let he: &web_sys::HtmlElement = html_el.unchecked_ref();
            he.offset_parent().is_some()
        };
        if !visible {
            return; // hidden instance — leave the token for the visible one.
        }
        let text = value.get_untracked();
        let cursor = get_textarea_cursor(&input_ref, &text);
        let new_text = format!("{}{token}{}", &text[..cursor], &text[cursor..]);
        let new_cursor = cursor + token.len();
        set_value.set(new_text.clone());
        set_textarea_cursor(&input_ref, &new_text, new_cursor);
        let _ = html_el.focus();
        state.pending_insert.set(None);
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
                let is_shell = state
                    .active_buffer
                    .get_untracked()
                    .and_then(|id| {
                        state
                            .buffers
                            .get_untracked()
                            .iter()
                            .find(|b| b.id == id)
                            .map(|b| b.buffer_type == "shell")
                    })
                    .unwrap_or(false);
                if is_shell {
                    return;
                }
                // Don't steal focus while the user is typing in another control
                // (the server wizard's fields, or any future modal/form). Only
                // grab focus when the keystroke originated from "nowhere".
                if let Some(el) = ev.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) {
                    let tag = el.tag_name();
                    if tag.eq_ignore_ascii_case("input")
                        || tag.eq_ignore_ascii_case("select")
                        || tag.eq_ignore_ascii_case("textarea")
                    {
                        return;
                    }
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
        // `/wizard server` opens the web add-server modal client-side (the
        // server-side handler would open the TUI overlay, useless to a web
        // client). The web wizard is add-only, so any id argument is ignored.
        // Checked before the active-buffer guard so it works at bootstrap, when
        // a client with no servers yet has no active buffer.
        let whole = text.trim();
        if whole == "/wizard server" || whole.starts_with("/wizard server ") {
            state.wizard_open.set(true);
            return;
        }
        // `/emoji` (and the `/emote`/`/emotes` aliases) open the GG emote picker
        // client-side rather than dispatching to the server. `/emote <name>`
        // still goes to the server for its insert/search behaviour.
        if matches!(whole.to_ascii_lowercase().as_str(), "/emoji" | "/emote" | "/emotes") {
            state.emote_picker_open.set(true);
            return;
        }
        let Some(buffer_id) = state.active_buffer.get() else {
            return;
        };
        // Coalesce consecutive plaintext lines into ONE SendMessage (preserving
        // interior blank lines) so the server frames them as a single
        // draft/multiline batch — matching the TUI paste path. Command lines
        // (leading '/') flush the pending plaintext first, then dispatch
        // individually as RunCommand. Trailing blank lines are dropped.
        //
        // Cap each plaintext run at MAX_PASTE_LINES (mirrors the TUI
        // `MAX_PASTE_LINES` guard) so a huge web paste can't have the backend
        // frame thousands of lines into batches and store/render one giant
        // message, bypassing the flood/render guard the TUI path applies.
        const MAX_PASTE_LINES: usize = 1000;
        let flush = |pending: &mut Vec<&str>| {
            while pending.last().is_some_and(|l| l.trim().is_empty()) {
                pending.pop();
            }
            if pending.len() > MAX_PASTE_LINES {
                web_sys::console::warn_1(
                    &format!("paste truncated to {MAX_PASTE_LINES} lines").into(),
                );
                pending.truncate(MAX_PASTE_LINES);
            }
            if !pending.is_empty() {
                crate::ws::send_command(&WebCommand::SendMessage {
                    buffer_id: buffer_id.clone(),
                    text: pending.join("\n"),
                });
            }
            pending.clear();
        };
        let mut pending: Vec<&str> = Vec::new();
        for line in text.lines() {
            if line.trim_start().starts_with('/') {
                flush(&mut pending);
                crate::ws::send_command(&WebCommand::RunCommand {
                    buffer_id: buffer_id.clone(),
                    text: line.trim().to_string(),
                });
            } else {
                pending.push(line);
            }
        }
        flush(&mut pending);
    };

    let on_keydown = move |ev: web_sys::KeyboardEvent| {
        // Ctrl+G opens the GG emote picker (parity with the TUI keybind).
        if ev.ctrl_key() && ev.key().eq_ignore_ascii_case("g") {
            ev.prevent_default();
            state.emote_picker_open.set(true);
            return;
        }

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
            let cursor = get_textarea_cursor(&input_ref, &text);

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
                set_value.set(new_text.clone());
                set_textarea_cursor(&input_ref, &new_text, new_cursor);
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
                        if m.starts_with('/') || m.starts_with(':') {
                            // commands and `:name:` emotes already carry their
                            // own delimiter; just add a trailing space.
                            format!("{m} ")
                        } else if word_start == 0 {
                            format!("{m}: ")
                        } else {
                            format!("{m} ")
                        }
                    })
                    .collect();

                let replace_start = if matches[0].starts_with('/') {
                    0
                } else {
                    word_start
                };
                let first = &completions[0];
                let after = &text[cursor..];
                let new_text = format!("{}{first}{after}", &text[..replace_start]);
                let new_cursor = replace_start + first.len();

                set_tab_matches.set(completions);
                set_tab_index.set(0);
                set_tab_replace_start.set(replace_start);
                set_tab_cursor_end.set(new_cursor);
                set_tab_active.set(true);
                set_value.set(new_text.clone());
                set_textarea_cursor(&input_ref, &new_text, new_cursor);
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
            style.style().set_property("height", &format!("{h}px")).ok();
        }
    };

    view! {
        <div class="input-line">
            <button
                type="button"
                class="input-emote-btn"
                title="GG emotes (/emoji, Ctrl+G)"
                on:click=move |_| state.emote_picker_open.set(true)
            >"GG"</button>
            <button
                type="button"
                class="input-emote-btn emoji"
                title="Emoji"
                on:click=move |_| state.emoji_picker_open.set(true)
            >"\u{1F600}"</button>
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

/// Largest char boundary `<= i` (clamped to `s.len()`). Guards `str` slicing
/// when an offset doesn't land on a UTF-8 boundary.
fn floor_char_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Convert a browser caret offset (UTF-16 code units) to a Rust byte index.
/// The DOM `selectionStart`/`selectionEnd` are UTF-16 offsets; Rust `str`
/// slicing is byte-indexed, so the two must be translated before slicing.
fn utf16_offset_to_byte(s: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (byte, ch) in s.char_indices() {
        if units >= utf16 {
            return byte;
        }
        units += ch.len_utf16();
    }
    s.len()
}

/// Convert a Rust byte index to a browser caret offset (UTF-16 code units).
fn byte_to_utf16_offset(s: &str, byte: usize) -> usize {
    let byte = floor_char_boundary(s, byte);
    s[..byte].chars().map(char::len_utf16).sum()
}

/// Get the caret position from the textarea as a Rust byte index into `text`.
fn get_textarea_cursor(input_ref: &NodeRef<leptos::html::Textarea>, text: &str) -> usize {
    input_ref
        .get_untracked()
        .and_then(|el| {
            let html_el: &web_sys::HtmlTextAreaElement = el.as_ref();
            html_el.selection_start().ok().flatten()
        })
        .map_or_else(|| text.len(), |p| utf16_offset_to_byte(text, p as usize))
}

/// Set the caret on the textarea from a Rust byte index into `text`.
fn set_textarea_cursor(input_ref: &NodeRef<leptos::html::Textarea>, text: &str, byte_pos: usize) {
    if let Some(el) = input_ref.get_untracked() {
        let html_el: &web_sys::HtmlTextAreaElement = el.as_ref();
        let off = u32::try_from(byte_to_utf16_offset(text, byte_pos)).unwrap_or(u32::MAX);
        let _ = html_el.set_selection_start(Some(off));
        let _ = html_el.set_selection_end(Some(off));
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

    // Case 3: `:name:` emote completion (gated on the emotes setting).
    if state.emotes_enabled.get_untracked() {
        let emotes = emote_tab_matches(typed);
        if !emotes.is_empty() {
            return emotes;
        }
    }

    // Case 4: Nick completion.
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

/// `:usm` → `[":usmiech:", ...]`. Returns empty unless `word` is a single
/// leading colon followed by a non-empty prefix with no closing colon. Each
/// match carries the closing colon; the caller appends the trailing space.
fn emote_tab_matches(word: &str) -> Vec<String> {
    let Some(rest) = word.strip_prefix(':') else {
        return Vec::new();
    };
    if rest.is_empty() || rest.contains(':') {
        return Vec::new();
    }
    let prefix = rest.to_ascii_lowercase();
    crate::emotes::EMOTE_NAMES
        .iter()
        .filter(|n| n.to_ascii_lowercase().starts_with(&prefix))
        .map(|n| format!(":{n}:"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_char_boundary_clamps_into_multibyte() {
        let s = "a\u{1F600}b"; // 'a' + 😀 (4 bytes) + 'b'
        assert_eq!(floor_char_boundary(s, 0), 0);
        assert_eq!(floor_char_boundary(s, 1), 1);
        // bytes 2..=4 fall inside the emoji → floor back to 1
        assert_eq!(floor_char_boundary(s, 3), 1);
        assert_eq!(floor_char_boundary(s, 5), 5);
        assert_eq!(floor_char_boundary(s, 999), s.len());
    }

    #[test]
    fn utf16_byte_offset_roundtrip_across_emoji() {
        // "a😀b": 'a' 1u/1b, 😀 2u/4b, 'b' 1u/1b → utf16 {0,1,3,4}, byte {0,1,5,6}
        let s = "a\u{1F600}b";
        assert_eq!(utf16_offset_to_byte(s, 0), 0);
        assert_eq!(utf16_offset_to_byte(s, 1), 1);
        assert_eq!(utf16_offset_to_byte(s, 3), 5); // caret after the emoji
        assert_eq!(utf16_offset_to_byte(s, 4), 6);
        assert_eq!(utf16_offset_to_byte(s, 99), s.len()); // past end clamps
        assert_eq!(byte_to_utf16_offset(s, 0), 0);
        assert_eq!(byte_to_utf16_offset(s, 1), 1);
        assert_eq!(byte_to_utf16_offset(s, 5), 3);
        assert_eq!(byte_to_utf16_offset(s, 6), 4);
    }

    #[test]
    fn emote_tab_no_colon_no_matches() {
        assert!(emote_tab_matches("usm").is_empty());
    }

    #[test]
    fn emote_tab_closing_colon_no_matches() {
        assert!(emote_tab_matches(":usm:").is_empty());
    }

    #[test]
    fn emote_tab_bare_colon_no_matches() {
        assert!(emote_tab_matches(":").is_empty());
    }

    #[test]
    fn emote_tab_prefix_returns_closing_colon_matches() {
        let m = emote_tab_matches(":usm");
        assert!(!m.is_empty(), "expected :usmiech:");
        assert!(m.iter().all(|s| s.starts_with(':') && s.ends_with(':')));
        assert!(m.iter().all(|s| !s.ends_with(": ")), "no trailing space yet");
    }

    #[test]
    fn emote_tab_is_case_insensitive() {
        assert_eq!(emote_tab_matches(":USM").len(), emote_tab_matches(":usm").len());
    }
}

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
    "typing.send_channels",
    "typing.send_queries",
    "typing.show",
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

/// One row of the completion popup.
#[derive(Clone, PartialEq, Eq)]
struct PopupItem {
    /// What the row displays (`doddy`, `/join`, `:usmiech:`).
    label: String,
    /// What accepting the row splices into the input (with its delimiter).
    insert: String,
    /// Inline thumbnail for emote rows (`/emotes/<stem>.gif`).
    emote_url: Option<String>,
}

/// The live completion popup: the byte range of the input it would replace
/// plus the candidate rows. `None` = hidden. Recomputed on every input event,
/// so the range always refers to the current `value`.
#[derive(Clone, PartialEq, Eq)]
struct PopupData {
    replace_start: usize,
    replace_end: usize,
    items: Vec<PopupItem>,
}

/// Cap on popup rows — a bare `@` in a big channel matches everyone; the
/// popup shows the first 50 and the user narrows by typing.
const MAX_POPUP_ITEMS: usize = 50;

#[component]
pub fn InputLine() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();
    let (value, set_value) = signal(String::new());

    // Report typing to the core, which owns the state machine, the throttle, the
    // flood budget and every guard. We send a predicate, never the text.
    let last_report = StoredValue::new(0.0_f64);
    let last_sent_state = StoredValue::new(false);
    let last_sent_buffer: StoredValue<Option<String>> = StoredValue::new(None);
    Effect::new(move |_| {
        let text = value.get();
        let Some(buffer_id) = state.active_buffer.get() else {
            return;
        };
        let typing = should_type(&text);
        let now = js_sys::Date::now();
        // A change of state OR of target buffer always reports immediately; a
        // steady state is rate-limited. The core still enforces the 3s IRC
        // throttle — this only keeps the websocket quiet.
        if !should_report_typing(
            typing,
            &buffer_id,
            last_sent_state.get_value(),
            last_sent_buffer.get_value().as_deref(),
            now - last_report.get_value(),
        ) {
            return;
        }
        last_report.set_value(now);
        last_sent_state.set_value(typing);
        last_sent_buffer.set_value(Some(buffer_id.clone()));
        crate::ws::send_command(&WebCommand::Typing { buffer_id, typing });
    });

    // Tab completion state.
    let (tab_matches, set_tab_matches) = signal(Vec::<String>::new());
    let (tab_index, set_tab_index) = signal(0usize);
    let (tab_cursor_end, set_tab_cursor_end) = signal(0usize);
    let (tab_replace_start, set_tab_replace_start) = signal(0usize);
    let (tab_active, set_tab_active) = signal(false);

    // Completion popup (tap-friendly alternative to Tab — phones have no Tab
    // key). Triggered by `@nick`, `/command`, and `:emote` prefixes.
    // `popup_engaged` flips true once the user navigates the list (arrows /
    // Shift+Tab); only then does Enter accept instead of sending — a fully
    // typed "/part" + Enter must send on the first press even though the
    // popup still matches it.
    let (popup, set_popup) = signal(None::<PopupData>);
    let (popup_sel, set_popup_sel) = signal(0usize);
    let (popup_engaged, set_popup_engaged) = signal(false);

    let input_ref = NodeRef::<leptos::html::Textarea>::new();

    // Sent-message history (irssi/TheLounge-style ↑/↓ recall). In-memory,
    // newest last, capped; `hist_pos` is the entry being browsed (`None` =
    // live draft, which is stashed in `hist_draft` on entry so ↓ past the
    // newest entry restores it).
    const MAX_HISTORY: usize = 100;
    let history = StoredValue::new(Vec::<String>::new());
    let hist_pos = StoredValue::new(None::<usize>);
    let hist_draft = StoredValue::new(String::new());

    let push_history = move |text: &str| {
        // Skip big multiline sends: recalling a 1000-line paste with ↑ is
        // never what the user wants, and 100 such entries would pin
        // megabytes for the session. Small multiline drafts still recall.
        const MAX_HISTORY_LINES: usize = 3;
        if !text.trim().is_empty() && text.lines().count() <= MAX_HISTORY_LINES {
            history.update_value(|h| {
                if h.last().map(String::as_str) != Some(text) {
                    h.push(text.to_string());
                    if h.len() > MAX_HISTORY {
                        let drop = h.len() - MAX_HISTORY;
                        h.drain(..drop);
                    }
                }
            });
        }
        hist_pos.set_value(None);
    };

    // Clear the inline height so an emptied textarea returns to its natural
    // single-row size — shared by the Enter and send-button paths. (Not
    // `resize_textarea`: measuring immediately after clearing the value
    // would race the reactive prop:value DOM write for no benefit.)
    let reset_textarea_height = move || {
        if let Some(el) = input_ref.get_untracked() {
            let html_el: &web_sys::HtmlTextAreaElement = el.as_ref();
            let el_html: &web_sys::HtmlElement = html_el.unchecked_ref();
            el_html.style().set_property("height", "").ok();
        }
    };

    // Fit the textarea to its content — shared by the input handler and the
    // programmatic value writes (history recall, mention insert).
    let resize_textarea = move || {
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

    // Hide the popup when the active buffer changes — its nick matches came
    // from the buffer it was computed in.
    Effect::new(move || {
        let _ = state.active_buffer.get();
        set_popup.set(None);
    });

    // Consume a tapped-nick mention (chat log → input). The delimiter
    // depends on caret context, which only this component knows: `nick: `
    // when the caret's line is still empty, `nick ` mid-sentence (with a
    // separating space if the caret touches a word). Mirrors the popup's
    // delimiter rules.
    Effect::new(move |_| {
        let Some(nick) = state.pending_mention.get() else {
            return;
        };
        let Some(el) = input_ref.get_untracked() else {
            return;
        };
        let html_el: &web_sys::HtmlTextAreaElement = el.as_ref();
        let text = value.get_untracked();
        let cursor = get_textarea_cursor(&input_ref, &text);
        let before = &text[..cursor];
        let token = if at_line_start(&text, cursor) {
            format!("{nick}: ")
        } else if before.ends_with(' ') {
            format!("{nick} ")
        } else {
            format!(" {nick} ")
        };
        let new_text = format!("{}{token}{}", &text[..cursor], &text[cursor..]);
        let new_cursor = cursor + token.len();
        set_value.set(new_text.clone());
        set_textarea_cursor(&input_ref, &new_text, new_cursor);
        resize_textarea();
        let _ = html_el.focus();
        state.pending_mention.set(None);
        hist_pos.set_value(None);
        set_popup.set(None);
    });

    // Accept popup row `idx`: splice its insert text over the trigger word,
    // restore focus/caret, and hide the popup. Captures only Copy signal
    // handles, so the closure itself is Copy and usable from both the keydown
    // handler and every row's mousedown handler.
    let accept_popup = move |idx: usize| {
        let Some(p) = popup.get_untracked() else {
            return;
        };
        let Some(item) = p.items.get(idx) else {
            return;
        };
        let text = value.get_untracked();
        // The popup is recomputed on every input event and cleared by the
        // picker-insert Effect, so the range should always be valid for the
        // current text — but a stale range slicing mid-UTF-8 would panic and
        // kill the whole WASM app, so guard boundaries too.
        if p.replace_start > p.replace_end
            || p.replace_end > text.len()
            || !text.is_char_boundary(p.replace_start)
            || !text.is_char_boundary(p.replace_end)
        {
            set_popup.set(None);
            return;
        }
        let new_text = format!(
            "{}{}{}",
            &text[..p.replace_start],
            item.insert,
            &text[p.replace_end..]
        );
        let new_cursor = p.replace_start + item.insert.len();
        set_value.set(new_text.clone());
        set_textarea_cursor(&input_ref, &new_text, new_cursor);
        // Re-fit: a long completion can wrap the draft onto another line,
        // and the stale inline height would clip it (overflow:hidden).
        resize_textarea();
        if let Some(el) = input_ref.get_untracked() {
            let html_el: &web_sys::HtmlTextAreaElement = el.as_ref();
            let _ = html_el.focus();
        }
        set_popup.set(None);
    };

    // Keep the keyboard-selected row visible when arrowing through a long
    // popup list. Only the mounted InputLine can have a popup.
    Effect::new(move || {
        let _ = popup_sel.get();
        if popup.get().is_none() {
            return;
        }
        if let Some(doc) = web_sys::window().and_then(|w| w.document())
            && let Ok(Some(el)) = doc.query_selector(".completion-popup .completion-item.selected")
        {
            el.scroll_into_view_with_bool(false);
        }
    });

    // Set enterkeyhint for mobile keyboard "Send" button.
    Effect::new(move || {
        if let Some(el) = input_ref.get() {
            let html_el: &web_sys::HtmlTextAreaElement = el.as_ref();
            let _ = html_el.set_attribute("enterkeyhint", "send");
        }
    });

    // Apply a picker-requested insertion (`:name:` or a Unicode emoji) at the
    // caret, then clear the signal. Keeps the `value` signal authoritative.
    Effect::new(move |_| {
        let Some(token) = state.pending_insert.get() else {
            return;
        };
        let Some(el) = input_ref.get_untracked() else {
            return;
        };
        let html_el: &web_sys::HtmlTextAreaElement = el.as_ref();
        let text = value.get_untracked();
        let cursor = get_textarea_cursor(&input_ref, &text);
        let new_text = format!("{}{token}{}", &text[..cursor], &text[cursor..]);
        let new_cursor = cursor + token.len();
        set_value.set(new_text.clone());
        set_textarea_cursor(&input_ref, &new_text, new_cursor);
        let _ = html_el.focus();
        state.pending_insert.set(None);
        // The splice changed `value` without going through on_input — any
        // open popup now holds a stale byte range; drop it. The draft also
        // diverged from any history entry being browsed.
        set_popup.set(None);
        hist_pos.set_value(None);
    });

    // Global keydown listener — focus textarea when user types anywhere.
    // Skipped when active buffer is a shell (shell_view captures input instead).
    let keydown_handle = leptos::leptos_dom::helpers::window_event_listener(
        leptos::ev::keydown,
        move |ev: web_sys::KeyboardEvent| {
            if ev.ctrl_key() || ev.alt_key() || ev.meta_key() {
                return;
            }
            if state.wizard_open.get_untracked()
                || state.emote_picker_open.get_untracked()
                || state.emoji_picker_open.get_untracked()
                || state.appearance_open.get_untracked()
            {
                return;
            }
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
            let key = ev.key();
            if let Some(el) = ev
                .target()
                .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            {
                let tag = el.tag_name();
                let contenteditable = el
                    .dyn_ref::<web_sys::HtmlElement>()
                    .is_some_and(web_sys::HtmlElement::is_content_editable);
                if should_keep_control_focus(&tag, contenteditable, &key) {
                    return;
                }
            }
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
    on_cleanup(move || keydown_handle.remove());

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
        if matches!(
            whole.to_ascii_lowercase().as_str(),
            "/emoji" | "/emote" | "/emotes"
        ) {
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
        // Leading `@nick` → "nick: " rewrite applies to single-line sends
        // only (the hand-typed mobile mention). Multiline text is a paste,
        // and pasted lines must go out byte-for-byte — a quoted log line
        // "@doddy said…" must not be rewritten.
        let text = if !text.contains('\n') && text.starts_with('@') {
            convert_leading_at_mention(&text, &completion_nicks(&state))
        } else {
            text
        };
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

        // Completion popup navigation takes precedence while it is open.
        // Arrows (and Shift+Tab) move the selection; Tab accepts; Enter
        // accepts ONLY once the user has engaged the list — otherwise Enter
        // still means send (a fully typed "/part" or "@doddy hi" must go out
        // on the first press even though the popup still matches its last
        // word). `PopupData.items` is never empty (popup_matches returns
        // None instead), so the modular arithmetic is safe.
        if let Some(p) = popup.get_untracked() {
            let len = p.items.len();
            match ev.key().as_str() {
                "ArrowDown" => {
                    ev.prevent_default();
                    set_popup_engaged.set(true);
                    set_popup_sel.update(|s| *s = (*s + 1) % len);
                    return;
                }
                "ArrowUp" => {
                    ev.prevent_default();
                    set_popup_engaged.set(true);
                    set_popup_sel.update(|s| *s = (*s + len - 1) % len);
                    return;
                }
                // Tab with an arrow-selected row accepts that row. An
                // UNENGAGED Tab closes the popup and falls through to the
                // legacy branch below — that branch arms tab_matches, so
                // repeated Tab presses cycle /join → /jump exactly as they
                // did before the popup existed (accepting row 0 here instead
                // used to kill cycling: the second Tab started a fresh,
                // wrong completion after the inserted trailing space).
                "Tab" => {
                    if popup_engaged.get_untracked() {
                        ev.prevent_default();
                        if ev.shift_key() {
                            set_popup_sel.update(|s| *s = (*s + len - 1) % len);
                        } else {
                            accept_popup(popup_sel.get_untracked().min(len - 1));
                        }
                        return;
                    }
                    set_popup.set(None);
                }
                "Enter" if !ev.shift_key() => {
                    if popup_engaged.get_untracked() {
                        ev.prevent_default();
                        accept_popup(popup_sel.get_untracked().min(len - 1));
                        return;
                    }
                    // Not engaged: Enter means send — close the popup and
                    // fall through to the send branch below.
                    set_popup.set(None);
                }
                "Escape" => {
                    ev.prevent_default();
                    set_popup.set(None);
                    return;
                }
                _ => {}
            }
        }

        // Input history — ↑ on the draft's first line recalls older sends,
        // ↓ on the last line walks back toward the live draft. Vertical
        // caret movement inside a multiline draft is untouched (only the
        // edge lines intercept). The popup block above already returned if
        // the completion list was open.
        let key = ev.key();
        if key == "ArrowUp" || key == "ArrowDown" {
            let up = key == "ArrowUp";
            let text = value.get_untracked();
            let cursor = get_textarea_cursor(&input_ref, &text);
            let at_edge_line = if up {
                !text[..cursor].contains('\n')
            } else {
                !text[cursor..].contains('\n')
            };
            if at_edge_line {
                let len = history.with_value(Vec::len);
                if let Some(next) = history_step(len, hist_pos.get_value(), up) {
                    ev.prevent_default();
                    if hist_pos.get_value().is_none() {
                        hist_draft.set_value(text);
                    }
                    let new_text = match next {
                        Some(i) => history.with_value(|h| h[i].clone()),
                        None => hist_draft.get_value(),
                    };
                    hist_pos.set_value(next);
                    set_value.set(new_text.clone());
                    let end = new_text.len();
                    set_textarea_cursor(&input_ref, &new_text, end);
                    resize_textarea();
                    return;
                }
            }
        }

        if ev.key() == "Enter" && !ev.shift_key() {
            ev.prevent_default();
            set_tab_active.set(false);
            set_popup.set(None);
            let text = value.get();
            push_history(&text);
            send_text(text);
            set_value.set(String::new());
            reset_textarea_height();
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
                // New tab completion. Word boundaries and the addressing
                // delimiter share the popup's rules (word_bounds /
                // at_line_start) so Tab and the popup can't drift.
                let (word_start, typed) = word_bounds(&text, cursor);

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
                        } else if at_line_start(&text, word_start) {
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
        resize_textarea();
        // An edit forks off whatever history entry was being browsed — the
        // current text is the live draft again.
        hist_pos.set_value(None);
        // Recompute the completion popup for the word at the caret.
        let text = value.get_untracked();
        let cursor = get_textarea_cursor(&input_ref, &text);
        let next = compute_popup(&text, cursor, &state);
        set_popup_engaged.set(false);
        set_popup_sel.set(0);
        set_popup.set(next);
    };

    view! {
        <div class="input-line">
            // Completion popup — anchored above the input row. Rows insert on
            // mousedown (with preventDefault) rather than click: mousedown
            // fires before the textarea's blur, so accepting a completion by
            // tap doesn't close the mobile keyboard.
            {move || {
                let p = popup.get()?;
                Some(view! {
                    <div class="completion-popup" role="listbox" aria-label="Completions">
                        {p.items.iter().enumerate().map(|(i, item)| {
                            // Per-row reactive class: arrowing through the
                            // list updates two class attributes instead of
                            // rebuilding all rows (which would restart the
                            // animated emote GIFs on every keypress).
                            let class = move || {
                                if popup_sel.get() == i {
                                    "completion-item selected"
                                } else {
                                    "completion-item"
                                }
                            };
                            let label = item.label.clone();
                            let emote = item.emote_url.clone();
                            let on_mousedown = move |ev: web_sys::MouseEvent| {
                                ev.prevent_default();
                                accept_popup(i);
                            };
                            view! {
                                <div class=class role="option"
                                    aria-selected=move || popup_sel.get() == i
                                    on:mousedown=on_mousedown>
                                    {emote.map(|src| view! {
                                        <img class="completion-emote" src=src alt="" />
                                    })}
                                    <span class="completion-label">{label}</span>
                                </div>
                            }
                        }).collect::<Vec<_>>()}
                    </div>
                })
            }}
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
                aria-label="Message"
                autofocus=true
                autocomplete="off"
                prop:value=value
                node_ref=input_ref
                on:input=on_input
                on:keydown=on_keydown
                // Popup rows accept on mousedown (preventDefault keeps focus),
                // so a blur here can only mean the user left the input — an
                // open popup would otherwise silently eat the next Enter.
                on:blur=move |_| set_popup.set(None)
            ></textarea>
            <button type="button" class="send-btn" aria-label="Send message" on:click=move |_| {
                set_popup.set(None);
                let text = value.get();
                push_history(&text);
                send_text(text);
                set_value.set(String::new());
                reset_textarea_height();
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

/// Next history position for an ↑/↓ step. `len` is the history length,
/// `current` the entry being browsed (`None` = live draft), `up` the
/// direction. `None` = the step does nothing (empty history, ↑ at the
/// oldest, ↓ while not browsing); `Some(next)` otherwise, where
/// `next == None` means "leave history, restore the draft".
fn history_step(len: usize, current: Option<usize>, up: bool) -> Option<Option<usize>> {
    if len == 0 {
        return None;
    }
    match (current, up) {
        (None, true) => Some(Some(len - 1)),
        (None, false) | (Some(0), true) => None,
        (Some(i), true) => Some(Some(i - 1)),
        (Some(i), false) if i + 1 < len => Some(Some(i + 1)),
        (Some(_), false) => Some(None),
    }
}

/// The slash commands that are *messages* rather than commands, with the
/// trailing space that proves they carry text. `/action` is a registered alias
/// of `/me` and produces the identical CTCP ACTION.
///
/// Mirrors `MESSAGE_COMMANDS` in the core (`src/irc/typing.rs`).
const MESSAGE_COMMANDS: [&str; 2] = ["/me ", "/action "];

/// Whether this input text should announce typing.
///
/// Mirrors `irc::typing::should_type` (core, `src/irc/typing.rs`): the spec's
/// carve-out is "the text is not a '/slash command'", and an action is a
/// message, not a command. The core's command parser lowercases command names
/// before dispatch, so `/ME waves` and `/ACTION waves` both execute as actions
/// and must count as typing too.
fn should_type(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    !text.starts_with('/')
        || MESSAGE_COMMANDS.iter().any(|cmd| {
            text.get(..cmd.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(cmd))
        })
}

/// Decide whether the typing predicate must be reported now.
/// A change of predicate OR of target buffer always reports immediately;
/// an unchanged steady state is rate-limited to one report per second.
///
/// The buffer comparison matters because the reporting Effect is
/// edge-triggered: switching buffers with a carried-over draft re-runs it
/// exactly once, and if that run were suppressed the core would keep this
/// session's typing source on the OLD buffer until the next keystroke.
fn should_report_typing(
    typing: bool,
    buffer_id: &str,
    last_state: bool,
    last_buffer: Option<&str>,
    since_last_ms: f64,
) -> bool {
    typing != last_state || last_buffer != Some(buffer_id) || since_last_ms >= 1000.0
}

/// Nicks that can be completed/mentioned in the active buffer: the live nick
/// list for channels, or the peer's nick for query buffers (queries have no
/// NAMES list). Reads signals untracked — callers are event handlers.
fn completion_nicks(state: &AppState) -> Vec<String> {
    let Some(active_id) = state.active_buffer.get_untracked() else {
        return Vec::new();
    };
    let from_list: Vec<String> = state.nick_lists.with_untracked(|lists| {
        lists
            .get(&active_id)
            .map(|l| l.iter().map(|n| n.nick.clone()).collect())
            .unwrap_or_default()
    });
    if !from_list.is_empty() {
        return from_list;
    }
    state.buffers.with_untracked(|bufs| {
        bufs.iter()
            .find(|b| b.id == active_id && b.buffer_type == "query")
            .map(|b| vec![b.name.clone()])
            .unwrap_or_default()
    })
}

/// `(word_start, word)` for the whitespace-delimited word ending at `cursor`.
/// Word boundaries are spaces AND newlines, so multiline drafts complete per
/// line. Shared by Tab completion and the popup so the two can't drift.
fn word_bounds(text: &str, cursor: usize) -> (usize, &str) {
    let cursor = floor_char_boundary(text, cursor);
    let before = &text[..cursor];
    let word_start = before.rfind([' ', '\n']).map_or(0, |i| i + 1);
    (word_start, &before[word_start..])
}

/// Whether everything before `pos` on its own line is blank — the "line
/// start" predicate that selects the addressing `": "` delimiter for
/// completed/tapped nicks (whitespace-only prefixes count as line start).
/// Shared by Tab completion, the popup, and the tapped-nick mention so the
/// three paths can't drift.
fn at_line_start(text: &str, pos: usize) -> bool {
    text[..pos]
        .rsplit('\n')
        .next()
        .unwrap_or("")
        .trim()
        .is_empty()
}

/// Popup computation with a cheap trigger pre-check: the nick list is only
/// materialized when the caret word actually starts with `@` — typing a plain
/// sentence in a 2000-user channel must not clone 2000 nick Strings per
/// keypress.
fn compute_popup(text: &str, cursor: usize, state: &AppState) -> Option<PopupData> {
    let (_, word) = word_bounds(text, cursor);
    let nicks = if word.starts_with('@') {
        completion_nicks(state)
    } else {
        Vec::new()
    };
    popup_matches(text, cursor, &nicks, state.emotes_enabled.get_untracked())
}

/// Completion popup candidates for the word at the caret.
///
/// Triggers (all tap-friendly — phones have no Tab key):
/// - `@prefix`  → nicks (canonical casing; `nick: ` at line start, `nick `
///   mid-line — same delimiters Tab completion uses)
/// - `/prefix`  → commands, only while typing the first word
/// - `:prefix`  → `:name:` emotes (when emotes are enabled), with thumbnails
///
/// Returns `None` when there is nothing to show; the item list is never empty.
fn popup_matches(
    text: &str,
    cursor: usize,
    nicks: &[String],
    emotes_enabled: bool,
) -> Option<PopupData> {
    let cursor = floor_char_boundary(text, cursor);
    let (word_start, word) = word_bounds(text, cursor);

    let mut items: Vec<PopupItem> = Vec::new();

    if word_start == 0 && word.starts_with('/') {
        let prefix = word[1..].to_ascii_lowercase();
        items.extend(
            COMMANDS
                .iter()
                .filter(|c| c.starts_with(&prefix))
                .map(|c| PopupItem {
                    label: format!("/{c}"),
                    insert: format!("/{c} "),
                    emote_url: None,
                }),
        );
    } else if let Some(word_prefix) = word.strip_prefix('@') {
        let prefix = word_prefix.to_lowercase();
        // Addressing delimiter at the start of any line (first or a later
        // line of a multiline draft); plain space mid-sentence.
        let delim = if at_line_start(text, word_start) {
            ": "
        } else {
            " "
        };
        let mut matched: Vec<&String> = nicks
            .iter()
            .filter(|n| n.to_lowercase().starts_with(&prefix))
            .collect();
        matched.sort_by_key(|n| n.to_lowercase());
        matched.dedup();
        items.extend(matched.into_iter().map(|n| PopupItem {
            label: n.clone(),
            insert: format!("{n}{delim}"),
            emote_url: None,
        }));
    } else if emotes_enabled && word.starts_with(':') {
        items.extend(emote_tab_matches(word).into_iter().map(|m| {
            let stem = crate::emotes::stem_for(m.trim_matches(':'));
            PopupItem {
                emote_url: (!stem.is_empty()).then(|| format!("/emotes/{stem}.gif")),
                insert: format!("{m} "),
                label: m,
            }
        }));
    }

    if items.is_empty() {
        return None;
    }
    items.truncate(MAX_POPUP_ITEMS);
    Some(PopupData {
        replace_start: word_start,
        replace_end: cursor,
        items,
    })
}

/// Convert a leading `@nick` mention to irssi-style `nick: ` — the mobile
/// typed form (`@doddy siema`) is sent to IRC as `doddy: siema`. Only the
/// first token converts, and only when the nick is actually present in the
/// buffer (case-insensitive; the list's canonical casing is used), so a
/// literal leading `@handle` that isn't anyone in the channel passes through
/// untouched. Mid-line `@nick` is always left alone.
fn convert_leading_at_mention(line: &str, nicks: &[String]) -> String {
    let Some(rest) = line.strip_prefix('@') else {
        return line.to_string();
    };
    let (token, tail) = match rest.find(' ') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, ""),
    };
    // Tolerate a typed delimiter on the token itself ("@nick:" / "@nick,").
    let token = token.trim_end_matches([':', ',']);
    if token.is_empty() {
        return line.to_string();
    }
    // Unicode-aware folding, matching the popup/Tab paths — an ASCII-only
    // comparison would silently skip non-ASCII nicks ("@żółw" vs "Żółw").
    let token_lower = token.to_lowercase();
    let Some(canonical) = nicks.iter().find(|n| n.to_lowercase() == token_lower) else {
        return line.to_string();
    };
    if tail.is_empty() {
        format!("{canonical}:")
    } else {
        format!("{canonical}: {tail}")
    }
}

/// Build tab completion matches based on input context.
fn build_tab_matches(text: &str, word_start: usize, typed: &str, state: &AppState) -> Vec<String> {
    // Case 1: /command completion — only while typing the FIRST word of the
    // draft (a `/word` at the start of a later line is not a command position
    // for completion; newline counts as a separator like space).
    if text.starts_with('/') && !text[..word_start.max(1)].contains([' ', '\n']) {
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

    // Case 4: Nick completion — same source as the popup (live nick list,
    // with the query-peer fallback), so Tab and the popup can't drift.
    let typed_lower = typed.to_lowercase();
    completion_nicks(state)
        .into_iter()
        .filter(|n| n.to_lowercase().starts_with(&typed_lower))
        .collect()
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

fn should_keep_control_focus(tag: &str, contenteditable: bool, key: &str) -> bool {
    tag.eq_ignore_ascii_case("input")
        || tag.eq_ignore_ascii_case("select")
        || tag.eq_ignore_ascii_case("textarea")
        || contenteditable
        || tag.eq_ignore_ascii_case("button") && matches!(key, " " | "Enter")
        || tag.eq_ignore_ascii_case("a") && key == "Enter"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_key_on_button_returns_focus_to_chat() {
        assert!(!should_keep_control_focus("BUTTON", false, "x"));
    }

    #[test]
    fn space_on_button_keeps_control_focus() {
        assert!(should_keep_control_focus("BUTTON", false, " "));
    }

    #[test]
    fn enter_on_link_keeps_control_focus() {
        assert!(should_keep_control_focus("A", false, "Enter"));
    }

    #[test]
    fn printable_key_in_text_control_keeps_control_focus() {
        assert!(should_keep_control_focus("INPUT", false, "x"));
    }

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
        assert!(
            m.iter().all(|s| !s.ends_with(": ")),
            "no trailing space yet"
        );
    }

    #[test]
    fn emote_tab_is_case_insensitive() {
        assert_eq!(
            emote_tab_matches(":USM").len(),
            emote_tab_matches(":usm").len()
        );
    }

    // ── completion popup ────────────────────────────────────────────────

    fn nicks(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn popup_at_line_start_appends_colon_delimiter() {
        let p = popup_matches("@do", 3, &nicks(&["doddy", "kofany"]), true).unwrap();
        assert_eq!(p.replace_start, 0);
        assert_eq!(p.replace_end, 3);
        assert_eq!(p.items.len(), 1);
        assert_eq!(p.items[0].label, "doddy");
        assert_eq!(p.items[0].insert, "doddy: ");
    }

    #[test]
    fn popup_at_midline_appends_space_delimiter() {
        let p = popup_matches("hej @do", 7, &nicks(&["doddy"]), true).unwrap();
        assert_eq!(p.replace_start, 4);
        assert_eq!(p.items[0].insert, "doddy ");
    }

    #[test]
    fn popup_bare_at_lists_all_nicks_sorted_case_insensitively() {
        let p = popup_matches("@", 1, &nicks(&["Zed", "alice"]), true).unwrap();
        let labels: Vec<&str> = p.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["alice", "Zed"]);
    }

    #[test]
    fn popup_nick_match_is_case_insensitive_with_canonical_casing() {
        let p = popup_matches("@DO", 3, &nicks(&["DoDdy"]), true).unwrap();
        assert_eq!(p.items[0].label, "DoDdy");
        assert_eq!(p.items[0].insert, "DoDdy: ");
    }

    #[test]
    fn popup_caps_item_count() {
        let many: Vec<String> = (0..200).map(|i| format!("nick{i:03}")).collect();
        let p = popup_matches("@nick", 5, &many, true).unwrap();
        assert_eq!(p.items.len(), MAX_POPUP_ITEMS);
    }

    #[test]
    fn popup_none_when_nothing_matches() {
        assert!(popup_matches("@zz", 3, &nicks(&["doddy"]), true).is_none());
        assert!(popup_matches("plain text", 10, &nicks(&["doddy"]), true).is_none());
        assert!(popup_matches("", 0, &nicks(&["doddy"]), true).is_none());
    }

    #[test]
    fn popup_command_trigger_only_in_first_word() {
        let p = popup_matches("/joi", 4, &[], true).unwrap();
        assert_eq!(p.items[0].label, "/join");
        assert_eq!(p.items[0].insert, "/join ");
        assert_eq!(p.replace_start, 0);
        // Past the first word, `/…` must not re-trigger command completion.
        assert!(popup_matches("/msg /joi", 9, &[], true).is_none());
    }

    #[test]
    fn popup_emote_trigger_carries_thumbnail_and_respects_toggle() {
        let p = popup_matches(":usm", 4, &[], true).unwrap();
        assert!(p.items[0].label.starts_with(':'));
        assert!(p.items[0].insert.ends_with(": ") || p.items[0].insert.ends_with(' '));
        assert!(
            p.items[0]
                .emote_url
                .as_deref()
                .is_some_and(|u| u.starts_with("/emotes/") && u.ends_with(".gif"))
        );
        assert!(popup_matches(":usm", 4, &[], false).is_none(), "gated off");
    }

    #[test]
    fn popup_survives_multibyte_text_before_word() {
        // "żółć @do" — multibyte chars before the trigger word.
        let text = "\u{17c}\u{f3}\u{142}\u{107} @do";
        let cursor = text.len();
        let p = popup_matches(text, cursor, &nicks(&["doddy"]), true).unwrap();
        assert_eq!(p.items[0].insert, "doddy ");
        assert_eq!(&text[p.replace_start..p.replace_end], "@do");
    }

    #[test]
    fn popup_whitespace_prefix_counts_as_line_start() {
        // "  @do" — only blanks before the trigger word: still the
        // addressing form, matching the tapped-nick mention path.
        let p = popup_matches("  @do", 5, &nicks(&["doddy"]), true).unwrap();
        assert_eq!(p.items[0].insert, "doddy: ");
    }

    #[test]
    fn popup_word_detection_respects_newlines() {
        // The start of a later line in a multiline draft is still a line
        // start — the addressing ": " delimiter applies there too.
        let text = "first line\n@do";
        let p = popup_matches(text, text.len(), &nicks(&["doddy"]), true).unwrap();
        assert_eq!(p.items[0].insert, "doddy: ");
        assert_eq!(&text[p.replace_start..p.replace_end], "@do");
    }

    // ── leading @nick → "nick: " send rewrite ───────────────────────────

    #[test]
    fn mention_converts_with_canonical_casing() {
        assert_eq!(
            convert_leading_at_mention("@DoDDy siema", &nicks(&["Doddy"])),
            "Doddy: siema"
        );
    }

    #[test]
    fn mention_unknown_nick_passes_through() {
        assert_eq!(
            convert_leading_at_mention("@stranger hi", &nicks(&["doddy"])),
            "@stranger hi"
        );
    }

    #[test]
    fn mention_midline_untouched() {
        assert_eq!(
            convert_leading_at_mention("hej @doddy co tam", &nicks(&["doddy"])),
            "hej @doddy co tam"
        );
    }

    #[test]
    fn mention_bare_token_gets_colon() {
        assert_eq!(
            convert_leading_at_mention("@doddy", &nicks(&["doddy"])),
            "doddy:"
        );
    }

    #[test]
    fn mention_tolerates_typed_delimiter() {
        assert_eq!(
            convert_leading_at_mention("@doddy: hej", &nicks(&["doddy"])),
            "doddy: hej"
        );
        assert_eq!(
            convert_leading_at_mention("@doddy, hej", &nicks(&["doddy"])),
            "doddy: hej"
        );
    }

    // ── input history stepping ──────────────────────────────────────────

    #[test]
    fn history_step_walks_up_then_back_to_draft() {
        // 3 entries; from the draft, ↑ lands on the newest (index 2).
        assert_eq!(history_step(3, None, true), Some(Some(2)));
        assert_eq!(history_step(3, Some(2), true), Some(Some(1)));
        assert_eq!(history_step(3, Some(1), true), Some(Some(0)));
        // ↑ at the oldest: no-op.
        assert_eq!(history_step(3, Some(0), true), None);
        // ↓ walks forward; past the newest → back to the draft (None).
        assert_eq!(history_step(3, Some(1), false), Some(Some(2)));
        assert_eq!(history_step(3, Some(2), false), Some(None));
    }

    #[test]
    fn history_step_noop_cases() {
        assert_eq!(history_step(0, None, true), None, "empty history");
        assert_eq!(
            history_step(3, None, false),
            None,
            "down while not browsing"
        );
    }

    // ── the typing predicate ────────────────────────────────────────────

    #[test]
    fn plain_text_types_and_slash_commands_do_not() {
        assert!(should_type("hello"));
        assert!(!should_type(""));
        assert!(!should_type("/join #rust"));
    }

    #[test]
    fn actions_type_under_either_name_and_in_any_case() {
        // `/me` and its `/action` alias are messages, not commands: both put a
        // CTCP ACTION on the wire, so both must announce typing. The core
        // lowercases the verb before dispatch, so case must not matter here.
        for text in [
            "/me waves",
            "/ME waves",
            "/action waves",
            "/ACTION waves",
            "/AcTiOn waves",
        ] {
            assert!(should_type(text), "{text} is a message, not a command");
        }
        // Bare, with no text, is just a command.
        assert!(!should_type("/me"));
        assert!(!should_type("/action"));
        // And a longer command that merely shares a prefix is still a command —
        // the trailing space in the pattern is what separates them.
        assert!(!should_type("/actionfoo bar"));
        assert!(!should_type("/mention bob"));
    }

    // ── typing report debounce ──────────────────────────────────────────

    #[test]
    fn typing_report_state_flip_inside_window_reports() {
        assert!(should_report_typing(true, "#a", false, Some("#a"), 100.0));
        assert!(should_report_typing(false, "#a", true, Some("#a"), 100.0));
    }

    #[test]
    fn typing_report_buffer_change_same_state_inside_window_reports() {
        // THE regression: a draft carried into a new buffer within the 1s
        // window — the predicate is unchanged but the target moved, and
        // Effects are edge-triggered, so suppressing here would leave the
        // core holding this session's typing source on the OLD buffer until
        // the user types again.
        assert!(should_report_typing(true, "#b", true, Some("#a"), 100.0));
    }

    #[test]
    fn typing_report_steady_state_inside_window_is_suppressed() {
        assert!(!should_report_typing(true, "#a", true, Some("#a"), 100.0));
    }

    #[test]
    fn typing_report_steady_state_past_window_reports() {
        assert!(should_report_typing(true, "#a", true, Some("#a"), 1000.0));
    }

    #[test]
    fn typing_report_first_ever_call_reports() {
        assert!(should_report_typing(false, "#a", false, None, 0.0));
    }

    #[test]
    fn mention_plain_line_untouched() {
        assert_eq!(
            convert_leading_at_mention("no mention here", &nicks(&["doddy"])),
            "no mention here"
        );
        assert_eq!(convert_leading_at_mention("@", &nicks(&["doddy"])), "@");
    }
}

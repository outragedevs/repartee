use leptos::prelude::*;
use web_sys::KeyboardEvent;

use crate::protocol::{ShellSpan, WebCommand};
use crate::state::AppState;

/// Renders an embedded shell terminal as a grid of styled text spans.
///
/// The backend serializes the vt100 screen into rows of RLE-compressed spans
/// and streams them via `WebEvent::ShellScreen`. This component renders them
/// as `<pre>` with `<span>` elements per style run.
#[component]
pub fn ShellView() -> impl IntoView {
    let state = use_context::<AppState>().expect("AppState not provided");

    // Handle keyboard input — forward to shell PTY via WebSocket.
    let on_keydown = move |ev: KeyboardEvent| {
        let Some(buffer_id) = state.active_buffer.get_untracked() else {
            return;
        };

        // Don't capture browser shortcuts.
        if ev.meta_key() || (ev.ctrl_key() && matches!(ev.key().as_str(), "c" | "v" | "a" | "r" | "l" | "t" | "w")) {
            return;
        }

        ev.prevent_default();

        let bytes = key_event_to_bytes(&ev);
        if bytes.is_empty() {
            return;
        }

        let data = base64_encode(&bytes);
        crate::ws::send_command(&WebCommand::ShellInput { buffer_id, data });
    };

    view! {
        <div
            class="shell-terminal"
            tabindex="0"
            on:keydown=on_keydown
        >
            {move || {
                let screen = state.shell_screen.get();
                match screen {
                    Some(data) => {
                        data.rows.iter().map(|row| {
                            let spans_view: Vec<_> = row.spans.iter().map(|span| {
                                let css = span_to_css(span);
                                let text = span.text.clone();
                                if css.is_empty() {
                                    view! { <span>{text}</span> }.into_any()
                                } else {
                                    view! { <span style=css>{text}</span> }.into_any()
                                }
                            }).collect();
                            view! { <div class="shell-row">{spans_view}</div> }.into_any()
                        }).collect::<Vec<_>>()
                    }
                    None => {
                        vec![view! { <div class="shell-placeholder">"Shell loading..."</div> }.into_any()]
                    }
                }
            }}
        </div>
    }
}

/// Convert a `ShellSpan` to inline CSS style string.
fn span_to_css(span: &ShellSpan) -> String {
    let mut parts = Vec::new();

    if !span.fg.is_empty() && !span.fg.starts_with("ansi(") {
        if span.inverse {
            parts.push(format!("background-color:{}", span.fg));
        } else {
            parts.push(format!("color:{}", span.fg));
        }
    }
    if !span.bg.is_empty() && !span.bg.starts_with("ansi(") {
        if span.inverse {
            parts.push(format!("color:{}", span.bg));
        } else {
            parts.push(format!("background-color:{}", span.bg));
        }
    }
    if span.bold {
        parts.push("font-weight:bold".to_string());
    }
    if span.italic {
        parts.push("font-style:italic".to_string());
    }
    if span.underline {
        parts.push("text-decoration:underline".to_string());
    }
    // Handle inverse when no explicit colors — swap default fg/bg via CSS class instead.
    if span.inverse && span.fg.is_empty() && span.bg.is_empty() {
        parts.push("filter:invert(1)".to_string());
    }

    parts.join(";")
}

/// Encode bytes to base64 using the browser's btoa().
fn base64_encode(bytes: &[u8]) -> String {
    let binary: String = bytes.iter().map(|&b| b as char).collect();
    web_sys::window()
        .and_then(|w| w.btoa(&binary).ok())
        .unwrap_or_default()
}

/// Convert a browser KeyboardEvent to terminal escape bytes.
fn key_event_to_bytes(ev: &KeyboardEvent) -> Vec<u8> {
    let key = ev.key();
    let ctrl = ev.ctrl_key();
    let alt = ev.alt_key();

    // Ctrl+letter → control character.
    if ctrl && key.len() == 1 {
        let ch = key.bytes().next().unwrap_or(0);
        if ch.is_ascii_alphabetic() {
            let byte = (ch.to_ascii_lowercase()) - b'a' + 1;
            return if alt { vec![0x1b, byte] } else { vec![byte] };
        }
    }

    // Special keys.
    let base: &[u8] = match key.as_str() {
        "Enter" => b"\r",
        "Backspace" => &[0x7f],
        "Tab" => b"\t",
        "Escape" => &[0x1b],
        "ArrowUp" => b"\x1b[A",
        "ArrowDown" => b"\x1b[B",
        "ArrowRight" => b"\x1b[C",
        "ArrowLeft" => b"\x1b[D",
        "Home" => b"\x1b[H",
        "End" => b"\x1b[F",
        "PageUp" => b"\x1b[5~",
        "PageDown" => b"\x1b[6~",
        "Insert" => b"\x1b[2~",
        "Delete" => b"\x1b[3~",
        "F1" => b"\x1bOP",
        "F2" => b"\x1bOQ",
        "F3" => b"\x1bOR",
        "F4" => b"\x1bOS",
        "F5" => b"\x1b[15~",
        "F6" => b"\x1b[17~",
        "F7" => b"\x1b[18~",
        "F8" => b"\x1b[19~",
        "F9" => b"\x1b[20~",
        "F10" => b"\x1b[21~",
        "F11" => b"\x1b[23~",
        "F12" => b"\x1b[24~",
        _ => b"",
    };

    if !base.is_empty() {
        return base.to_vec();
    }

    // Regular character input.
    if key.len() == 1 || key.chars().count() == 1 {
        let mut bytes = Vec::new();
        if alt {
            bytes.push(0x1b);
        }
        bytes.extend_from_slice(key.as_bytes());
        return bytes;
    }

    Vec::new()
}

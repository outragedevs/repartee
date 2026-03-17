use std::cell::RefCell;
use std::rc::Rc;

use beamterm_renderer::{
    CellData, FontStyle, GlyphEffect, SelectionMode, Terminal, is_double_width,
    mouse::MouseSelectOptions,
};
use leptos::prelude::*;
use web_sys::KeyboardEvent;

use crate::protocol::{ShellScreenData, ShellSpan, WebCommand};
use crate::state::AppState;

/// Wraps the beamterm Terminal with size tracking to avoid redundant resizes.
struct ShellTerminal {
    terminal: Terminal,
    last_width: i32,
    last_height: i32,
}

/// Default terminal colors (dark background, light text).
const DEFAULT_FG: u32 = 0xc0_c0_c0;
const DEFAULT_BG: u32 = 0x00_00_00;

/// Standard ANSI 16-color palette.
const ANSI_COLORS: [u32; 16] = [
    0x00_00_00, // 0: black
    0xaa_00_00, // 1: red
    0x00_aa_00, // 2: green
    0xaa_55_00, // 3: brown/yellow
    0x00_00_aa, // 4: blue
    0xaa_00_aa, // 5: magenta
    0x00_aa_aa, // 6: cyan
    0xaa_aa_aa, // 7: white
    0x55_55_55, // 8: bright black
    0xff_55_55, // 9: bright red
    0x55_ff_55, // 10: bright green
    0xff_ff_55, // 11: bright yellow
    0x55_55_ff, // 12: bright blue
    0xff_55_ff, // 13: bright magenta
    0x55_ff_ff, // 14: bright cyan
    0xff_ff_ff, // 15: bright white
];

/// 6x6x6 color cube intensity values for 256-color palette (indices 16–231).
const COLOR_CUBE: [u8; 6] = [0x00, 0x5f, 0x87, 0xaf, 0xd7, 0xff];

/// Renders an embedded shell terminal via a WebGL2 canvas powered by beamterm.
///
/// The backend serializes the vt100 screen into rows of RLE-compressed spans
/// and streams them via `WebEvent::ShellScreen`. This component converts them
/// to beamterm `CellData` and renders via GPU-accelerated instanced draw calls.
#[component]
pub fn ShellView() -> impl IntoView {
    let state = use_context::<AppState>().expect("AppState not provided");
    let shell_term: Rc<RefCell<Option<ShellTerminal>>> = Rc::new(RefCell::new(None));
    let canvas_ref = NodeRef::<leptos::html::Canvas>::new();

    // Combined init + render effect: fires on every shell_screen signal change.
    let term = shell_term.clone();
    Effect::new(move || {
        let screen = state.shell_screen.get();
        let mut borrow = term.borrow_mut();

        // Lazy-init the beamterm Terminal on first render.
        if borrow.is_none() {
            match Terminal::builder("#shell-canvas")
                .dynamic_font_atlas(
                    &[
                        "Menlo",
                        "DejaVu Sans Mono",
                        "Liberation Mono",
                        "Consolas",
                        "Courier New",
                        "monospace",
                    ],
                    13.0,
                )
                .canvas_padding_color(DEFAULT_BG)
                .mouse_selection_handler(
                    MouseSelectOptions::new()
                        .selection_mode(SelectionMode::Linear)
                        .trim_trailing_whitespace(true),
                )
                .build()
            {
                Ok(mut t) => {
                    // The canvas defaults to 300x150. Read the actual CSS layout
                    // dimensions and resize to fill the chat area.
                    let (w, h) = canvas_parent_size();
                    if w > 0 && h > 0 {
                        let _ = t.resize(w, h);
                    }
                    // Tell the server to resize the PTY to match our grid.
                    send_shell_resize(&state, &t);
                    *borrow = Some(ShellTerminal {
                        terminal: t,
                        last_width: w,
                        last_height: h,
                    });
                }
                Err(e) => {
                    web_sys::console::error_1(
                        &format!("beamterm init: {e:?}").into(),
                    );
                    return;
                }
            }
        }

        let Some(st) = borrow.as_mut() else { return };
        let Some(data) = screen else { return };

        // Resize if the container changed (browser window resize).
        let (w, h) = canvas_parent_size();
        if w > 0 && h > 0 && (w != st.last_width || h != st.last_height) {
            let _ = st.terminal.resize(w, h);
            st.last_width = w;
            st.last_height = h;
            // Tell the server to resize the PTY to match our new grid.
            send_shell_resize(&state, &st.terminal);
        }

        render_screen(&mut st.terminal, &data);
    });

    // Auto-focus the canvas when shell screen updates or mounts.
    Effect::new(move || {
        let _ = state.shell_screen.get();
        if let Some(el) = canvas_ref.get() {
            let html_el: &web_sys::HtmlElement = el.as_ref();
            let _ = html_el.focus();
        }
    });

    // Keyboard input — forward to shell PTY via WebSocket.
    let on_keydown = move |ev: KeyboardEvent| {
        let Some(buffer_id) = state.active_buffer.get_untracked() else {
            return;
        };

        // Don't capture browser shortcuts.
        let key_lower = ev.key().to_lowercase();
        if ev.meta_key()
            || (ev.ctrl_key()
                && matches!(
                    key_lower.as_str(),
                    "c" | "v" | "a" | "r" | "l" | "t" | "w"
                ))
        {
            return;
        }

        let bytes = key_event_to_bytes(&ev);
        if bytes.is_empty() {
            return;
        }

        // Only suppress browser defaults for keys we actually handle.
        ev.prevent_default();

        let data = base64_encode(&bytes);
        crate::ws::send_command(&WebCommand::ShellInput { buffer_id, data });
    };

    view! {
        <canvas
            id="shell-canvas"
            class="shell-terminal"
            tabindex="0"
            node_ref=canvas_ref
            on:keydown=on_keydown
        />
    }
}

/// Full-screen refresh: convert shell screen data to a flat cell grid for beamterm.
///
/// Uses `update_cells()` (not `by_position`) so every cell in the grid is written,
/// which clears stale content and ensures selection rendering works on every frame.
/// Each PTY row is padded with spaces to match the beamterm grid width.
fn render_screen(terminal: &mut Terminal, data: &ShellScreenData) {
    let (grid_cols, grid_rows) = terminal.terminal_size();
    // Use the smaller of PTY cols vs beamterm grid cols to avoid overflow.
    let pty_cols = data.cols;
    let cols = grid_cols.min(pty_cols);
    let total = grid_cols as usize * grid_rows as usize;
    let mut cells: Vec<CellData<'_>> = Vec::with_capacity(total);

    let blank = CellData::new(" ", FontStyle::Normal, GlyphEffect::None, DEFAULT_FG, DEFAULT_BG);

    for row_idx in 0..grid_rows as usize {
        let mut col: u16 = 0;

        if let Some(row) = data.rows.get(row_idx) {
            for span in &row.spans {
                let (fg, bg) = resolve_colors(span);
                let style = resolve_style(span);
                let effect = if span.underline {
                    GlyphEffect::Underline
                } else {
                    GlyphEffect::None
                };

                for (byte_idx, ch) in span.text.char_indices() {
                    if col >= cols {
                        break;
                    }

                    let end = byte_idx + ch.len_utf8();
                    let symbol = &span.text[byte_idx..end];

                    // Render cursor as inverted block.
                    let is_cursor = data.cursor_visible
                        && row_idx == data.cursor_row as usize
                        && col == data.cursor_col;

                    if is_cursor {
                        let cursor_sym = if symbol.trim().is_empty() { "█" } else { symbol };
                        cells.push(CellData::new(cursor_sym, style, effect, bg, fg));
                    } else {
                        cells.push(CellData::new(symbol, style, effect, fg, bg));
                    }
                    col += 1;

                    // Wide characters occupy 2 terminal columns.
                    if is_double_width(symbol) && col < cols {
                        cells.push(CellData::new(" ", FontStyle::Normal, GlyphEffect::None, fg, bg));
                        col += 1;
                    }
                }
            }
        }

        // Pad remainder of row with blank cells.
        while col < grid_cols {
            cells.push(blank);
            col += 1;
        }
    }

    let _ = terminal.update_cells(cells.into_iter());
    let _ = terminal.render_frame();
}

/// Send a `ShellResize` command to the server so the PTY matches our grid.
fn send_shell_resize(state: &AppState, terminal: &Terminal) {
    let Some(buffer_id) = state.active_buffer.get_untracked() else {
        return;
    };
    let (cols, rows) = terminal.terminal_size();
    if cols > 0 && rows > 0 {
        crate::ws::send_command(&WebCommand::ShellResize {
            buffer_id,
            cols,
            rows,
        });
    }
}

/// Read the chat-area container dimensions for canvas sizing.
///
/// We read the **parent** element (`.chat-area`) rather than the canvas itself,
/// because the canvas drawing buffer size can differ from its CSS layout size.
fn canvas_parent_size() -> (i32, i32) {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("shell-canvas"))
        .and_then(|el| el.parent_element())
        .map(|parent| (parent.client_width(), parent.client_height()))
        .unwrap_or((0, 0))
}

/// Resolve foreground and background colors from a span, handling inverse.
fn resolve_colors(span: &ShellSpan) -> (u32, u32) {
    let fg = parse_color(&span.fg, DEFAULT_FG);
    let bg = parse_color(&span.bg, DEFAULT_BG);
    if span.inverse { (bg, fg) } else { (fg, bg) }
}

/// Map bold/italic flags to beamterm FontStyle.
fn resolve_style(span: &ShellSpan) -> FontStyle {
    match (span.bold, span.italic) {
        (true, true) => FontStyle::BoldItalic,
        (true, false) => FontStyle::Bold,
        (false, true) => FontStyle::Italic,
        (false, false) => FontStyle::Normal,
    }
}

/// Parse a color string from the backend into a u32 RGB value.
///
/// Supported formats:
/// - `"#rrggbb"` — CSS hex color
/// - `"ansi(N)"` — 256-color palette index
/// - `""` (empty) — returns the default
fn parse_color(color_str: &str, default: u32) -> u32 {
    if color_str.is_empty() {
        return default;
    }

    if let Some(hex) = color_str.strip_prefix('#') {
        return u32::from_str_radix(hex, 16).unwrap_or(default);
    }

    if let Some(inner) = color_str
        .strip_prefix("ansi(")
        .and_then(|s| s.strip_suffix(')'))
        && let Ok(idx) = inner.parse::<u8>()
    {
        return ansi_index_to_rgb(idx);
    }

    default
}

/// Convert a 256-color palette index to an RGB u32.
fn ansi_index_to_rgb(idx: u8) -> u32 {
    match idx {
        0..=15 => ANSI_COLORS[idx as usize],
        16..=231 => {
            let i = idx - 16;
            let r = COLOR_CUBE[(i / 36) as usize];
            let g = COLOR_CUBE[((i % 36) / 6) as usize];
            let b = COLOR_CUBE[(i % 6) as usize];
            (r as u32) << 16 | (g as u32) << 8 | (b as u32)
        }
        232..=255 => {
            let v = 8 + 10 * (idx as u32 - 232);
            v << 16 | v << 8 | v
        }
    }
}

/// Encode bytes to base64 using the browser's `btoa()`.
fn base64_encode(bytes: &[u8]) -> String {
    let binary: String = bytes.iter().map(|&b| b as char).collect();
    web_sys::window()
        .and_then(|w| w.btoa(&binary).ok())
        .unwrap_or_default()
}

/// Convert a browser `KeyboardEvent` to terminal escape bytes.
fn key_event_to_bytes(ev: &KeyboardEvent) -> Vec<u8> {
    let key = ev.key();
    let ctrl = ev.ctrl_key();
    let alt = ev.alt_key();

    // Ctrl+letter → control character.
    if ctrl && key.len() == 1 {
        let ch = key.bytes().next().unwrap_or(0);
        if ch.is_ascii_alphabetic() {
            let byte = ch.to_ascii_lowercase() - b'a' + 1;
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

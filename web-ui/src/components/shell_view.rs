use std::cell::RefCell;
use std::rc::Rc;

use beamterm_renderer::{
    CellData, FontStyle, GlyphEffect, SelectionMode, Terminal, is_double_width,
    mouse::MouseSelectOptions,
};
use leptos::prelude::*;
use wasm_bindgen::prelude::*;
use web_sys::KeyboardEvent;

use crate::protocol::{ShellScreenData, ShellSpan, WebCommand};
use crate::state::AppState;

/// Wraps the beamterm Terminal with size tracking and font state.
struct ShellTerminal {
    terminal: Terminal,
    last_width: i32,
    last_height: i32,
    font_size: f32,
}

// ── Theme: Ghostty palette (from user's ghostty config) ──────────────────────

/// Default foreground color (ghostty foreground).
const DEFAULT_FG: u32 = 0xed_ef_f1;
/// Default background color (ghostty background).
const DEFAULT_BG: u32 = 0x28_32_37;
/// Cursor color (ghostty cursor-color).
const CURSOR_COLOR: u32 = 0xee_ee_ee;

/// Ghostty 16-color ANSI palette.
const ANSI_COLORS: [u32; 16] = [
    0x43_5b_67, // 0: black
    0xfc_38_41, // 1: red
    0x5c_f1_9e, // 2: green
    0xfe_d0_32, // 3: yellow
    0x37_b6_ff, // 4: blue
    0xfc_22_6e, // 5: magenta
    0x59_ff_d1, // 6: cyan
    0xff_ff_ff, // 7: white
    0xa1_b0_b8, // 8: bright black
    0xfc_74_6d, // 9: bright red
    0xad_f7_be, // 10: bright green
    0xfe_e1_6c, // 11: bright yellow
    0x70_cf_ff, // 12: bright blue
    0xfc_66_9b, // 13: bright magenta
    0x9a_ff_e6, // 14: bright cyan
    0xff_ff_ff, // 15: bright white
];

/// 6x6x6 color cube intensity values for 256-color palette (indices 16-231).
const COLOR_CUBE: [u8; 6] = [0x00, 0x5f, 0x87, 0xaf, 0xd7, 0xff];

// ── Font configuration ───────────────────────────────────────────────────────

/// Font families for beamterm dynamic atlas (order = priority).
const FONT_FAMILIES: &[&str] = &[
    "FiraCode Nerd Font Mono",
    "Fira Code",
    "JetBrains Mono",
    "monospace",
];

const DEFAULT_FONT_SIZE: f32 = 15.0;
const MIN_FONT_SIZE: f32 = 8.0;
const MAX_FONT_SIZE: f32 = 42.0;
const FONT_STEP: f32 = 1.0;

// ── Component ────────────────────────────────────────────────────────────────

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
    // Track whether the rAF loop has been started (start only once).
    let loop_started: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));

    // Single Effect: when shell_screen changes, start the rAF loop (once) and
    // auto-focus. The rAF loop handles ALL rendering — init, resize, update_cells,
    // render_frame. This avoids borrow conflicts between Effect and rAF.
    let term = shell_term.clone();
    let started = loop_started.clone();
    Effect::new(move || {
        // Subscribe to changes.
        let _ = state.shell_screen.get();

        // Auto-focus canvas.
        if let Some(el) = canvas_ref.get() {
            let html_el: &web_sys::HtmlElement = el.as_ref();
            let _ = html_el.focus();
        }

        // Start the rAF render loop exactly once.
        if !*started.borrow() {
            *started.borrow_mut() = true;
            start_render_loop(term.clone(), state);
        }
    });

    // Keyboard input — forward to shell PTY via WebSocket, handle font resize.
    let term_key = shell_term.clone();
    let on_keydown = move |ev: KeyboardEvent| {
        let Some(buffer_id) = state.active_buffer.get_untracked() else {
            return;
        };

        // Don't capture browser shortcuts (except our font size keys).
        let key_lower = ev.key().to_lowercase();
        let is_ctrl_or_meta = ev.ctrl_key() || ev.meta_key();

        // Ctrl/Cmd +/- font resize (handled locally, not sent to PTY).
        if is_ctrl_or_meta
            && matches!(key_lower.as_str(), "=" | "+" | "-" | "0")
        {
            ev.prevent_default();
            let mut borrow = term_key.borrow_mut();
            if let Some(st) = borrow.as_mut() {
                let new_size = match key_lower.as_str() {
                    "=" | "+" => (st.font_size + FONT_STEP).min(MAX_FONT_SIZE),
                    "-" => (st.font_size - FONT_STEP).max(MIN_FONT_SIZE),
                    "0" => DEFAULT_FONT_SIZE,
                    _ => st.font_size,
                };
                if (new_size - st.font_size).abs() > f32::EPSILON {
                    st.font_size = new_size;
                    let _ = st.terminal.replace_with_dynamic_atlas(FONT_FAMILIES, new_size);
                    // Grid dimensions may have changed — resize PTY.
                    let (w, h) = canvas_parent_size();
                    if w > 0 && h > 0 {
                        let _ = st.terminal.resize(w, h);
                    }
                    send_shell_resize(&state, &st.terminal);
                }
            }
            return;
        }

        // Pass through browser shortcuts.
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

// ── Animation loop ───────────────────────────────────────────────────────────

/// Single requestAnimationFrame loop that owns ALL rendering.
///
/// Every frame: lazy-init terminal → check resize → rebuild cells → render.
/// This avoids borrow conflicts with Leptos Effects and ensures selection
/// highlights are always painted (beamterm needs dirty cells for selection
/// color flipping, which requires update_cells() every frame).
fn start_render_loop(term: Rc<RefCell<Option<ShellTerminal>>>, state: AppState) {
    let cb: Rc<RefCell<Option<Closure<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
    let cb_clone = cb.clone();

    *cb.borrow_mut() = Some(Closure::new(move || {
        // Try to borrow — if something else holds it (e.g. keydown handler),
        // just skip this frame. The next rAF will pick it up.
        let Ok(mut borrow) = term.try_borrow_mut() else {
            schedule_next_frame(&cb_clone);
            return;
        };

        // Lazy-init the beamterm Terminal.
        if borrow.is_none() {
            match Terminal::builder("#shell-canvas")
                .dynamic_font_atlas(FONT_FAMILIES, DEFAULT_FONT_SIZE)
                .canvas_padding_color(DEFAULT_BG)
                .mouse_selection_handler(
                    MouseSelectOptions::new()
                        .selection_mode(SelectionMode::Linear)
                        .trim_trailing_whitespace(true),
                )
                .build()
            {
                Ok(mut t) => {
                    let (w, h) = canvas_parent_size();
                    if w > 0 && h > 0 {
                        let _ = t.resize(w, h);
                    }
                    send_shell_resize(&state, &t);
                    *borrow = Some(ShellTerminal {
                        terminal: t,
                        last_width: w,
                        last_height: h,
                        font_size: DEFAULT_FONT_SIZE,
                    });
                }
                Err(e) => {
                    web_sys::console::error_1(&format!("beamterm init: {e:?}").into());
                    schedule_next_frame(&cb_clone);
                    return;
                }
            }
        }

        let Some(st) = borrow.as_mut() else {
            schedule_next_frame(&cb_clone);
            return;
        };

        // Check for container resize.
        let (w, h) = canvas_parent_size();
        if w > 0 && h > 0 && (w != st.last_width || h != st.last_height) {
            let _ = st.terminal.resize(w, h);
            st.last_width = w;
            st.last_height = h;
            send_shell_resize(&state, &st.terminal);
        }

        // Rebuild cells from current screen data and render.
        // update_cells() marks all dirty → flush_cells() does selection flipping.
        if let Some(data) = state.shell_screen.get_untracked() {
            render_screen(&mut st.terminal, &data);
        } else {
            let _ = st.terminal.render_frame();
        }

        // Release borrow before scheduling next frame.
        drop(borrow);
        schedule_next_frame(&cb_clone);
    }));

    schedule_next_frame(&cb);
}

fn schedule_next_frame(cb: &Rc<RefCell<Option<Closure<dyn FnMut()>>>>) {
    if let Some(win) = web_sys::window() {
        if let Some(ref closure) = *cb.borrow() {
            let _ = win.request_animation_frame(closure.as_ref().unchecked_ref());
        }
    }
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Full-screen refresh: convert shell screen data to a flat cell grid for beamterm.
///
/// Uses `update_cells()` so every cell in the grid is written, which clears
/// stale content and ensures selection rendering works on every frame.
fn render_screen(terminal: &mut Terminal, data: &ShellScreenData) {
    let (grid_cols, grid_rows) = terminal.terminal_size();
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

                    // Cursor: always show block (█) with cursor color.
                    let is_cursor = data.cursor_visible
                        && row_idx == data.cursor_row as usize
                        && col == data.cursor_col;

                    if is_cursor {
                        // Block cursor: show character over cursor-colored background.
                        let cursor_bg = CURSOR_COLOR;
                        let cursor_fg = DEFAULT_BG;
                        cells.push(CellData::new(symbol, style, effect, cursor_fg, cursor_bg));
                    } else {
                        cells.push(CellData::new(symbol, style, effect, fg, bg));
                    }
                    col += 1;

                    if is_double_width(symbol) && col < cols {
                        cells.push(CellData::new(
                            " ",
                            FontStyle::Normal,
                            GlyphEffect::None,
                            fg,
                            bg,
                        ));
                        col += 1;
                    }
                }
            }
        }

        // Pad remainder of row — check if cursor is in the padding area.
        while col < grid_cols {
            let is_cursor = data.cursor_visible
                && row_idx == data.cursor_row as usize
                && col == data.cursor_col;
            if is_cursor {
                cells.push(CellData::new(
                    " ",
                    FontStyle::Normal,
                    GlyphEffect::None,
                    DEFAULT_BG,
                    CURSOR_COLOR,
                ));
            } else {
                cells.push(blank);
            }
            col += 1;
        }
    }

    let _ = terminal.update_cells(cells.into_iter());
    let _ = terminal.render_frame();
}

// ── Helpers ──────────────────────────────────────────────────────────────────

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

    // Ctrl+letter -> control character.
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

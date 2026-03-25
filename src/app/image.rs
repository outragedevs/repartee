use ratatui::layout::Rect;
use ratatui_image::picker::ProtocolType;

use super::App;

impl App {
    /// Show an image preview for the given URL.
    ///
    /// Re-detects the outer terminal and image protocol every time — the user
    /// may have attached from a different terminal (e.g. `tmux a` from iTerm2
    /// after starting in Ghostty).
    pub fn show_image_preview(&mut self, url: &str) {
        // Don't re-fetch if already loading or showing this URL.
        match &self.image_preview {
            crate::image_preview::PreviewStatus::Loading { url: u }
            | crate::image_preview::PreviewStatus::Ready { url: u, .. }
                if u == url =>
            {
                return;
            }
            _ => {}
        }

        // Re-detect terminal and protocol before every preview.
        self.refresh_image_protocol();

        self.image_preview = crate::image_preview::PreviewStatus::Loading {
            url: url.to_string(),
        };

        let term_size = self.terminal_size();

        crate::image_preview::spawn_preview(
            url,
            &self.config.image_preview,
            &self.picker,
            &self.http_client,
            self.preview_tx.clone(),
            term_size,
        );
    }

    /// Re-detect outer terminal and update the picker's protocol.
    pub fn refresh_image_protocol(&mut self) {
        // When socket-attached, use the shim's env vars for terminal detection
        // (the daemon's own env vars are frozen from fork time).
        let env_override = self.shim_term_env.as_ref();
        let in_tmux = env_override.map_or_else(
            || std::env::var("TMUX").is_ok_and(|s| !s.is_empty()),
            |vars| vars.get("TMUX").is_some_and(|s| !s.is_empty()),
        );
        self.in_tmux = in_tmux;

        let (outer_terminal, outer_proto, outer_source) =
            super::detect_outer_terminal(in_tmux, env_override);

        let (resolved_proto, source) = super::resolve_image_protocol(
            &self.config.image_preview.protocol,
            &self.picker,
            outer_terminal,
            outer_proto,
            outer_source,
            env_override.is_some(), // shim env is authoritative
        );

        if let Some(proto) = resolved_proto {
            if proto != self.picker.protocol_type() {
                tracing::info!(
                    from = ?self.picker.protocol_type(),
                    to = ?proto,
                    source = %source,
                    outer = %outer_terminal,
                    "image protocol changed"
                );
            }
            self.picker.set_protocol_type(proto);
        }

        self.outer_terminal = outer_terminal.to_string();
        self.color_support = crate::nick_color::detect_color_support(outer_terminal);
        self.image_proto_source = source;
    }

    /// Dismiss the image preview overlay (e.g. on Escape press).
    ///
    /// Sends protocol-specific cleanup sequences and forces a full terminal
    /// redraw on the next frame. The full redraw is essential because:
    /// - **Kitty**: images live on a separate graphics layer; `set_skip(true)`
    ///   on ratatui cells means the diff algorithm won't detect changes in the
    ///   underlying chat content, so stale graphics persist without a full repaint.
    /// - **iTerm2+tmux**: images are written directly to stdout after ratatui's
    ///   buffer flush, so ratatui has no knowledge of those pixels. A diff-based
    ///   update may not rewrite every cell the image covered.
    pub fn dismiss_image_preview(&mut self) {
        // Capture popup rect before clearing, for targeted repaint.
        let popup_rect = self.image_preview_popup_rect();

        if matches!(
            self.image_preview,
            crate::image_preview::PreviewStatus::Ready { .. }
        ) {
            self.cleanup_image_graphics();
        }
        self.image_preview = crate::image_preview::PreviewStatus::Hidden;

        // Decide cleanup strategy based on graphics protocol.
        // Kitty/iTerm2: escape sequences already deleted the graphics layer,
        // so we only need ratatui to repaint the cells underneath (no full clear).
        // Halfblocks/Sixel: graphics are cell-based, need a full terminal clear.
        match self.picker.protocol_type() {
            ProtocolType::Kitty | ProtocolType::Iterm2 => {
                // Store the popup rect so the renderer can invalidate just
                // that region on the next frame (differential repaint).
                self.image_clear_rect = popup_rect;
            }
            _ => {
                // Sixel / Halfblocks: full redraw required.
                self.needs_full_redraw = true;
            }
        }
    }

    /// Compute the popup Rect for the current image preview.
    pub(crate) fn image_preview_popup_rect(&self) -> Option<Rect> {
        let (w, h) = match &self.image_preview {
            crate::image_preview::PreviewStatus::Ready { width, height, .. } => (*width, *height),
            crate::image_preview::PreviewStatus::Loading { .. } => (40, 5),
            crate::image_preview::PreviewStatus::Error { .. } => (50, 5),
            crate::image_preview::PreviewStatus::Hidden => return None,
        };
        let term_w = self.cached_term_cols;
        let term_h = self.cached_term_rows;
        let pw = w.min(term_w);
        let ph = h.min(term_h);
        let px = (term_w.saturating_sub(pw)) / 2;
        let py = (term_h.saturating_sub(ph)) / 2;
        Some(Rect::new(px, py, pw, ph))
    }

    /// Send protocol-specific escape sequences to clear image graphics.
    ///
    /// **Kitty**: Send `ESC_Ga=d,d=A,q=2 ESC\` to delete all visible image
    /// placements. In tmux, wrapped in DCS passthrough. Matches yazi's cleanup
    /// approach (`a=d,d=A` = delete all visible placements).
    ///
    /// **iTerm2+tmux**: Write spaces over the image area directly to stdout
    /// (same path as the image was written), ensuring the direct-written pixels
    /// are overwritten immediately.
    fn cleanup_image_graphics(&self) {
        use std::io::Write;

        match self.picker.protocol_type() {
            ProtocolType::Kitty => {
                // Delete all visible Kitty image placements.
                // d=A = all visible placements, q=2 = suppress response.
                let seq = if self.in_tmux {
                    "\x1bPtmux;\x1b\x1b_Ga=d,d=A,q=2\x1b\x1b\\\x1b\\"
                } else {
                    "\x1b_Ga=d,d=A,q=2\x1b\\"
                };
                let _ = std::io::stdout().write_all(seq.as_bytes());
                let _ = std::io::stdout().flush();
                // Also clear the area with spaces for tmux direct writes.
                if self.in_tmux {
                    self.clear_direct_image_area();
                }
            }
            ProtocolType::Iterm2 if self.in_tmux => {
                // iTerm2+tmux: image was written directly to stdout.
                // Write spaces over the same area to clear the pixels.
                self.clear_direct_image_area();
            }
            _ => {
                // Sixel / Halfblocks / iTerm2 direct: ratatui's full redraw
                // (triggered by needs_full_redraw) handles cleanup.
            }
        }
    }

    /// Write spaces over the image area directly to stdout for tmux cleanup.
    ///
    /// Mirrors the cursor positioning from `write_tmux_direct_image()` but
    /// writes space characters instead of an image, clearing any direct-written
    /// image pixels.
    fn clear_direct_image_area(&self) {
        use std::io::Write;

        let (popup_width, popup_height) = match &self.image_preview {
            crate::image_preview::PreviewStatus::Ready { width, height, .. } => (*width, *height),
            _ => return,
        };

        let term_size = self.terminal_size();
        let popup_w = popup_width.min(term_size.0);
        let popup_h = popup_height.min(term_size.1);
        let popup_x = (term_size.0.saturating_sub(popup_w)) / 2;
        let popup_y = (term_size.1.saturating_sub(popup_h)) / 2;

        let inner_x = popup_x + 1;
        let inner_y = popup_y + 1;
        let inner_w = popup_w.saturating_sub(2);
        let inner_h = popup_h.saturating_sub(2);

        if inner_w == 0 || inner_h == 0 {
            return;
        }

        let mut out = std::io::stdout().lock();
        // Save cursor, move to inner area, fill with spaces row by row.
        let _ = write!(out, "\x1b7");
        let spaces: String = " ".repeat(usize::from(inner_w));
        for row in 0..inner_h {
            let r = inner_y + row + 1; // 1-based for CUP
            let c = inner_x + 1;
            let _ = write!(out, "\x1b[{r};{c}H{spaces}");
        }
        let _ = write!(out, "\x1b8");
        let _ = out.flush();
    }

    /// Write image directly to stdout for tmux passthrough (all protocols).
    ///
    /// ratatui-image embeds escape sequences as cell symbols, which causes:
    /// - Quality loss (pre-downscales to `cell×font_size` pixels)
    /// - Cleanup issues (`set_skip(true)` breaks ratatui diff)
    ///
    /// Instead, write the image directly to stdout with DCS passthrough,
    /// sending the original PNG at full resolution. The terminal handles
    /// scaling, producing much better quality.
    ///
    /// Called AFTER `terminal.draw()` so ratatui has already flushed the
    /// border/popup. The image is drawn on top at the correct position.
    ///
    /// Supports:
    /// - **Kitty**: PNG format (`f=100`), `c`/`r` params for cell area scaling
    /// - **iTerm2**: OSC 1337 inline image with cell dimensions
    pub fn write_tmux_direct_image(&mut self) {
        // tmux direct-write only applies when we're running directly in tmux
        // with a local terminal. When socket-attached, the shim handles its
        // own terminal — ratatui-image's widget rendering works correctly.
        if !self.in_tmux || self.is_socket_attached {
            return;
        }

        let proto = self.picker.protocol_type();
        if proto == ProtocolType::Halfblocks || proto == ProtocolType::Sixel {
            return;
        }

        // Capture terminal size before mutable borrow of image_preview.
        let term_size = self.terminal_size();

        let (raw_png, popup_width, popup_height) = match &mut self.image_preview {
            crate::image_preview::PreviewStatus::Ready {
                raw_png,
                width,
                height,
                direct_written,
                ..
            } => {
                if *direct_written {
                    return;
                }
                *direct_written = true;
                (&*raw_png, *width, *height)
            }
            _ => return,
        };

        if raw_png.is_empty() {
            return;
        }

        // Calculate popup position (must match image_overlay::centered_rect).
        let popup_w = popup_width.min(term_size.0);
        let popup_h = popup_height.min(term_size.1);
        let popup_x = (term_size.0.saturating_sub(popup_w)) / 2;
        let popup_y = (term_size.1.saturating_sub(popup_h)) / 2;

        // Inner area (1-cell border).
        let inner_x = popup_x + 1;
        let inner_y = popup_y + 1;
        let inner_w = popup_w.saturating_sub(2);
        let inner_h = popup_h.saturating_sub(2);

        if inner_w == 0 || inner_h == 0 {
            return;
        }

        tracing::debug!(
            ?proto,
            inner_w,
            inner_h,
            inner_x,
            inner_y,
            png_len = raw_png.len(),
            "writing tmux direct image"
        );

        match proto {
            ProtocolType::Kitty => {
                write_kitty_tmux_direct(raw_png, inner_x, inner_y, inner_w, inner_h);
            }
            ProtocolType::Iterm2 => {
                write_iterm2_tmux_direct(raw_png, inner_x, inner_y, inner_w, inner_h);
            }
            _ => {}
        }
    }

    /// Handle a completed image preview event from a background task.
    ///
    /// Discards stale results: only accepts the event if we are still
    /// `Loading` the same URL. If the user dismissed the preview (Escape)
    /// or switched to a different URL, the decoded image is dropped
    /// immediately instead of resurrecting into `Ready`.
    pub(crate) fn handle_preview_event(&mut self, event: crate::image_preview::ImagePreviewEvent) {
        use crate::image_preview::{ImagePreviewEvent, PreviewStatus};

        let loading_url = match &self.image_preview {
            PreviewStatus::Loading { url } => url.as_str(),
            _ => return, // dismissed or different preview — drop the stale event
        };

        let event_url = match &event {
            ImagePreviewEvent::Ready { url, .. } | ImagePreviewEvent::Error { url, .. } => {
                url.as_str()
            }
        };

        if loading_url != event_url {
            return; // stale result from a previous URL
        }

        self.image_preview = match event {
            ImagePreviewEvent::Ready {
                url,
                title,
                image,
                raw_png,
                width,
                height,
            } => PreviewStatus::Ready {
                url,
                title,
                image,
                raw_png,
                width,
                height,
                direct_written: false,
            },
            ImagePreviewEvent::Error { url, message } => PreviewStatus::Error { url, message },
        };
    }
}

// ---------------------------------------------------------------------------
// Direct-write image functions (free functions, called from App methods)
// ---------------------------------------------------------------------------

/// Write Kitty graphics image directly to stdout via tmux DCS passthrough.
///
/// Sends the original PNG (`f=100`) at full resolution with `c`/`r` params
/// telling the terminal to scale the image to fit the cell area. This
/// produces much better quality than ratatui-image's pre-downscaled RGBA.
///
/// Image data is chunked into 4096-byte base64 pieces, each individually
/// wrapped in DCS passthrough (tmux has a ~1MB limit per passthrough block).
fn write_kitty_tmux_direct(raw_png: &[u8], inner_x: u16, inner_y: u16, inner_w: u16, inner_h: u16) {
    use std::io::Write;

    const CHARS_PER_CHUNK: usize = 4096;
    const CHUNK_SIZE: usize = (CHARS_PER_CHUNK / 4) * 3;

    let mut out = std::io::stdout().lock();

    // Save cursor + position at inner area (1-based for CUP).
    let row = inner_y + 1;
    let col = inner_x + 1;
    let _ = write!(out, "\x1b7\x1b[{row};{col}H");
    let _ = out.flush();

    let chunks: Vec<&[u8]> = raw_png.chunks(CHUNK_SIZE).collect();
    let chunk_count = chunks.len();

    for (i, chunk) in chunks.iter().enumerate() {
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, chunk);
        let more = u8::from(i + 1 < chunk_count);

        // DCS passthrough: \x1bPtmux; <escaped-kitty-cmd> \x1b\\
        // Inside DCS, ESC is doubled: \x1b → \x1b\x1b
        if i == 0 {
            // First chunk: transmit with display params.
            // f=100 = PNG format (terminal decodes at native quality)
            // a=T   = transmit and display
            // c/r   = cell area (terminal scales image to fit)
            // q=2   = suppress response
            let _ = write!(
                out,
                "\x1bPtmux;\x1b\x1b_Gq=2,a=T,f=100,t=d,c={inner_w},r={inner_h},m={more};{b64}\x1b\x1b\\\x1b\\"
            );
        } else {
            // Continuation chunks: just data + more flag.
            let _ = write!(out, "\x1bPtmux;\x1b\x1b_Gm={more};{b64}\x1b\x1b\\\x1b\\");
        }
        let _ = out.flush();
    }

    // Restore cursor.
    let _ = write!(out, "\x1b8");
    let _ = out.flush();
}

/// Write iTerm2 image directly to stdout via tmux DCS passthrough.
///
/// Sends the original PNG via OSC 1337 at full resolution with cell-based
/// dimensions. The terminal handles scaling.
fn write_iterm2_tmux_direct(
    raw_png: &[u8],
    inner_x: u16,
    inner_y: u16,
    inner_w: u16,
    inner_h: u16,
) {
    use std::io::Write;

    // Mouse tracking modes — must be disabled during DCS image write to
    // prevent interference with tmux passthrough (matches kokoirc).
    const MOUSE_DISABLE: &[u8] = b"\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l";
    const MOUSE_ENABLE: &[u8] = b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h";

    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw_png);

    // Build the iTerm2 OSC 1337 sequence.
    let osc = format!(
        "\x1b]1337;File=inline=1;width={inner_w};height={inner_h};preserveAspectRatio=0:{b64}\x07"
    );

    // Wrap in tmux DCS passthrough: double all ESC bytes in the payload.
    let escaped = osc.replace('\x1b', "\x1b\x1b");
    let dcs = format!("\x1bPtmux;{escaped}\x1b\\");

    // Terminal rows/cols are 1-based for CUP.
    let row = inner_y + 1;
    let col = inner_x + 1;

    let mut out = std::io::stdout().lock();

    // Step 1: Disable mouse tracking.
    let _ = out.write_all(MOUSE_DISABLE);
    let _ = out.flush();

    // Step 2: Save cursor + position.
    let _ = write!(out, "\x1b7\x1b[{row};{col}H");
    let _ = out.flush();

    // Step 3: Write DCS-wrapped image data.
    let _ = out.write_all(dcs.as_bytes());
    let _ = out.flush();

    // Step 4: Restore cursor.
    let _ = out.write_all(b"\x1b8");
    let _ = out.flush();

    // Step 5: Re-enable mouse tracking.
    let _ = out.write_all(MOUSE_ENABLE);
    let _ = out.flush();
}

//! Drives inline emote animation: a process clock maps elapsed time to a frame
//! index per emote, and a cache holds per-(emote, frame) protocol images for
//! compositing over the placeholder cells the chat renderer reserved.

use std::collections::HashMap;

use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use crate::ui::emote_layout::EmotePlacement;

/// Pick the current frame index for a loop of `delays` (ms) at `elapsed_ms`.
#[must_use]
pub fn frame_index_at(delays: &[u32], elapsed_ms: u128) -> usize {
    if delays.len() <= 1 {
        return 0;
    }
    let total: u128 = delays.iter().map(|d| u128::from(*d)).sum();
    if total == 0 {
        return 0;
    }
    let mut t = elapsed_ms % total;
    for (i, d) in delays.iter().enumerate() {
        let d = u128::from(*d);
        if t < d {
            return i;
        }
        t -= d;
    }
    0
}

/// Packed `0x00RRGGBB` background color the emote frames are flattened onto.
type BgRgb = u32;

#[derive(Default)]
pub struct EmoteAnimator {
    /// Keyed by (emote, frame, background) so a theme background change produces
    /// fresh flattened images rather than reusing the old-background ones.
    cache: HashMap<(u32, usize, BgRgb), StatefulProtocol>,
}

impl EmoteAnimator {
    /// Get or build the protocol image for one emote frame, with transparent
    /// pixels alpha-blended onto `bg` (the theme background) so the emote has no
    /// visible box against the chat background. Returns `None` if undecodable.
    fn protocol_for(
        &mut self,
        picker: &Picker,
        emote_index: u32,
        frame_index: usize,
        bg: (u8, u8, u8),
    ) -> Option<&mut StatefulProtocol> {
        use std::collections::hash_map::Entry;
        let bg_key = u32::from(bg.0) << 16 | u32::from(bg.1) << 8 | u32::from(bg.2);
        match self.cache.entry((emote_index, frame_index, bg_key)) {
            Entry::Occupied(e) => Some(e.into_mut()),
            Entry::Vacant(slot) => {
                let names = crate::emotes::names();
                let name = names.get(emote_index as usize)?;
                let frames = crate::emotes::frames(name)?;
                let (img, _delay) = frames.get(frame_index)?;
                let canvas = render_frame_on_bg(picker, img, bg);
                Some(
                    slot.insert(
                        picker.new_resize_protocol(image::DynamicImage::ImageRgba8(canvas)),
                    ),
                )
            }
        }
    }

    /// Frame delays for an emote (ms), or empty if unknown.
    #[must_use]
    pub fn delays(emote_index: u32) -> Vec<u32> {
        let names = crate::emotes::names();
        names
            .get(emote_index as usize)
            .and_then(|n| crate::emotes::frames(n))
            .map(|f| f.iter().map(|(_, d)| *d).collect())
            .unwrap_or_default()
    }
}

/// Render an emote frame onto an opaque canvas sized to the *exact pixel
/// dimensions of its cell rectangle* (`EMOTE_COLS * font_w` × `font_h`), filled
/// with the theme background. The emote is scaled to fit (preserving aspect) and
/// centered. Because the canvas matches the cell rect's aspect ratio and is fully
/// painted, no terminal background shows through — not around the emote and not
/// in the strip below it that a plain aspect-fit would leave in a non-square cell.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "scaled dims are rounded, clamped to [1, cell_px] — non-negative and in range"
)]
fn render_frame_on_bg(
    picker: &Picker,
    img: &image::RgbaImage,
    bg: (u8, u8, u8),
) -> image::RgbaImage {
    use crate::ui::emote_layout::EMOTE_COLS;

    let (fw, fh) = picker.font_size();
    let cw = u32::try_from(EMOTE_COLS).unwrap_or(2) * u32::from(fw.max(1));
    let ch = u32::from(fh.max(1));

    // Scale the emote to fit the cell, preserving aspect ratio.
    let (iw, ih) = (img.width().max(1), img.height().max(1));
    let scale = f64::min(f64::from(cw) / f64::from(iw), f64::from(ch) / f64::from(ih));
    let sw = (f64::from(iw) * scale).round().max(1.0).min(f64::from(cw)) as u32;
    let sh = (f64::from(ih) * scale).round().max(1.0).min(f64::from(ch)) as u32;
    let scaled = image::imageops::resize(img, sw, sh, image::imageops::FilterType::Triangle);

    let blend = |fg: u8, bg: u8, a: u16| -> u8 {
        u8::try_from((u16::from(fg) * a + u16::from(bg) * (255 - a)) / 255).unwrap_or(255)
    };

    let mut canvas = image::RgbaImage::from_pixel(cw, ch, image::Rgba([bg.0, bg.1, bg.2, 255]));
    let ox = (cw - sw) / 2;
    let oy = (ch - sh) / 2;
    for (x, y, px) in scaled.enumerate_pixels() {
        let a = u16::from(px.0[3]);
        canvas.put_pixel(
            ox + x,
            oy + y,
            image::Rgba([
                blend(px.0[0], bg.0, a),
                blend(px.0[1], bg.1, a),
                blend(px.0[2], bg.2, a),
                255,
            ]),
        );
    }
    canvas
}

/// Composite the current frame of every recorded placement onto the frame buffer.
/// Called from `layout::draw` after the chat view renders. `bg` is the theme
/// background color (RGB) the emotes are flattened onto.
pub fn composite(
    frame: &mut ratatui::Frame,
    picker: &Picker,
    animator: &mut EmoteAnimator,
    placements: &[EmotePlacement],
    elapsed_ms: u128,
    bg: (u8, u8, u8),
) {
    use ratatui_image::StatefulImage;
    for p in placements {
        let delays = EmoteAnimator::delays(p.emote_index);
        let fi = frame_index_at(&delays, elapsed_ms);
        if let Some(proto) = animator.protocol_for(picker, p.emote_index, fi, bg) {
            // Clear the placeholder cells, then draw the frame on top.
            frame.render_widget(ratatui::widgets::Clear, p.rect);
            frame.render_stateful_widget(StatefulImage::default(), p.rect, proto);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_index_advances_with_time() {
        let delays = [100u32, 100, 100];
        assert_eq!(frame_index_at(&delays, 0), 0);
        assert_eq!(frame_index_at(&delays, 150), 1);
        assert_eq!(frame_index_at(&delays, 250), 2);
        assert_eq!(frame_index_at(&delays, 350), 0); // wrapped
    }

    #[test]
    fn single_frame_is_static() {
        assert_eq!(frame_index_at(&[100], 99_999), 0);
    }

    #[test]
    fn empty_delays_is_zero() {
        assert_eq!(frame_index_at(&[], 123), 0);
    }
}

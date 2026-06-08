//! Drives inline emote animation: a process clock maps elapsed time to a frame
//! index per emote, and a cache holds per-(emote, frame) protocol images for
//! compositing over the placeholder cells the chat renderer reserved.

use std::collections::HashMap;

use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use crate::ui::emote_layout::EmotePlacement;

/// Packed `0x00RRGGBB` background color the emote frames are flattened onto.
type BgRgb = u32;

#[derive(Default)]
pub struct EmoteAnimator {
    /// Keyed by (emote, frame, background, cols, rows): a theme background change
    /// or a different cell footprint (font resize, picker grid vs chat) produces
    /// fresh flattened images rather than reusing wrongly-sized/coloured ones.
    cache: HashMap<(u32, usize, BgRgb, u16, u16), StatefulProtocol>,
}

impl EmoteAnimator {
    /// Get or build the protocol image for one emote frame at a `cols × rows`
    /// cell footprint, with transparent pixels alpha-blended onto `bg` (the theme
    /// background) so the emote has no visible box against the chat background.
    /// Returns `None` if undecodable.
    fn protocol_for(
        &mut self,
        picker: &Picker,
        emote_index: u32,
        frame_index: usize,
        bg: (u8, u8, u8),
        cols: u16,
        rows: u16,
    ) -> Option<&mut StatefulProtocol> {
        use std::collections::hash_map::Entry;
        let bg_key = u32::from(bg.0) << 16 | u32::from(bg.1) << 8 | u32::from(bg.2);
        match self.cache.entry((emote_index, frame_index, bg_key, cols, rows)) {
            Entry::Occupied(e) => Some(e.into_mut()),
            Entry::Vacant(slot) => {
                let names = crate::emotes::names();
                let name = names.get(emote_index as usize)?;
                let frames = crate::emotes::frames(name)?;
                let (img, _delay) = frames.get(frame_index)?;
                let canvas = render_frame_on_bg(picker, img, bg, cols, rows);
                Some(
                    slot.insert(
                        picker.new_resize_protocol(image::DynamicImage::ImageRgba8(canvas)),
                    ),
                )
            }
        }
    }

    /// Cached static first-frame protocol image for `emote_index` at a `cols ×
    /// rows` footprint, flattened onto `bg`. For non-animated thumbnails such as
    /// the emote picker grid (rendering every visible emote animated would be far
    /// too much per-frame work).
    pub fn thumbnail(
        &mut self,
        picker: &Picker,
        emote_index: u32,
        bg: (u8, u8, u8),
        cols: u16,
        rows: u16,
    ) -> Option<&mut StatefulProtocol> {
        self.protocol_for(picker, emote_index, 0, bg, cols, rows)
    }

    /// Drop every cached protocol image. Call after the terminal cell pixel size
    /// changes (font resize / reattach): cache keys do not include the cell size,
    /// so without this the animator keeps serving bitmaps rendered for the old
    /// font and emotes display at the wrong size.
    pub fn clear(&mut self) {
        self.cache.clear();
    }

    /// Number of cached protocol images (test-only observability for [`clear`]).
    #[cfg(test)]
    pub(crate) fn cache_len(&self) -> usize {
        self.cache.len()
    }

    /// Whether the emote has more than one frame (i.e. actually animates).
    /// Allocation-free — reads the cached `&'static` frame slice directly.
    #[must_use]
    pub fn is_animated(emote_index: u32) -> bool {
        crate::emotes::names()
            .get(emote_index as usize)
            .and_then(|n| crate::emotes::frames(n))
            .is_some_and(|f| f.len() > 1)
    }
}

/// Current frame index for an emote's `&'static` frame slice at `elapsed_ms`,
/// without allocating a delay `Vec` (used on the per-frame render path).
fn frame_index_of(frames: &[crate::emotes::Frame], elapsed_ms: u128) -> usize {
    if frames.len() <= 1 {
        return 0;
    }
    let total: u128 = frames.iter().map(|(_, d)| u128::from(*d)).sum();
    if total == 0 {
        return 0;
    }
    let mut t = elapsed_ms % total;
    for (i, (_, d)) in frames.iter().enumerate() {
        let d = u128::from(*d);
        if t < d {
            return i;
        }
        t -= d;
    }
    0
}

/// Render an emote frame onto an opaque canvas sized to the *exact pixel
/// dimensions of its cell rectangle* (`cols * font_w` × `rows * font_h`), filled
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
    cols: u16,
    rows: u16,
) -> image::RgbaImage {
    let (fw, fh) = picker.font_size();
    let cw = u32::from(cols.max(1)) * u32::from(fw.max(1));
    let ch = u32::from(rows.max(1)) * u32::from(fh.max(1));

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
    let names = crate::emotes::names();
    for p in placements {
        let Some(frames) = names
            .get(p.emote_index as usize)
            .and_then(|n| crate::emotes::frames(n))
        else {
            continue;
        };
        let fi = frame_index_of(frames, elapsed_ms);
        if let Some(proto) =
            animator.protocol_for(picker, p.emote_index, fi, bg, p.rect.width, p.rect.height)
        {
            // Clear the placeholder cells, then draw the frame on top.
            frame.render_widget(ratatui::widgets::Clear, p.rect);
            frame.render_stateful_widget(StatefulImage::default(), p.rect, proto);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames_with(delays: &[u32]) -> Vec<crate::emotes::Frame> {
        delays
            .iter()
            .map(|d| (image::RgbaImage::new(1, 1), *d))
            .collect()
    }

    #[test]
    fn frame_index_advances_with_time() {
        let fs = frames_with(&[100, 100, 100]);
        assert_eq!(frame_index_of(&fs, 0), 0);
        assert_eq!(frame_index_of(&fs, 150), 1);
        assert_eq!(frame_index_of(&fs, 250), 2);
        assert_eq!(frame_index_of(&fs, 350), 0); // wrapped
    }

    #[test]
    fn single_frame_is_static() {
        assert_eq!(frame_index_of(&frames_with(&[100]), 99_999), 0);
    }

    #[test]
    fn empty_frames_is_zero() {
        assert_eq!(frame_index_of(&frames_with(&[]), 123), 0);
    }

    #[test]
    fn clear_empties_the_protocol_cache() {
        // halfblocks needs no terminal, so the cache can be populated offline.
        let picker = ratatui_image::picker::Picker::halfblocks();
        let mut animator = EmoteAnimator::default();
        let idx = crate::emotes::resolve("usmiech").expect("usmiech exists");
        assert!(
            animator.thumbnail(&picker, idx, (0, 0, 0), 2, 1).is_some(),
            "thumbnail must build and cache a protocol image"
        );
        assert_eq!(animator.cache_len(), 1, "one image cached after thumbnail");

        animator.clear();

        assert_eq!(animator.cache_len(), 0, "clear() must drop every cached image");
    }
}

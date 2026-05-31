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

/// Holds per-(`emote_index`, `frame_index`) protocol images sized for compositing.
#[derive(Default)]
pub struct EmoteAnimator {
    cache: HashMap<(u32, usize), StatefulProtocol>,
}

impl EmoteAnimator {
    /// Get or build the protocol image for one emote frame. Returns `None` if the
    /// emote/frame can't be decoded.
    fn protocol_for(
        &mut self,
        picker: &Picker,
        emote_index: u32,
        frame_index: usize,
    ) -> Option<&mut StatefulProtocol> {
        use std::collections::hash_map::Entry;
        match self.cache.entry((emote_index, frame_index)) {
            Entry::Occupied(e) => Some(e.into_mut()),
            Entry::Vacant(slot) => {
                let names = crate::emotes::names();
                let name = names.get(emote_index as usize)?;
                let frames = crate::emotes::frames(name)?;
                let (img, _delay) = frames.get(frame_index)?;
                let dyn_img = image::DynamicImage::ImageRgba8(img.clone());
                Some(slot.insert(picker.new_resize_protocol(dyn_img)))
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

/// Composite the current frame of every recorded placement onto the frame buffer.
/// Called from `layout::draw` after the chat view renders.
pub fn composite(
    frame: &mut ratatui::Frame,
    picker: &Picker,
    animator: &mut EmoteAnimator,
    placements: &[EmotePlacement],
    elapsed_ms: u128,
) {
    use ratatui_image::StatefulImage;
    for p in placements {
        let delays = EmoteAnimator::delays(p.emote_index);
        let fi = frame_index_at(&delays, elapsed_ms);
        if let Some(proto) = animator.protocol_for(picker, p.emote_index, fi) {
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

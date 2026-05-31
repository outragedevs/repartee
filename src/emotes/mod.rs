//! Built-in GG7 emote registry: embedded GIF assets + name whitelist.
//!
//! UI-agnostic. The 183 curated GIFs in `assets/emotes/` are embedded at compile
//! time via `rust-embed`. Names are the file stems (`usmiech.gif` -> `usmiech`).

use std::borrow::Cow;
use std::sync::LazyLock;

use rust_embed::Embed;

pub mod parse;

#[derive(Embed)]
#[folder = "assets/emotes/"]
struct EmoteAssets;

/// Sorted list of emote names (file stems, `.gif` removed).
static NAMES: LazyLock<Vec<String>> = LazyLock::new(|| {
    let mut v: Vec<String> = EmoteAssets::iter()
        .filter_map(|f| f.strip_suffix(".gif").map(ToOwned::to_owned))
        .collect();
    v.sort_unstable();
    v
});

/// All known emote names, sorted ascending.
#[must_use]
pub fn names() -> &'static [String] {
    &NAMES
}

/// Whether `name` (without `.gif`) is a known emote. Used as the tokenizer whitelist.
#[must_use]
pub fn contains(name: &str) -> bool {
    NAMES.binary_search_by(|n| n.as_str().cmp(name)).is_ok()
}

/// Raw GIF bytes for `name` (without `.gif`), or `None` if unknown.
#[must_use]
pub fn bytes(name: &str) -> Option<Cow<'static, [u8]>> {
    if !contains(name) {
        return None;
    }
    EmoteAssets::get(&format!("{name}.gif")).map(|f| f.data)
}

/// One decoded animation frame and its display duration (ms, floored at 20).
pub type Frame = (image::RgbaImage, u32);

/// Lazily decode and cache all frames of an emote GIF. Returns `None` if unknown
/// or if decoding fails. The returned slice is stable for the process lifetime.
///
/// Frames are decoded only on first display and live for the whole session; the
/// `&'static` return keeps the render path lifetime-free. The set is bounded
/// (183 emotes) so leaking the decoded buffers is acceptable.
#[must_use]
pub fn frames(name: &str) -> Option<&'static [Frame]> {
    use std::collections::HashMap;
    use std::sync::{OnceLock, RwLock};

    static CACHE: OnceLock<RwLock<HashMap<String, &'static [Frame]>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| RwLock::new(HashMap::new()));

    if let Some(slice) = cache.read().ok()?.get(name) {
        return Some(slice);
    }
    let decoded = decode_frames(name)?;
    let leaked: &'static [Frame] = Box::leak(decoded.into_boxed_slice());
    cache.write().ok()?.insert(name.to_owned(), leaked);
    Some(leaked)
}

fn decode_frames(name: &str) -> Option<Vec<Frame>> {
    use image::AnimationDecoder;
    use image::codecs::gif::GifDecoder;

    let data = bytes(name)?;
    let decoder = GifDecoder::new(std::io::Cursor::new(data.into_owned())).ok()?;
    let mut out = Vec::new();
    for frame in decoder.into_frames().collect_frames().ok()? {
        let (num, den) = frame.delay().numer_denom_ms();
        let delay = num.checked_div(den).map_or(100, |ms| ms.max(20));
        out.push((frame.into_buffer(), delay));
    }
    if out.is_empty() { None } else { Some(out) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_sorted_and_nonempty() {
        let names = names();
        assert!(
            names.len() >= 180,
            "expected the full GG7 set, got {}",
            names.len()
        );
        assert!(
            names.windows(2).all(|w| w[0] <= w[1]),
            "names must be sorted"
        );
        assert!(names.iter().all(|n| !n.is_empty() && !n.contains('.')));
    }

    #[test]
    fn known_emote_resolves_to_gif_bytes() {
        assert!(contains("usmiech"));
        let bytes = bytes("usmiech").expect("usmiech must exist");
        assert!(bytes.starts_with(b"GIF"), "embedded asset must be a GIF");
    }

    #[test]
    fn unknown_emote_is_absent() {
        assert!(!contains("definitely_not_an_emote"));
        assert!(bytes("definitely_not_an_emote").is_none());
    }

    #[test]
    fn frames_decode_with_delays() {
        let fs = frames("usmiech").expect("usmiech frames");
        assert!(!fs.is_empty(), "at least one frame");
        let (img, _delay) = &fs[0];
        assert!(img.width() > 0 && img.height() > 0);
        assert!(fs.iter().all(|(_, d)| *d >= 20));
        // Stable identity across calls (cached, same pointer).
        let again = frames("usmiech").unwrap();
        assert!(std::ptr::eq(fs.as_ptr(), again.as_ptr()));
    }

    #[test]
    fn frames_unknown_is_none() {
        assert!(frames("definitely_not_an_emote").is_none());
    }

    #[test]
    fn all_embedded_names_are_valid_tokens() {
        for n in names() {
            assert!(
                n.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "name {n:?} contains a byte outside [a-z0-9_]"
            );
        }
    }
}

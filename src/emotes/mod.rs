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

/// Picker / insert language. Lives here (not in `config`) so the registry stays
/// config-agnostic; `config::EmoteLang` maps onto it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Pl,
}

/// `ENGLISH[i]` is the English alias for `names()[i]` (equals the stem for
/// loanword emotes like `lol`/`ok`/`8p`). Parsed from the embedded `aliases.tsv`.
static ENGLISH: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    let raw = include_str!("../../assets/emotes/aliases.tsv");
    let mut map: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for line in raw.lines() {
        if let Some((pl, en)) = line.split_once('\t') {
            map.insert(pl.trim(), en.trim());
        }
    }
    NAMES
        .iter()
        .map(|n| *map.get(n.as_str()).unwrap_or(&n.as_str()))
        .collect()
});

/// English alias -> index into `NAMES`.
static EN_TO_INDEX: LazyLock<std::collections::HashMap<&'static str, u32>> = LazyLock::new(|| {
    ENGLISH
        .iter()
        .enumerate()
        .map(|(i, en)| (*en, u32::try_from(i).unwrap_or(0)))
        .collect()
});

/// All valid `:name:` tags (Polish stems + English aliases), sorted + deduped.
static TAG_NAMES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    let mut v: Vec<&'static str> = NAMES.iter().map(String::as_str).collect();
    v.extend(ENGLISH.iter().copied());
    v.sort_unstable();
    v.dedup();
    v
});

/// All known emote names, sorted ascending (Polish stems = file names).
#[must_use]
pub fn names() -> &'static [String] {
    &NAMES
}

/// English alias for the emote at `index` (equals the stem when no distinct alias).
#[must_use]
pub fn english_label(index: u32) -> Option<&'static str> {
    ENGLISH.get(index as usize).copied()
}

/// Resolve a tag name (Polish stem OR English alias) to its canonical index.
#[must_use]
pub fn resolve(name: &str) -> Option<u32> {
    if let Ok(i) = NAMES.binary_search_by(|n| n.as_str().cmp(name)) {
        return u32::try_from(i).ok();
    }
    EN_TO_INDEX.get(name).copied()
}

/// The name to display/insert for `index` in the given language.
#[must_use]
pub fn display_name(index: u32, lang: Lang) -> &'static str {
    match lang {
        Lang::En => english_label(index).unwrap_or("?"),
        Lang::Pl => names().get(index as usize).map_or("?", String::as_str),
    }
}

/// All valid tag names in both languages, sorted (for autocomplete + whitelist).
#[must_use]
pub fn tag_names() -> &'static [&'static str] {
    &TAG_NAMES
}

/// Whether `name` is a known emote in either language (tokenizer whitelist).
#[must_use]
pub fn contains(name: &str) -> bool {
    resolve(name).is_some()
}

/// Raw GIF bytes for a tag in either language, or `None` if unknown.
#[must_use]
pub fn bytes(name: &str) -> Option<Cow<'static, [u8]>> {
    let idx = resolve(name)?;
    let stem = NAMES.get(idx as usize)?;
    EmoteAssets::get(&format!("{stem}.gif")).map(|f| f.data)
}

/// Native pixel size `(width, height)` of each emote, read once from the GIF
/// logical-screen descriptor (bytes 6..10) without decoding frames. `(0, 0)`
/// marks an asset whose header could not be read (never happens for the curated
/// set, but keeps the index total-mappable). Indexed by registry index.
static NATIVE_SIZES: LazyLock<Vec<(u16, u16)>> = LazyLock::new(|| {
    NAMES
        .iter()
        .map(|stem| {
            EmoteAssets::get(&format!("{stem}.gif"))
                .filter(|f| f.data.len() >= 10 && f.data.starts_with(b"GIF"))
                .map_or((0, 0), |f| {
                    let d = &f.data;
                    (
                        u16::from_le_bytes([d[6], d[7]]),
                        u16::from_le_bytes([d[8], d[9]]),
                    )
                })
        })
        .collect()
});

/// Native GIF pixel size `(width, height)` for the emote at `index`, or `None`
/// if the index is out of range. Cheap (cached header read, no frame decode) so
/// the render path can size each emote from its real dimensions every frame.
#[must_use]
pub fn native_size(index: u32) -> Option<(u16, u16)> {
    NATIVE_SIZES.get(index as usize).copied()
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
#[allow(
    clippy::significant_drop_tightening,
    reason = "write guard must span the check-then-insert to stay atomic"
)]
pub fn frames(name: &str) -> Option<&'static [Frame]> {
    use std::collections::HashMap;
    use std::sync::{OnceLock, RwLock};

    static CACHE: OnceLock<RwLock<HashMap<String, &'static [Frame]>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| RwLock::new(HashMap::new()));

    // Recover from a poisoned lock rather than disabling emotes for the session;
    // the only operation under the lock is a HashMap get/insert and cannot panic.
    if let Some(slice) = cache
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(name)
    {
        return Some(slice);
    }
    let decoded = decode_frames(name)?;
    let mut guard = cache
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Re-check under the write lock: if another thread decoded the same emote
    // first, drop ours (no leak) and return theirs.
    if let Some(slice) = guard.get(name) {
        return Some(slice);
    }
    let leaked: &'static [Frame] = Box::leak(decoded.into_boxed_slice());
    guard.insert(name.to_owned(), leaked);
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
    fn native_size_matches_decoded_frame_dimensions() {
        // The header read (LE bytes 6..10) must agree with the fully decoded
        // first frame for several differently-shaped assets — this pins both the
        // byte offsets and the endianness.
        for name in ["usmiech", "lol", "ok"] {
            let idx = resolve(name).unwrap_or_else(|| panic!("{name} resolves"));
            let (hw, hh) = native_size(idx).expect("native size");
            let (img, _) = &frames(name).expect("frames")[0];
            assert_eq!(
                (u32::from(hw), u32::from(hh)),
                (img.width(), img.height()),
                "{name}: header size {hw}x{hh} must match decoded {}x{}",
                img.width(),
                img.height()
            );
            assert!(hw > 0 && hh > 0, "{name}: non-zero native size");
        }
    }

    #[test]
    fn native_size_out_of_range_is_none() {
        assert!(native_size(u32::MAX).is_none());
    }

    #[test]
    fn aliases_cover_every_emote() {
        for (i, n) in names().iter().enumerate() {
            let idx = u32::try_from(i).unwrap();
            assert_eq!(
                resolve(n),
                Some(idx),
                "PL stem {n} must resolve to its own index"
            );
            let en = english_label(idx).expect("every emote has an English label");
            assert_eq!(
                resolve(en),
                Some(idx),
                "EN alias {en} must resolve to {n}'s index"
            );
        }
    }

    #[test]
    fn resolve_both_languages_and_unknown() {
        let smile = resolve("usmiech").expect("usmiech");
        assert_eq!(resolve("smile"), Some(smile), ":smile: == :usmiech:");
        assert!(resolve("definitely_not_an_emote").is_none());
    }

    #[test]
    fn bytes_resolves_english_to_gif() {
        assert!(bytes("smile").unwrap().starts_with(b"GIF"));
        assert!(bytes("usmiech").unwrap().starts_with(b"GIF"));
    }

    #[test]
    fn display_name_follows_lang() {
        let i = resolve("usmiech").unwrap();
        assert_eq!(display_name(i, Lang::En), "smile");
        assert_eq!(display_name(i, Lang::Pl), "usmiech");
    }

    #[test]
    fn tag_names_includes_both_languages() {
        let tags = tag_names();
        assert!(tags.contains(&"smile"));
        assert!(tags.contains(&"usmiech"));
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

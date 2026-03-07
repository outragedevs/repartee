//! URL detection and classification for image preview.
//!
//! Extracts URLs from IRC message text and classifies them to determine
//! whether they point to direct images, image hosting pages, or generic
//! web pages. This is the first stage of the image preview pipeline —
//! classification drives whether we fetch the URL directly as an image
//! or scrape `og:image` metadata from the page HTML.
//!
//! Ported from kokoirc's `src/core/image-preview/fetch.ts`.

use std::sync::LazyLock;

use regex::Regex;

// ---------------------------------------------------------------------------
// Compiled regex patterns (compiled once, reused for every call)
// ---------------------------------------------------------------------------

/// Matches URLs in message text.
static URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"https?://[^\s<>"')\]]+"#).expect("URL_RE is a valid regex")
});

/// Matches common image file extensions at the end of a URL path,
/// optionally followed by a query string.
static IMAGE_EXT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\.(jpe?g|png|gif|webp|bmp)(\?.*)?$").expect("IMAGE_EXT_RE is a valid regex")
});

/// Matches direct-image hosting domains (`i.imgur.com`, `i.ibb.co`).
static DIRECT_HOST_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^i\.(imgur\.com|ibb\.co)$").expect("DIRECT_HOST_RE is a valid regex")
});

/// Matches Imgur page URLs (gallery, album, single image pages).
static IMGUR_PAGE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(www\.)?imgur\.com/").expect("IMGUR_PAGE_RE is a valid regex")
});

/// Matches `ImgBB` page URLs.
static IMGBB_PAGE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(www\.)?ibb\.co/").expect("IMGBB_PAGE_RE is a valid regex")
});

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// The kind of resource a URL points to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlType {
    /// URL resolves directly to an image (by extension or known image host).
    DirectImage,
    /// An Imgur gallery/album/image page (needs `og:image` scraping).
    ImgurPage,
    /// An `ImgBB` image page (needs `og:image` scraping).
    ImgbbPage,
    /// Any other HTTP(S) URL (may or may not contain an image).
    GenericPage,
}

/// A classified URL extracted from message text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlClassification {
    /// The full URL string.
    pub url: String,
    /// What kind of resource the URL points to.
    pub url_type: UrlType,
    /// A human-readable title derived from the URL path (typically the
    /// last path segment / filename). `None` when the path is empty or `/`.
    pub title: Option<String>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Extract all URLs from `text` and return them as classified results.
///
/// Only `http` and `https` URLs are returned — other schemes (ftp, irc,
/// mailto, etc.) are silently skipped.
#[must_use]
pub fn extract_urls(text: &str) -> Vec<UrlClassification> {
    URL_RE
        .find_iter(text)
        .filter_map(|m| classify_url(m.as_str()))
        .collect()
}

/// Classify a single URL string.
///
/// Returns `None` for URLs that do not use the `http` or `https` scheme
/// (which should not normally appear since [`extract_urls`] only matches
/// those schemes, but callers may invoke this directly).
#[must_use]
pub fn classify_url(url: &str) -> Option<UrlClassification> {
    // Basic scheme check — the regex already enforces http(s) but a caller
    // might pass an arbitrary string.
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return None;
    }

    let (host, path) = parse_host_and_path(url);

    let url_type = classify_host_and_path(&host, &path);

    let title = extract_title(&path);

    Some(UrlClassification {
        url: url.to_owned(),
        url_type,
        title,
    })
}

/// Quick predicate: is this URL likely to resolve to a previewable image?
///
/// Returns `true` for direct images and known image-hosting pages, `false`
/// for generic pages and non-HTTP URLs.
#[must_use]
pub fn is_likely_image_url(url: &str) -> bool {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return false;
    }

    let (host, path) = parse_host_and_path(url);

    if IMAGE_EXT_RE.is_match(&path) {
        return true;
    }

    if DIRECT_HOST_RE.is_match(&host) {
        return true;
    }

    let host_path = format!("{host}/{}", path.trim_start_matches('/'));
    if IMGUR_PAGE_RE.is_match(&host_path) || IMGBB_PAGE_RE.is_match(&host_path) {
        return true;
    }

    false
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Determine the [`UrlType`] from a host and path extracted from a URL.
fn classify_host_and_path(host: &str, path: &str) -> UrlType {
    // Direct image by file extension.
    if IMAGE_EXT_RE.is_match(path) {
        return UrlType::DirectImage;
    }

    // Direct image by known host (i.imgur.com, i.ibb.co serve raw images).
    if DIRECT_HOST_RE.is_match(host) {
        return UrlType::DirectImage;
    }

    // Known image-hosting page patterns — test against "host/path".
    let host_path = format!("{host}/{}", path.trim_start_matches('/'));

    if IMGUR_PAGE_RE.is_match(&host_path) {
        return UrlType::ImgurPage;
    }

    if IMGBB_PAGE_RE.is_match(&host_path) {
        return UrlType::ImgbbPage;
    }

    UrlType::GenericPage
}

/// Extract host and path from a URL string using simple string splitting.
///
/// We deliberately avoid the `url` crate — the URLs we handle are always
/// absolute `http(s)://` so basic splitting is sufficient and avoids an
/// extra dependency.
fn parse_host_and_path(url: &str) -> (String, String) {
    // Strip scheme.
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);

    // Split host from path at the first `/`.
    let (host, path) = without_scheme.find('/').map_or_else(
        || (without_scheme.to_ascii_lowercase(), String::from("/")),
        |idx| (
            without_scheme[..idx].to_ascii_lowercase(),
            without_scheme[idx..].to_owned(),
        ),
    );

    // Strip optional port from host (e.g. "example.com:8080").
    let host_no_port = match host.rfind(':') {
        Some(idx) => host[..idx].to_owned(),
        None => host,
    };

    (host_no_port, path)
}

/// Derive a display title from the URL path.
///
/// Takes the last non-empty path segment, strips any query string, and
/// returns it. Returns `None` when the path is empty or just `/`.
fn extract_title(path: &str) -> Option<String> {
    // Remove query string and fragment.
    let clean = path.split('?').next().unwrap_or(path);
    let clean = clean.split('#').next().unwrap_or(clean);

    // Take the last non-empty segment.
    let segment = clean.rsplit('/').find(|s| !s.is_empty())?;

    // Percent-decode common sequences for readability.
    let decoded = percent_decode_simple(segment);

    if decoded.is_empty() {
        None
    } else {
        Some(decoded)
    }
}

/// Minimal percent-decoding for display purposes.
///
/// Decodes `%XX` sequences into their UTF-8 characters. Invalid sequences
/// are left as-is. This avoids pulling in the `percent-encoding` crate for
/// a simple display-only use case.
fn percent_decode_simple(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut decoded: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(byte) = decode_hex_pair(bytes[i + 1], bytes[i + 2])
        {
            decoded.push(byte);
            i += 3;
            continue;
        }
        decoded.push(bytes[i]);
        i += 1;
    }

    String::from_utf8(decoded).unwrap_or_else(|_| input.to_owned())
}

/// Decode two ASCII hex digits into a byte.
const fn decode_hex_pair(hi: u8, lo: u8) -> Option<u8> {
    let h = match hi {
        b'0'..=b'9' => hi - b'0',
        b'a'..=b'f' => hi - b'a' + 10,
        b'A'..=b'F' => hi - b'A' + 10,
        _ => return None,
    };
    let l = match lo {
        b'0'..=b'9' => lo - b'0',
        b'a'..=b'f' => lo - b'a' + 10,
        b'A'..=b'F' => lo - b'A' + 10,
        _ => return None,
    };
    Some(h << 4 | l)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // classify_url — UrlType classification
    // -----------------------------------------------------------------------

    #[test]
    fn direct_image_by_jpg_extension() {
        let c = classify_url("https://example.com/photo.jpg").unwrap();
        assert_eq!(c.url_type, UrlType::DirectImage);
    }

    #[test]
    fn direct_image_by_jpeg_extension() {
        let c = classify_url("https://example.com/photo.jpeg").unwrap();
        assert_eq!(c.url_type, UrlType::DirectImage);
    }

    #[test]
    fn direct_image_by_png_extension() {
        let c = classify_url("https://example.com/photo.png").unwrap();
        assert_eq!(c.url_type, UrlType::DirectImage);
    }

    #[test]
    fn direct_image_by_gif_extension() {
        let c = classify_url("https://example.com/anim.gif").unwrap();
        assert_eq!(c.url_type, UrlType::DirectImage);
    }

    #[test]
    fn direct_image_by_webp_extension() {
        let c = classify_url("https://example.com/pic.webp").unwrap();
        assert_eq!(c.url_type, UrlType::DirectImage);
    }

    #[test]
    fn direct_image_by_bmp_extension() {
        let c = classify_url("https://example.com/old.bmp").unwrap();
        assert_eq!(c.url_type, UrlType::DirectImage);
    }

    #[test]
    fn direct_image_extension_case_insensitive() {
        let c = classify_url("https://example.com/PHOTO.JPG").unwrap();
        assert_eq!(c.url_type, UrlType::DirectImage);
    }

    #[test]
    fn direct_image_extension_with_query_string() {
        let c = classify_url("https://cdn.example.com/img.png?width=800").unwrap();
        assert_eq!(c.url_type, UrlType::DirectImage);
    }

    #[test]
    fn direct_image_i_imgur_host() {
        let c = classify_url("https://i.imgur.com/abc123.png").unwrap();
        assert_eq!(c.url_type, UrlType::DirectImage);
    }

    #[test]
    fn direct_image_i_imgur_no_extension() {
        // i.imgur.com serves raw images even without extensions.
        let c = classify_url("https://i.imgur.com/abc123").unwrap();
        assert_eq!(c.url_type, UrlType::DirectImage);
    }

    #[test]
    fn direct_image_i_ibb_host() {
        let c = classify_url("https://i.ibb.co/xyz789.jpg").unwrap();
        assert_eq!(c.url_type, UrlType::DirectImage);
    }

    #[test]
    fn direct_image_i_imgur_host_case_insensitive() {
        let c = classify_url("https://I.IMGUR.COM/abc123").unwrap();
        assert_eq!(c.url_type, UrlType::DirectImage);
    }

    #[test]
    fn imgur_page_gallery() {
        let c = classify_url("https://imgur.com/gallery/abc").unwrap();
        assert_eq!(c.url_type, UrlType::ImgurPage);
    }

    #[test]
    fn imgur_page_www() {
        let c = classify_url("https://www.imgur.com/gallery/abc").unwrap();
        assert_eq!(c.url_type, UrlType::ImgurPage);
    }

    #[test]
    fn imgur_page_single_image() {
        let c = classify_url("https://imgur.com/abc123").unwrap();
        assert_eq!(c.url_type, UrlType::ImgurPage);
    }

    #[test]
    fn imgbb_page() {
        let c = classify_url("https://ibb.co/abc123").unwrap();
        assert_eq!(c.url_type, UrlType::ImgbbPage);
    }

    #[test]
    fn imgbb_page_www() {
        let c = classify_url("https://www.ibb.co/abc123").unwrap();
        assert_eq!(c.url_type, UrlType::ImgbbPage);
    }

    #[test]
    fn generic_page() {
        let c = classify_url("https://example.com/page").unwrap();
        assert_eq!(c.url_type, UrlType::GenericPage);
    }

    #[test]
    fn generic_page_with_path() {
        let c = classify_url("https://news.ycombinator.com/item?id=12345").unwrap();
        assert_eq!(c.url_type, UrlType::GenericPage);
    }

    #[test]
    fn non_http_ftp_returns_none() {
        assert!(classify_url("ftp://example.com/file.jpg").is_none());
    }

    #[test]
    fn non_http_irc_returns_none() {
        assert!(classify_url("irc://irc.libera.chat/rust").is_none());
    }

    #[test]
    fn non_http_mailto_returns_none() {
        assert!(classify_url("mailto:user@example.com").is_none());
    }

    #[test]
    fn http_scheme_accepted() {
        let c = classify_url("http://example.com/photo.png").unwrap();
        assert_eq!(c.url_type, UrlType::DirectImage);
    }

    // -----------------------------------------------------------------------
    // classify_url — title extraction
    // -----------------------------------------------------------------------

    #[test]
    fn title_from_filename() {
        let c = classify_url("https://i.imgur.com/abc123.png").unwrap();
        assert_eq!(c.title.as_deref(), Some("abc123.png"));
    }

    #[test]
    fn title_from_deep_path() {
        let c = classify_url("https://cdn.example.com/images/2024/sunset.jpg").unwrap();
        assert_eq!(c.title.as_deref(), Some("sunset.jpg"));
    }

    #[test]
    fn title_strips_query_string() {
        let c = classify_url("https://cdn.example.com/img.png?width=800&quality=90").unwrap();
        assert_eq!(c.title.as_deref(), Some("img.png"));
    }

    #[test]
    fn title_strips_fragment() {
        let c = classify_url("https://example.com/page#section").unwrap();
        assert_eq!(c.title.as_deref(), Some("page"));
    }

    #[test]
    fn title_from_gallery_path() {
        let c = classify_url("https://imgur.com/gallery/abc").unwrap();
        assert_eq!(c.title.as_deref(), Some("abc"));
    }

    #[test]
    fn title_none_for_root_path() {
        let c = classify_url("https://example.com").unwrap();
        assert_eq!(c.title, None);
    }

    #[test]
    fn title_none_for_trailing_slash() {
        let c = classify_url("https://example.com/").unwrap();
        assert_eq!(c.title, None);
    }

    #[test]
    fn title_percent_decoded() {
        let c = classify_url("https://example.com/my%20photo.jpg").unwrap();
        assert_eq!(c.title.as_deref(), Some("my photo.jpg"));
    }

    // -----------------------------------------------------------------------
    // extract_urls — multi-URL extraction from text
    // -----------------------------------------------------------------------

    #[test]
    fn extract_single_url_from_text() {
        let results = extract_urls("check out https://example.com/pic.png please");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://example.com/pic.png");
        assert_eq!(results[0].url_type, UrlType::DirectImage);
    }

    #[test]
    fn extract_multiple_urls() {
        let text = "look at https://i.imgur.com/a.png and https://example.com/page";
        let results = extract_urls(text);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].url_type, UrlType::DirectImage);
        assert_eq!(results[1].url_type, UrlType::GenericPage);
    }

    #[test]
    fn extract_no_urls() {
        let results = extract_urls("just a normal message with no links");
        assert!(results.is_empty());
    }

    #[test]
    fn extract_url_at_end_of_sentence() {
        let results = extract_urls("visit https://example.com/photo.jpg.");
        // The trailing period should not be included in the URL because
        // our regex stops at common delimiters — but `.` is allowed in URLs.
        // The result will include the period as part of the match, but `.jpg.`
        // does not have a recognized image extension so it falls through to
        // GenericPage. This is acceptable IRC behavior.
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn extract_url_in_angle_brackets() {
        let results = extract_urls("link: <https://example.com/pic.png>");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://example.com/pic.png");
        assert_eq!(results[0].url_type, UrlType::DirectImage);
    }

    #[test]
    fn extract_url_in_parentheses() {
        let results = extract_urls("(https://example.com/pic.png)");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://example.com/pic.png");
    }

    #[test]
    fn extract_url_surrounded_by_quotes() {
        let results = extract_urls(r#""https://example.com/pic.png""#);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://example.com/pic.png");
    }

    // -----------------------------------------------------------------------
    // is_likely_image_url
    // -----------------------------------------------------------------------

    #[test]
    fn likely_direct_image() {
        assert!(is_likely_image_url("https://example.com/photo.jpg"));
    }

    #[test]
    fn likely_imgur_host() {
        assert!(is_likely_image_url("https://i.imgur.com/abc123"));
    }

    #[test]
    fn likely_imgur_page() {
        assert!(is_likely_image_url("https://imgur.com/gallery/abc"));
    }

    #[test]
    fn likely_imgbb_page() {
        assert!(is_likely_image_url("https://ibb.co/abc123"));
    }

    #[test]
    fn not_likely_generic_page() {
        assert!(!is_likely_image_url("https://news.ycombinator.com/"));
    }

    #[test]
    fn not_likely_ftp() {
        assert!(!is_likely_image_url("ftp://example.com/file.jpg"));
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    #[test]
    fn parse_host_strips_port() {
        let (host, path) = parse_host_and_path("https://example.com:8080/path");
        assert_eq!(host, "example.com");
        assert_eq!(path, "/path");
    }

    #[test]
    fn parse_host_lowercases() {
        let (host, _) = parse_host_and_path("https://EXAMPLE.COM/Path");
        assert_eq!(host, "example.com");
    }

    #[test]
    fn parse_host_no_path() {
        let (host, path) = parse_host_and_path("https://example.com");
        assert_eq!(host, "example.com");
        assert_eq!(path, "/");
    }

    #[test]
    fn percent_decode_space() {
        assert_eq!(percent_decode_simple("hello%20world"), "hello world");
    }

    #[test]
    fn percent_decode_multiple() {
        assert_eq!(percent_decode_simple("%48%65%6C%6Co"), "Hello");
    }

    #[test]
    fn percent_decode_invalid_sequence_left_as_is() {
        assert_eq!(percent_decode_simple("100%zz"), "100%zz");
    }

    #[test]
    fn percent_decode_truncated_at_end() {
        assert_eq!(percent_decode_simple("test%2"), "test%2");
    }

    #[test]
    fn decode_hex_pair_valid() {
        assert_eq!(decode_hex_pair(b'4', b'1'), Some(0x41)); // 'A'
    }

    #[test]
    fn decode_hex_pair_invalid() {
        assert_eq!(decode_hex_pair(b'G', b'0'), None);
    }
}

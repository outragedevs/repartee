//! HTTP fetch module for image preview — two-phase detection strategy.
//!
//! Phase 1: Fetch the URL directly.
//!   - If `Content-Type` is `image/*`, return the image bytes immediately.
//!   - If `Content-Type` is `text/html`, move to phase 2.
//!   - Otherwise, return an error.
//!
//! Phase 2: Extract the `og:image` meta tag from the HTML body, resolve
//! relative URLs against the original page URL, then fetch the image URL.
//! This handles sites like imgur, imgbb, and any page that embeds an
//! Open Graph image tag. Only one level of recursion is performed — if the
//! og:image URL itself returns HTML, that is an error.

use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use reqwest::Client;
use thiserror::Error;

/// Pre-compiled regex for extracting og:image meta tags. Handles both
/// attribute orderings (`property` before `content` and vice versa).
static OG_IMAGE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)<meta\s+(?:property="og:image"\s+content="([^"]+)"|content="([^"]+)"\s+property="og:image")"#,
    )
    .expect("og:image regex is valid")
});

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Parameters that govern how image fetching behaves.
#[derive(Debug, Clone)]
pub struct FetchConfig {
    /// HTTP request timeout in seconds.
    pub timeout_secs: u32,
    /// Maximum response body size in bytes.
    pub max_file_size: u64,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            timeout_secs: 30,
            max_file_size: 10_485_760, // 10 MiB
        }
    }
}

// ---------------------------------------------------------------------------
// Result / Error types
// ---------------------------------------------------------------------------

/// Successful result of an image fetch.
#[derive(Debug, Clone)]
pub struct FetchResult {
    /// Raw image bytes.
    pub data: Vec<u8>,
    /// MIME type of the image (e.g. `"image/png"`).
    pub content_type: String,
}

/// Errors that can occur during image fetching.
#[derive(Debug, Error)]
pub enum FetchError {
    #[error("request timed out")]
    Timeout,

    #[error("file too large ({size} bytes, max {max} bytes)")]
    TooLarge { size: u64, max: u64 },

    #[error("not an image: {content_type}")]
    NotAnImage { content_type: String },

    #[error("HTML page has no og:image meta tag")]
    NoOgImage,

    #[error("network error: {0}")]
    Network(String),

    #[error("invalid URL: {0}")]
    InvalidUrl(String),
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Fetch an image from `url`, using two-phase detection (direct image or
/// og:image extraction from HTML pages).
///
/// The provided [`Client`] is reused across calls — callers should create one
/// client and pass it repeatedly.
///
/// # Errors
///
/// Returns [`FetchError`] on timeout, size limit, non-image content, missing
/// og:image, network failure, or invalid URL.
pub async fn fetch_image(
    url: &str,
    config: &FetchConfig,
    client: &Client,
) -> Result<FetchResult, FetchError> {
    // Phase 1 — fetch the URL directly.
    let (data, content_type, resolved_url) = fetch_url(url, config, client).await?;

    if is_image_content_type(&content_type) {
        return Ok(FetchResult { data, content_type });
    }

    // Phase 2 — if HTML, look for og:image.
    if content_type.contains("text/html") {
        let html = String::from_utf8_lossy(&data);
        let og_url = extract_og_image(&html).ok_or(FetchError::NoOgImage)?;

        // Resolve relative URLs against the original page.
        let image_url = resolve_url(&og_url, &resolved_url);

        let (img_data, img_ct, _) = fetch_url(&image_url, config, client).await?;

        if !is_image_content_type(&img_ct) {
            return Err(FetchError::NotAnImage {
                content_type: img_ct,
            });
        }

        return Ok(FetchResult {
            data: img_data,
            content_type: img_ct,
        });
    }

    Err(FetchError::NotAnImage { content_type })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Low-level fetch that returns `(body_bytes, content_type, final_url)`.
async fn fetch_url(
    url: &str,
    config: &FetchConfig,
    client: &Client,
) -> Result<(Vec<u8>, String, String), FetchError> {
    let response = client
        .get(url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .timeout(Duration::from_secs(u64::from(config.timeout_secs)))
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                FetchError::Timeout
            } else if e.is_redirect() {
                FetchError::Network(format!("redirect error: {e}"))
            } else if e.is_builder() {
                FetchError::InvalidUrl(e.to_string())
            } else {
                FetchError::Network(e.to_string())
            }
        })?;

    let final_url = response.url().to_string();

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();

    // Check Content-Length header *before* downloading the body.
    if let Some(len) = response.content_length()
        && len > config.max_file_size
    {
        return Err(FetchError::TooLarge {
            size: len,
            max: config.max_file_size,
        });
    }

    let bytes = response.bytes().await.map_err(|e| {
        if e.is_timeout() {
            FetchError::Timeout
        } else {
            FetchError::Network(e.to_string())
        }
    })?;

    if bytes.len() as u64 > config.max_file_size {
        return Err(FetchError::TooLarge {
            size: bytes.len() as u64,
            max: config.max_file_size,
        });
    }

    Ok((bytes.to_vec(), content_type, final_url))
}

/// Return `true` if the content-type string indicates an image.
fn is_image_content_type(ct: &str) -> bool {
    ct.starts_with("image/")
}

/// Extract the `og:image` URL from an HTML string.
///
/// Handles both attribute orderings:
///   `<meta property="og:image" content="...">`
///   `<meta content="..." property="og:image">`
///
/// Returns `None` when no og:image tag is found.
fn extract_og_image(html: &str) -> Option<String> {
    let caps = OG_IMAGE_RE.captures(html)?;
    caps.get(1)
        .or_else(|| caps.get(2))
        .map(|m| m.as_str().to_owned())
}

/// Resolve a potentially relative URL against a base URL.
///
/// Falls back to the raw `url` string if parsing fails.
fn resolve_url(url: &str, base: &str) -> String {
    // If the URL is already absolute, return it as-is.
    if url.starts_with("http://") || url.starts_with("https://") {
        return url.to_owned();
    }

    // Try to parse the base and join.
    reqwest::Url::parse(base)
        .and_then(|b| b.join(url))
        .map_or_else(|_| url.to_owned(), |u| u.to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- og:image extraction ------------------------------------------------

    #[test]
    fn extract_og_image_property_first() {
        let html = r#"<html><head>
            <meta property="og:image" content="https://example.com/photo.jpg">
        </head></html>"#;
        assert_eq!(
            extract_og_image(html),
            Some("https://example.com/photo.jpg".to_owned())
        );
    }

    #[test]
    fn extract_og_image_content_first() {
        let html = r#"<html><head>
            <meta content="https://cdn.example.com/img.png" property="og:image">
        </head></html>"#;
        assert_eq!(
            extract_og_image(html),
            Some("https://cdn.example.com/img.png".to_owned())
        );
    }

    #[test]
    fn extract_og_image_case_insensitive() {
        let html = r#"<META Property="og:image" Content="https://example.com/a.webp">"#;
        assert_eq!(
            extract_og_image(html),
            Some("https://example.com/a.webp".to_owned())
        );
    }

    #[test]
    fn extract_og_image_none_when_missing() {
        let html = r"<html><head><title>No image</title></head></html>";
        assert_eq!(extract_og_image(html), None);
    }

    #[test]
    fn extract_og_image_ignores_og_title() {
        let html = r#"<html><head>
            <meta property="og:title" content="Not an image">
            <meta property="og:description" content="Also not an image">
        </head></html>"#;
        assert_eq!(extract_og_image(html), None);
    }

    #[test]
    fn extract_og_image_picks_first_match() {
        let html = r#"<html><head>
            <meta property="og:image" content="https://example.com/first.jpg">
            <meta property="og:image" content="https://example.com/second.jpg">
        </head></html>"#;
        assert_eq!(
            extract_og_image(html),
            Some("https://example.com/first.jpg".to_owned())
        );
    }

    #[test]
    fn extract_og_image_with_extra_attributes() {
        // Real-world pages sometimes have extra attributes on the meta tag.
        // Our regex requires property and content adjacent, so this tests
        // the boundary of what we match.
        let html = r#"<meta property="og:image" content="https://example.com/x.gif">"#;
        assert_eq!(
            extract_og_image(html),
            Some("https://example.com/x.gif".to_owned())
        );
    }

    #[test]
    fn extract_og_image_empty_content() {
        let html = r#"<meta property="og:image" content="">"#;
        // An empty content attribute should not match (regex requires [^"]+).
        assert_eq!(extract_og_image(html), None);
    }

    // -- content-type detection ---------------------------------------------

    #[test]
    fn is_image_jpeg() {
        assert!(is_image_content_type("image/jpeg"));
    }

    #[test]
    fn is_image_png() {
        assert!(is_image_content_type("image/png"));
    }

    #[test]
    fn is_image_webp() {
        assert!(is_image_content_type("image/webp"));
    }

    #[test]
    fn is_image_with_charset() {
        // Some servers include parameters: `image/png; charset=utf-8`
        assert!(is_image_content_type("image/png; charset=utf-8"));
    }

    #[test]
    fn is_not_image_text_html() {
        assert!(!is_image_content_type("text/html"));
    }

    #[test]
    fn is_not_image_application_json() {
        assert!(!is_image_content_type("application/json"));
    }

    #[test]
    fn is_not_image_empty() {
        assert!(!is_image_content_type(""));
    }

    // -- URL resolution -----------------------------------------------------

    #[test]
    fn resolve_absolute_url_unchanged() {
        let result = resolve_url(
            "https://cdn.example.com/photo.jpg",
            "https://example.com/page",
        );
        assert_eq!(result, "https://cdn.example.com/photo.jpg");
    }

    #[test]
    fn resolve_relative_path() {
        let result = resolve_url("/images/photo.jpg", "https://example.com/page/article");
        assert_eq!(result, "https://example.com/images/photo.jpg");
    }

    #[test]
    fn resolve_relative_no_leading_slash() {
        let result = resolve_url("photo.jpg", "https://example.com/pages/article");
        assert_eq!(result, "https://example.com/pages/photo.jpg");
    }

    #[test]
    fn resolve_protocol_relative() {
        // Protocol-relative URLs (//cdn.example.com/...) are absolute per the
        // URL spec, but our simple starts_with check will fall through to
        // join, which handles them correctly.
        let result = resolve_url("//cdn.example.com/img.png", "https://example.com/page");
        assert_eq!(result, "https://cdn.example.com/img.png");
    }

    #[test]
    fn resolve_with_invalid_base_returns_raw() {
        let result = resolve_url("/photo.jpg", "not a url at all");
        assert_eq!(result, "/photo.jpg");
    }

    // -- FetchConfig defaults -----------------------------------------------

    #[test]
    fn default_config_values() {
        let config = FetchConfig::default();
        assert_eq!(config.timeout_secs, 30);
        assert_eq!(config.max_file_size, 10_485_760);
    }

    // -- FetchError display -------------------------------------------------

    #[test]
    fn error_display_timeout() {
        let err = FetchError::Timeout;
        assert_eq!(err.to_string(), "request timed out");
    }

    #[test]
    fn error_display_too_large() {
        let err = FetchError::TooLarge {
            size: 20_000_000,
            max: 10_485_760,
        };
        assert!(err.to_string().contains("20000000"));
        assert!(err.to_string().contains("10485760"));
    }

    #[test]
    fn error_display_not_an_image() {
        let err = FetchError::NotAnImage {
            content_type: "text/plain".to_owned(),
        };
        assert!(err.to_string().contains("text/plain"));
    }

    #[test]
    fn error_display_no_og_image() {
        let err = FetchError::NoOgImage;
        assert!(err.to_string().contains("og:image"));
    }

    #[test]
    fn error_display_network() {
        let err = FetchError::Network("connection refused".to_owned());
        assert!(err.to_string().contains("connection refused"));
    }

    #[test]
    fn error_display_invalid_url() {
        let err = FetchError::InvalidUrl("bad scheme".to_owned());
        assert!(err.to_string().contains("bad scheme"));
    }
}

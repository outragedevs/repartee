//! Server-side image preview / thumbnail pipeline for the web frontend.
//!
//! When `web.image_previews` is enabled, every chat message broadcast to
//! the web UI is scanned for URLs. Each URL is classified and (when
//! eligible) pre-mapped to a stable hash so the browser can request its
//! thumbnail via `GET /api/preview?h=<hash>`.
//!
//! ## Imgur exception
//!
//! Imgur (`*.imgur.com`) blocks most server-side IP ranges. We therefore:
//!
//! - serve `i.imgur.com/abc.png`-style direct images as `ClientDirect`
//!   (the browser fetches them straight from Imgur), and
//! - skip preview for `imgur.com/gallery/xyz`-style page URLs entirely
//!   (the og:image scrape would just fail).
//!
//! Every other host goes through the server proxy, which lets us:
//!
//! - hide the user's IP from third-party CDNs,
//! - cache thumbnails on disk, and
//! - downscale large source images before transferring them to the
//!   browser (saving mobile bandwidth).

use std::collections::HashMap;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::CookieJar;
use image::ImageReader;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use serde::{Deserialize, Serialize};

use crate::image_preview::detect::{UrlClassification, UrlType, extract_urls};
use crate::image_preview::fetch::{FetchConfig, fetch_image};

use super::auth::session_cookie_name;
use super::server::AppHandle;

/// Maximum thumbnail dimensions, in CSS pixels. We never upscale, so smaller
/// source images stay at their original size.
pub const THUMB_MAX_W: u32 = 400;
pub const THUMB_MAX_H: u32 = 300;

/// Wire-format link preview attached to every `WireMessage` whose text
/// contains a URL the server thinks could yield a thumbnail.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkPreview {
    /// Original URL as it appeared in the message text.
    pub link: String,
    pub kind: LinkPreviewKind,
    /// `Some(url)` for both kinds:
    /// - `ClientDirect`: third-party URL the browser should `<img src=…>`
    /// - `ServerProxy`:  our `/api/preview?h=<hash>` URL
    ///
    /// `None` indicates "no preview" (returned only inside
    /// `WebPreviewExtractor` for skipped URLs; never serialised to the wire).
    pub thumb_url: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LinkPreviewKind {
    /// Browser fetches the source URL directly (Imgur exception).
    ClientDirect,
    /// Server proxies + thumbnails via `/api/preview?h=<hash>`.
    ServerProxy,
}

/// Long-lived extractor shared between the broadcast path (writes the
/// `hash → url` registry) and the `/api/preview` handler (reads it).
pub struct WebPreviewExtractor {
    /// HMAC key for hashing URLs into stable preview ids.
    secret: Vec<u8>,
    /// Cap per message — matches The Lounge's behaviour.
    max_per_msg: usize,
    /// In-memory map of `hash_hex → original_url`. Populated by
    /// [`Self::extract`] and read by [`preview_handler`]. Capped at
    /// [`MAX_REGISTRY_ENTRIES`] so a long-running server can't grow it
    /// without bound; on overflow the entire map is cleared (very old
    /// messages may then show a broken thumbnail until re-broadcast).
    registry: Mutex<HashMap<String, String>>,
    /// Maximum total cache size, in megabytes.
    cache_max_mb: u32,
    /// Cache directory; created lazily on first write.
    cache_dir: PathBuf,
    /// Shared HTTP client (rustls). Reqwest already pools connections.
    http: reqwest::Client,
}

/// Hard ceiling on the in-memory hash → URL registry. ~100 bytes per
/// entry means this caps the extractor at roughly 10 MB of strings,
/// which corresponds to many months of activity for a normal user.
const MAX_REGISTRY_ENTRIES: usize = 100_000;

impl WebPreviewExtractor {
    #[must_use]
    pub fn new(secret: Vec<u8>, max_per_msg: usize, cache_max_mb: u32) -> Self {
        let cache_dir = crate::constants::home_dir().join("web_thumbnails");
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            secret,
            max_per_msg,
            registry: Mutex::new(HashMap::new()),
            cache_max_mb,
            cache_dir,
            http,
        }
    }

    /// Run URL detection on `text` and return preview metadata for each URL
    /// the server is willing to surface to the client. URLs classified as
    /// `Skip` (e.g. Imgur gallery pages) yield no entry at all.
    #[must_use]
    pub fn extract(&self, text: &str) -> Vec<LinkPreview> {
        let mut out: Vec<LinkPreview> = Vec::new();
        for cls in extract_urls(text) {
            if out.len() >= self.max_per_msg {
                break;
            }
            // Dedupe within one message.
            if out.iter().any(|p| p.link == cls.url) {
                continue;
            }
            if let Some(preview) = self.classify(&cls) {
                out.push(preview);
            }
        }
        out
    }

    /// Apply the Imgur exception on top of the base URL classification, then
    /// register a hash for server-proxied entries.
    fn classify(&self, cls: &UrlClassification) -> Option<LinkPreview> {
        let host = host_of(&cls.url).unwrap_or_default();
        let is_imgur = host == "imgur.com" || host.ends_with(".imgur.com");

        match cls.url_type {
            // Imgur direct images: client-side load (server IP is blocked).
            UrlType::DirectImage if is_imgur => Some(LinkPreview {
                link: cls.url.clone(),
                kind: LinkPreviewKind::ClientDirect,
                thumb_url: Some(cls.url.clone()),
            }),
            // Imgur page: og:image scraping fails reliably; skip.
            UrlType::ImgurPage => None,
            // Everything else goes through the server proxy.
            UrlType::DirectImage | UrlType::ImgbbPage | UrlType::GenericPage => {
                let hash = self.hash_url(&cls.url);
                {
                    let mut reg = self.registry.lock().ok()?;
                    if reg.len() >= MAX_REGISTRY_ENTRIES {
                        reg.clear();
                    }
                    reg.insert(hash.clone(), cls.url.clone());
                }
                Some(LinkPreview {
                    link: cls.url.clone(),
                    kind: LinkPreviewKind::ServerProxy,
                    thumb_url: Some(format!("/api/preview?h={hash}")),
                })
            }
        }
    }

    /// HMAC-SHA256 the URL under the server secret and hex-encode.
    fn hash_url(&self, url: &str) -> String {
        use hmac::Mac;
        type HmacSha256 = hmac::Hmac<sha2::Sha256>;
        let mut mac = HmacSha256::new_from_slice(&self.secret).expect("HMAC accepts any key");
        mac.update(url.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// Look up a URL by its hash; returns `None` if the server has never
    /// extracted that URL from any message (this is what stops the endpoint
    /// from being abused as an open proxy).
    fn lookup(&self, hash: &str) -> Option<String> {
        self.registry.lock().ok()?.get(hash).cloned()
    }
}

// ---------------------------------------------------------------------------
// /api/preview handler
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct PreviewQuery {
    h: String,
}

/// `GET /api/preview?h=<hex_hash>` — auth + lookup + cache + fetch + thumbnail.
pub async fn preview_handler(
    jar: CookieJar,
    State(state): State<Arc<AppHandle>>,
    Query(q): Query<PreviewQuery>,
) -> Response {
    // 1. Authenticate. We deliberately authenticate before touching the
    //    extractor or filesystem so unknown-hash probes from unauth'd
    //    clients always look the same: 401.
    let Some(token) = jar.get(&session_cookie_name()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let valid = state
        .session_store
        .lock()
        .await
        .validate(token.value())
        .is_some();
    if !valid {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // 2. Image previews disabled → behave as if the route doesn't exist.
    let Some(extractor) = state.preview_extractor.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };

    // 3. Look up the URL by hash. Unknown hash = 404.
    let Some(url) = extractor.lookup(&q.h) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    // 4. Try the on-disk cache first.
    let cache_path = extractor.cache_dir.join(format!("{}.jpg", &q.h));
    if let Ok(bytes) = tokio::fs::read(&cache_path).await {
        return jpeg_response(bytes);
    }

    // 5. Cache miss — fetch + thumbnail. Run the CPU-bound decode/encode
    //    on a blocking thread so the runtime stays responsive.
    let fetch_cfg = FetchConfig::default();
    let result = match fetch_image(&url, &fetch_cfg, &extractor.http).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(url = %url, error = %e, "preview fetch failed");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let data = result.data;
    let bytes = match tokio::task::spawn_blocking(move || encode_thumbnail(&data)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => {
            tracing::debug!(url = %url, error = %e, "preview decode failed");
            return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
        }
        Err(e) => {
            tracing::warn!(error = %e, "preview decode task panicked");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // 6. Best-effort cache write. We never fail the request because of disk
    //    issues — the browser still gets the thumbnail.
    if let Err(e) = write_cache(&cache_path, &bytes).await {
        tracing::debug!(error = %e, "preview cache write failed");
    }

    // 7. Best-effort prune. Small constant cost per request — keeps the
    //    cache bounded without a separate scheduler.
    let prune_dir = extractor.cache_dir.clone();
    let cap_mb = u64::from(extractor.cache_max_mb);
    tokio::task::spawn_blocking(move || prune_cache(&prune_dir, cap_mb));

    jpeg_response(bytes)
}

fn jpeg_response(bytes: Vec<u8>) -> Response {
    (
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, "image/jpeg"),
            (
                axum::http::header::CACHE_CONTROL,
                "public, max-age=86400, immutable",
            ),
        ],
        bytes,
    )
        .into_response()
}

/// Decode source bytes, downscale to fit within `THUMB_MAX_W` × `THUMB_MAX_H`
/// while preserving aspect, and re-encode as JPEG quality 80.
///
/// Sources smaller than the bounds in either axis are returned at their
/// original dimensions — `image::DynamicImage::resize` would otherwise
/// upscale to fill the box.
fn encode_thumbnail(src: &[u8]) -> color_eyre::eyre::Result<Vec<u8>> {
    let img = ImageReader::new(Cursor::new(src))
        .with_guessed_format()?
        .decode()?;
    let target_w = THUMB_MAX_W.min(img.width());
    let target_h = THUMB_MAX_H.min(img.height());
    let thumb = if target_w == img.width() && target_h == img.height() {
        img
    } else {
        img.resize(target_w, target_h, FilterType::Triangle)
    };
    // JPEG can't carry alpha — flatten to RGB8.
    let rgb = thumb.to_rgb8();
    let mut out = Vec::with_capacity(48 * 1024);
    let encoder = JpegEncoder::new_with_quality(&mut out, 80);
    rgb.write_with_encoder(encoder)?;
    Ok(out)
}

async fn write_cache(path: &std::path::Path, bytes: &[u8]) -> color_eyre::eyre::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    // Atomic replace: write to .tmp first, then rename.
    let tmp = path.with_extension("jpg.tmp");
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

/// Drop the oldest files in `dir` until total size ≤ `cap_mb` MiB.
/// Errors are swallowed — pruning is best-effort.
fn prune_cache(dir: &std::path::Path, cap_mb: u64) {
    let cap_bytes = cap_mb.saturating_mul(1024 * 1024);
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<(std::time::SystemTime, u64, PathBuf)> = read
        .filter_map(std::result::Result::ok)
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            let mtime = meta.modified().ok()?;
            Some((mtime, meta.len(), e.path()))
        })
        .collect();
    let total: u64 = entries.iter().map(|(_, len, _)| *len).sum();
    if total <= cap_bytes {
        return;
    }
    // Oldest first.
    entries.sort_by_key(|e| e.0);
    let mut over = total.saturating_sub(cap_bytes);
    for (_, len, path) in entries {
        if over == 0 {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            over = over.saturating_sub(len);
        }
    }
}

/// Strip scheme, port, and path from a URL — same parser the
/// `image_preview::detect` module uses.
fn host_of(url: &str) -> Option<String> {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host_with_port = without_scheme
        .split_once('/')
        .map_or(without_scheme, |(h, _)| h);
    let host = host_with_port
        .rsplit_once(':')
        .map_or(host_with_port, |(h, _)| h);
    Some(host.to_ascii_lowercase())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ext() -> WebPreviewExtractor {
        WebPreviewExtractor::new(vec![0xAB; 32], 4, 200)
    }

    #[test]
    fn imgur_direct_image_is_client_direct() {
        let e = ext();
        let p = e.extract("look at https://i.imgur.com/abc123.png");
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].kind, LinkPreviewKind::ClientDirect);
        assert_eq!(p[0].thumb_url.as_deref(), Some("https://i.imgur.com/abc123.png"));
    }

    #[test]
    fn lookalike_host_is_not_treated_as_imgur() {
        // Regression: `host.ends_with("imgur.com")` would incorrectly match
        // `notimgur.com`. The dot-prefix check rules it out.
        let e = ext();
        let p = e.extract("https://notimgur.com/photo.png");
        assert_eq!(p.len(), 1);
        assert_eq!(
            p[0].kind,
            LinkPreviewKind::ServerProxy,
            "non-imgur lookalike must go through the server proxy"
        );
    }

    #[test]
    fn imgur_no_extension_still_client_direct() {
        // i.imgur.com/abc123 is classified as DirectImage by the detector.
        let e = ext();
        let p = e.extract("https://i.imgur.com/abc123");
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].kind, LinkPreviewKind::ClientDirect);
    }

    #[test]
    fn imgur_gallery_page_is_skipped() {
        let e = ext();
        let p = e.extract("https://imgur.com/gallery/xyz");
        assert!(p.is_empty(), "imgur gallery pages must produce no preview");
    }

    #[test]
    fn other_direct_image_is_server_proxied() {
        let e = ext();
        let p = e.extract("https://example.com/photo.jpg");
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].kind, LinkPreviewKind::ServerProxy);
        assert!(
            p[0].thumb_url
                .as_deref()
                .is_some_and(|t| t.starts_with("/api/preview?h=")),
            "server-proxied previews must use /api/preview?h="
        );
    }

    #[test]
    fn imgbb_page_is_server_proxied() {
        let e = ext();
        let p = e.extract("https://ibb.co/abc123");
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].kind, LinkPreviewKind::ServerProxy);
    }

    #[test]
    fn generic_page_is_server_proxied() {
        let e = ext();
        let p = e.extract("https://example.com/some/article");
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].kind, LinkPreviewKind::ServerProxy);
    }

    #[test]
    fn cap_limits_previews_per_message() {
        let e = WebPreviewExtractor::new(vec![0; 32], 2, 200);
        let p = e.extract(
            "a https://a.example/x.png b https://b.example/x.png c https://c.example/x.png",
        );
        assert_eq!(p.len(), 2, "max_per_msg must be honoured");
    }

    #[test]
    fn duplicate_urls_collapse() {
        let e = ext();
        let p = e.extract("https://a.example/x.png https://a.example/x.png");
        assert_eq!(p.len(), 1, "identical URLs must be deduped per message");
    }

    #[test]
    fn extractor_registry_is_populated_for_server_proxy() {
        let e = ext();
        let p = e.extract("https://example.com/photo.jpg");
        let url = p[0]
            .thumb_url
            .as_deref()
            .and_then(|t| t.strip_prefix("/api/preview?h="))
            .unwrap();
        assert_eq!(
            e.lookup(url).as_deref(),
            Some("https://example.com/photo.jpg")
        );
    }

    #[test]
    fn unknown_hash_lookup_returns_none() {
        let e = ext();
        assert!(e.lookup("00".repeat(32).as_str()).is_none());
    }

    #[test]
    fn hash_is_deterministic_for_same_secret_and_url() {
        let e1 = ext();
        let e2 = ext();
        assert_eq!(e1.hash_url("https://x"), e2.hash_url("https://x"));
    }

    #[test]
    fn hash_changes_when_secret_changes() {
        let a = WebPreviewExtractor::new(vec![1; 32], 4, 200);
        let b = WebPreviewExtractor::new(vec![2; 32], 4, 200);
        assert_ne!(a.hash_url("https://x"), b.hash_url("https://x"));
    }

    #[test]
    fn host_of_extracts_lowercase_no_port() {
        assert_eq!(host_of("https://Example.COM:8443/path").as_deref(), Some("example.com"));
    }

    #[test]
    fn host_of_handles_no_path() {
        assert_eq!(host_of("https://example.com").as_deref(), Some("example.com"));
    }

    #[test]
    fn host_of_rejects_non_http() {
        assert!(host_of("ftp://example.com/x").is_none());
    }

    #[test]
    fn encode_thumbnail_produces_jpeg() {
        // Build a 600x400 RGB image and verify the encoder produces JPEG bytes
        // that start with the SOI marker (0xFF 0xD8).
        let img = image::DynamicImage::new_rgb8(600, 400);
        let mut src = Vec::new();
        img.write_to(&mut Cursor::new(&mut src), image::ImageFormat::Png)
            .unwrap();
        let out = encode_thumbnail(&src).unwrap();
        assert_eq!(&out[..2], b"\xFF\xD8", "output must begin with JPEG SOI");
    }

    #[test]
    fn encode_thumbnail_downscales_large_input() {
        // 4000x4000 source → after thumbnailing must decode to ≤ THUMB bounds.
        let img = image::DynamicImage::new_rgb8(4000, 4000);
        let mut src = Vec::new();
        img.write_to(&mut Cursor::new(&mut src), image::ImageFormat::Png)
            .unwrap();
        let out = encode_thumbnail(&src).unwrap();
        let decoded = ImageReader::new(Cursor::new(&out))
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap();
        assert!(decoded.width() <= THUMB_MAX_W);
        assert!(decoded.height() <= THUMB_MAX_H);
    }

    #[test]
    fn encode_thumbnail_does_not_upscale_small_input() {
        // 50x40 source must come back at exactly 50x40 (no upscaling).
        let img = image::DynamicImage::new_rgb8(50, 40);
        let mut src = Vec::new();
        img.write_to(&mut Cursor::new(&mut src), image::ImageFormat::Png)
            .unwrap();
        let out = encode_thumbnail(&src).unwrap();
        let decoded = ImageReader::new(Cursor::new(&out))
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap();
        assert_eq!(decoded.width(), 50);
        assert_eq!(decoded.height(), 40);
    }
}

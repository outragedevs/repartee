//! Image preview support — renders inline image previews in the terminal.
//!
//! Orchestrates the image preview pipeline: URL detection, async fetching,
//! disk caching, image decoding, and protocol encoding for ratatui-image.

pub mod cache;
pub mod detect;
pub mod fetch;

use std::io::Cursor;

use image::ImageReader;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use tokio::sync::mpsc;
use tracing::{error, warn};

use crate::config::ImagePreviewConfig;

// ---------------------------------------------------------------------------
// Preview status (lives on App, driven by events from background tasks)
// ---------------------------------------------------------------------------

/// Current state of the image preview overlay.
#[derive(Default)]
pub enum PreviewStatus {
    /// No preview is active.
    #[default]
    Hidden,
    /// A preview is being fetched/decoded in the background.
    Loading { url: String },
    /// The image is ready to display.
    Ready {
        url: String,
        title: Option<String>,
        /// Pre-encoded image for the ratatui-image widget.
        image: Box<StatefulProtocol>,
        /// Raw PNG bytes for direct-write path (iTerm2+tmux).
        raw_png: Vec<u8>,
        /// Width in terminal cells (including border).
        width: u16,
        /// Height in terminal cells (including border).
        height: u16,
    },
    /// Fetching or decoding failed.
    Error { url: String, message: String },
}

// ---------------------------------------------------------------------------
// Events sent from background tasks back to the main loop
// ---------------------------------------------------------------------------

/// Result of an async image preview task, sent via channel.
pub enum ImagePreviewEvent {
    /// Image is ready to display.
    Ready {
        url: String,
        title: Option<String>,
        image: Box<StatefulProtocol>,
        raw_png: Vec<u8>,
        width: u16,
        height: u16,
    },
    /// Fetching or decoding failed.
    Error { url: String, message: String },
}

// ---------------------------------------------------------------------------
// Spawn a background task to fetch, cache, decode, and encode an image
// ---------------------------------------------------------------------------

/// Spawn an async task to fetch, cache, and encode an image for preview.
///
/// Results are sent back via the provided channel. The caller should set the
/// preview status to `Loading` before calling this.
///
/// # Arguments
///
/// * `url` - The image URL to fetch.
/// * `config` - Image preview configuration (limits, timeouts).
/// * `picker` - The ratatui-image picker (cloned into the task).
/// * `http_client` - Shared reqwest client.
/// * `tx` - Channel sender for delivering results.
/// * `term_size` - Terminal dimensions `(cols, rows)` for sizing the popup.
pub fn spawn_preview(
    url: &str,
    config: &ImagePreviewConfig,
    picker: &Picker,
    http_client: &reqwest::Client,
    tx: mpsc::UnboundedSender<ImagePreviewEvent>,
    term_size: (u16, u16),
) {
    let config = config.clone();
    let picker = picker.clone();
    let client = http_client.clone();
    let url = url.to_owned();

    tokio::task::spawn_blocking(move || {
        let result = fetch_decode_encode(&url, &config, &picker, &client, term_size);
        let event = match result {
            Ok((title, protocol, png_buf, width, height)) => ImagePreviewEvent::Ready {
                url,
                title,
                image: Box::new(protocol),
                raw_png: png_buf,
                width,
                height,
            },
            Err(e) => {
                error!(url = %url, error = %e, "image preview failed");
                ImagePreviewEvent::Error {
                    url,
                    message: e.to_string(),
                }
            }
        };

        if tx.send(event).is_err() {
            warn!("image preview channel closed before result could be sent");
        }
    });
}

/// Synchronous pipeline: fetch (or load from cache) -> validate -> decode -> encode.
///
/// Called inside `spawn_blocking` because image decoding and protocol encoding
/// are CPU-bound operations.
/// (title, protocol, `raw_png`, width, height)
type DecodeResult = (Option<String>, StatefulProtocol, Vec<u8>, u16, u16);

fn fetch_decode_encode(
    url: &str,
    config: &ImagePreviewConfig,
    picker: &Picker,
    client: &reqwest::Client,
    term_size: (u16, u16),
) -> color_eyre::eyre::Result<DecodeResult> {
    // 1. Check the disk cache first.
    let (data, title) = if let Some(cached_path) = cache::is_cached(url) {
        let data = std::fs::read(&cached_path)?;
        let title = detect::classify_url(url).and_then(|c| c.title);
        (data, title)
    } else {
        // 2. Fetch from network using the tokio runtime handle.
        let handle = tokio::runtime::Handle::current();
        let fetch_config = fetch::FetchConfig {
            timeout_secs: config.fetch_timeout,
            max_file_size: config.max_file_size,
        };
        let result = handle.block_on(fetch::fetch_image(url, &fetch_config, client))?;

        // 3. Validate magic bytes.
        if !cache::validate_magic_bytes(&result.data) {
            return Err(color_eyre::eyre::eyre!(
                "downloaded data does not appear to be a valid image"
            ));
        }

        // 4. Store in cache.
        if let Err(e) = cache::store(url, &result.data, &result.content_type) {
            warn!(url, error = %e, "failed to cache image");
        }

        let title = detect::classify_url(url).and_then(|c| c.title);
        (result.data, title)
    };

    // 5. Decode the image.
    let dyn_img = ImageReader::new(Cursor::new(&data))
        .with_guessed_format()?
        .decode()?;

    // 6. Calculate display dimensions (matching kokoirc aspect ratio logic).
    let (width, height) = calculate_display_size(config, term_size, &dyn_img);

    // 7. Encode as PNG for the direct-write path (iTerm2+tmux).
    let mut png_buf: Vec<u8> = Vec::new();
    dyn_img.write_to(&mut Cursor::new(&mut png_buf), image::ImageFormat::Png)?;

    // 8. Create the protocol image via the picker.
    let protocol = picker.new_resize_protocol(dyn_img);

    Ok((title, protocol, png_buf, width, height))
}

/// Calculate the popup dimensions in terminal cells.
///
/// The popup includes a 1-cell border on each side, so the inner image area
/// is `(width - 2, height - 2)`. The image is scaled to fit while preserving
/// its aspect ratio.
///
/// Terminal cells are roughly twice as tall as they are wide, so a
/// `cell_aspect` factor of 2 is applied when converting pixel dimensions
/// to cell dimensions.
fn calculate_display_size(
    config: &ImagePreviewConfig,
    term_size: (u16, u16),
    img: &image::DynamicImage,
) -> (u16, u16) {
    let max_cols = if config.max_width > 0 {
        u16::try_from(config.max_width).unwrap_or(u16::MAX)
    } else {
        term_size.0 * 3 / 4
    };

    let max_rows = if config.max_height > 0 {
        u16::try_from(config.max_height).unwrap_or(u16::MAX)
    } else {
        term_size.1 * 3 / 4
    };

    // Reserve 2 cells on each axis for the border.
    let inner_cols = max_cols.saturating_sub(2).max(1);
    let inner_rows = max_rows.saturating_sub(2).max(1);

    let img_w = img.width();
    let img_h = img.height();

    if img_w == 0 || img_h == 0 {
        return (max_cols.min(10), max_rows.min(5));
    }

    // Terminal cells are ~2:1 height:width, so each row of cells represents
    // about 2x the pixels of a column. To maintain the visual aspect ratio,
    // we scale the image height by the cell aspect ratio.
    let cell_aspect: f64 = 2.0;

    // How many columns and rows would the image occupy if scaled to fit?
    let scale_x = f64::from(inner_cols) / f64::from(img_w);
    let scale_y = f64::from(inner_rows) / (f64::from(img_h) / cell_aspect);
    let scale = scale_x.min(scale_y).min(1.0); // never upscale

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "dimensions are small positive values; truncation is intentional"
    )]
    let fitted_cols = (f64::from(img_w) * scale).round().max(1.0) as u16;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "dimensions are small positive values; truncation is intentional"
    )]
    let fitted_rows = (f64::from(img_h) / cell_aspect * scale)
        .round()
        .max(1.0) as u16;

    // Add the border back.
    (fitted_cols + 2, fitted_rows + 2)
}

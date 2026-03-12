//! Disk cache for downloaded image previews.
//!
//! Images are stored under `~/.repartee/image_cache/` with filenames derived
//! from the SHA-256 hash of the source URL plus a file extension. All I/O is
//! synchronous (`std::fs`) and intended to be called from async code via
//! `spawn_blocking`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use color_eyre::eyre::Result;
use sha2::{Digest, Sha256};

/// Known image file extensions we scan for when checking the cache.
const KNOWN_EXTENSIONS: &[&str] = &[".jpg", ".jpeg", ".png", ".gif", ".webp", ".bmp", ".bin"];

/// Magic byte signatures for image format validation.
const MAGIC_JPEG: &[u8] = &[0xFF, 0xD8, 0xFF];
const MAGIC_PNG: &[u8] = &[0x89, 0x50, 0x4E, 0x47];
const MAGIC_GIF: &[u8] = &[0x47, 0x49, 0x46];
const MAGIC_RIFF: &[u8] = &[0x52, 0x49, 0x46, 0x46];
const MAGIC_WEBP: &[u8] = &[0x57, 0x45, 0x42, 0x50];

/// Statistics returned by [`cleanup`].
#[derive(Debug, Clone, Copy)]
pub struct CleanupStats {
    pub files_removed: u64,
    pub bytes_freed: u64,
}

/// Statistics returned by [`stats`].
#[derive(Debug, Clone, Copy)]
pub struct CacheStats {
    pub total_files: u64,
    pub total_bytes: u64,
    /// Age of the oldest file in seconds, or 0 if the cache is empty.
    pub oldest_age_secs: u64,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return the cache directory path (`~/.repartee/image_cache/`).
#[must_use]
pub fn cache_dir() -> PathBuf {
    crate::constants::home_dir().join("image_cache")
}

/// Ensure the cache directory exists.
fn ensure_cache_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    Ok(())
}

/// Compute the SHA-256 hex digest of a URL string.
#[must_use]
fn url_hash(url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    let result = hasher.finalize();
    // Format as lowercase hex without pulling in the `hex` crate — sha2's
    // GenericArray implements LowerHex.
    format!("{result:x}")
}

/// Map a `Content-Type` header value to a file extension.
#[must_use]
fn extension_for_content_type(content_type: &str) -> &'static str {
    // Normalize: take the part before any `;` (e.g. "image/jpeg; charset=…")
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim();
    match mime {
        "image/jpeg" => ".jpg",
        "image/png" => ".png",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "image/bmp" => ".bmp",
        _ => ".bin",
    }
}

/// Validate raw image data by inspecting magic bytes.
///
/// Returns `true` if the header matches JPEG, PNG, GIF, or WEBP.
#[must_use]
pub fn validate_magic_bytes(data: &[u8]) -> bool {
    if data.len() >= MAGIC_JPEG.len() && data[..MAGIC_JPEG.len()] == *MAGIC_JPEG {
        return true;
    }
    if data.len() >= MAGIC_PNG.len() && data[..MAGIC_PNG.len()] == *MAGIC_PNG {
        return true;
    }
    if data.len() >= MAGIC_GIF.len() && data[..MAGIC_GIF.len()] == *MAGIC_GIF {
        return true;
    }
    // WEBP: RIFF header at offset 0, "WEBP" at offset 8
    if data.len() >= 12 && data[..MAGIC_RIFF.len()] == *MAGIC_RIFF && data[8..12] == *MAGIC_WEBP {
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Check whether a cached file exists for `url`.
///
/// Scans for `<sha256(url)>.*` among [`KNOWN_EXTENSIONS`]. Returns the first
/// matching path, or `None` if no cached file is found.
#[must_use]
pub fn is_cached(url: &str) -> Option<PathBuf> {
    is_cached_in(&cache_dir(), url)
}

/// Like [`is_cached`] but operates against an explicit cache directory.
#[must_use]
pub fn is_cached_in(dir: &Path, url: &str) -> Option<PathBuf> {
    let hash = url_hash(url);
    for ext in KNOWN_EXTENSIONS {
        let candidate = dir.join(format!("{hash}{ext}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Store image `data` in the cache, deriving the filename from `url` and
/// `content_type`.
///
/// Returns the path of the written file.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be created or the file
/// cannot be written.
pub fn store(url: &str, data: &[u8], content_type: &str) -> Result<PathBuf> {
    store_in(&cache_dir(), url, data, content_type)
}

/// Like [`store`] but writes into an explicit cache directory.
pub fn store_in(dir: &Path, url: &str, data: &[u8], content_type: &str) -> Result<PathBuf> {
    ensure_cache_dir(dir)?;
    let hash = url_hash(url);
    let ext = extension_for_content_type(content_type);
    let path = dir.join(format!("{hash}{ext}"));
    fs::write(&path, data)?;
    Ok(path)
}

/// Remove files that exceed age or total-size limits.
///
/// 1. Delete every file older than `max_days`.
/// 2. Delete the oldest remaining files until total size is at most `max_mb` MiB.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be read.
pub fn cleanup(max_mb: u32, max_days: u32) -> Result<CleanupStats> {
    cleanup_in(&cache_dir(), max_mb, max_days)
}

/// Like [`cleanup`] but operates against an explicit cache directory.
pub fn cleanup_in(dir: &Path, max_mb: u32, max_days: u32) -> Result<CleanupStats> {
    struct Entry {
        path: PathBuf,
        size: u64,
        modified: SystemTime,
    }

    let mut files_removed: u64 = 0;
    let mut bytes_freed: u64 = 0;

    let entries = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CleanupStats {
                files_removed: 0,
                bytes_freed: 0,
            });
        }
        Err(e) => return Err(e.into()),
    };

    let now = SystemTime::now();
    let max_age = std::time::Duration::from_secs(u64::from(max_days) * 24 * 60 * 60);

    // Collect surviving entries (after age-based pruning).
    let mut surviving: Vec<Entry> = Vec::new();

    for entry in entries {
        let entry = entry?;
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }

        let modified = meta.modified().unwrap_or(now);
        let age = now.duration_since(modified).unwrap_or_default();

        if age > max_age {
            let size = meta.len();
            if fs::remove_file(entry.path()).is_ok() {
                files_removed += 1;
                bytes_freed += size;
            }
        } else {
            surviving.push(Entry {
                path: entry.path(),
                size: meta.len(),
                modified,
            });
        }
    }

    // Sort oldest-first so we evict the stalest files first.
    surviving.sort_by_key(|e| e.modified);

    let max_bytes = u64::from(max_mb) * 1024 * 1024;
    let mut total_size: u64 = surviving.iter().map(|e| e.size).sum();

    for entry in &surviving {
        if total_size <= max_bytes {
            break;
        }
        if fs::remove_file(&entry.path).is_ok() {
            files_removed += 1;
            bytes_freed += entry.size;
            total_size = total_size.saturating_sub(entry.size);
        }
    }

    Ok(CleanupStats {
        files_removed,
        bytes_freed,
    })
}

/// Return statistics about the current cache contents.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be read.
pub fn stats() -> Result<CacheStats> {
    stats_in(&cache_dir())
}

/// Like [`stats`] but operates against an explicit cache directory.
pub fn stats_in(dir: &Path) -> Result<CacheStats> {
    let entries = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CacheStats {
                total_files: 0,
                total_bytes: 0,
                oldest_age_secs: 0,
            });
        }
        Err(e) => return Err(e.into()),
    };

    let now = SystemTime::now();
    let mut total_files: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut oldest_age = std::time::Duration::ZERO;

    for entry in entries {
        let entry = entry?;
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        total_files += 1;
        total_bytes += meta.len();

        let modified = meta.modified().unwrap_or(now);
        let age = now.duration_since(modified).unwrap_or_default();
        if age > oldest_age {
            oldest_age = age;
        }
    }

    Ok(CacheStats {
        total_files,
        total_bytes,
        oldest_age_secs: oldest_age.as_secs(),
    })
}

/// Delete all cached files and return the number of files removed.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be read.
pub fn clear() -> Result<u64> {
    clear_in(&cache_dir())
}

/// Like [`clear`] but operates against an explicit cache directory.
pub fn clear_in(dir: &Path) -> Result<u64> {
    let entries = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };

    let mut removed: u64 = 0;
    for entry in entries {
        let entry = entry?;
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_file() && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    /// Helper: create a temp dir to act as an isolated cache directory.
    fn temp_cache() -> tempfile::TempDir {
        tempfile::TempDir::new().expect("failed to create temp dir")
    }

    // -- url_hash -----------------------------------------------------------

    #[test]
    fn url_hash_is_deterministic() {
        let h1 = url_hash("https://example.com/image.png");
        let h2 = url_hash("https://example.com/image.png");
        assert_eq!(h1, h2);
    }

    #[test]
    fn url_hash_differs_for_different_urls() {
        let h1 = url_hash("https://example.com/a.png");
        let h2 = url_hash("https://example.com/b.png");
        assert_ne!(h1, h2);
    }

    #[test]
    fn url_hash_is_64_hex_chars() {
        let h = url_hash("https://example.com/test");
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // -- extension_for_content_type -----------------------------------------

    #[test]
    fn content_type_mapping() {
        assert_eq!(extension_for_content_type("image/jpeg"), ".jpg");
        assert_eq!(extension_for_content_type("image/png"), ".png");
        assert_eq!(extension_for_content_type("image/gif"), ".gif");
        assert_eq!(extension_for_content_type("image/webp"), ".webp");
        assert_eq!(extension_for_content_type("image/bmp"), ".bmp");
        assert_eq!(
            extension_for_content_type("application/octet-stream"),
            ".bin"
        );
    }

    #[test]
    fn content_type_with_params() {
        assert_eq!(
            extension_for_content_type("image/jpeg; charset=utf-8"),
            ".jpg"
        );
        assert_eq!(
            extension_for_content_type("image/png; boundary=something"),
            ".png"
        );
    }

    // -- validate_magic_bytes -----------------------------------------------

    #[test]
    fn magic_bytes_jpeg() {
        assert!(validate_magic_bytes(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00]));
    }

    #[test]
    fn magic_bytes_png() {
        assert!(validate_magic_bytes(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A]));
    }

    #[test]
    fn magic_bytes_gif() {
        assert!(validate_magic_bytes(&[0x47, 0x49, 0x46, 0x38, 0x39, 0x61]));
    }

    #[test]
    fn magic_bytes_webp() {
        let mut data = vec![0u8; 12];
        data[..4].copy_from_slice(&[0x52, 0x49, 0x46, 0x46]); // RIFF
        // bytes 4-7 are file size (don't care)
        data[8..12].copy_from_slice(&[0x57, 0x45, 0x42, 0x50]); // WEBP
        assert!(validate_magic_bytes(&data));
    }

    #[test]
    fn magic_bytes_riff_not_webp() {
        let mut data = vec![0u8; 12];
        data[..4].copy_from_slice(&[0x52, 0x49, 0x46, 0x46]); // RIFF
        data[8..12].copy_from_slice(&[0x41, 0x56, 0x49, 0x20]); // AVI
        assert!(!validate_magic_bytes(&data));
    }

    #[test]
    fn magic_bytes_invalid() {
        assert!(!validate_magic_bytes(&[0x00, 0x00, 0x00]));
        assert!(!validate_magic_bytes(&[0x50, 0x4B, 0x03, 0x04])); // ZIP
    }

    #[test]
    fn magic_bytes_too_short() {
        assert!(!validate_magic_bytes(&[]));
        assert!(!validate_magic_bytes(&[0xFF]));
        assert!(!validate_magic_bytes(&[0xFF, 0xD8]));
    }

    // -- store + is_cached --------------------------------------------------

    #[test]
    fn store_and_find() {
        let dir = temp_cache();
        let url = "https://example.com/photo.jpg";
        let data = &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];

        let path = store_in(dir.path(), url, data, "image/jpeg").unwrap();
        assert!(path.exists());
        assert!(path.to_string_lossy().ends_with(".jpg"));
        assert_eq!(fs::read(&path).unwrap(), data);

        // is_cached should find it
        let found = is_cached_in(dir.path(), url);
        assert_eq!(found, Some(path));
    }

    #[test]
    fn store_creates_directory() {
        let dir = temp_cache();
        let nested = dir.path().join("sub").join("cache");
        let url = "https://example.com/img.png";
        let data = &[0x89, 0x50, 0x4E, 0x47];

        let path = store_in(&nested, url, data, "image/png").unwrap();
        assert!(path.exists());
        assert!(nested.is_dir());
    }

    #[test]
    fn is_cached_returns_none_when_missing() {
        let dir = temp_cache();
        assert!(is_cached_in(dir.path(), "https://no-such-url.example.com").is_none());
    }

    #[test]
    fn is_cached_returns_none_for_nonexistent_dir() {
        let dir = PathBuf::from("/tmp/repartee_test_nonexistent_42");
        assert!(is_cached_in(&dir, "https://example.com").is_none());
    }

    #[test]
    fn store_unknown_content_type_uses_bin() {
        let dir = temp_cache();
        let path = store_in(
            dir.path(),
            "https://x.com/f",
            b"data",
            "application/octet-stream",
        )
        .unwrap();
        assert!(path.to_string_lossy().ends_with(".bin"));
    }

    #[test]
    fn same_url_overwrites() {
        let dir = temp_cache();
        let url = "https://example.com/dup.png";

        store_in(dir.path(), url, b"first", "image/png").unwrap();
        let path = store_in(dir.path(), url, b"second", "image/png").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"second");
    }

    // -- stats --------------------------------------------------------------

    #[test]
    fn stats_empty_cache() {
        let dir = temp_cache();
        let s = stats_in(dir.path()).unwrap();
        assert_eq!(s.total_files, 0);
        assert_eq!(s.total_bytes, 0);
        assert_eq!(s.oldest_age_secs, 0);
    }

    #[test]
    fn stats_counts_files() {
        let dir = temp_cache();
        store_in(dir.path(), "https://a.com/1.jpg", &[1; 100], "image/jpeg").unwrap();
        store_in(dir.path(), "https://a.com/2.png", &[2; 200], "image/png").unwrap();

        let s = stats_in(dir.path()).unwrap();
        assert_eq!(s.total_files, 2);
        assert_eq!(s.total_bytes, 300);
    }

    #[test]
    fn stats_nonexistent_dir() {
        let s = stats_in(Path::new("/tmp/repartee_test_no_such_dir_xyz")).unwrap();
        assert_eq!(s.total_files, 0);
    }

    // -- clear --------------------------------------------------------------

    #[test]
    fn clear_removes_all_files() {
        let dir = temp_cache();
        store_in(dir.path(), "https://a.com/1.jpg", b"img1", "image/jpeg").unwrap();
        store_in(dir.path(), "https://a.com/2.png", b"img2", "image/png").unwrap();
        store_in(dir.path(), "https://a.com/3.gif", b"img3", "image/gif").unwrap();

        let removed = clear_in(dir.path()).unwrap();
        assert_eq!(removed, 3);

        let s = stats_in(dir.path()).unwrap();
        assert_eq!(s.total_files, 0);
    }

    #[test]
    fn clear_empty_cache() {
        let dir = temp_cache();
        let removed = clear_in(dir.path()).unwrap();
        assert_eq!(removed, 0);
    }

    #[test]
    fn clear_nonexistent_dir() {
        let removed = clear_in(Path::new("/tmp/repartee_test_no_such_dir_clear")).unwrap();
        assert_eq!(removed, 0);
    }

    // -- cleanup ------------------------------------------------------------

    #[test]
    fn cleanup_removes_by_size() {
        let dir = temp_cache();
        // Store 3 files of 1 byte each with slight time gaps so ordering is
        // deterministic.
        store_in(dir.path(), "https://a.com/1", &[1], "image/jpeg").unwrap();
        thread::sleep(Duration::from_millis(50));
        store_in(dir.path(), "https://a.com/2", &[2], "image/png").unwrap();
        thread::sleep(Duration::from_millis(50));
        store_in(dir.path(), "https://a.com/3", &[3], "image/gif").unwrap();

        // max_mb=0 means remove everything; max_days=999 means nothing is too old
        let result = cleanup_in(dir.path(), 0, 999).unwrap();
        assert_eq!(result.files_removed, 3);
        assert_eq!(result.bytes_freed, 3);

        let s = stats_in(dir.path()).unwrap();
        assert_eq!(s.total_files, 0);
    }

    #[test]
    fn cleanup_keeps_files_within_limit() {
        let dir = temp_cache();
        // Each file is 100 bytes. 3 files = 300 bytes.
        store_in(dir.path(), "https://a.com/1", &[0; 100], "image/jpeg").unwrap();
        thread::sleep(Duration::from_millis(50));
        store_in(dir.path(), "https://a.com/2", &[0; 100], "image/png").unwrap();
        thread::sleep(Duration::from_millis(50));
        store_in(dir.path(), "https://a.com/3", &[0; 100], "image/gif").unwrap();

        // 1 MB is way more than 300 bytes — nothing should be removed.
        let result = cleanup_in(dir.path(), 1, 999).unwrap();
        assert_eq!(result.files_removed, 0);
        assert_eq!(result.bytes_freed, 0);

        let s = stats_in(dir.path()).unwrap();
        assert_eq!(s.total_files, 3);
    }

    #[test]
    fn cleanup_nonexistent_dir() {
        let result = cleanup_in(Path::new("/tmp/repartee_test_no_such_cleanup"), 10, 30).unwrap();
        assert_eq!(result.files_removed, 0);
    }

    #[test]
    fn cleanup_removes_old_files() {
        let dir = temp_cache();
        let url = "https://a.com/old";
        let path = store_in(dir.path(), url, b"old", "image/jpeg").unwrap();

        // Backdate the file's mtime to 100 days ago using std::fs::File::set_times.
        let old_time = SystemTime::now() - Duration::from_secs(100 * 24 * 60 * 60);
        let file = fs::File::options().write(true).open(&path).unwrap();
        file.set_times(fs::FileTimes::new().set_modified(old_time))
            .unwrap();
        drop(file);

        let result = cleanup_in(dir.path(), 1000, 30).unwrap();
        assert_eq!(result.files_removed, 1);
        assert_eq!(result.bytes_freed, 3); // b"old" is 3 bytes
    }

    // -- cache_dir ----------------------------------------------------------

    #[test]
    fn cache_dir_ends_with_image_cache() {
        let dir = cache_dir();
        assert!(dir.ends_with("image_cache"));
    }

    // -- different content types produce different filenames -----------------

    #[test]
    fn different_content_types_different_paths() {
        let dir = temp_cache();
        let url = "https://a.com/image";
        let p1 = store_in(dir.path(), url, b"a", "image/jpeg").unwrap();
        let p2 = store_in(dir.path(), url, b"b", "image/png").unwrap();
        // Same hash, different extension — two distinct files
        assert_ne!(p1, p2);
        assert!(p1.exists());
        assert!(p2.exists());
    }
}

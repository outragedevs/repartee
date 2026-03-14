//! Multilingual spell checker backed by `spellbook` (Hunspell-compatible).
//!
//! Loads one `spellbook::Dictionary` per configured language from `.dic`/`.aff`
//! files. A word is considered correct if ANY dictionary accepts it (union check).
//! Suggestions are ranked by dictionary priority (config order).
//!
//! Follows `WeeChat`'s spell plugin UX: strip trailing punctuation, skip URLs,
//! skip nicks, skip number-like strings, minimum word length 2.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use tokio::sync::mpsc;

/// Maximum number of suggestions returned per misspelled word.
const MAX_SUGGESTIONS: usize = 4;

/// Maximum suggestions to collect from a single dictionary before moving on.
const MAX_PER_DICT: usize = 6;

/// Minimum word length to spell-check (after punctuation stripping).
const MIN_WORD_LEN: usize = 2;

/// URL prefixes that should be skipped entirely.
const URL_PREFIXES: &[&str] = &[
    "http:", "https:", "ftp:", "ftps:", "ssh:", "irc:", "ircs:", "git:", "svn:", "file:", "telnet:",
];

/// A loaded language dictionary.
struct LangDict {
    /// Language code (e.g. `en_US`).
    #[expect(dead_code, reason = "stored for diagnostics and /spellcheck status")]
    lang: String,
    dict: Arc<spellbook::Dictionary>,
}

/// Multilingual spell checker. Thread-safe (`Send + Sync`).
pub struct SpellChecker {
    dicts: Vec<LangDict>,
}

impl SpellChecker {
    /// Load dictionaries for the given language codes from `dict_dir`.
    ///
    /// Each language needs `{lang}.aff` and `{lang}.dic` in the directory.
    /// Languages that fail to load are logged and skipped.
    /// Dictionary order determines suggestion priority (first = highest).
    pub fn load(languages: &[String], dict_dir: &Path) -> Self {
        let mut dicts = Vec::new();
        for lang in languages {
            match load_dictionary(lang, dict_dir) {
                Ok(dict) => {
                    tracing::info!(lang = %lang, "spellcheck dictionary loaded");
                    dicts.push(LangDict {
                        lang: lang.clone(),
                        dict: Arc::new(dict),
                    });
                }
                Err(e) => {
                    tracing::warn!(lang = %lang, error = %e, "failed to load spellcheck dictionary");
                }
            }
        }
        Self { dicts }
    }

    /// Check whether a word should be flagged as misspelled.
    ///
    /// Returns `true` if the word is correct (or should be skipped).
    /// The word should already be stripped of surrounding punctuation.
    pub fn check(&self, word: &str, nicks: &HashSet<String>) -> bool {
        if self.dicts.is_empty() || word.len() < MIN_WORD_LEN {
            return true;
        }
        // Skip URLs
        if is_url(word) {
            return true;
        }
        // Skip number-like strings (digits + punctuation only)
        if is_number_like(word) {
            return true;
        }
        // Skip words containing underscores (variable names, etc.)
        if word.contains('_') {
            return true;
        }
        // Skip channel nicks (case-insensitive)
        let word_lower = word.to_lowercase();
        if nicks.iter().any(|n| n.to_lowercase() == word_lower) {
            return true;
        }
        // Union check: correct if ANY dictionary accepts
        self.dicts.iter().any(|ld| ld.dict.check(word))
    }

    /// Get spelling suggestions for a misspelled word, ranked by dictionary
    /// priority (config order). First dictionary's suggestions come first.
    ///
    /// Returns up to [`MAX_SUGGESTIONS`] unique suggestions.
    pub fn suggest(&self, word: &str) -> Vec<String> {
        let mut all: Vec<String> = Vec::new();
        let mut seen = HashSet::new();

        // Collect from each dictionary in priority order.
        // First dictionary = highest priority, its suggestions appear first.
        for ld in &self.dicts {
            let mut dict_suggestions = Vec::new();
            ld.dict.suggest(word, &mut dict_suggestions);

            for s in dict_suggestions.into_iter().take(MAX_PER_DICT) {
                let lower = s.to_lowercase();
                if seen.contains(&lower) {
                    continue;
                }
                seen.insert(lower);
                all.push(s);
                if all.len() >= MAX_SUGGESTIONS {
                    return all;
                }
            }
        }
        all
    }

    /// Whether any dictionaries are loaded.
    pub const fn is_active(&self) -> bool {
        !self.dicts.is_empty()
    }

    /// Number of loaded dictionaries.
    pub const fn dict_count(&self) -> usize {
        self.dicts.len()
    }

    /// Resolve the dictionary directory path.
    pub fn resolve_dict_dir(configured: &str) -> PathBuf {
        if configured.is_empty() {
            crate::constants::dicts_dir()
        } else {
            PathBuf::from(configured)
        }
    }
}

// ── Dictionary download types ──────────────────────────────────────────

/// Remote dictionary manifest (mirrors `manifest.json` in repartee-dicts repo).
#[derive(Debug, Clone, Deserialize)]
pub struct DictManifest {
    #[expect(dead_code, reason = "reserved for future manifest format changes")]
    pub version: u32,
    pub dictionaries: HashMap<String, DictInfo>,
}

/// Metadata for a single dictionary in the manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct DictInfo {
    pub name: String,
}

/// A single entry in the dictionary list result.
#[derive(Debug)]
pub struct DictListEntry {
    /// Language code (e.g. `en_US`).
    pub code: String,
    /// Human-readable name (e.g. "English (US)").
    pub name: String,
    /// Whether this dictionary is already installed locally.
    pub installed: bool,
}

/// Events sent from async dictionary download tasks back to the main loop.
#[derive(Debug)]
pub enum DictEvent {
    /// Manifest fetched successfully — contains available dicts and which are installed.
    ListResult {
        entries: Vec<DictListEntry>,
    },
    /// A dictionary was downloaded and saved.
    Downloaded { lang: String },
    /// An error occurred during list or download.
    Error { message: String },
}

/// Spawn an async task to fetch the manifest and report available dictionaries.
pub fn spawn_fetch_manifest(
    client: reqwest::Client,
    dict_dir: PathBuf,
    tx: mpsc::UnboundedSender<DictEvent>,
) {
    tokio::spawn(async move {
        let event = match fetch_manifest(&client).await {
            Ok(manifest) => {
                let mut entries: Vec<DictListEntry> = manifest
                    .dictionaries
                    .into_iter()
                    .map(|(code, info)| {
                        let installed = dict_dir.join(format!("{code}.dic")).exists();
                        DictListEntry {
                            code,
                            name: info.name,
                            installed,
                        }
                    })
                    .collect();
                entries.sort_by(|a, b| a.code.cmp(&b.code));
                DictEvent::ListResult { entries }
            }
            Err(e) => DictEvent::Error {
                message: format!("Failed to fetch dictionary list: {e}"),
            },
        };
        let _ = tx.send(event);
    });
}

/// Spawn an async task to download a single dictionary (`.aff` + `.dic`).
pub fn spawn_download_dict(
    lang: String,
    client: reqwest::Client,
    dict_dir: PathBuf,
    tx: mpsc::UnboundedSender<DictEvent>,
) {
    tokio::spawn(async move {
        let base = crate::constants::DICTS_REPO_URL;
        let event = match download_dict_files(&client, base, &lang, &dict_dir).await {
            Ok(()) => DictEvent::Downloaded { lang },
            Err(e) => DictEvent::Error {
                message: format!("Failed to download {lang}: {e}"),
            },
        };
        let _ = tx.send(event);
    });
}

/// Fetch and parse the remote manifest.
async fn fetch_manifest(client: &reqwest::Client) -> color_eyre::eyre::Result<DictManifest> {
    let url = crate::constants::DICTS_MANIFEST_URL;
    let resp = client.get(url).send().await?.error_for_status()?;
    let manifest: DictManifest = resp.json().await?;
    Ok(manifest)
}

/// Download `.aff` and `.dic` files for a language and write them to `dict_dir`.
async fn download_dict_files(
    client: &reqwest::Client,
    base_url: &str,
    lang: &str,
    dict_dir: &Path,
) -> color_eyre::eyre::Result<()> {
    for ext in &["aff", "dic"] {
        let url = format!("{base_url}/{lang}.{ext}");
        let resp = client.get(&url).send().await?.error_for_status()?;
        let bytes = resp.bytes().await?;
        let path = dict_dir.join(format!("{lang}.{ext}"));
        tokio::fs::write(&path, &bytes).await?;
        tracing::info!(lang = %lang, ext = %ext, bytes = bytes.len(), "dictionary file saved");
    }
    Ok(())
}

/// Strip leading and trailing non-alphanumeric characters from a word.
///
/// Keeps apostrophes (`'`) and hyphens (`-`) that are INSIDE the word
/// (between alphanumeric chars), matching `WeeChat`'s word boundary rules.
/// Returns the stripped word and byte offsets relative to the input.
///
/// Examples:
/// - `"hello!"` → `("hello", 0, 5)`
/// - `"do?"` → `("do", 0, 2)`
/// - `"'test'"` → `("test", 1, 5)`
/// - `"don't"` → `("don't", 0, 5)`
/// - `"--well-known--"` → `("well-known", 2, 12)`
pub fn strip_word_punctuation(word: &str) -> (&str, usize, usize) {
    let bytes = word.as_bytes();
    let len = word.len();

    // Find first alphanumeric char
    let start = word
        .char_indices()
        .find(|(_, c)| c.is_alphanumeric())
        .map_or(len, |(i, _)| i);

    if start >= len {
        return ("", 0, 0);
    }

    // Find last alphanumeric char
    let end = word
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_alphanumeric())
        .map_or(start, |(i, c)| i + c.len_utf8());

    // Safety: start..end are valid char boundaries found by char_indices
    let _ = bytes; // suppress unused warning
    (&word[start..end], start, end)
}

/// Check if a word looks like a URL.
fn is_url(word: &str) -> bool {
    let lower = word.to_lowercase();
    URL_PREFIXES.iter().any(|prefix| lower.starts_with(prefix))
}

/// Check if a string contains only digits and punctuation (no letters).
/// Matches `WeeChat`'s "simili number" detection: `"123"`, `"10:30"`, `"$5.99"`.
fn is_number_like(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_punctuation())
}

/// Load a single Hunspell dictionary from `.aff` + `.dic` files.
fn load_dictionary(lang: &str, dir: &Path) -> color_eyre::eyre::Result<spellbook::Dictionary> {
    let aff_path = dir.join(format!("{lang}.aff"));
    let dic_path = dir.join(format!("{lang}.dic"));

    let aff_content = std::fs::read_to_string(&aff_path)
        .map_err(|e| color_eyre::eyre::eyre!("{}: {e}", aff_path.display()))?;
    let dic_content = std::fs::read_to_string(&dic_path)
        .map_err(|e| color_eyre::eyre::eyre!("{}: {e}", dic_path.display()))?;

    let dict = spellbook::Dictionary::new(&aff_content, &dic_content)
        .map_err(|e| color_eyre::eyre::eyre!("parse error for {lang}: {e}"))?;

    Ok(dict)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_checker_accepts_everything() {
        let checker = SpellChecker { dicts: vec![] };
        assert!(checker.check("anything", &HashSet::new()));
        assert!(checker.check("xyzzy", &HashSet::new()));
    }

    #[test]
    fn short_words_always_accepted() {
        let checker = SpellChecker { dicts: vec![] };
        assert!(checker.check("a", &HashSet::new()));
        assert!(checker.check("", &HashSet::new()));
    }

    #[test]
    fn words_with_digits_skipped() {
        let checker = SpellChecker { dicts: vec![] };
        assert!(checker.check("123", &HashSet::new()));
        assert!(checker.check("10:30", &HashSet::new()));
    }

    #[test]
    fn words_with_underscore_skipped() {
        let checker = SpellChecker { dicts: vec![] };
        assert!(checker.check("foo_bar", &HashSet::new()));
    }

    #[test]
    fn urls_skipped() {
        let checker = SpellChecker { dicts: vec![] };
        assert!(checker.check("https://example.com", &HashSet::new()));
        assert!(checker.check("irc://server", &HashSet::new()));
    }

    #[test]
    fn nicks_skipped() {
        let checker = SpellChecker { dicts: vec![] };
        let nicks: HashSet<String> = ["kofany", "ferris"].iter().map(|s| s.to_string()).collect();
        assert!(checker.check("kofany", &nicks));
        assert!(checker.check("Kofany", &nicks)); // case-insensitive
    }

    #[test]
    fn number_like_detection() {
        assert!(is_number_like("123"));
        assert!(is_number_like("10:30"));
        assert!(is_number_like("$5.99"));
        assert!(!is_number_like("hello"));
        assert!(!is_number_like("test123")); // has letters
        assert!(!is_number_like(""));
    }

    #[test]
    fn strip_punctuation_trailing() {
        let (word, start, end) = strip_word_punctuation("hello!");
        assert_eq!(word, "hello");
        assert_eq!(start, 0);
        assert_eq!(end, 5);
    }

    #[test]
    fn strip_punctuation_question() {
        let (word, _, _) = strip_word_punctuation("do?");
        assert_eq!(word, "do");
    }

    #[test]
    fn strip_punctuation_quotes() {
        let (word, start, end) = strip_word_punctuation("'test'");
        assert_eq!(word, "test");
        assert_eq!(start, 1);
        assert_eq!(end, 5);
    }

    #[test]
    fn strip_punctuation_apostrophe_inside() {
        let (word, _, _) = strip_word_punctuation("don't");
        assert_eq!(word, "don't");
    }

    #[test]
    fn strip_punctuation_hyphen_inside() {
        let (word, start, end) = strip_word_punctuation("--well-known--");
        assert_eq!(word, "well-known");
        assert_eq!(start, 2);
        assert_eq!(end, 12);
    }

    #[test]
    fn strip_punctuation_empty() {
        let (word, _, _) = strip_word_punctuation("...");
        assert_eq!(word, "");
    }

    #[test]
    fn strip_punctuation_clean_word() {
        let (word, start, end) = strip_word_punctuation("hello");
        assert_eq!(word, "hello");
        assert_eq!(start, 0);
        assert_eq!(end, 5);
    }

    #[test]
    fn is_active_empty() {
        let checker = SpellChecker { dicts: vec![] };
        assert!(!checker.is_active());
    }

    #[test]
    fn resolve_dict_dir_default() {
        let path = SpellChecker::resolve_dict_dir("");
        assert!(path.ends_with("dicts"));
    }

    #[test]
    fn resolve_dict_dir_custom() {
        let path = SpellChecker::resolve_dict_dir("/custom/path");
        assert_eq!(path, PathBuf::from("/custom/path"));
    }

    #[test]
    fn load_nonexistent_directory() {
        let checker = SpellChecker::load(
            &["nonexistent_XX".to_string()],
            Path::new("/tmp/repartee_test_no_dicts"),
        );
        assert!(!checker.is_active());
        assert_eq!(checker.dict_count(), 0);
    }

    #[test]
    fn suggest_empty_checker() {
        let checker = SpellChecker { dicts: vec![] };
        let suggestions = checker.suggest("hello");
        assert!(suggestions.is_empty());
    }

    #[test]
    fn url_detection() {
        assert!(is_url("https://example.com"));
        assert!(is_url("HTTP://FOO.BAR"));
        assert!(is_url("ftp://files"));
        assert!(!is_url("hello"));
        assert!(!is_url("httpwhat"));
    }

    #[test]
    fn manifest_deserialize() {
        let json = r#"{
            "version": 1,
            "dictionaries": {
                "en_US": { "name": "English (US)" },
                "pl_PL": { "name": "Polish" }
            }
        }"#;
        let manifest: DictManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.dictionaries.len(), 2);
        assert_eq!(manifest.dictionaries["en_US"].name, "English (US)");
        assert_eq!(manifest.dictionaries["pl_PL"].name, "Polish");
    }

    #[test]
    fn manifest_empty_dictionaries() {
        let json = r#"{ "version": 1, "dictionaries": {} }"#;
        let manifest: DictManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.dictionaries.is_empty());
    }
}

//! Multilingual spell checker backed by `spellbook` (Hunspell-compatible).
//!
//! Loads one `spellbook::Dictionary` per configured language from `.dic`/`.aff`
//! files. A word is considered correct if ANY dictionary accepts it (union check).

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Maximum number of suggestions returned per misspelled word.
const MAX_SUGGESTIONS: usize = 4;

/// Minimum word length to bother spell-checking.
const MIN_WORD_LEN: usize = 2;

/// A loaded language dictionary.
struct LangDict {
    /// Language code (e.g. `en_US`).
    #[allow(dead_code)]
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

    /// Returns true if the word is accepted by at least one dictionary.
    ///
    /// Empty strings, very short words, and words containing digits are
    /// always considered correct to avoid noise.
    pub fn check(&self, word: &str) -> bool {
        if self.dicts.is_empty() || word.len() < MIN_WORD_LEN {
            return true;
        }
        // Skip words with digits or special characters (URLs, nicks, etc.)
        if word
            .chars()
            .any(|c| c.is_ascii_digit() || c == '_' || c == '-')
        {
            return true;
        }
        self.dicts.iter().any(|ld| ld.dict.check(word))
    }

    /// Get spelling suggestions for a misspelled word, merged from all dictionaries.
    ///
    /// Returns up to [`MAX_SUGGESTIONS`] unique suggestions.
    pub fn suggest(&self, word: &str) -> Vec<String> {
        let mut suggestions = Vec::new();
        for ld in &self.dicts {
            ld.dict.suggest(word, &mut suggestions);
        }
        // Deduplicate while preserving order (first occurrence wins).
        let mut seen = Vec::new();
        suggestions.retain(|s| {
            let lower = s.to_lowercase();
            if seen.contains(&lower) {
                false
            } else {
                seen.push(lower);
                true
            }
        });
        suggestions.truncate(MAX_SUGGESTIONS);
        suggestions
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
        assert!(checker.check("anything"));
        assert!(checker.check("xyzzy"));
    }

    #[test]
    fn short_words_always_accepted() {
        let checker = SpellChecker { dicts: vec![] };
        assert!(checker.check("a"));
        assert!(checker.check(""));
    }

    #[test]
    fn words_with_digits_skipped() {
        let checker = SpellChecker { dicts: vec![] };
        assert!(checker.check("test123"));
        assert!(checker.check("h4ck"));
    }

    #[test]
    fn words_with_underscore_or_dash_skipped() {
        let checker = SpellChecker { dicts: vec![] };
        assert!(checker.check("foo_bar"));
        assert!(checker.check("foo-bar"));
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
}

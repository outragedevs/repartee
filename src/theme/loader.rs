use std::collections::HashMap;
use std::path::Path;

use color_eyre::eyre::Result;

use super::{ThemeColors, ThemeFile, ThemeFormats, ThemeMeta};

/// Build the minimal fallback theme (Tokyo Night / Nightfall defaults).
pub fn default_theme() -> ThemeFile {
    ThemeFile {
        meta: ThemeMeta {
            name: "Fallback".to_string(),
            description: "Minimal fallback theme".to_string(),
        },
        colors: ThemeColors::default(),
        abstracts: HashMap::from([
            ("timestamp".into(), "$*".into()),
            ("msgnick".into(), "$0$1> ".into()),
            ("ownnick".into(), "$*".into()),
            ("pubnick".into(), "$*".into()),
        ]),
        formats: ThemeFormats::default(),
    }
}

/// Load a theme file from TOML, merging with defaults for missing sections.
pub fn load_theme(path: &Path) -> Result<ThemeFile> {
    if !path.exists() {
        return Ok(default_theme());
    }
    let content = std::fs::read_to_string(path)?;

    // Parse as a loose TOML Value first, then merge sections
    let parsed: toml::Value = toml::from_str(&content)?;
    let default = default_theme();

    let meta: ThemeMeta = if let Some(meta) = parsed.get("meta") {
        meta.clone().try_into().unwrap_or_else(|e| {
            tracing::warn!("Failed to parse theme [meta]: {e}, using defaults");
            default.meta
        })
    } else {
        default.meta
    };

    let colors: ThemeColors = parsed.get("colors").map_or_else(ThemeColors::default, |v| {
        v.clone().try_into().unwrap_or_else(|e| {
            tracing::warn!("Failed to parse theme [colors]: {e}, using defaults");
            ThemeColors::default()
        })
    });

    let abstracts: HashMap<String, String> = if let Some(abs) = parsed.get("abstracts") {
        match abs.clone().try_into::<HashMap<String, String>>() {
            Ok(user_abs) => {
                let mut merged = default.abstracts;
                merged.extend(user_abs);
                merged
            }
            Err(e) => {
                tracing::warn!("Failed to parse theme [abstracts]: {e}, using defaults");
                default.abstracts
            }
        }
    } else {
        default.abstracts
    };

    let formats: ThemeFormats = if let Some(fmts) = parsed.get("formats") {
        fmts.clone().try_into().unwrap_or_else(|e| {
            tracing::warn!("Failed to parse theme [formats]: {e}, using defaults");
            ThemeFormats::default()
        })
    } else {
        default.formats
    };

    Ok(ThemeFile {
        meta,
        colors,
        abstracts,
        formats,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_has_nightfall_colors() {
        let theme = default_theme();
        assert_eq!(theme.colors.bg, "#1a1b26");
        assert_eq!(theme.colors.accent, "#7aa2f7");
    }

    #[test]
    fn load_theme_missing_file_returns_default() {
        let path = std::path::PathBuf::from("/tmp/nonexistent_theme.theme");
        let theme = load_theme(&path).unwrap();
        assert_eq!(theme.meta.name, "Fallback");
    }

    #[test]
    fn load_kokoirc_default_theme() {
        let path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("themes/default.theme");
        if path.exists() {
            let theme = load_theme(&path).unwrap();
            assert_eq!(theme.meta.name, "Nightfall");
            assert_eq!(theme.colors.bg, "#1a1b26");
            assert!(theme.abstracts.contains_key("timestamp"));
            assert!(theme.formats.messages.contains_key("pubmsg"));
        }
    }
}

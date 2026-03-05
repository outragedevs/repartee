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

    let meta = if let Some(meta) = parsed.get("meta") {
        toml::from_str(&toml::to_string(meta)?).unwrap_or(default.meta)
    } else {
        default.meta
    };

    // For colors: parse, then merge with defaults (individual color fields via serde default)
    let colors: ThemeColors = if let Some(colors_val) = parsed.get("colors") {
        toml::from_str(&toml::to_string(colors_val)?).unwrap_or_default()
    } else {
        ThemeColors::default()
    };

    let abstracts = if let Some(abs) = parsed.get("abstracts") {
        toml::from_str(&toml::to_string(abs)?).unwrap_or(default.abstracts)
    } else {
        default.abstracts
    };

    let formats = if let Some(fmts) = parsed.get("formats") {
        toml::from_str(&toml::to_string(fmts)?).unwrap_or_default()
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

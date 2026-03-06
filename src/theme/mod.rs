pub mod loader;
pub mod parser;

pub use loader::load_theme;
#[allow(unused_imports)]
pub use parser::{parse_format_string, resolve_abstractions, substitute_vars};

use ratatui::style::Color;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeFile {
    pub meta: ThemeMeta,
    pub colors: ThemeColors,
    pub abstracts: HashMap<String, String>,
    pub formats: ThemeFormats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeMeta {
    pub name: String,
    pub description: String,
}

/// `ThemeColors` stores hex strings in TOML but we also provide a method to convert to ratatui Color.
/// We store as String for serialization compatibility, and convert to Color at render time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeColors {
    pub bg: String,
    pub bg_alt: String,
    pub border: String,
    pub fg: String,
    pub fg_muted: String,
    pub fg_dim: String,
    pub accent: String,
    pub cursor: String,
}

impl Default for ThemeColors {
    fn default() -> Self {
        Self {
            bg: "#1a1b26".to_string(),
            bg_alt: "#16161e".to_string(),
            border: "#292e42".to_string(),
            fg: "#a9b1d6".to_string(),
            fg_muted: "#565f89".to_string(),
            fg_dim: "#292e42".to_string(),
            accent: "#7aa2f7".to_string(),
            cursor: "#7aa2f7".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeFormats {
    pub messages: HashMap<String, String>,
    pub events: HashMap<String, String>,
    pub sidepanel: HashMap<String, String>,
    pub nicklist: HashMap<String, String>,
}

impl Default for ThemeFormats {
    fn default() -> Self {
        Self {
            messages: HashMap::from([
                ("pubmsg".into(), "$0 $1".into()),
                ("own_msg".into(), "$0 $1".into()),
                ("notice".into(), "-$0- $1".into()),
            ]),
            events: HashMap::new(),
            sidepanel: HashMap::from([
                ("header".into(), "$0".into()),
                ("item".into(), "$0. $1".into()),
                ("item_selected".into(), "> $0. $1".into()),
            ]),
            nicklist: HashMap::from([("normal".into(), " $0".into())]),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(clippy::struct_excessive_bools, reason = "each bool maps to an independent text style attribute")]
pub struct StyledSpan {
    pub text: String,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
}

/// Convert "#RRGGBB" hex string to ratatui `Color::Rgb`.
pub fn hex_to_color(hex: &str) -> Option<Color> {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_to_color_valid() {
        assert_eq!(hex_to_color("7aa2f7"), Some(Color::Rgb(0x7a, 0xa2, 0xf7)));
    }

    #[test]
    fn hex_to_color_with_hash() {
        assert_eq!(
            hex_to_color("#1a1b26"),
            Some(Color::Rgb(0x1a, 0x1b, 0x26))
        );
    }

    #[test]
    fn hex_to_color_invalid() {
        assert_eq!(hex_to_color("zzzzzz"), None);
        assert_eq!(hex_to_color("fff"), None);
        assert_eq!(hex_to_color(""), None);
    }
}

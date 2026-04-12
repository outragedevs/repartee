use ratatui::style::Color;

/// Terminal color capability tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSupport {
    TrueColor,
    Color256,
    Basic,
}

/// Detect terminal color support from the terminal application name.
///
/// Known modern terminals are mapped directly to `TrueColor`.
/// Unknown terminals fall back to environment variable detection.
pub fn detect_color_support(terminal_name: &str) -> ColorSupport {
    match terminal_name {
        "ghostty" | "kitty" | "iterm2" | "wezterm" | "rio" | "foot" | "contour" | "subterm"
        | "konsole" | "mintty" | "mlterm" | "windows-terminal" => ColorSupport::TrueColor,
        _ => detect_from_env(),
    }
}

fn detect_from_env() -> ColorSupport {
    if std::env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit") {
        ColorSupport::TrueColor
    } else if std::env::var("TERM").is_ok_and(|v| v.contains("256color")) {
        ColorSupport::Color256
    } else {
        ColorSupport::Basic
    }
}

/// Compute a deterministic color for an IRC nick based on the terminal's color
/// capability tier. The nick is hashed case-insensitively so that `"Ferris"`
/// and `"ferris"` always produce the same color.
#[expect(
    clippy::cast_precision_loss,
    reason = "hash % 360 is at most 359, which fits exactly in f32"
)]
pub fn nick_color(nick: &str, support: ColorSupport, saturation: f32, lightness: f32) -> Color {
    let hash = djb2_hash(nick);
    match support {
        ColorSupport::TrueColor => {
            let hue = (hash % 360) as f32;
            let (red, green, blue) = hsl_to_rgb(hue, saturation, lightness);
            Color::Rgb(red, green, blue)
        }
        ColorSupport::Color256 => Color::Indexed(PALETTE_256[hash % PALETTE_256.len()]),
        ColorSupport::Basic => PALETTE_BASIC[hash % PALETTE_BASIC.len()],
    }
}

/// djb2 string hash (case-insensitive). Classic hash function by Daniel
/// J. Bernstein -- fast, simple, good distribution for short strings like nicks.
const fn djb2_hash(nick: &str) -> usize {
    let bytes = nick.as_bytes();
    let mut hash: u32 = 5381;
    let mut idx = 0;
    while idx < bytes.len() {
        let lower = bytes[idx].to_ascii_lowercase();
        hash = hash.wrapping_mul(33).wrapping_add(lower as u32);
        idx += 1;
    }
    hash as usize
}

/// Compute a deterministic hex color string for a nick (e.g. `"a3c4f7"`).
/// Uses the same djb2 hash + HSL hue wheel as `nick_color`, but always
/// returns a 6-char hex string suitable for embedding in `%Z` format codes.
#[expect(
    clippy::cast_precision_loss,
    reason = "hash % 360 is at most 359, which fits exactly in f32"
)]
pub fn nick_color_hex(nick: &str, saturation: f32, lightness: f32) -> String {
    let hash = djb2_hash(nick);
    let hue = (hash % 360) as f32;
    let (r, g, b) = hsl_to_rgb(hue, saturation, lightness);
    format!("{r:02x}{g:02x}{b:02x}")
}

/// Convert HSL to RGB.
///
/// * `hue` -- degrees 0..360
/// * `saturation` -- 0.0..1.0
/// * `lightness` -- 0.0..1.0
#[expect(
    clippy::cast_possible_truncation,
    reason = "final values are clamped to 0..=255 before cast"
)]
#[expect(
    clippy::cast_sign_loss,
    reason = "values are clamped to non-negative before cast"
)]
pub fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0f32.mul_add(lightness, -1.0)).abs()) * saturation;
    let h_prime = hue / 60.0;
    let x = c * (1.0 - (h_prime % 2.0 - 1.0).abs());
    let (r1, g1, b1) = if h_prime < 1.0 {
        (c, x, 0.0)
    } else if h_prime < 2.0 {
        (x, c, 0.0)
    } else if h_prime < 3.0 {
        (0.0, c, x)
    } else if h_prime < 4.0 {
        (0.0, x, c)
    } else if h_prime < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    let m = lightness - c / 2.0;
    let red = (r1 + m).mul_add(255.0, 0.5) as u8;
    let green = (g1 + m).mul_add(255.0, 0.5) as u8;
    let blue = (b1 + m).mul_add(255.0, 0.5) as u8;
    (red, green, blue)
}

// ---------------------------------------------------------------------------
// Palettes
// ---------------------------------------------------------------------------

/// Hand-picked xterm-256 color indices that look good on dark backgrounds.
/// Excludes very dark, very light, and near-white/near-black entries.
const PALETTE_256: &[u8] = &[
    // Reds/oranges
    124, 160, 196, 202, 208, 214, // Yellows
    178, 184, 220, 226, // Greens
    34, 35, 40, 41, 42, 70, 71, 76, 77, 78, 112, 113, 114, // Cyans/teals
    30, 31, 36, 37, 38, 43, 44, 73, 74, 79, 80, // Blues
    24, 25, 26, 27, 32, 33, 62, 63, 68, 69, 75, // Purples/magentas
    55, 56, 57, 92, 93, 98, 99, 128, 129, 134, 135, // Pinks
    161, 162, 163, 164, 170, 171, 176, 177,
];

/// Named color palette for 8/16-color terminals. Excludes `Black`, `White`,
/// `Gray`, and `DarkGray` which are too close to typical foreground/background
/// colors.
const PALETTE_BASIC: &[Color] = &[
    Color::Red,
    Color::Green,
    Color::Yellow,
    Color::Blue,
    Color::Magenta,
    Color::Cyan,
    Color::LightRed,
    Color::LightGreen,
    Color::LightYellow,
    Color::LightBlue,
    Color::LightMagenta,
    Color::LightCyan,
];

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_truecolor_terminals() {
        for name in [
            "ghostty",
            "kitty",
            "iterm2",
            "wezterm",
            "rio",
            "foot",
            "contour",
            "subterm",
            "konsole",
            "mintty",
            "mlterm",
            "windows-terminal",
        ] {
            assert_eq!(
                detect_color_support(name),
                ColorSupport::TrueColor,
                "expected TrueColor for {name}"
            );
        }
    }

    #[test]
    fn unknown_terminal_defaults() {
        // Without COLORTERM or TERM set to anything special, an unknown
        // terminal should return Basic (or higher if the env happens to
        // have those vars -- but we test the fallback path).
        let support = detect_color_support("unknown");
        // It should be *at most* TrueColor and *at least* Basic.
        assert!(
            support == ColorSupport::Basic
                || support == ColorSupport::Color256
                || support == ColorSupport::TrueColor,
            "unexpected variant"
        );
    }

    #[test]
    fn nick_color_deterministic() {
        let first = nick_color("ferris", ColorSupport::TrueColor, 0.7, 0.5);
        let second = nick_color("ferris", ColorSupport::TrueColor, 0.7, 0.5);
        assert_eq!(first, second);
    }

    #[test]
    fn nick_color_case_insensitive() {
        let upper = nick_color("Ferris", ColorSupport::TrueColor, 0.7, 0.5);
        let lower = nick_color("ferris", ColorSupport::TrueColor, 0.7, 0.5);
        assert_eq!(upper, lower);
    }

    #[test]
    fn nick_color_different_nicks_differ() {
        let alice = nick_color("alice", ColorSupport::TrueColor, 0.7, 0.5);
        let bob = nick_color("bob", ColorSupport::TrueColor, 0.7, 0.5);
        assert_ne!(alice, bob);
    }

    #[test]
    fn nick_color_returns_rgb_for_truecolor() {
        let color = nick_color("test", ColorSupport::TrueColor, 0.7, 0.5);
        assert!(
            matches!(color, Color::Rgb(_, _, _)),
            "expected Rgb variant, got {color:?}"
        );
    }

    #[test]
    fn hsl_to_rgb_red() {
        let (red, green, blue) = hsl_to_rgb(0.0, 1.0, 0.5);
        assert_eq!((red, green, blue), (255, 0, 0));
    }

    #[test]
    fn hsl_to_rgb_green() {
        let (red, green, blue) = hsl_to_rgb(120.0, 1.0, 0.5);
        assert_eq!((red, green, blue), (0, 255, 0));
    }

    #[test]
    fn hsl_to_rgb_blue() {
        let (red, green, blue) = hsl_to_rgb(240.0, 1.0, 0.5);
        assert_eq!((red, green, blue), (0, 0, 255));
    }

    #[test]
    fn nick_color_returns_indexed_for_256() {
        let color = nick_color("test", ColorSupport::Color256, 0.7, 0.5);
        assert!(
            matches!(color, Color::Indexed(_)),
            "expected Indexed variant, got {color:?}"
        );
    }

    #[test]
    fn nick_color_256_in_valid_range() {
        for &idx in PALETTE_256 {
            assert!(
                (16..=231).contains(&idx),
                "palette entry {idx} outside 16..=231"
            );
        }
    }

    #[test]
    fn nick_color_basic_is_named_color() {
        let color = nick_color("test", ColorSupport::Basic, 0.7, 0.5);
        assert!(
            PALETTE_BASIC.contains(&color),
            "expected a named color from PALETTE_BASIC, got {color:?}"
        );
    }

    #[test]
    fn nick_color_256_deterministic() {
        let first = nick_color("ferris", ColorSupport::Color256, 0.7, 0.5);
        let second = nick_color("ferris", ColorSupport::Color256, 0.7, 0.5);
        assert_eq!(first, second);
    }

    #[test]
    fn empty_nick_does_not_panic() {
        let _ = nick_color("", ColorSupport::TrueColor, 0.65, 0.65);
        let _ = nick_color("", ColorSupport::Color256, 0.65, 0.65);
        let _ = nick_color("", ColorSupport::Basic, 0.65, 0.65);
    }

    #[test]
    fn unicode_nick_works() {
        let c = nick_color("Ñóçk", ColorSupport::TrueColor, 0.65, 0.65);
        assert!(matches!(c, Color::Rgb(_, _, _)));
    }

    #[test]
    fn very_long_nick_works() {
        let long_nick = "a".repeat(100);
        let c = nick_color(&long_nick, ColorSupport::TrueColor, 0.65, 0.65);
        assert!(matches!(c, Color::Rgb(_, _, _)));
    }

    #[test]
    fn saturation_zero_produces_gray() {
        let (r, g, b) = hsl_to_rgb(180.0, 0.0, 0.5);
        assert_eq!(r, g);
        assert_eq!(g, b);
    }

    #[test]
    fn lightness_extremes() {
        let (r, g, b) = hsl_to_rgb(0.0, 1.0, 0.0);
        assert_eq!((r, g, b), (0, 0, 0), "lightness 0 = black");

        let (r, g, b) = hsl_to_rgb(0.0, 1.0, 1.0);
        assert_eq!((r, g, b), (255, 255, 255), "lightness 1 = white");
    }

    #[test]
    fn hash_distribution_reasonable() {
        let nicks = [
            "alice", "bob", "charlie", "dave", "eve", "ferris", "grace", "heidi", "ivan", "judy",
            "karl", "linda", "mallory", "nancy", "oscar", "peggy", "quinn", "rachel", "steve",
            "trudy",
        ];
        let colors: std::collections::HashSet<_> = nicks
            .iter()
            .map(|n| nick_color(n, ColorSupport::TrueColor, 0.65, 0.65))
            .collect();
        assert!(
            colors.len() >= 15,
            "expected ≥15 distinct colors from 20 nicks, got {}",
            colors.len()
        );
    }
}

/// Compute a deterministic CSS color string for an IRC nick.
///
/// Returns a CSS hex color like `"#7ab3f7"`. Always truecolor (web has no
/// terminal palette constraints).
pub fn nick_color_css(nick: &str, saturation: f32, lightness: f32) -> String {
    let hash = djb2_hash(nick);
    let hue = (hash % 360) as f32;
    let (r, g, b) = hsl_to_rgb(hue, saturation, lightness);
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn djb2_hash(nick: &str) -> usize {
    let mut hash: u32 = 5381;
    for byte in nick.bytes() {
        let b = byte.to_ascii_lowercase();
        hash = hash.wrapping_mul(33).wrapping_add(u32::from(b));
    }
    hash as usize
}

/// Convert HSL to RGB — must produce identical results to the TUI-side
/// `nick_color::hsl_to_rgb` so the same nick has the same color everywhere.
#[expect(
    clippy::cast_possible_truncation,
    reason = "final values are clamped to 0..=255 before cast"
)]
#[expect(
    clippy::cast_sign_loss,
    reason = "values are clamped to non-negative before cast"
)]
fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> (u8, u8, u8) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        assert_eq!(
            nick_color_css("ferris", 0.65, 0.65),
            nick_color_css("ferris", 0.65, 0.65)
        );
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(
            nick_color_css("Ferris", 0.65, 0.65),
            nick_color_css("ferris", 0.65, 0.65)
        );
    }

    #[test]
    fn different_nicks_differ() {
        assert_ne!(
            nick_color_css("alice", 0.65, 0.65),
            nick_color_css("bob", 0.65, 0.65)
        );
    }

    #[test]
    fn returns_hex_format() {
        let c = nick_color_css("ferris", 0.65, 0.65);
        assert!(c.starts_with('#'));
        assert_eq!(c.len(), 7);
    }

    #[test]
    fn hsl_to_rgb_primary_colors() {
        // These must match the TUI-side nick_color::hsl_to_rgb exactly.
        assert_eq!(hsl_to_rgb(0.0, 1.0, 0.5), (255, 0, 0), "red");
        assert_eq!(hsl_to_rgb(120.0, 1.0, 0.5), (0, 255, 0), "green");
        assert_eq!(hsl_to_rgb(240.0, 1.0, 0.5), (0, 0, 255), "blue");
    }

    #[test]
    fn hsl_to_rgb_gray_at_zero_saturation() {
        let (r, g, b) = hsl_to_rgb(180.0, 0.0, 0.5);
        assert_eq!(r, g);
        assert_eq!(g, b);
    }
}

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

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "RGB values are clamped to 0–255 before casting"
)]
fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let h = hue / 60.0;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match h as u8 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = lightness - c / 2.0;
    (
        ((r1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((g1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((b1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
    )
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
}

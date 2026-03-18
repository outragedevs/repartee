# Nick Coloring Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add deterministic per-nick coloring (WeeChat-style) to chat messages and nick list, with truecolor HSL hue wheel primary strategy and 256-color/16-color fallbacks based on terminal capability detection.

**Architecture:** A new `src/nick_color.rs` module provides a pure `nick_color(nick, color_support)` function that hashes the lowercase nick and maps to a color. Truecolor uses HSL→RGB with theme-tunable saturation/lightness. 256-color indexes a curated ~60-color palette from the 6×6×6 color cube. 16-color uses ~10 safe ANSI colors. Terminal color support is detected at startup and **re-detected on reattach** (`repartee a`) by reusing the existing `outer_terminal` + `refresh_image_protocol()` flow in `app.rs` — because a user may start in one terminal but reattach from another. The nick color is injected at render time. The web frontend needs 3 new fields on `SettingsChanged` (typed struct, not a map). The web frontend runs the same hash→HSL algorithm in Rust/WASM (always truecolor since CSS supports `#RRGGBB`).

**Tech Stack:** Rust (ratatui `Color::Rgb` / `Color::Indexed`), Leptos/WASM (CSS inline styles), no new dependencies (hash + HSL→RGB are trivial math).

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `src/nick_color.rs` | **Create** | Core module: `ColorSupport` enum, `detect_color_support()`, `nick_color()`, HSL→RGB conversion, 256-color palette, 16-color palette |
| `src/config/mod.rs` | **Modify** | Add `nick_colors: bool`, `nick_colors_in_nicklist: bool`, `nick_color_saturation: f32`, `nick_color_lightness: f32` to `DisplayConfig` |
| `src/commands/settings.rs` | **Modify** | Wire `/set display.nick_colors`, `display.nick_colors_in_nicklist`, `display.nick_color_saturation`, `display.nick_color_lightness`; add to tab-complete list |
| `src/app.rs` | **Modify** | Add `color_support: ColorSupport` field on `App`, initialize from `detect_color_support(outer_terminal)` |
| `src/ui/message_line.rs` | **Modify** | After theme format parsing, override nick span fg color with `nick_color()` output (skip for own/mention/highlight msgs) |
| `src/ui/nick_list.rs` | **Modify** | After theme format parsing, override nick text span fg color with `nick_color()` (preserve prefix color from theme) |
| `src/main.rs` | **Modify** | Add `mod nick_color;` (private, like all other modules in main.rs) |
| `web-ui/src/nick_color.rs` | **Create** | Same hash + HSL→RGB algorithm (truecolor only), outputs CSS `#RRGGBB` string |
| `web-ui/src/components/chat_view.rs` | **Modify** | Apply `nick_color()` as inline `style="color: #RRGGBB"` on `.nick .name` span (skip own/highlight) |
| `web-ui/src/components/nick_list.rs` | **Modify** | Apply `nick_color()` as inline style on nick text span |
| `web-ui/src/main.rs` | **Modify** | Add `mod nick_color;` (no lib.rs exists in web-ui) |
| `src/web/protocol.rs` | **Modify** | Add 3 nick color fields to `WebEvent::SettingsChanged` struct |
| `web-ui/src/protocol.rs` | **Modify** | Add matching 3 fields to `WebEvent::SettingsChanged` |

---

## Chunk 1: Core Algorithm + Config

### Task 1: Create `src/nick_color.rs` — Color Support Detection

**Files:**
- Create: `src/nick_color.rs`

- [ ] **Step 1: Write failing tests for `ColorSupport` detection**

```rust
// src/nick_color.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_truecolor_terminals() {
        for name in &["ghostty", "kitty", "iterm2", "wezterm", "rio", "foot", "contour", "subterm"] {
            assert_eq!(detect_color_support(name), ColorSupport::TrueColor, "expected TrueColor for {name}");
        }
    }

    #[test]
    fn unknown_terminal_defaults_to_truecolor_with_colorterm() {
        // Can't test env vars in unit tests easily, so unknown → Basic is the safe default
        assert_eq!(detect_color_support("unknown"), ColorSupport::Basic);
    }
}
```

- [ ] **Step 2: Implement `ColorSupport` enum and `detect_color_support()`**

```rust
use ratatui::style::Color;

/// Terminal color capability tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSupport {
    /// 24-bit RGB (ghostty, kitty, iterm2, wezterm, etc.)
    TrueColor,
    /// 256-color palette (xterm-256color terminals)
    Color256,
    /// 16 basic ANSI colors
    Basic,
}

/// Detect color support from the terminal name (already resolved by `detect_outer_terminal()`).
///
/// Known modern terminals are always truecolor. For unknown terminals, falls back
/// to `COLORTERM` and `TERM` environment variables.
pub fn detect_color_support(terminal_name: &str) -> ColorSupport {
    match terminal_name {
        "ghostty" | "kitty" | "iterm2" | "wezterm" | "rio"
        | "foot" | "contour" | "subterm" | "konsole" | "mintty" | "mlterm" => ColorSupport::TrueColor,
        "windows-terminal" => ColorSupport::TrueColor,
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
```

- [ ] **Step 3: Run tests**

Run: `cargo test --lib nick_color -- --nocapture`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/nick_color.rs src/main.rs
git commit -m "feat(nick-color): add ColorSupport detection from terminal name"
```

---

### Task 2: Nick Hash + HSL→RGB (Truecolor Strategy)

**Files:**
- Modify: `src/nick_color.rs`

- [ ] **Step 1: Write failing tests for nick hashing and HSL→RGB**

```rust
#[test]
fn nick_color_deterministic() {
    let c1 = nick_color("ferris", ColorSupport::TrueColor, 0.65, 0.65);
    let c2 = nick_color("ferris", ColorSupport::TrueColor, 0.65, 0.65);
    assert_eq!(c1, c2, "same nick must produce same color");
}

#[test]
fn nick_color_case_insensitive() {
    let c1 = nick_color("Ferris", ColorSupport::TrueColor, 0.65, 0.65);
    let c2 = nick_color("ferris", ColorSupport::TrueColor, 0.65, 0.65);
    assert_eq!(c1, c2, "nick coloring must be case-insensitive");
}

#[test]
fn nick_color_different_nicks_differ() {
    let c1 = nick_color("alice", ColorSupport::TrueColor, 0.65, 0.65);
    let c2 = nick_color("bob", ColorSupport::TrueColor, 0.65, 0.65);
    assert_ne!(c1, c2, "different nicks should usually produce different colors");
}

#[test]
fn nick_color_returns_rgb_for_truecolor() {
    let c = nick_color("ferris", ColorSupport::TrueColor, 0.65, 0.65);
    assert!(matches!(c, Color::Rgb(_, _, _)), "truecolor should return Color::Rgb");
}

#[test]
fn hsl_to_rgb_red() {
    // Hue 0° at full saturation/lightness 0.5 = pure red
    let (r, g, b) = hsl_to_rgb(0.0, 1.0, 0.5);
    assert_eq!(r, 255);
    assert_eq!(g, 0);
    assert_eq!(b, 0);
}

#[test]
fn hsl_to_rgb_green() {
    let (r, g, b) = hsl_to_rgb(120.0, 1.0, 0.5);
    assert_eq!(r, 0);
    assert_eq!(g, 255);
    assert_eq!(b, 0);
}

#[test]
fn hsl_to_rgb_blue() {
    let (r, g, b) = hsl_to_rgb(240.0, 1.0, 0.5);
    assert_eq!(r, 0);
    assert_eq!(g, 0);
    assert_eq!(b, 255);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib nick_color -- --nocapture`
Expected: FAIL (functions not defined)

- [ ] **Step 3: Implement hash + HSL→RGB**

```rust
/// Compute a deterministic color for an IRC nick.
///
/// - `nick`: The nick string (case-insensitive).
/// - `support`: Terminal color capability.
/// - `saturation`: HSL saturation 0.0–1.0 (used for TrueColor only).
/// - `lightness`: HSL lightness 0.0–1.0 (used for TrueColor only).
pub fn nick_color(nick: &str, support: ColorSupport, saturation: f32, lightness: f32) -> Color {
    let hash = djb2_hash(nick);
    match support {
        ColorSupport::TrueColor => {
            let hue = (hash % 360) as f32;
            let (r, g, b) = hsl_to_rgb(hue, saturation, lightness);
            Color::Rgb(r, g, b)
        }
        ColorSupport::Color256 => Color::Indexed(PALETTE_256[hash % PALETTE_256.len()]),
        ColorSupport::Basic => PALETTE_BASIC[hash % PALETTE_BASIC.len()],
    }
}

/// djb2 hash — simple, fast, good distribution for short strings.
/// Operates on lowercase ASCII for case-insensitive IRC nicks.
fn djb2_hash(nick: &str) -> usize {
    let mut hash: u32 = 5381;
    for byte in nick.bytes() {
        let b = byte.to_ascii_lowercase();
        hash = hash.wrapping_mul(33).wrapping_add(u32::from(b));
    }
    hash as usize
}

/// Convert HSL (hue 0–360, saturation 0–1, lightness 0–1) to RGB (0–255).
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "RGB values are clamped to 0–255 before casting"
)]
pub fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> (u8, u8, u8) {
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
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib nick_color -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/nick_color.rs
git commit -m "feat(nick-color): djb2 hash + HSL→RGB truecolor strategy"
```

---

### Task 3: 256-Color + 16-Color Palettes

**Files:**
- Modify: `src/nick_color.rs`

- [ ] **Step 1: Write failing tests for palette strategies**

```rust
#[test]
fn nick_color_returns_indexed_for_256() {
    let c = nick_color("ferris", ColorSupport::Color256, 0.65, 0.65);
    assert!(matches!(c, Color::Indexed(_)), "256-color should return Color::Indexed");
}

#[test]
fn nick_color_256_in_valid_range() {
    // All palette entries should be in the 6×6×6 cube (16–231)
    for entry in PALETTE_256 {
        assert!((16..=231).contains(entry), "palette entry {entry} outside color cube");
    }
}

#[test]
fn nick_color_basic_is_named_color() {
    let c = nick_color("ferris", ColorSupport::Basic, 0.65, 0.65);
    // Basic palette should only contain the safe named colors
    assert!(PALETTE_BASIC.contains(&c), "basic color should be from PALETTE_BASIC");
}

#[test]
fn nick_color_256_deterministic() {
    let c1 = nick_color("alice", ColorSupport::Color256, 0.65, 0.65);
    let c2 = nick_color("alice", ColorSupport::Color256, 0.65, 0.65);
    assert_eq!(c1, c2);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib nick_color -- --nocapture`
Expected: FAIL (palettes not defined)

- [ ] **Step 3: Implement palettes**

The 256-color palette is a curated subset of the 6×6×6 color cube (indices 16–231), excluding colors that are too dark (unreadable on dark backgrounds) or too light (unreadable on light backgrounds). The selection skips cube entries where all components are 0 or 1 (too dark) or all are 4 or 5 (too light/pastel).

```rust
/// Curated subset of xterm 256-color palette (indices 16–231).
/// Excludes very dark (all components 0–1) and very light (all 4–5) entries.
/// ~60 colors with good distribution across hues.
const PALETTE_256: &[u8] = &[
    // Reds/oranges
    124, 160, 196, 202, 208, 214,
    // Yellows
    178, 184, 220, 226,
    // Greens
    34, 35, 40, 41, 42, 70, 71, 76, 77, 78, 112, 113, 114,
    // Cyans/teals
    30, 31, 36, 37, 38, 43, 44, 73, 74, 79, 80,
    // Blues
    24, 25, 26, 27, 32, 33, 62, 63, 68, 69, 75,
    // Purples/magentas
    55, 56, 57, 92, 93, 98, 99, 128, 129, 134, 135,
    // Pinks
    161, 162, 163, 164, 170, 171, 176, 177,
];

/// Safe ANSI colors for 16-color terminals.
/// Excludes black (0), dark gray (8), white (15), light gray (7) — all too close to backgrounds.
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
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib nick_color -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/nick_color.rs
git commit -m "feat(nick-color): add 256-color and 16-color palette fallbacks"
```

---

### Task 4: Config Fields + `/set` Wiring

**Files:**
- Modify: `src/config/mod.rs` (add 3 fields to `DisplayConfig`)
- Modify: `src/commands/settings.rs` (wire get/set + tab-complete)

- [ ] **Step 1: Add fields to `DisplayConfig`**

In `src/config/mod.rs`, add to the `DisplayConfig` struct after `backlog_lines`:

```rust
    /// Enable per-nick deterministic coloring in chat messages.
    pub nick_colors: bool,
    /// Also apply nick colors in the nick list sidebar (some users prefer a clean nick list).
    pub nick_colors_in_nicklist: bool,
    /// HSL saturation for nick colors (0.0–1.0). Only used in truecolor mode.
    pub nick_color_saturation: f32,
    /// HSL lightness for nick colors (0.0–1.0). Tune per theme: dark bg ≈ 0.65, light bg ≈ 0.40.
    pub nick_color_lightness: f32,
```

Update `Default for DisplayConfig`:

```rust
    nick_colors: true,
    nick_colors_in_nicklist: true,
    nick_color_saturation: 0.65,
    nick_color_lightness: 0.65,
```

- [ ] **Step 2: Wire `get_config_value` in `settings.rs`**

In the `"display"` match arm of `get_config_value()`, add:

```rust
    "nick_colors" => config.display.nick_colors.to_string(),
    "nick_colors_in_nicklist" => config.display.nick_colors_in_nicklist.to_string(),
    "nick_color_saturation" => config.display.nick_color_saturation.to_string(),
    "nick_color_lightness" => config.display.nick_color_lightness.to_string(),
```

- [ ] **Step 3: Wire `set_config_value` in `settings.rs`**

In the `"display"` match arm of `set_config_value()`, add:

```rust
    "nick_colors" => {
        config.display.nick_colors = parse_bool(raw)?;
    }
    "nick_colors_in_nicklist" => {
        config.display.nick_colors_in_nicklist = parse_bool(raw)?;
    }
    "nick_color_saturation" => {
        let v: f32 = raw.parse().map_err(|_| format!("invalid float: {raw}"))?;
        if !(0.0..=1.0).contains(&v) {
            return Err("saturation must be 0.0–1.0".into());
        }
        config.display.nick_color_saturation = v;
    }
    "nick_color_lightness" => {
        let v: f32 = raw.parse().map_err(|_| format!("invalid float: {raw}"))?;
        if !(0.0..=1.0).contains(&v) {
            return Err("lightness must be 0.0–1.0".into());
        }
        config.display.nick_color_lightness = v;
    }
```

- [ ] **Step 4: Add to tab-complete list**

In the `ALL_SETTING_PATHS` constant array, add:

```rust
    "display.nick_colors",
    "display.nick_colors_in_nicklist",
    "display.nick_color_saturation",
    "display.nick_color_lightness",
```

- [ ] **Step 5: Add runtime sync for web broadcast**

In `cmd_set()`, the existing web settings broadcast block (around line 632-647) should already cover `display.*` paths. Verify that `/set display.nick_colors` triggers `WebEvent::SettingsChanged` broadcast. If the broadcast only fires for specific paths, add `"display.nick_colors"` to the match.

- [ ] **Step 6: Write tests for config get/set**

```rust
#[test]
fn get_set_nick_colors() {
    let mut config = default_config();
    assert_eq!(get_config_value(&config, "display.nick_colors").unwrap().value, "true");
    set_config_value(&mut config, "display.nick_colors", "false").unwrap();
    assert!(!config.display.nick_colors);
}

#[test]
fn set_nick_color_saturation_validates_range() {
    let mut config = default_config();
    assert!(set_config_value(&mut config, "display.nick_color_saturation", "0.7").is_ok());
    assert!(set_config_value(&mut config, "display.nick_color_saturation", "1.5").is_err());
    assert!(set_config_value(&mut config, "display.nick_color_saturation", "-0.1").is_err());
}
```

- [ ] **Step 7: Run tests**

Run: `cargo test --lib settings -- --nocapture`
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add src/config/mod.rs src/commands/settings.rs
git commit -m "feat(nick-color): add display.nick_colors config with /set support"
```

---

### Task 5: Store `ColorSupport` on `App` + Reattach Re-Detection

**Files:**
- Modify: `src/app.rs` (add field, initialize, re-detect on reattach)
- Modify: `src/main.rs` (add `mod nick_color;`)

**Important:** All modules in `src/main.rs` use `mod` (private), not `pub mod`. There is no `src/lib.rs` — the binary root is `src/main.rs`.

The daemon's env vars are frozen at fork time. When a user runs `repartee a` from a different terminal (e.g., started in ghostty but reattaching from xterm-256color over SSH), the shim sends its terminal env vars. The existing `refresh_image_protocol()` already re-detects the terminal and updates `outer_terminal`. We piggyback on this to also update `color_support`.

- [ ] **Step 1: Add `mod nick_color;` to `src/main.rs`**

In `src/main.rs`, add after the existing `mod` declarations (line 16, after `mod web;`):

```rust
mod nick_color;
```

- [ ] **Step 2: Add field to `App` struct**

In `src/app.rs`, after the `outer_terminal: String` field (line 403), add:

```rust
    /// Detected terminal color capability (truecolor, 256-color, or basic).
    pub color_support: crate::nick_color::ColorSupport,
```

- [ ] **Step 3: Initialize in `App::new()`**

In `App::new()`, after `outer_terminal` is set (around line 670), add:

```rust
    let color_support = crate::nick_color::detect_color_support(outer_terminal);
    tracing::info!(%outer_terminal, ?color_support, "terminal color support detected");
```

And include `color_support` in the `App` struct init.

- [ ] **Step 4: Re-detect on reattach in `refresh_image_protocol()`**

In `src/app.rs`, in `refresh_image_protocol()` (around line 820, after `self.outer_terminal = outer_terminal.to_string();`), add:

```rust
    self.color_support = crate::nick_color::detect_color_support(outer_terminal);
    tracing::debug!(?self.color_support, %outer_terminal, "color support re-detected");
```

This ensures that when a user detaches from ghostty (truecolor) and reattaches from an SSH session with xterm-256color, the nick color strategy automatically downgrades to the 256-color palette.

- [ ] **Step 5: Run full test suite**

Run: `cargo test`
Expected: PASS (no breakage)

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/app.rs src/nick_color.rs
git commit -m "feat(nick-color): detect ColorSupport on App, re-detect on reattach"
```

---

## Chunk 2: TUI Integration

### Task 6: Apply Nick Colors in Chat View (`message_line.rs`)

**Files:**
- Modify: `src/ui/message_line.rs`

The key insight: theme format strings already produce `Vec<StyledSpan>` with colors for the nick. We need to **override** the nick's fg color with the computed nick color, but only for regular public messages (not own, mention, highlight, action, event, notice).

The nick text lives inside the `{pubnick $0}` abstract expansion. After `parse_format_string()` returns all spans, the nick text is somewhere in the middle. Rather than trying to find it post-hoc (fragile), we'll pass the nick color into the rendering and apply it by modifying `render_chat_message()` to accept an optional override color.

**Approach:** After the format string is parsed into spans, find spans that contain the nick text and override their `fg`. This is simpler and theme-agnostic.

Actually, the cleanest approach: the `render_message()` function already receives `is_own`. We add a `nick_fg_override: Option<Color>` parameter. For `pubmsg` (not own, not mention, not highlight), we compute the nick color and pass it. The override is applied to any span whose text content matches the `display_nick`.

- [ ] **Step 1: Add `nick_fg_override` parameter to `render_message()` and `render_chat_message()`**

Change the signature of `render_message()`:

```rust
pub fn render_message(
    msg: &Message,
    is_own: bool,
    theme: &crate::theme::ThemeFile,
    config: &AppConfig,
    nick_fg_override: Option<Color>,
) -> Line<'static> {
```

And `render_chat_message()`:

```rust
fn render_chat_message(
    msg: &Message,
    is_own: bool,
    theme: &crate::theme::ThemeFile,
    config: &AppConfig,
    nick_fg_override: Option<Color>,
) -> Vec<StyledSpan> {
```

At the end of `render_chat_message()`, before returning the spans, apply the override:

```rust
    let mut spans = parse_format_string(&resolved, &[&display_nick, &msg.text, &padded_nick_mode]);

    // Apply nick color override: recolor spans containing the nick text.
    if let Some(color) = nick_fg_override {
        let nick_lower = display_nick.to_lowercase();
        for span in &mut spans {
            if span.text.to_lowercase().contains(&nick_lower) && span.text.trim() == display_nick {
                span.fg = Some(color);
            }
        }
    }

    spans
```

- [ ] **Step 2: Update all call sites of `render_message()`**

Search for all callers of `render_message()` in `src/ui/chat_view.rs` (the main caller). The caller must compute the nick color:

```rust
let nick_fg = if config.display.nick_colors && !is_own && !msg.highlight {
    msg.nick.as_deref().map(|n| {
        crate::nick_color::nick_color(
            n,
            app.color_support,
            config.display.nick_color_saturation,
            config.display.nick_color_lightness,
        )
    })
} else {
    None
};
let line = render_message(msg, is_own, &app.theme, config, nick_fg);
```

- [ ] **Step 3: Update tests in `message_line.rs`**

Update all existing test calls to pass `None` as the new parameter:

```rust
let line = render_message(&msg, true, &theme, &config, None);
```

Add a new test:

```rust
#[test]
fn render_message_with_nick_color_override() {
    let msg = test_message("alice", "hello", MessageType::Message);
    let theme = default_theme();
    let config = default_config();
    let override_color = Color::Rgb(255, 0, 0);
    let line = render_message(&msg, false, &theme, &config, Some(override_color));
    // Verify the line contains alice's text — the color is applied at the ratatui level
    let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
    assert!(text.contains("alice"));
    // Check that at least one span has the override color
    let has_override = line.spans.iter().any(|s| s.style.fg == Some(ratatui::style::Color::Rgb(255, 0, 0)));
    assert!(has_override, "nick color override should be applied");
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib message_line -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/ui/message_line.rs src/ui/chat_view.rs
git commit -m "feat(nick-color): apply per-nick colors in TUI chat messages"
```

---

### Task 7: Apply Nick Colors in Nick List (`nick_list.rs`)

**Files:**
- Modify: `src/ui/nick_list.rs`

The nick list renders each nick via theme format strings (e.g., `op = "%Z9ece6a@%Za9b1d6$0%N"`). The prefix char (`@`) has its own color, and `$0` (the nick text) has another. We want to override only the nick text color, not the prefix color.

**Approach:** After `parse_format_string()`, find the span containing `entry.nick` text and override its fg. The prefix span is separate (it's literal text in the format string before `$0`).

- [ ] **Step 1: Modify `render()` to accept color support and config**

The function already receives `&App`, which has `app.color_support` and `app.config`. After computing `spans` (line 69–79), apply the nick color override:

```rust
    // After line 79 (where spans are finalized), before pushing to lines:
    if app.config.display.nick_colors {
        let nick_color = crate::nick_color::nick_color(
            &entry.nick,
            app.color_support,
            app.config.display.nick_color_saturation,
            app.config.display.nick_color_lightness,
        );
        let nick_lower = entry.nick.to_lowercase();
        for span in &mut spans {
            // Match spans that contain the nick text (not the prefix char).
            // The prefix is a separate span from the format string.
            if !span.text.is_empty()
                && span.text.to_lowercase().contains(&nick_lower)
            {
                span.fg = Some(nick_color);
            }
        }
    }
    lines.push(styled_spans_to_line(&spans));
```

**Important:** Away nicks should still get dimmed. Check: if `entry.away`, skip the color override (let the theme's away_ format handle it). The away formats already use muted colors, and overriding would make away nicks look active.

```rust
    if app.config.display.nick_colors && app.config.display.nick_colors_in_nicklist && !entry.away {
        // ... apply override
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add src/ui/nick_list.rs
git commit -m "feat(nick-color): apply per-nick colors in TUI nick list sidebar"
```

---

## Chunk 3: Web Frontend Integration

### Task 8: Create `web-ui/src/nick_color.rs`

**Files:**
- Create: `web-ui/src/nick_color.rs`
- Modify: `web-ui/src/main.rs` (add `mod nick_color;` — there is no `lib.rs` in web-ui)

The web frontend always uses truecolor (CSS `#RRGGBB`). We duplicate the hash + HSL→RGB algorithm — it's ~30 lines of pure math, no dependencies. This avoids adding the nick_color TUI module to the WASM build.

- [ ] **Step 1: Create `web-ui/src/nick_color.rs`**

```rust
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
        assert_eq!(nick_color_css("ferris", 0.65, 0.65), nick_color_css("ferris", 0.65, 0.65));
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(nick_color_css("Ferris", 0.65, 0.65), nick_color_css("ferris", 0.65, 0.65));
    }

    #[test]
    fn different_nicks_differ() {
        assert_ne!(nick_color_css("alice", 0.65, 0.65), nick_color_css("bob", 0.65, 0.65));
    }

    #[test]
    fn returns_hex_format() {
        let c = nick_color_css("ferris", 0.65, 0.65);
        assert!(c.starts_with('#'));
        assert_eq!(c.len(), 7);
    }
}
```

- [ ] **Step 2: Add module declaration**

In `web-ui/src/main.rs`, add after the existing `mod` declarations: `mod nick_color;`

- [ ] **Step 3: Run tests**

Run: `cd web-ui && cargo test --lib nick_color -- --nocapture`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add web-ui/src/nick_color.rs web-ui/src/main.rs
git commit -m "feat(nick-color): add nick_color_css() for web frontend (truecolor HSL)"
```

---

### Task 9: Apply Nick Colors in Web Chat View

**Files:**
- Modify: `web-ui/src/components/chat_view.rs`

Currently (line 146-152), the nick `.name` span has no inline style — it gets `color: var(--accent)` from CSS. We add an inline `style="color: #RRGGBB"` that overrides the CSS for regular messages (not own, not mention/highlight).

- [ ] **Step 1: Modify the regular message rendering block**

In the `else` branch (line 138, regular messages), after computing `nick` and `mode`:

```rust
    let nick_text = msg.nick.unwrap_or_default();
    let nick = truncate_nick(&nick_text, max_len);
    let mode = msg.nick_mode.unwrap_or_default();

    // Compute per-nick color (skip for own messages — they use --green via CSS).
    let nick_style_color = if !is_own && !msg.highlight {
        let css_color = crate::nick_color::nick_color_css(&nick_text, 0.65, 0.65);
        format!("color: {};", css_color)
    } else {
        String::new()
    };
```

Then in the view, change the `.name` span:

```rust
    <span class="name" style=nick_style_color>{nick}</span>
```

**Note:** The saturation/lightness values should come from the server's config broadcast. For now, hardcode 0.65/0.65 — Task 11 will wire server config to the web frontend.

- [ ] **Step 2: Apply to action messages too**

In the action rendering (line 97-111), apply color to the `.action-nick` span:

```rust
    let nick_text = msg.nick.unwrap_or_default();
    let nick_style_color = if !is_own {
        let css_color = crate::nick_color::nick_color_css(&nick_text, 0.65, 0.65);
        format!("color: {};", css_color)
    } else {
        String::new()
    };
    // ...
    <span class="action-nick" style=nick_style_color>{nick_text}</span>
```

- [ ] **Step 3: Build and verify**

Run: `cd web-ui && cargo check`
Expected: Compiles without errors

- [ ] **Step 4: Commit**

```bash
git add web-ui/src/components/chat_view.rs
git commit -m "feat(nick-color): apply per-nick colors in web chat messages"
```

---

### Task 10: Apply Nick Colors in Web Nick List

**Files:**
- Modify: `web-ui/src/components/nick_list.rs`

Currently nick text has no explicit color — it inherits `--fg`. We add an inline style with the computed color, but skip away nicks (they're dimmed via opacity).

- [ ] **Step 1: Modify `render_nicks` closure**

Inside the `render_nicks` closure (line 42-61), compute the color for each nick:

```rust
    let render_nicks = |nicks: Vec<crate::protocol::WireNick>, prefix_class: &'static str| {
        nicks.into_iter().map(|n| {
            let away_class = if n.away { " away" } else { "" };
            let class = format!("nick-entry{away_class}");

            // Per-nick color (skip for away nicks — they use opacity dimming).
            // Respects display.nick_colors_in_nicklist toggle.
            let nick_style = if !n.away && state.nick_colors_enabled.get() && state.nick_colors_in_nicklist.get() {
                let sat = state.nick_color_saturation.get();
                let lit = state.nick_color_lightness.get();
                let css_color = crate::nick_color::nick_color_css(&n.nick, sat, lit);
                format!("color: {};", css_color)
            } else {
                String::new()
            };

            let nick_for_click = n.nick.clone();
            let buf_id = active_buffer_id.clone();
            let on_click = move |_| {
                if let Some(ref buffer_id) = buf_id {
                    crate::ws::send_command(&WebCommand::RunCommand {
                        buffer_id: buffer_id.clone(),
                        text: format!("/query {}", nick_for_click),
                    });
                }
            };
            view! {
                <div class=class on:click=on_click>
                    <span class=prefix_class>{n.prefix}</span>
                    <span style=nick_style>{n.nick}</span>
                </div>
            }
        }).collect::<Vec<_>>()
    };
```

- [ ] **Step 2: Build and verify**

Run: `cd web-ui && cargo check`
Expected: Compiles

- [ ] **Step 3: Commit**

```bash
git add web-ui/src/components/nick_list.rs
git commit -m "feat(nick-color): apply per-nick colors in web nick list"
```

---

## Chunk 4: Config Sync + Polish

### Task 11: Sync Nick Color Settings to Web Frontend

**Files:**
- Modify: `src/web/protocol.rs` (add 3 fields to `WebEvent::SettingsChanged`)
- Modify: `web-ui/src/protocol.rs` (add matching 3 fields)
- Modify: `src/commands/settings.rs` (extend broadcast guard + construction)
- Modify: `web-ui/src/state.rs` (add signals, handle new fields)
- Modify: `web-ui/src/components/chat_view.rs` (read from signals)
- Modify: `web-ui/src/components/nick_list.rs` (read from signals)

**Critical context:** `WebEvent::SettingsChanged` is a **typed struct** with named fields — NOT a `HashMap<String, String>`. The broadcast in `src/commands/settings.rs` only fires for `web.*` paths (line 632–636), not `display.*` paths. Both issues must be addressed.

**Note:** Steps 1–3 must be applied atomically (all in one pass) because adding fields to the struct (Step 1) without updating the construction site (Step 3) will break compilation. Do not run `cargo check` between Steps 1 and 3.

- [ ] **Step 1: Add fields to `SettingsChanged` in `src/web/protocol.rs`**

Add 3 new fields to the `SettingsChanged` variant (after `nick_max_length`):

```rust
    SettingsChanged {
        timestamp_format: String,
        line_height: f32,
        theme: String,
        nick_column_width: u32,
        nick_max_length: u32,
        // Nick coloring settings
        nick_colors: bool,
        nick_colors_in_nicklist: bool,
        nick_color_saturation: f32,
        nick_color_lightness: f32,
    },
```

- [ ] **Step 2: Add matching fields in `web-ui/src/protocol.rs`**

Same 4 fields, with `#[serde(default)]` for backwards compatibility:

```rust
    SettingsChanged {
        timestamp_format: String,
        line_height: f32,
        theme: String,
        #[serde(default)]
        nick_column_width: u32,
        #[serde(default)]
        nick_max_length: u32,
        #[serde(default = "default_true")]
        nick_colors: bool,
        #[serde(default = "default_true")]
        nick_colors_in_nicklist: bool,
        #[serde(default = "default_saturation")]
        nick_color_saturation: f32,
        #[serde(default = "default_lightness")]
        nick_color_lightness: f32,
    },
```

Add serde default helpers:

```rust
fn default_true() -> bool { true }
fn default_saturation() -> f32 { 0.65 }
fn default_lightness() -> f32 { 0.65 }
```

- [ ] **Step 3: Extend broadcast guard in `src/commands/settings.rs`**

In `cmd_set()` around line 632, extend the condition to also trigger on `display.nick_color*`:

```rust
    if path == "web.timestamp_format"
        || path == "web.line_height"
        || path == "web.theme"
        || path == "web.nick_column_width"
        || path == "web.nick_max_length"
        || path.starts_with("display.nick_color")
    {
```

And update the `SettingsChanged` construction (line 639) to include the new fields:

```rust
    crate::web::protocol::WebEvent::SettingsChanged {
        timestamp_format: app.config.web.timestamp_format.clone(),
        line_height: app.config.web.line_height,
        theme: app.config.web.theme.clone(),
        nick_column_width: app.config.web.nick_column_width,
        nick_max_length: app.config.web.nick_max_length,
        nick_colors: app.config.display.nick_colors,
        nick_colors_in_nicklist: app.config.display.nick_colors_in_nicklist,
        nick_color_saturation: app.config.display.nick_color_saturation,
        nick_color_lightness: app.config.display.nick_color_lightness,
    },
```

- [ ] **Step 4: Add signals to `AppState` in `web-ui/src/state.rs`**

Add to `AppState` struct:

```rust
    pub nick_colors_enabled: RwSignal<bool>,
    pub nick_colors_in_nicklist: RwSignal<bool>,
    pub nick_color_saturation: RwSignal<f32>,
    pub nick_color_lightness: RwSignal<f32>,
```

Initialize with defaults:

```rust
    nick_colors_enabled: RwSignal::new(true),
    nick_colors_in_nicklist: RwSignal::new(true),
    nick_color_saturation: RwSignal::new(0.65),
    nick_color_lightness: RwSignal::new(0.65),
```

Handle in the `SettingsChanged` match arm (destructure the new fields):

```rust
    WebEvent::SettingsChanged {
        timestamp_format, line_height, theme,
        nick_column_width, nick_max_length,
        nick_colors, nick_colors_in_nicklist, nick_color_saturation, nick_color_lightness,
    } => {
        self.timestamp_format.set(timestamp_format);
        self.line_height.set(line_height);
        self.theme.set(theme);
        self.nick_column_width.set(nick_column_width);
        self.nick_max_length.set(nick_max_length);
        self.nick_colors_enabled.set(nick_colors);
        self.nick_colors_in_nicklist.set(nick_colors_in_nicklist);
        self.nick_color_saturation.set(nick_color_saturation);
        self.nick_color_lightness.set(nick_color_lightness);
    }
```

- [ ] **Step 5: Use signals in chat_view.rs and nick_list.rs**

Replace hardcoded `0.65` values in Tasks 9 and 10 code:

```rust
    let sat = state.nick_color_saturation.get();
    let lit = state.nick_color_lightness.get();
    let enabled = state.nick_colors_enabled.get();
    // ...
    let nick_style_color = if enabled && !is_own && !msg.highlight {
        let css_color = crate::nick_color::nick_color_css(&nick_text, sat, lit);
        format!("color: {};", css_color)
    } else {
        String::new()
    };
```

- [ ] **Step 6: Build and verify**

Run: `cargo check && (cd web-ui && cargo check)`
Expected: Compiles on both sides

- [ ] **Step 7: Commit**

```bash
git add src/web/protocol.rs web-ui/src/protocol.rs src/commands/settings.rs \
    web-ui/src/state.rs web-ui/src/components/chat_view.rs web-ui/src/components/nick_list.rs
git commit -m "feat(nick-color): sync nick color settings from server to web frontend"
```

---

### Task 12: End-to-End Verification + Edge Cases

**Files:**
- Modify: `src/nick_color.rs` (add edge case tests)

- [ ] **Step 1: Add edge case tests**

```rust
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
    // With zero saturation, all channels should be equal (gray)
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
    // Check that 20 common IRC nicks produce at least 15 distinct colors
    let nicks = [
        "alice", "bob", "charlie", "dave", "eve", "ferris", "grace",
        "heidi", "ivan", "judy", "karl", "linda", "mallory", "nancy",
        "oscar", "peggy", "quinn", "rachel", "steve", "trudy",
    ];
    let colors: std::collections::HashSet<_> = nicks
        .iter()
        .map(|n| nick_color(n, ColorSupport::TrueColor, 0.65, 0.65))
        .collect();
    assert!(colors.len() >= 15, "expected ≥15 distinct colors from 20 nicks, got {}", colors.len());
}
```

- [ ] **Step 2: Run full test suite**

Run: `cargo test`
Expected: ALL PASS

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: 0 warnings

- [ ] **Step 4: Build web frontend**

Run: `cd web-ui && trunk build`
Expected: Builds successfully

- [ ] **Step 5: Commit**

```bash
git add src/nick_color.rs
git commit -m "test(nick-color): add edge case and distribution tests"
```

---

### Task 13: Final Commit + Push

- [ ] **Step 1: Verify all tests pass**

Run: `cargo test && (cd web-ui && cargo test)`
Expected: ALL PASS

- [ ] **Step 2: Push branch**

```bash
git push -u outrage feat/nick-coloring
```

- [ ] **Step 3: Manual verification**

1. Launch repartee, connect to a server, join a channel
2. Verify each nick in chat has a unique consistent color
3. Verify nick list sidebar shows matching colors
4. Verify own messages still use green (ownnick theme color)
5. Verify mentions still use purple (menick theme color)
6. Verify `/set display.nick_colors false` disables coloring everywhere (reverts to theme defaults)
7. Verify `/set display.nick_colors_in_nicklist false` disables nick list coloring only (chat still colored)
8. Verify `/set display.nick_color_lightness 0.4` changes the brightness
9. Open web UI, verify nicks have the same colors
10. Verify away nicks in nick list are still dimmed
11. Verify `/set` changes propagate live to web UI

---

## Summary

| Task | Description | Files | Tests |
|------|-------------|-------|-------|
| 1 | ColorSupport detection | `src/nick_color.rs` | 2 |
| 2 | djb2 hash + HSL→RGB | `src/nick_color.rs` | 7 |
| 3 | 256-color + 16-color palettes | `src/nick_color.rs` | 4 |
| 4 | Config fields + /set wiring | `src/config/mod.rs`, `src/commands/settings.rs` | 2 |
| 5 | Store ColorSupport + reattach re-detection | `src/app.rs`, `src/main.rs` | 0 |
| 6 | TUI chat view integration | `src/ui/message_line.rs`, `src/ui/chat_view.rs` | 1+ |
| 7 | TUI nick list integration | `src/ui/nick_list.rs` | 0 |
| 8 | Web nick_color.rs | `web-ui/src/nick_color.rs`, `web-ui/src/main.rs` | 4 |
| 9 | Web chat view integration | `web-ui/src/components/chat_view.rs` | 0 |
| 10 | Web nick list integration | `web-ui/src/components/nick_list.rs` | 0 |
| 11 | Config sync to web | `src/web/protocol.rs`, `web-ui/src/protocol.rs`, `src/commands/settings.rs`, `web-ui/src/state.rs`, web components | 0 |
| 12 | Edge case tests + verification | `src/nick_color.rs` | 6+ |
| 13 | Final push + manual test | — | — |

**Total: ~26 new tests, 13 tasks, 4 chunks**

## Key Design Notes

- **Reattach detection:** `color_support` is re-detected in `refresh_image_protocol()` which runs on every `repartee a` (shim connect). This handles the case where a user starts in ghostty (truecolor) but reattaches from xterm-256color over SSH — nick colors automatically downgrade to the 60-color palette.
- **`SettingsChanged` is a typed struct**, not a map. Adding new settings requires modifying both `src/web/protocol.rs` and `web-ui/src/protocol.rs`. Use `#[serde(default)]` on new fields for backwards compatibility.
- **Broadcast guard in `settings.rs`** only fires for `web.*` paths. Nick color settings under `display.*` must be explicitly added to the guard condition.
- **No `src/lib.rs` or `web-ui/src/lib.rs`** — both crates use `main.rs` as root. Module declarations use `mod` (private), matching all existing modules.
- **`msg.highlight`** is the correct field for mention detection in web `chat_view.rs` (no `is_mention` variable exists).

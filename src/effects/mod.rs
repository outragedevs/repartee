//! Visual transition effects powered by `tachyonfx`.
//!
//! Effects are applied as post-processing on the ratatui frame buffer after
//! widgets render. They are opt-in (disabled by default) and configurable
//! via `[effects]` in `config.toml`.

use std::time::Instant;

use ratatui::layout::Rect;
use ratatui::style::Color;
use tachyonfx::Interpolation;
use tachyonfx::fx;
use tachyonfx::{Effect, EffectManager};

use crate::config::EffectsConfig;

/// Effect categories for keyed (unique) effect management.
/// Using `add_unique_effect` with a key ensures only one effect of
/// that category runs at a time — starting a new one cancels the old.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FxCategory {
    /// Chat area transition when switching buffers.
    #[default]
    BufferSwitch,
    /// Nick mention highlight flash on a message line.
    #[expect(dead_code, reason = "highlight wiring planned for next iteration")]
    Highlight,
}

/// Manages visual effects lifecycle and timing.
pub struct EffectState {
    manager: EffectManager<FxCategory>,
    last_frame: Instant,
    enabled: bool,
}

impl EffectState {
    pub fn new(config: &EffectsConfig) -> Self {
        Self {
            manager: EffectManager::default(),
            last_frame: Instant::now(),
            enabled: config.enabled,
        }
    }

    /// Process all active effects against the frame buffer.
    /// Call this AFTER all widgets have been rendered.
    pub fn process(&mut self, buf: &mut ratatui::buffer::Buffer, area: Rect) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_frame);
        self.last_frame = now;

        self.manager.process_effects(elapsed, buf, area);
    }

    /// Whether any effects are currently running (used for animation tick gating).
    pub fn has_active_effects(&self) -> bool {
        self.enabled && self.manager.is_running()
    }

    /// Trigger a buffer switch transition effect on the chat area.
    pub fn trigger_buffer_switch(&mut self, config: &EffectsConfig, chat_area: Rect, bg: Color) {
        if !self.enabled || config.buffer_switch == "none" || config.buffer_switch_ms == 0 {
            return;
        }

        tracing::debug!(
            style = %config.buffer_switch,
            ms = config.buffer_switch_ms,
            area = ?chat_area,
            "triggering buffer switch effect"
        );

        let effect = build_buffer_switch_effect(config, bg);
        let effect = effect.with_area(chat_area);
        self.manager
            .add_unique_effect(FxCategory::BufferSwitch, effect);
    }

    /// Trigger a highlight flash effect on a message line area.
    #[expect(dead_code, reason = "highlight wiring planned for next iteration")]
    pub fn trigger_highlight(&mut self, config: &EffectsConfig, line_area: Rect, accent: Color) {
        if !self.enabled || !config.highlight_flash || config.highlight_ms == 0 {
            return;
        }

        let effect = fx::fade_from_fg(accent, (config.highlight_ms, Interpolation::SineOut))
            .with_area(line_area);
        self.manager.add_effect(effect);
    }

    /// Update enabled state (called from `/set effects.enabled`).
    pub const fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
}

/// Build the appropriate buffer switch effect based on config style.
fn build_buffer_switch_effect(config: &EffectsConfig, bg: Color) -> Effect {
    let ms = config.buffer_switch_ms;
    let timer = (ms, Interpolation::CubicOut);

    match config.buffer_switch.as_str() {
        "sweep" => fx::sweep_in(tachyonfx::Motion::LeftToRight, 8, 0, bg, timer),
        "coalesce" => fx::coalesce(timer),
        // "fade" and anything else defaults to fade
        _ => fx::fade_from_fg(bg, timer),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EffectsConfig;

    #[test]
    fn effect_state_disabled_by_default() {
        let config = EffectsConfig::default();
        let state = EffectState::new(&config);
        assert!(!state.enabled);
        assert!(!state.has_active_effects());
    }

    #[test]
    fn effect_state_enabled_from_config() {
        let config = EffectsConfig {
            enabled: true,
            ..Default::default()
        };
        let state = EffectState::new(&config);
        assert!(state.enabled);
    }

    #[test]
    fn trigger_buffer_switch_when_disabled_is_noop() {
        let config = EffectsConfig::default(); // enabled = false
        let mut state = EffectState::new(&config);
        state.trigger_buffer_switch(&config, Rect::new(0, 0, 80, 24), Color::Black);
        assert!(!state.has_active_effects());
    }

    #[test]
    fn trigger_buffer_switch_none_style_is_noop() {
        let config = EffectsConfig {
            enabled: true,
            buffer_switch: "none".to_string(),
            ..Default::default()
        };
        let mut state = EffectState::new(&config);
        state.trigger_buffer_switch(&config, Rect::new(0, 0, 80, 24), Color::Black);
        assert!(!state.has_active_effects());
    }

    #[test]
    fn trigger_buffer_switch_fade_starts_running() {
        let config = EffectsConfig {
            enabled: true,
            buffer_switch: "fade".to_string(),
            buffer_switch_ms: 100,
            ..Default::default()
        };
        let mut state = EffectState::new(&config);
        state.trigger_buffer_switch(&config, Rect::new(0, 0, 80, 24), Color::Black);
        assert!(state.has_active_effects());
    }

    #[test]
    fn trigger_buffer_switch_sweep_starts_running() {
        let config = EffectsConfig {
            enabled: true,
            buffer_switch: "sweep".to_string(),
            buffer_switch_ms: 100,
            ..Default::default()
        };
        let mut state = EffectState::new(&config);
        state.trigger_buffer_switch(&config, Rect::new(0, 0, 80, 24), Color::Black);
        assert!(state.has_active_effects());
    }

    #[test]
    fn trigger_buffer_switch_coalesce_starts_running() {
        let config = EffectsConfig {
            enabled: true,
            buffer_switch: "coalesce".to_string(),
            buffer_switch_ms: 100,
            ..Default::default()
        };
        let mut state = EffectState::new(&config);
        state.trigger_buffer_switch(&config, Rect::new(0, 0, 80, 24), Color::Black);
        assert!(state.has_active_effects());
    }

    #[test]
    fn build_fade_effect() {
        let config = EffectsConfig {
            buffer_switch: "fade".to_string(),
            buffer_switch_ms: 150,
            ..Default::default()
        };
        let _effect = build_buffer_switch_effect(&config, Color::Black);
    }

    #[test]
    fn set_enabled_toggles_state() {
        let config = EffectsConfig::default();
        let mut state = EffectState::new(&config);
        assert!(!state.enabled);
        state.set_enabled(true);
        assert!(state.enabled);
        state.set_enabled(false);
        assert!(!state.enabled);
    }

    #[test]
    fn coalesce_effect_actually_modifies_buffer_cells() {
        use ratatui::buffer::Buffer;

        let area = Rect::new(0, 0, 20, 3);
        let mut buf = Buffer::empty(area);

        // Fill buffer with known content.
        for y in 0..3 {
            for x in 0..20 {
                buf[(x, y)].set_char('X');
            }
        }

        // Create coalesce effect and process with 0 elapsed (alpha=0).
        // At alpha=0, coalesce should show random chars (not the original content).
        let mut effect = fx::coalesce((1000_u32, Interpolation::Linear));
        effect.process(std::time::Duration::ZERO, &mut buf, area);

        // Count how many cells were modified from 'X'.
        let modified = (0..3)
            .flat_map(|y| (0..20).map(move |x| (x, y)))
            .filter(|&(x, y)| buf[(x, y)].symbol() != "X")
            .count();

        // At alpha=0 (start of coalesce), most cells should be randomized.
        assert!(
            modified > 30,
            "coalesce at alpha=0 should modify most cells, but only {modified}/60 were changed"
        );
    }

    #[test]
    fn fade_effect_modifies_buffer_colors() {
        use ratatui::buffer::Buffer;

        let area = Rect::new(0, 0, 10, 2);
        let mut buf = Buffer::empty(area);

        // Fill with white-on-black text.
        for y in 0..2 {
            for x in 0..10 {
                buf[(x, y)].set_char('A');
                buf[(x, y)].set_fg(Color::White);
            }
        }

        // Apply fade_from_fg(Black) at alpha=0 — fg should be near Black.
        let mut effect = fx::fade_from_fg(Color::Black, (500_u32, Interpolation::Linear));
        effect.process(std::time::Duration::ZERO, &mut buf, area);

        // After processing at t=0, fg should have been shifted toward Black.
        let cell_fg = buf[(0, 0)].fg;
        assert_ne!(
            cell_fg,
            Color::White,
            "fade_from_fg at alpha=0 should shift fg away from White"
        );
    }

    #[test]
    fn process_completes_effect_after_duration() {
        let config = EffectsConfig {
            enabled: true,
            buffer_switch: "fade".to_string(),
            buffer_switch_ms: 100,
            ..Default::default()
        };
        let mut state = EffectState::new(&config);
        state.trigger_buffer_switch(&config, Rect::new(0, 0, 40, 10), Color::Black);
        assert!(state.has_active_effects());

        // Process for longer than the effect duration.
        let mut buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 40, 10));
        state.process(&mut buf, Rect::new(0, 0, 40, 10));

        // Simulate enough time passing.
        std::thread::sleep(std::time::Duration::from_millis(150));
        state.process(&mut buf, Rect::new(0, 0, 40, 10));

        assert!(
            !state.has_active_effects(),
            "effect should have completed after its duration"
        );
    }
}

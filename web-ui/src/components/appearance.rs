//! Appearance menu — client-side font size and line spacing, adjustable
//! without recompiling (tester feedback: 13 px monospace is too small on a
//! phone and nothing could change it). Values live in localStorage and are
//! applied as `--font-size` / `--line-height` CSS vars by an Effect in
//! `app.rs`; "Reset" clears both overrides so the stylesheet / server
//! defaults take over again.

use leptos::prelude::*;

use crate::state::AppState;

/// Adjustment bounds — generous but sane; values outside these render the
/// chat unusable.
pub const FONT_MIN: u32 = 10;
pub const FONT_MAX: u32 = 24;
pub const LINE_H_MIN: f32 = 1.0;
pub const LINE_H_MAX: f32 = 2.2;
const LINE_H_STEP: f32 = 0.05;

/// Clamp a proposed font size (px) into the supported range.
pub fn clamp_font(px: i64) -> u32 {
    u32::try_from(px.clamp(i64::from(FONT_MIN), i64::from(FONT_MAX))).unwrap_or(FONT_MIN)
}

/// Clamp a proposed line height into the supported range, quantized to the
/// step so repeated +/- clicks produce clean values (1.35, 1.40, …).
pub fn clamp_line_h(v: f32) -> f32 {
    let clamped = v.clamp(LINE_H_MIN, LINE_H_MAX);
    (clamped / LINE_H_STEP).round() * LINE_H_STEP
}

/// The effective font size in px when no override is set — reads the
/// *computed* style so the mobile (14 px) vs desktop (13 px) stylesheet
/// defaults are respected without duplicating them in Rust.
fn computed_font_px() -> u32 {
    let px = web_sys::window()
        .and_then(|w| {
            let doc = w.document()?;
            let root = doc.document_element()?;
            w.get_computed_style(&root).ok().flatten()
        })
        .and_then(|s| s.get_property_value("font-size").ok())
        .and_then(|v| v.trim_end_matches("px").parse::<f32>().ok())
        .unwrap_or(13.0);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "computed font-size is a small px value; i64 can't overflow from f32"
    )]
    let rounded = px.round() as i64;
    clamp_font(rounded)
}

/// The "Aa" button — safe to instantiate in any container (desktop bottom
/// bar, mobile slide panel). It only flips the shared `appearance_open`
/// flag; the modal itself is [`AppearanceModal`], rendered ONCE at the app
/// root like the other modals. It must not live here: the mobile instance
/// sits inside a `transform`ed slide panel, and a transformed ancestor
/// becomes the containing block for `position: fixed` — the modal would
/// render clipped inside the 220px panel instead of centered on screen.
#[component]
pub fn AppearanceButton() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();
    view! {
        <button
            type="button"
            class="appearance-btn"
            title="Text size & spacing"
            on:click=move |_| state.appearance_open.set(true)
        >"Aa"</button>
    }
}

/// Modal with font-size / line-spacing steppers (render once, at the root).
#[component]
pub fn AppearanceModal() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();

    let effective_font = move || {
        state
            .font_size_override
            .get()
            .unwrap_or_else(computed_font_px)
    };
    let effective_line_h = move || {
        state
            .line_height_override
            .get()
            .unwrap_or_else(|| state.line_height.get())
    };

    let bump_font = move |delta: i64| {
        let current = i64::from(effective_font());
        state
            .font_size_override
            .set(Some(clamp_font(current + delta)));
    };
    let bump_line_h = move |delta: f32| {
        let current = effective_line_h();
        state
            .line_height_override
            .set(Some(clamp_line_h(current + delta)));
    };

    view! {
        {move || {
            if !state.appearance_open.get() {
                return None;
            }
            Some(view! {
                <div
                    class="appearance-backdrop"
                    on:click=move |_| state.appearance_open.set(false)
                ></div>
                <div class="appearance-modal" role="dialog" aria-label="Appearance">
                    <div class="appearance-row">
                        <span class="appearance-label">"Text size"</span>
                        <button type="button" class="appearance-step"
                            on:click=move |_| bump_font(-1)>"\u{2212}"</button>
                        <span class="appearance-value">
                            {move || format!("{} px", effective_font())}
                        </span>
                        <button type="button" class="appearance-step"
                            on:click=move |_| bump_font(1)>"+"</button>
                    </div>
                    <div class="appearance-row">
                        <span class="appearance-label">"Line spacing"</span>
                        <button type="button" class="appearance-step"
                            on:click=move |_| bump_line_h(-LINE_H_STEP)>"\u{2212}"</button>
                        <span class="appearance-value">
                            {move || format!("{:.2}", effective_line_h())}
                        </span>
                        <button type="button" class="appearance-step"
                            on:click=move |_| bump_line_h(LINE_H_STEP)>"+"</button>
                    </div>
                    <div class="appearance-actions">
                        <button type="button" class="appearance-reset"
                            on:click=move |_| {
                                state.font_size_override.set(None);
                                state.line_height_override.set(None);
                            }
                        >"Reset to defaults"</button>
                        <button type="button" class="appearance-close"
                            on:click=move |_| state.appearance_open.set(false)
                        >"Done"</button>
                    </div>
                </div>
            })
        }}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_clamps_to_range() {
        assert_eq!(clamp_font(5), FONT_MIN);
        assert_eq!(clamp_font(13), 13);
        assert_eq!(clamp_font(99), FONT_MAX);
        assert_eq!(clamp_font(-3), FONT_MIN);
    }

    #[test]
    fn line_height_clamps_and_quantizes() {
        assert!((clamp_line_h(0.2) - LINE_H_MIN).abs() < f32::EPSILON);
        assert!((clamp_line_h(9.9) - LINE_H_MAX).abs() < 1e-4);
        // 1.35 + step lands exactly on 1.40, not 1.400000001-style noise.
        let stepped = clamp_line_h(1.35 + LINE_H_STEP);
        assert!((stepped - 1.4).abs() < 1e-4, "got {stepped}");
    }
}

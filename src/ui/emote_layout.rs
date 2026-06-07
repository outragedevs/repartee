//! Encoding of emote placeholders into chat lines and resolution of their screen
//! rectangles after wrapping. Pure logic — no rendering side effects.
//!
//! An emote is rewritten (in graphical mode) into a fixed-width run of Private
//! Use Area codepoints so the existing unicode-width-based wrapper reserves the
//! right number of cells. After wrapping, [`resolve_placements`] walks the
//! visible lines and recovers each run's screen rectangle so the animator can
//! composite the current GIF frame over it.

use ratatui::layout::Rect;
use ratatui::text::Line;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Width in terminal cells reserved for one emote (square-ish at 1 row tall).
/// Used as the fixed thumbnail footprint in the emote picker grid and as a
/// fallback; chat rendering sizes each emote per [`emote_footprint`].
pub const EMOTE_COLS: usize = 2;

/// Compute the cell footprint `(cols, rows)` for an emote from its native GIF
/// pixel size, the terminal cell pixel size, and the configured caps.
///
/// The emote is fit within a `max_cols × max_rows` cell box, preserving aspect
/// ratio and **never upscaling beyond the GIF's native pixel size**. So a small
/// emote keeps roughly its real on-screen size (≈ 2×1 today) while a large or
/// wide GIF grows up to the cap instead of being crushed into a fixed 2×1 box,
/// which letterboxed wide emotes down to a tiny squished strip.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "inputs are small positive cell/pixel counts; the ratio is rounded then clamped to [1, cap]"
)]
pub fn emote_footprint(
    native_w: u16,
    native_h: u16,
    font_w: u16,
    font_h: u16,
    max_cols: u16,
    max_rows: u16,
) -> (u16, u16) {
    let nw = f64::from(native_w.max(1));
    let nh = f64::from(native_h.max(1));
    let fw = f64::from(font_w.max(1));
    let fh = f64::from(font_h.max(1));
    let max_cols = max_cols.max(1);
    let max_rows = max_rows.max(1);

    // Fit within the cap box preserving aspect; `min(.., 1.0)` forbids upscaling
    // past the native pixel size.
    let cap_w = f64::from(max_cols) * fw;
    let cap_h = f64::from(max_rows) * fh;
    let scale = (cap_w / nw).min(cap_h / nh).min(1.0);
    let disp_w = nw * scale;
    let disp_h = nh * scale;

    let cols = ((disp_w / fw).round() as u16).clamp(1, max_cols);
    let rows = ((disp_h / fh).round() as u16).clamp(1, max_rows);
    (cols, rows)
}

/// Base of the Private Use Area range we use to mark emote placeholders.
/// PUA-A (U+F0000..=U+FFFFD) gives ~65k slots; 183 emotes fit easily, and these
/// codepoints have terminal display width 1.
const PUA_BASE: u32 = 0x000F_0000;
const PUA_MAX: u32 = 0x000F_FFFD;

/// Per-frame inputs that map an emote's native GIF size to a cell footprint:
/// the terminal cell pixel size (from the image picker) and the configured caps.
#[derive(Debug, Clone, Copy)]
pub struct EmoteSizing {
    pub font_w: u16,
    pub font_h: u16,
    pub max_cols: u16,
    pub max_rows: u16,
}

impl EmoteSizing {
    /// Cell footprint `(cols, rows)` for the emote at `emote_index`, reading its
    /// native GIF size. Unknown indices fall back to a 1×1 minimum.
    #[must_use]
    pub fn footprint(self, emote_index: u32) -> (u16, u16) {
        let (nw, nh) = crate::emotes::native_size(emote_index).unwrap_or((0, 0));
        emote_footprint(nw, nh, self.font_w, self.font_h, self.max_cols, self.max_rows)
    }
}

/// Build the placeholder string for an emote: `cols` identical PUA chars, each
/// encoding the registry index. Identical chars => a contiguous run of width
/// `cols` that the resolver collapses back to one emote.
#[must_use]
pub fn placeholder_for_emote(index: u32, cols: u16) -> String {
    let c = char::from_u32(PUA_BASE + index).unwrap_or('\u{FFFD}');
    std::iter::repeat_n(c, usize::from(cols.max(1))).collect()
}

/// Fixed-width [`EMOTE_COLS`] placeholder — test helper for wrap/resolve tests.
#[cfg(test)]
#[must_use]
pub fn placeholder_for_index(index: u32) -> String {
    placeholder_for_emote(index, u16::try_from(EMOTE_COLS).unwrap_or(2))
}

/// Recover the emote registry index from a placeholder char.
#[must_use]
pub const fn decode_placeholder_index(c: char) -> Option<u32> {
    let u = c as u32;
    if u >= PUA_BASE && u <= PUA_MAX {
        Some(u - PUA_BASE)
    } else {
        None
    }
}

/// Decode a placeholder index from a single grapheme cluster. Placeholders are
/// single Private Use Area codepoints, so any multi-codepoint grapheme (a real
/// emoji sequence) is never mistaken for one.
#[must_use]
pub fn decode_placeholder_grapheme(g: &str) -> Option<u32> {
    let mut chars = g.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => decode_placeholder_index(c),
        _ => None,
    }
}

/// Where one emote should be composited, in absolute screen cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmotePlacement {
    pub emote_index: u32,
    pub rect: Rect,
}

/// An emote placeholder run being accumulated across cells on one line.
struct Run {
    idx: u32,
    /// Column (relative to `area.x`) where the run starts.
    start: usize,
    /// Cells accumulated so far.
    width: usize,
    /// Target width in cells for this emote (`footprint().0`); caps the run so
    /// adjacent identical emotes split into separate placements.
    cols: usize,
    /// Height in rows for this emote (`footprint().1`).
    rows: u16,
}

/// Walk the already-wrapped visible lines and compute the screen rect of each
/// emote placeholder run. `area` is the chat region; `lines[k]` maps to row
/// `area.y + k`. `footprint_of` maps an emote index to its `(cols, rows)` cell
/// footprint (closure seam so callers supply the live picker/config sizing and
/// tests stay asset-independent). Placements whose row exceeds `area` height are
/// dropped.
#[must_use]
pub fn resolve_placements(
    lines: &[Line<'_>],
    area: Rect,
    footprint_of: impl Fn(u32) -> (u16, u16),
) -> Vec<EmotePlacement> {
    let mut out = Vec::new();
    for (row, line) in lines.iter().enumerate() {
        if row >= area.height as usize {
            break;
        }
        let y = area.y + u16::try_from(row).unwrap_or(u16::MAX);
        let mut col: usize = 0;
        let mut run: Option<Run> = None;
        for span in &line.spans {
            // Measure by grapheme clusters so the column cursor matches ratatui's
            // own grapheme-based layout. A per-`char` walk would mis-advance `col`
            // across multi-codepoint emoji (VS16, flags, skin-tone, ZWJ) in the
            // surrounding text and place the emote rect on the wrong cells.
            for g in span.content.graphemes(true) {
                let cw = UnicodeWidthStr::width(g);
                if let Some(idx) = decode_placeholder_grapheme(g) {
                    match &mut run {
                        // Extend only within this emote's own width. Two identical
                        // emotes back-to-back (`:x::x:`) share an index, so the
                        // per-emote `cols` cap keeps them as separate placements
                        // instead of one stretched run.
                        Some(r) if r.idx == idx && r.width + cw <= r.cols => {
                            r.width += cw;
                        }
                        _ => {
                            flush_run(&mut run, y, area.x, &mut out);
                            let (cols, rows) = footprint_of(idx);
                            run = Some(Run {
                                idx,
                                start: col,
                                width: cw,
                                cols: usize::from(cols.max(1)),
                                rows: rows.max(1),
                            });
                        }
                    }
                } else {
                    flush_run(&mut run, y, area.x, &mut out);
                }
                col += cw;
            }
        }
        flush_run(&mut run, y, area.x, &mut out);
    }
    out
}

fn flush_run(run: &mut Option<Run>, y: u16, area_x: u16, out: &mut Vec<EmotePlacement>) {
    if let Some(r) = run.take() {
        let x = area_x.saturating_add(u16::try_from(r.start).unwrap_or(u16::MAX));
        out.push(EmotePlacement {
            emote_index: r.idx,
            // Width is the cells actually occupied (`r.width`), not the target
            // footprint (`r.cols`). They are equal for an unsplit run; if the
            // wrapper had to split an over-wide emote across a line edge, this
            // fragment must cover only its own cells — the compositor sizes the
            // image canvas from this rect, so a wider claim would overpaint text.
            rect: Rect::new(x, y, u16::try_from(r.width).unwrap_or(u16::MAX), r.rows),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;

    // ── emote_footprint tests (font 8×16, caps 8×3 unless noted) ──

    #[test]
    fn footprint_small_square_stays_two_by_one() {
        // A 15×15 GIF (the most common size) keeps today's ~2×1 footprint:
        // no upscaling, height fits one row.
        assert_eq!(emote_footprint(15, 15, 8, 16, 8, 3), (2, 1));
    }

    #[test]
    fn footprint_wide_emote_gets_more_columns_not_letterboxed() {
        // A 40×18 banner used to be crushed into 2 cols (≈16×7px, squished).
        // It should now span ~5 cols at one row tall, preserving its shape.
        assert_eq!(emote_footprint(40, 18, 8, 16, 8, 3), (5, 1));
        // A 50×20 GIF spans ~6 cols.
        assert_eq!(emote_footprint(50, 20, 8, 16, 8, 3), (6, 1));
    }

    #[test]
    fn footprint_large_gif_grows_to_multiple_rows_within_cap() {
        // The biggest asset (64×46) grows to the full 8×3 cap.
        assert_eq!(emote_footprint(64, 46, 8, 16, 8, 3), (8, 3));
    }

    #[test]
    fn footprint_never_exceeds_caps() {
        // A hypothetical huge GIF is scaled down to fit the cap, never beyond.
        let (cols, rows) = emote_footprint(2000, 1000, 8, 16, 8, 3);
        assert!(cols <= 8 && rows <= 3, "got {cols}x{rows}, cap is 8x3");
        assert!(cols >= 1 && rows >= 1);
    }

    #[test]
    fn footprint_max_rows_one_keeps_single_row_but_still_widens() {
        // With rows capped at 1 (the conservative default), a tall/wide emote
        // still gets extra columns — it just never grows vertically.
        let (_, rows) = emote_footprint(64, 46, 8, 16, 8, 1);
        assert_eq!(rows, 1, "max_rows=1 must force a single row");
        let (cols, rows2) = emote_footprint(40, 18, 8, 16, 8, 1);
        assert_eq!(rows2, 1);
        assert!(cols >= 3, "wide emote still widens at 1 row, got {cols} cols");
    }

    #[test]
    fn footprint_is_resilient_to_zero_inputs() {
        // Degenerate inputs must never divide by zero or return a 0 dimension.
        let (cols, rows) = emote_footprint(0, 0, 0, 0, 0, 0);
        assert!(cols >= 1 && rows >= 1);
    }

    #[test]
    fn footprint_scales_with_font_size() {
        // A bigger font (cell 16×32) halves the column count for the same GIF.
        assert_eq!(emote_footprint(40, 18, 16, 32, 8, 3), (3, 1));
    }

    #[test]
    fn resolve_uses_footprint_width_and_splits_adjacent_wide_emotes() {
        // Two adjacent copies of the same 5-col emote must become two 5-wide
        // placements, not one 10-wide stretched run.
        let mut s = placeholder_for_emote(3, 5);
        s.push_str(&placeholder_for_emote(3, 5));
        let line = Line::from(Span::raw(s));
        let placements = resolve_placements(&[line], Rect::new(0, 0, 40, 1), |_| (5, 1));
        assert_eq!(placements.len(), 2);
        assert_eq!(placements[0].rect.width, 5);
        assert_eq!(placements[1].rect.width, 5);
        assert_eq!(placements[0].rect.x, 0);
        assert_eq!(placements[1].rect.x, 5);
    }

    #[test]
    fn resolve_sets_rect_height_from_footprint_rows() {
        let line = Line::from(Span::raw(placeholder_for_emote(2, 4)));
        let placements = resolve_placements(&[line], Rect::new(0, 0, 40, 3), |_| (4, 3));
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].rect.width, 4);
        assert_eq!(placements[0].rect.height, 3);
    }

    #[test]
    fn encode_then_decode_roundtrips_index() {
        let ph = placeholder_for_index(42);
        assert_eq!(ph.chars().count(), EMOTE_COLS);
        assert!(ph.chars().all(|c| decode_placeholder_index(c).is_some()));
        assert_eq!(
            decode_placeholder_index(ph.chars().next().unwrap()),
            Some(42)
        );
    }

    #[test]
    fn placeholder_width_is_emote_cols() {
        use unicode_width::UnicodeWidthStr;
        assert_eq!(placeholder_for_index(0).width(), EMOTE_COLS);
    }

    #[test]
    fn non_placeholder_char_decodes_none() {
        assert_eq!(decode_placeholder_index('a'), None);
    }

    #[test]
    fn resolves_placement_after_text() {
        let ph = placeholder_for_index(5);
        let line = Line::from(vec![Span::raw("ab"), Span::raw(ph), Span::raw("c")]);
        let area = Rect::new(10, 4, 40, 5);

        let placements = resolve_placements(&[line], area, |_| (2, 1));
        assert_eq!(placements.len(), 1);
        let p = &placements[0];
        assert_eq!(p.emote_index, 5);
        assert_eq!(p.rect.x, 10 + 2); // area.x + width("ab")
        assert_eq!(p.rect.y, 4);
        assert_eq!(p.rect.width as usize, EMOTE_COLS);
        assert_eq!(p.rect.height, 1);
    }

    #[test]
    fn two_emotes_on_consecutive_rows() {
        let ph = placeholder_for_index(1);
        let lines = vec![Line::from(Span::raw(ph.clone())), Line::from(Span::raw(ph))];
        let area = Rect::new(0, 0, 10, 2);
        let placements = resolve_placements(&lines, area, |_| (2, 1));
        assert_eq!(placements.len(), 2);
        assert_eq!(placements[0].rect.y, 0);
        assert_eq!(placements[1].rect.y, 1);
    }

    #[test]
    fn adjacent_identical_emotes_split() {
        // Same emote twice, back-to-back (`:x::x:`) — must be TWO placements of
        // width EMOTE_COLS each, not one merged stretched run.
        let mut s = placeholder_for_index(5);
        s.push_str(&placeholder_for_index(5));
        let line = Line::from(Span::raw(s));
        let placements = resolve_placements(&[line], Rect::new(0, 0, 20, 1), |_| (2, 1));
        assert_eq!(placements.len(), 2);
        assert_eq!(placements[0].emote_index, 5);
        assert_eq!(placements[1].emote_index, 5);
        assert_eq!(placements[0].rect.width as usize, EMOTE_COLS);
        assert_eq!(placements[1].rect.width as usize, EMOTE_COLS);
        assert_eq!(placements[1].rect.x as usize, EMOTE_COLS);
    }

    #[test]
    fn adjacent_distinct_emotes_split() {
        // index 1 then index 2, adjacent — must produce two placements.
        let mut s = placeholder_for_index(1);
        s.push_str(&placeholder_for_index(2));
        let line = Line::from(Span::raw(s));
        let placements = resolve_placements(&[line], Rect::new(0, 0, 20, 1), |_| (2, 1));
        assert_eq!(placements.len(), 2);
        assert_eq!(placements[0].emote_index, 1);
        assert_eq!(placements[1].emote_index, 2);
        assert_eq!(placements[1].rect.x as usize, EMOTE_COLS);
    }

    #[test]
    fn placement_x_uses_grapheme_width_of_preceding_emoji() {
        // A ZWJ family emoji before the placeholder is ONE grapheme of display
        // width 2 (what ratatui reserves). Summing per-`char` widths would count
        // it as 6 and push the emote rect to the wrong column.
        let emoji = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let ph = placeholder_for_index(5);
        let line = Line::from(vec![Span::raw(emoji.to_owned()), Span::raw(ph)]);
        let placements = resolve_placements(&[line], Rect::new(0, 0, 40, 1), |_| (2, 1));
        assert_eq!(placements.len(), 1);
        assert_eq!(
            placements[0].rect.x, 2,
            "preceding ZWJ emoji occupies 2 cells, not 6"
        );
    }

    #[test]
    fn rows_beyond_area_height_dropped() {
        let ph = placeholder_for_index(7);
        let lines = vec![Line::from(Span::raw(ph.clone())), Line::from(Span::raw(ph))];
        let area = Rect::new(0, 0, 10, 1); // only 1 row visible
        let placements = resolve_placements(&lines, area, |_| (2, 1));
        assert_eq!(placements.len(), 1);
    }
}

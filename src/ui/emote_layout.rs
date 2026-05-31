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
use unicode_width::UnicodeWidthChar;

/// Width in terminal cells reserved for one emote (square-ish at 1 row tall).
pub const EMOTE_COLS: usize = 2;

/// Base of the Private Use Area range we use to mark emote placeholders.
/// PUA-A (U+F0000..=U+FFFFD) gives ~65k slots; 183 emotes fit easily, and these
/// codepoints have terminal display width 1.
const PUA_BASE: u32 = 0x000F_0000;
const PUA_MAX: u32 = 0x000F_FFFD;

/// Build the placeholder string for an emote registry index: `EMOTE_COLS`
/// identical PUA chars, each encoding the index. Identical chars => a contiguous
/// run of width `EMOTE_COLS` that the resolver collapses back to one emote.
#[must_use]
pub fn placeholder_for_index(index: u32) -> String {
    let c = char::from_u32(PUA_BASE + index).unwrap_or('\u{FFFD}');
    std::iter::repeat_n(c, EMOTE_COLS).collect()
}

/// True if `c` is one of our placeholder codepoints.
#[must_use]
pub fn is_placeholder_char(c: char) -> bool {
    (PUA_BASE..=PUA_MAX).contains(&(c as u32))
}

/// Recover the emote registry index from a placeholder char.
#[must_use]
pub fn decode_placeholder_index(c: char) -> Option<u32> {
    let u = c as u32;
    if (PUA_BASE..=PUA_MAX).contains(&u) { Some(u - PUA_BASE) } else { None }
}

/// Where one emote should be composited, in absolute screen cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmotePlacement {
    pub emote_index: u32,
    pub rect: Rect,
}

/// Walk the already-wrapped visible lines and compute the screen rect of each
/// emote placeholder run. `area` is the chat region; `lines[k]` maps to row
/// `area.y + k`. Placements whose row exceeds `area` height are dropped.
#[must_use]
pub fn resolve_placements(lines: &[Line<'_>], area: Rect) -> Vec<EmotePlacement> {
    let mut out = Vec::new();
    for (row, line) in lines.iter().enumerate() {
        if row >= area.height as usize {
            break;
        }
        let y = area.y + u16::try_from(row).unwrap_or(u16::MAX);
        let mut col: usize = 0;
        // (index, start_col, width) of the run currently being accumulated.
        let mut run: Option<(u32, usize, usize)> = None;
        for span in &line.spans {
            for ch in span.content.chars() {
                let cw = ch.width().unwrap_or(0);
                if let Some(idx) = decode_placeholder_index(ch) {
                    match &mut run {
                        Some((cur, _start, w)) if *cur == idx => *w += cw,
                        _ => {
                            flush_run(&mut run, y, area.x, &mut out);
                            run = Some((idx, col, cw));
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

fn flush_run(run: &mut Option<(u32, usize, usize)>, y: u16, area_x: u16, out: &mut Vec<EmotePlacement>) {
    if let Some((idx, start, w)) = run.take() {
        let x = area_x.saturating_add(u16::try_from(start).unwrap_or(u16::MAX));
        out.push(EmotePlacement {
            emote_index: idx,
            rect: Rect::new(x, y, u16::try_from(w).unwrap_or(u16::MAX), 1),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;

    #[test]
    fn encode_then_decode_roundtrips_index() {
        let ph = placeholder_for_index(42);
        assert_eq!(ph.chars().count(), EMOTE_COLS);
        assert!(ph.chars().all(is_placeholder_char));
        assert_eq!(decode_placeholder_index(ph.chars().next().unwrap()), Some(42));
    }

    #[test]
    fn placeholder_width_is_emote_cols() {
        use unicode_width::UnicodeWidthStr;
        assert_eq!(placeholder_for_index(0).width(), EMOTE_COLS);
    }

    #[test]
    fn non_placeholder_char_decodes_none() {
        assert_eq!(decode_placeholder_index('a'), None);
        assert!(!is_placeholder_char('a'));
    }

    #[test]
    fn resolves_placement_after_text() {
        let ph = placeholder_for_index(5);
        let line = Line::from(vec![Span::raw("ab"), Span::raw(ph), Span::raw("c")]);
        let area = Rect::new(10, 4, 40, 5);

        let placements = resolve_placements(&[line], area);
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
        let placements = resolve_placements(&lines, area);
        assert_eq!(placements.len(), 2);
        assert_eq!(placements[0].rect.y, 0);
        assert_eq!(placements[1].rect.y, 1);
    }

    #[test]
    fn adjacent_distinct_emotes_split() {
        // index 1 then index 2, adjacent — must produce two placements.
        let mut s = placeholder_for_index(1);
        s.push_str(&placeholder_for_index(2));
        let line = Line::from(Span::raw(s));
        let placements = resolve_placements(&[line], Rect::new(0, 0, 20, 1));
        assert_eq!(placements.len(), 2);
        assert_eq!(placements[0].emote_index, 1);
        assert_eq!(placements[1].emote_index, 2);
        assert_eq!(placements[1].rect.x as usize, EMOTE_COLS);
    }

    #[test]
    fn rows_beyond_area_height_dropped() {
        let ph = placeholder_for_index(7);
        let lines = vec![Line::from(Span::raw(ph.clone())), Line::from(Span::raw(ph))];
        let area = Rect::new(0, 0, 10, 1); // only 1 row visible
        let placements = resolve_placements(&lines, area);
        assert_eq!(placements.len(), 1);
    }
}

//! Bare display: the active buffer printed as plain text that the terminal
//! wraps by itself. Ratatui positions every row on its own, so a terminal sees
//! a wrapped message as unrelated rows; printed whole, the message is one line
//! to mouse selection and URL detection.

use std::fmt::Write as _;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// A message as the bare display prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BareMessage {
    lines: Vec<String>,
}

impl BareMessage {
    /// Control characters are dropped, so text from IRC never reaches the
    /// terminal as an escape sequence. Embedded newlines start a new line and
    /// tabs become a space.
    pub fn new(text: &str) -> Self {
        let lines = text
            .split('\n')
            .map(|line| {
                line.chars()
                    .filter_map(|ch| match ch {
                        '\t' => Some(' '),
                        _ if ch.is_control() => None,
                        _ => Some(ch),
                    })
                    .collect()
            })
            .collect();
        Self { lines }
    }

    /// Terminal rows the message takes at `cols` columns.
    pub fn rows(&self, cols: usize) -> usize {
        self.lines.iter().map(|line| line_rows(line, cols)).sum()
    }
}

/// Rows a line wraps to, the way a terminal wraps it: a wide character that
/// does not fit at the end of a row moves whole to the next one. Measured per
/// grapheme, as the chat view does, so emoji sequences count as one glyph.
fn line_rows(line: &str, cols: usize) -> usize {
    fit(line, cols, usize::MAX).1
}

/// The longest prefix of `line` that fits in `rows` rows of `cols` columns.
fn fit_rows(line: &str, cols: usize, rows: usize) -> &str {
    fit(line, cols, rows).0
}

/// The prefix of `line` that fits in `rows` rows, and the rows it takes.
fn fit(line: &str, cols: usize, rows: usize) -> (&str, usize) {
    let cols = cols.max(1);
    let mut row = 1;
    let mut col = 0;
    for (index, grapheme) in line.grapheme_indices(true) {
        let width = grapheme.width();
        if col + width > cols {
            if row == rows {
                return (&line[..index], row);
            }
            row += 1;
            col = 0;
        }
        col += width;
    }
    (line, row)
}

/// Where the shown messages start, as `(index, row)` in display order.
///
/// A live view (`start` is `None`) ends with the last message on the bottom
/// row; a scrolled view begins with message `start` on the top row. Only whole
/// messages are shown, except a single message taller than the screen, which
/// is cut off at the bottom.
pub fn layout(
    count: usize,
    start: Option<usize>,
    rows: usize,
    mut height: impl FnMut(usize) -> usize,
) -> Vec<(usize, usize)> {
    let mut placed = Vec::new();
    let mut used = 0;
    match start {
        None => {
            for index in (0..count).rev() {
                let lines = height(index);
                if used + lines > rows {
                    if placed.is_empty() {
                        placed.push((index, 0));
                    }
                    break;
                }
                used += lines;
                placed.push((index, rows - used));
            }
            placed.reverse();
        }
        Some(start) => {
            for index in start..count {
                let lines = height(index);
                if used + lines > rows {
                    if placed.is_empty() {
                        placed.push((index, 0));
                    }
                    break;
                }
                placed.push((index, used));
                used += lines;
            }
        }
    }
    placed
}

/// Terminal output that draws `placed` messages on a cleared screen. Each
/// message is positioned once and then printed without line breaks of its own,
/// so the terminal wraps it.
pub fn render<'a>(
    placed: impl IntoIterator<Item = (usize, &'a BareMessage)>,
    cols: usize,
    rows: usize,
) -> Vec<u8> {
    let mut out = String::from("\x1b[0m\x1b[?7h\x1b[2J\x1b[H");
    for (row, message) in placed {
        let mut left = rows.saturating_sub(row);
        if left == 0 {
            continue;
        }
        let _ = write!(out, "\x1b[{};1H", row + 1);
        for (number, line) in message.lines.iter().enumerate() {
            if left == 0 {
                break;
            }
            if number > 0 {
                out.push_str("\r\n");
            }
            let line = fit_rows(line, cols, left);
            out.push_str(line);
            left -= line_rows(line, cols);
        }
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_never_reach_the_terminal() {
        let message = BareMessage::new("a\x1b]52;c;cHduZWQ=\x07b\u{9b}31mc\td\re");
        let out = String::from_utf8(render([(0, &message)], 80, 24)).unwrap();
        let printed = out.rsplit("\x1b[1;1H").next().unwrap();
        assert_eq!(printed, "a]52;c;cHduZWQ=b31mc de");
    }

    #[test]
    fn a_long_url_is_printed_in_one_piece() {
        let url = format!("https://example.org/{}", "x".repeat(150));
        let message = BareMessage::new(&format!("12:00 <peer> see {url}"));
        let out = String::from_utf8(render([(0, &message)], 40, 10)).unwrap();
        assert!(out.contains(&url));
        assert_eq!(message.rows(40), 5);
    }

    #[test]
    fn rows_follow_terminal_wrapping_of_wide_characters() {
        assert_eq!(line_rows("", 10), 1);
        assert_eq!(line_rows("abcdefghij", 10), 1);
        assert_eq!(line_rows("abcdefghijk", 10), 2);
        // Nine columns, then a wide character that moves to the next row.
        assert_eq!(line_rows("abcdefghi界", 10), 2);
        assert_eq!(BareMessage::new("one\ntwo").rows(10), 2);
    }

    #[test]
    fn emoji_sequences_are_measured_and_cut_as_one_glyph() {
        let family = "👨\u{200d}👩\u{200d}👧";
        assert_eq!(line_rows(&format!("{family}x"), 3), 1);
        assert_eq!(line_rows("ab\u{2764}\u{fe0f}", 3), 2);
        assert_eq!(fit_rows(&format!("ab{family}cd"), 3, 1), "ab");
        assert_eq!(fit_rows(&format!("a{family}cd"), 3, 1), format!("a{family}"));
    }

    #[test]
    fn live_layout_ends_on_the_bottom_row_with_whole_messages() {
        let heights = [3, 1, 2, 2];
        let placed = layout(4, None, 5, |index| heights[index]);
        assert_eq!(placed, vec![(1, 0), (2, 1), (3, 3)]);
    }

    #[test]
    fn scrolled_layout_starts_on_the_top_row() {
        let heights = [3, 1, 2, 2];
        let placed = layout(4, Some(0), 5, |index| heights[index]);
        assert_eq!(placed, vec![(0, 0), (1, 3)]);
    }

    #[test]
    fn a_message_taller_than_the_screen_is_cut_off() {
        let placed = layout(2, None, 3, |_| 10);
        assert_eq!(placed, vec![(1, 0)]);
        let message = BareMessage::new(&"x".repeat(100));
        let out = String::from_utf8(render([(0, &message)], 10, 3)).unwrap();
        assert_eq!(out.matches('x').count(), 30);
    }

    #[test]
    fn output_has_no_trailing_line_break() {
        let first = BareMessage::new("first");
        let second = BareMessage::new("second\nline");
        let out = String::from_utf8(render([(0, &first), (1, &second)], 20, 3)).unwrap();
        assert!(out.ends_with("second\r\nline"));
        assert!(out.contains("\x1b[1;1Hfirst\x1b[2;1Hsecond"));
    }
}

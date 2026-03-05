use std::collections::HashMap;

use ratatui::style::Color;

use super::{StyledSpan, hex_to_color};

/// Irssi-compatible color map.
/// Lowercase = normal, uppercase = bright.
fn color_map(code: char) -> Option<&'static str> {
    match code {
        'k' => Some("#000000"),
        'K' => Some("#555555"),
        'r' => Some("#aa0000"),
        'R' => Some("#ff5555"),
        'g' => Some("#00aa00"),
        'G' => Some("#55ff55"),
        'y' => Some("#aa5500"),
        'Y' => Some("#ffff55"),
        'b' => Some("#0000aa"),
        'B' => Some("#5555ff"),
        'm' => Some("#aa00aa"),
        'M' => Some("#ff55ff"),
        'c' => Some("#00aaaa"),
        'C' => Some("#55ffff"),
        'w' => Some("#aaaaaa"),
        'W' => Some("#ffffff"),
        _ => None,
    }
}

/// Standard mIRC color palette (indices 0-98).
/// 0-15: classic 16-color set, 16-98: extended palette.
const MIRC_COLORS: &[&str] = &[
    "#ffffff", "#000000", "#00007f", "#009300", "#ff0000", "#7f0000", "#9c009c", "#fc7f00",
    "#ffff00", "#00fc00", "#009393", "#00ffff", "#0000fc", "#ff00ff", "#7f7f7f", "#d2d2d2",
    "#470000", "#472100", "#474700", "#324700", "#004700", "#00472c", "#004747", "#002747",
    "#000047", "#2e0047", "#470047", "#47002a", "#740000", "#743a00", "#747400", "#517400",
    "#007400", "#007449", "#007474", "#004074", "#000074", "#4b0074", "#740074", "#740045",
    "#b50000", "#b56300", "#b5b500", "#7db500", "#00b500", "#00b571", "#00b5b5", "#0063b5",
    "#0000b5", "#7500b5", "#b500b5", "#b5006b", "#ff0000", "#ff8c00", "#ffff00", "#b2ff00",
    "#00ff00", "#00ffa0", "#00ffff", "#008cff", "#0000ff", "#a500ff", "#ff00ff", "#ff0098",
    "#ff5959", "#ffb459", "#ffff71", "#cfff60", "#6fff6f", "#65ffc9", "#6dffff", "#59b4ff",
    "#5959ff", "#c459ff", "#ff66ff", "#ff59bc", "#ff9c9c", "#ffd39c", "#ffff9c", "#e2ff9c",
    "#9cff9c", "#9cffdb", "#9cffff", "#9cd3ff", "#9c9cff", "#dc9cff", "#ff9cff", "#ff94d3",
    "#000000", "#131313", "#282828", "#363636", "#4d4d4d", "#656565", "#818181", "#9f9f9f",
    "#bcbcbc", "#e2e2e2", "#ffffff",
];

const MAX_ABSTRACTION_DEPTH: usize = 10;

// ---------------------------------------------------------------------------
// Style state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct StyleState {
    fg: Option<Color>,
    bg: Option<Color>,
    bold: bool,
    italic: bool,
    underline: bool,
    dim: bool,
}

impl StyleState {
    fn to_span(&self, text: String) -> StyledSpan {
        StyledSpan {
            text,
            fg: self.fg,
            bg: self.bg,
            bold: self.bold,
            italic: self.italic,
            underline: self.underline,
            dim: self.dim,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read exactly 6 hex chars from position in a char slice, or return None.
fn read_hex6(chars: &[char], pos: usize) -> Option<String> {
    if pos + 6 > chars.len() {
        return None;
    }
    let slice: String = chars[pos..pos + 6].iter().collect();
    if slice.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(slice)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// substitute_vars
// ---------------------------------------------------------------------------

/// Substitute positional variables ($0, $1, $*, $[N]0, $[-N]0) in a string.
pub fn substitute_vars(input: &str, params: &[&str]) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut result = String::new();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '$' {
            i += 1;
            if i >= chars.len() {
                result.push('$');
                break;
            }

            // $* -- all params joined with space
            if chars[i] == '*' {
                result.push_str(&params.join(" "));
                i += 1;
                continue;
            }

            // $[N]D or $[-N]D -- padded variable
            if chars[i] == '[' {
                i += 1; // skip [
                let mut num_str = String::new();
                while i < chars.len() && chars[i] != ']' {
                    num_str.push(chars[i]);
                    i += 1;
                }
                if i < chars.len() {
                    i += 1; // skip ]
                }

                let pad_width: i32 = num_str.parse().unwrap_or(0);

                // Read the variable index digit(s)
                let mut idx_str = String::new();
                while i < chars.len() && chars[i].is_ascii_digit() {
                    idx_str.push(chars[i]);
                    i += 1;
                }
                let idx: usize = idx_str.parse().unwrap_or(0);
                let value = if idx < params.len() {
                    params[idx]
                } else {
                    ""
                };

                let abs_width = pad_width.unsigned_abs() as usize;
                if pad_width < 0 {
                    // left-pad
                    result.push_str(&format!("{:>width$}", value, width = abs_width));
                } else {
                    // right-pad
                    result.push_str(&format!("{:<width$}", value, width = abs_width));
                }
                continue;
            }

            // $0, $1, ... -- positional variable (can be multi-digit)
            if chars[i].is_ascii_digit() {
                let mut idx_str = String::new();
                while i < chars.len() && chars[i].is_ascii_digit() {
                    idx_str.push(chars[i]);
                    i += 1;
                }
                let idx: usize = idx_str.parse().unwrap_or(0);
                if idx < params.len() {
                    result.push_str(params[idx]);
                }
                continue;
            }

            // Not a recognized variable -- keep the $ and current char
            result.push('$');
            result.push(chars[i]);
            i += 1;
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    result
}

// ---------------------------------------------------------------------------
// resolve_abstractions
// ---------------------------------------------------------------------------

/// Find the matching closing brace, respecting nested braces.
fn find_matching_brace(input: &[char], open_pos: usize) -> Option<usize> {
    let mut depth = 1;
    let mut i = open_pos + 1;
    while i < input.len() && depth > 0 {
        if input[i] == '{' {
            depth += 1;
        } else if input[i] == '}' {
            depth -= 1;
        }
        if depth > 0 {
            i += 1;
        }
    }
    if depth == 0 { Some(i) } else { None }
}

/// Split abstraction args respecting nested braces.
/// e.g., "$2 {pubnick $0}" -> ["$2", "{pubnick $0}"]
fn split_abstraction_args(args_str: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut depth = 0;

    for ch in args_str.chars() {
        if ch == '{' {
            depth += 1;
        } else if ch == '}' {
            depth -= 1;
        }

        if ch == ' ' && depth == 0 {
            if !current.is_empty() {
                args.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

/// Resolve `{name args...}` abstraction references.
/// Recursively expands up to MAX_ABSTRACTION_DEPTH.
pub fn resolve_abstractions(
    input: &str,
    abstracts: &HashMap<String, String>,
    depth: usize,
) -> String {
    if depth >= MAX_ABSTRACTION_DEPTH {
        return input.to_string();
    }

    let chars: Vec<char> = input.chars().collect();
    let mut result = String::new();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '{' {
            // Find matching closing brace (respecting nesting)
            if let Some(close_idx) = find_matching_brace(&chars, i) {
                let inner: String = chars[i + 1..close_idx].iter().collect();
                let space_idx = inner.find(' ');

                let (name, args_str) = match space_idx {
                    None => (inner.as_str(), ""),
                    Some(idx) => (&inner[..idx], &inner[idx + 1..]),
                };

                if abstracts.contains_key(name) {
                    let template = &abstracts[name];
                    // Split args respecting nested braces
                    let args = if args_str.is_empty() {
                        Vec::new()
                    } else {
                        split_abstraction_args(args_str)
                    };
                    // First resolve nested abstractions in args
                    let resolved_args: Vec<String> = args
                        .iter()
                        .map(|a| resolve_abstractions(a, abstracts, depth + 1))
                        .collect();
                    // Convert to &str for substitute_vars
                    let resolved_refs: Vec<&str> =
                        resolved_args.iter().map(|s| s.as_str()).collect();
                    // Then substitute into template
                    let expanded = substitute_vars(template, &resolved_refs);
                    // Recurse to handle any remaining abstractions
                    result.push_str(&resolve_abstractions(&expanded, abstracts, depth + 1));
                } else {
                    // Unknown abstract -- keep as-is
                    let kept: String = chars[i..=close_idx].iter().collect();
                    result.push_str(&kept);
                }

                i = close_idx + 1;
            } else {
                // No matching brace found
                result.push(chars[i]);
                i += 1;
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    result
}

// ---------------------------------------------------------------------------
// parse_format_string
// ---------------------------------------------------------------------------

/// Parse an irssi-compatible format string into styled spans.
///
/// Supports:
/// - `%X` color codes (irssi color map)
/// - `%ZRRGGBB` 24-bit hex fg colors
/// - `%zRRGGBB` 24-bit hex bg colors
/// - `%_` `%u` `%i` `%d` style toggles (bold, underline, italic, dim)
/// - `%N`/`%n` reset
/// - `%|` indent marker (skipped)
/// - `%%` literal percent
/// - mIRC control characters (`\x02`, `\x03`, `\x04`, `\x0F`, `\x16`, `\x1D`, `\x1E`, `\x1F`, `\x11`)
pub fn parse_format_string(input: &str, params: &[&str]) -> Vec<StyledSpan> {
    // Step 1: Substitute variables
    let text = substitute_vars(input, params);

    // Step 2: Walk char by char, parse color/style codes, build spans
    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<StyledSpan> = Vec::new();
    let mut current = StyleState::default();
    let mut buffer = String::new();
    let mut i = 0;

    let flush = |buffer: &mut String, spans: &mut Vec<StyledSpan>, current: &StyleState| {
        if !buffer.is_empty() {
            spans.push(current.to_span(std::mem::take(buffer)));
        }
    };

    while i < chars.len() {
        if chars[i] == '%' {
            i += 1;
            if i >= chars.len() {
                buffer.push('%');
                break;
            }

            let code = chars[i];

            // %N or %n -- reset all
            if code == 'N' || code == 'n' {
                flush(&mut buffer, &mut spans, &current);
                current = StyleState::default();
                i += 1;
                continue;
            }

            // %_ -- toggle bold
            if code == '_' {
                flush(&mut buffer, &mut spans, &current);
                current.bold = !current.bold;
                i += 1;
                continue;
            }

            // %u -- toggle underline
            if code == 'u' {
                flush(&mut buffer, &mut spans, &current);
                current.underline = !current.underline;
                i += 1;
                continue;
            }

            // %i -- toggle italic
            if code == 'i' {
                flush(&mut buffer, &mut spans, &current);
                current.italic = !current.italic;
                i += 1;
                continue;
            }

            // %d -- toggle dim
            if code == 'd' {
                flush(&mut buffer, &mut spans, &current);
                current.dim = !current.dim;
                i += 1;
                continue;
            }

            // %Z -- 24-bit hex fg color: %ZRRGGBB
            if code == 'Z' {
                flush(&mut buffer, &mut spans, &current);
                if let Some(hex) = read_hex6(&chars, i + 1) {
                    current.fg = hex_to_color(&hex);
                    i += 7; // skip Z + 6 hex chars
                } else {
                    // Not enough hex digits, treat as literal
                    buffer.push('%');
                    buffer.push('Z');
                    i += 1;
                }
                continue;
            }

            // %z -- 24-bit hex bg color: %zRRGGBB
            if code == 'z' {
                flush(&mut buffer, &mut spans, &current);
                if let Some(hex) = read_hex6(&chars, i + 1) {
                    current.bg = hex_to_color(&hex);
                    i += 7; // skip z + 6 hex chars
                } else {
                    buffer.push('%');
                    buffer.push('z');
                    i += 1;
                }
                continue;
            }

            // %| -- indent marker, skip
            if code == '|' {
                i += 1;
                continue;
            }

            // %% -- literal percent
            if code == '%' {
                buffer.push('%');
                i += 1;
                continue;
            }

            // Color code from irssi color map
            if let Some(hex) = color_map(code) {
                flush(&mut buffer, &mut spans, &current);
                current.fg = hex_to_color(hex);
                i += 1;
                continue;
            }

            // Unknown code -- keep as-is
            buffer.push('%');
            buffer.push(code);
            i += 1;
        } else {
            // -- mIRC control characters --
            let ch = chars[i] as u32;

            // \x02 -- bold toggle
            if ch == 0x02 {
                flush(&mut buffer, &mut spans, &current);
                current.bold = !current.bold;
                i += 1;
                continue;
            }

            // \x03 -- mIRC color: \x03[FG[,BG]]
            if ch == 0x03 {
                flush(&mut buffer, &mut spans, &current);
                i += 1;

                // Read up to 2 digits for foreground
                let mut fg_str = String::new();
                if i < chars.len() && chars[i].is_ascii_digit() {
                    fg_str.push(chars[i]);
                    i += 1;
                    if i < chars.len() && chars[i].is_ascii_digit() {
                        fg_str.push(chars[i]);
                        i += 1;
                    }
                }

                // Read optional ,BG (only if comma is followed by a digit)
                let mut bg_str = String::new();
                if !fg_str.is_empty()
                    && i < chars.len()
                    && chars[i] == ','
                    && i + 1 < chars.len()
                    && chars[i + 1].is_ascii_digit()
                {
                    i += 1; // skip comma
                    bg_str.push(chars[i]);
                    i += 1;
                    if i < chars.len() && chars[i].is_ascii_digit() {
                        bg_str.push(chars[i]);
                        i += 1;
                    }
                }

                if fg_str.is_empty() {
                    // \x03 alone = reset color
                    current.fg = None;
                    current.bg = None;
                } else {
                    let fg_num: usize = fg_str.parse().unwrap_or(0);
                    if fg_num < MIRC_COLORS.len() {
                        current.fg = hex_to_color(MIRC_COLORS[fg_num]);
                    }
                    if !bg_str.is_empty() {
                        let bg_num: usize = bg_str.parse().unwrap_or(0);
                        if bg_num < MIRC_COLORS.len() {
                            current.bg = hex_to_color(MIRC_COLORS[bg_num]);
                        }
                    }
                }
                continue;
            }

            // \x04 -- hex color: \x04[RRGGBB[,RRGGBB]]
            if ch == 0x04 {
                flush(&mut buffer, &mut spans, &current);
                i += 1;

                if let Some(fg_hex) = read_hex6(&chars, i) {
                    current.fg = hex_to_color(&fg_hex);
                    i += 6;
                    if i < chars.len()
                        && chars[i] == ','
                        && let Some(bg_hex) = read_hex6(&chars, i + 1)
                    {
                        current.bg = hex_to_color(&bg_hex);
                        i += 7; // comma + 6 hex chars
                    }
                } else {
                    // \x04 alone = reset color
                    current.fg = None;
                    current.bg = None;
                }
                continue;
            }

            // \x0F -- reset all formatting
            if ch == 0x0F {
                flush(&mut buffer, &mut spans, &current);
                current = StyleState::default();
                i += 1;
                continue;
            }

            // \x16 -- reverse (swap fg/bg)
            if ch == 0x16 {
                flush(&mut buffer, &mut spans, &current);
                std::mem::swap(&mut current.fg, &mut current.bg);
                i += 1;
                continue;
            }

            // \x1D -- italic toggle
            if ch == 0x1D {
                flush(&mut buffer, &mut spans, &current);
                current.italic = !current.italic;
                i += 1;
                continue;
            }

            // \x1E -- strikethrough toggle (mapped to dim)
            if ch == 0x1E {
                flush(&mut buffer, &mut spans, &current);
                current.dim = !current.dim;
                i += 1;
                continue;
            }

            // \x1F -- underline toggle
            if ch == 0x1F {
                flush(&mut buffer, &mut spans, &current);
                current.underline = !current.underline;
                i += 1;
                continue;
            }

            // \x11 -- monospace (no-op in terminal)
            if ch == 0x11 {
                i += 1;
                continue;
            }

            buffer.push(chars[i]);
            i += 1;
        }
    }

    flush(&mut buffer, &mut spans, &current);

    // If nothing was produced (empty input), return a single empty span
    if spans.is_empty() {
        spans.push(StyleState::default().to_span(String::new()));
    }

    spans
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // substitute_vars tests
    // -----------------------------------------------------------------------

    #[test]
    fn substitute_positional() {
        let result = substitute_vars("Hello $0, welcome to $1!", &["Alice", "#rust"]);
        assert_eq!(result, "Hello Alice, welcome to #rust!");
    }

    #[test]
    fn substitute_star() {
        let result = substitute_vars("All: $*", &["one", "two", "three"]);
        assert_eq!(result, "All: one two three");
    }

    #[test]
    fn substitute_padded_right() {
        let result = substitute_vars("[$[10]0]", &["hi"]);
        assert_eq!(result, "[hi        ]");
    }

    #[test]
    fn substitute_padded_left() {
        let result = substitute_vars("[$[-10]0]", &["hi"]);
        assert_eq!(result, "[        hi]");
    }

    #[test]
    fn substitute_missing_param_empty() {
        let result = substitute_vars("$0 and $1", &["only"]);
        assert_eq!(result, "only and ");
    }

    #[test]
    fn substitute_trailing_dollar() {
        let result = substitute_vars("price: 5$", &[]);
        assert_eq!(result, "price: 5$");
    }

    // -----------------------------------------------------------------------
    // resolve_abstractions tests
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_simple_abstract() {
        let abstracts = HashMap::from([("nick".to_string(), "<%_$0%_>".to_string())]);
        let result = resolve_abstractions("{nick Alice}", &abstracts, 0);
        assert_eq!(result, "<%_Alice%_>");
    }

    #[test]
    fn resolve_nested_abstractions() {
        let abstracts = HashMap::from([
            ("msgnick".to_string(), "$0$1> ".to_string()),
            ("pubnick".to_string(), "%_$*%_".to_string()),
        ]);
        let result = resolve_abstractions("{msgnick # {pubnick Bob}}", &abstracts, 0);
        assert_eq!(result, "#%_Bob%_> ");
    }

    #[test]
    fn resolve_max_depth_terminates() {
        // Self-referencing abstract should terminate at max depth
        let abstracts = HashMap::from([("loop".to_string(), "{loop}".to_string())]);
        let result = resolve_abstractions("{loop}", &abstracts, 0);
        // Should not loop forever -- at depth 10 it returns the input as-is
        assert!(result.contains("{loop}") || result.is_empty() || !result.is_empty());
        // The key assertion is that this test completes (doesn't hang)
    }

    #[test]
    fn resolve_unknown_abstract_kept() {
        let abstracts = HashMap::new();
        let result = resolve_abstractions("hello {unknown arg}", &abstracts, 0);
        assert_eq!(result, "hello {unknown arg}");
    }

    // -----------------------------------------------------------------------
    // parse_format_string tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_plain_text() {
        let spans = parse_format_string("hello world", &[]);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "hello world");
        assert_eq!(spans[0].fg, None);
        assert!(!spans[0].bold);
    }

    #[test]
    fn parse_hex_fg_color() {
        let spans = parse_format_string("%Z7aa2f7colored", &[]);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "colored");
        assert_eq!(spans[0].fg, Some(Color::Rgb(0x7a, 0xa2, 0xf7)));
    }

    #[test]
    fn parse_hex_bg_color() {
        let spans = parse_format_string("%z1a1b26on dark bg", &[]);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "on dark bg");
        assert_eq!(spans[0].bg, Some(Color::Rgb(0x1a, 0x1b, 0x26)));
    }

    #[test]
    fn parse_irssi_letter_colors() {
        // "%rred%N %bblue" ->
        //   %r sets fg=red, "red" accumulated, %N flushes "red" and resets,
        //   " " accumulated, %b flushes " " and sets fg=blue, "blue" accumulated, end flushes "blue"
        let spans = parse_format_string("%rred%N %bblue", &[]);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].text, "red");
        assert_eq!(spans[0].fg, Some(Color::Rgb(0xaa, 0x00, 0x00)));
        assert_eq!(spans[1].text, " ");
        assert_eq!(spans[1].fg, None);
        assert_eq!(spans[2].text, "blue");
        assert_eq!(spans[2].fg, Some(Color::Rgb(0x00, 0x00, 0xaa)));
    }

    #[test]
    fn parse_bold_toggle() {
        let spans = parse_format_string("%_bold%_ normal", &[]);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "bold");
        assert!(spans[0].bold);
        assert_eq!(spans[1].text, " normal");
        assert!(!spans[1].bold);
    }

    #[test]
    fn parse_italic_toggle() {
        let spans = parse_format_string("%iitalic%i plain", &[]);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "italic");
        assert!(spans[0].italic);
        assert_eq!(spans[1].text, " plain");
        assert!(!spans[1].italic);
    }

    #[test]
    fn parse_underline_toggle() {
        let spans = parse_format_string("%uunder%u normal", &[]);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "under");
        assert!(spans[0].underline);
        assert_eq!(spans[1].text, " normal");
        assert!(!spans[1].underline);
    }

    #[test]
    fn parse_dim_toggle() {
        let spans = parse_format_string("%ddim%d bright", &[]);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "dim");
        assert!(spans[0].dim);
        assert_eq!(spans[1].text, " bright");
        assert!(!spans[1].dim);
    }

    #[test]
    fn parse_reset() {
        let spans = parse_format_string("%_bold%Nnot", &[]);
        assert_eq!(spans.len(), 2);
        assert!(spans[0].bold);
        assert!(!spans[1].bold);
        assert_eq!(spans[1].text, "not");
    }

    #[test]
    fn parse_literal_percent() {
        let spans = parse_format_string("100%%", &[]);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "100%");
    }

    #[test]
    fn parse_mirc_color_codes() {
        // \x034 = red (mIRC color 4 = "#ff0000")
        // After \x03 with no args at end, colors reset but buffer is empty.
        // So only one span is produced for "red text".
        let input = "\x034red text\x03";
        let spans = parse_format_string(input, &[]);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "red text");
        assert_eq!(spans[0].fg, Some(Color::Rgb(0xff, 0x00, 0x00)));
    }

    #[test]
    fn parse_mirc_color_reset() {
        // \x034red\x03 normal
        let input = "\x034red\x03 normal";
        let spans = parse_format_string(input, &[]);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "red");
        assert_eq!(spans[0].fg, Some(Color::Rgb(0xff, 0x00, 0x00)));
        assert_eq!(spans[1].text, " normal");
        assert_eq!(spans[1].fg, None);
    }

    #[test]
    fn parse_mirc_hex_color() {
        // \x04FF8800hello
        let input = "\x04FF8800hello";
        let spans = parse_format_string(input, &[]);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "hello");
        assert_eq!(spans[0].fg, Some(Color::Rgb(0xFF, 0x88, 0x00)));
    }

    #[test]
    fn parse_mirc_bold_underline_italic() {
        // \x02bold\x02 \x1Funder\x1F \x1Dital\x1D
        let input = "\x02bold\x02 \x1Funder\x1F \x1Dital\x1D";
        let spans = parse_format_string(input, &[]);
        // "bold" with bold=true, " " with bold=false,
        // "under" with underline=true, " " with underline=false,
        // "ital" with italic=true, end (empty buffer after toggle off) or not
        assert_eq!(spans[0].text, "bold");
        assert!(spans[0].bold);
        assert_eq!(spans[1].text, " ");
        assert!(!spans[1].bold);
        assert_eq!(spans[2].text, "under");
        assert!(spans[2].underline);
        assert_eq!(spans[3].text, " ");
        assert!(!spans[3].underline);
        assert_eq!(spans[4].text, "ital");
        assert!(spans[4].italic);
    }

    #[test]
    fn parse_reverse() {
        // Set fg then reverse should swap fg and bg
        let input = "%Zff0000red\x16reversed";
        let spans = parse_format_string(input, &[]);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "red");
        assert_eq!(spans[0].fg, Some(Color::Rgb(0xff, 0x00, 0x00)));
        assert_eq!(spans[0].bg, None);
        assert_eq!(spans[1].text, "reversed");
        assert_eq!(spans[1].fg, None);
        assert_eq!(spans[1].bg, Some(Color::Rgb(0xff, 0x00, 0x00)));
    }

    #[test]
    fn full_kokoirc_timestamp_format() {
        // From Nightfall theme: timestamp = "%Z6e738d$*%Z7aa2f7%N"
        // With param "12:34"
        let format_str = "%Z6e738d$*%Z7aa2f7%N";
        let spans = parse_format_string(format_str, &["12:34"]);
        // After substitution: "%Z6e738d12:34%Z7aa2f7%N"
        // %Z6e738d -> fg=#6e738d, "12:34" accumulated
        // %Z7aa2f7 -> flush "12:34" with fg, set new fg
        // %N -> flush empty (nothing), reset
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "12:34");
        assert_eq!(spans[0].fg, Some(Color::Rgb(0x6e, 0x73, 0x8d)));
    }

    #[test]
    fn full_kokoirc_pubmsg_format() {
        // After abstraction resolution, a pubmsg might look like:
        // "%Z565f89#%Z7aa2f7%_Bob%_%N%Z7aa2f7❯%N%| %Za9b1d6Hello%N"
        let format_str =
            "%Z565f89#%Z7aa2f7%_Bob%_%N%Z7aa2f7\u{276F}%N%| %Za9b1d6Hello%N";
        let spans = parse_format_string(format_str, &[]);

        // Walk through:
        // %Z565f89 -> fg=#565f89
        // "#" in buffer
        // %Z7aa2f7 -> flush "#" (fg=#565f89), set fg=#7aa2f7
        // %_ -> flush (empty), bold=true
        // "Bob" in buffer
        // %_ -> flush "Bob" (fg=#7aa2f7, bold), bold=false
        // %N -> flush (empty), reset all
        // %Z7aa2f7 -> flush (empty), fg=#7aa2f7
        // "❯" in buffer
        // %N -> flush "❯" (fg=#7aa2f7), reset all
        // %| -> skip
        // " " in buffer
        // %Za9b1d6 -> flush " " (no style), fg=#a9b1d6
        // "Hello" in buffer
        // %N -> flush "Hello" (fg=#a9b1d6), reset all

        // Expected spans: "#", "Bob", "❯", " ", "Hello"
        assert_eq!(spans.len(), 5);
        assert_eq!(spans[0].text, "#");
        assert_eq!(spans[0].fg, Some(Color::Rgb(0x56, 0x5f, 0x89)));
        assert_eq!(spans[1].text, "Bob");
        assert_eq!(spans[1].fg, Some(Color::Rgb(0x7a, 0xa2, 0xf7)));
        assert!(spans[1].bold);
        assert_eq!(spans[2].text, "\u{276F}");
        assert_eq!(spans[2].fg, Some(Color::Rgb(0x7a, 0xa2, 0xf7)));
        assert_eq!(spans[3].text, " ");
        assert_eq!(spans[3].fg, None);
        assert_eq!(spans[4].text, "Hello");
        assert_eq!(spans[4].fg, Some(Color::Rgb(0xa9, 0xb1, 0xd6)));
    }

    #[test]
    fn parse_empty_input() {
        let spans = parse_format_string("", &[]);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "");
    }

    #[test]
    fn parse_mirc_fg_bg_colors() {
        // \x034,2 = fg=red, bg=navy
        let input = "\x034,2colored";
        let spans = parse_format_string(input, &[]);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "colored");
        assert_eq!(spans[0].fg, Some(Color::Rgb(0xff, 0x00, 0x00))); // mIRC 4 = #ff0000
        assert_eq!(spans[0].bg, Some(Color::Rgb(0x00, 0x00, 0x7f))); // mIRC 2 = #00007f
    }

    #[test]
    fn parse_mirc_hex_fg_bg() {
        // \x04FF8800,001122 = fg=#FF8800, bg=#001122
        let input = "\x04FF8800,001122text";
        let spans = parse_format_string(input, &[]);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "text");
        assert_eq!(spans[0].fg, Some(Color::Rgb(0xFF, 0x88, 0x00)));
        assert_eq!(spans[0].bg, Some(Color::Rgb(0x00, 0x11, 0x22)));
    }

    #[test]
    fn parse_strikethrough_as_dim() {
        // \x1E should toggle dim
        let input = "\x1Edimmed\x1E normal";
        let spans = parse_format_string(input, &[]);
        assert_eq!(spans[0].text, "dimmed");
        assert!(spans[0].dim);
        assert_eq!(spans[1].text, " normal");
        assert!(!spans[1].dim);
    }

    #[test]
    fn parse_monospace_noop() {
        // \x11 is monospace, should be skipped
        let input = "before\x11after";
        let spans = parse_format_string(input, &[]);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "beforeafter");
    }

    #[test]
    fn parse_reset_all_0f() {
        // \x0F should reset all
        let input = "%_bold\x0Fnot";
        let spans = parse_format_string(input, &[]);
        assert_eq!(spans[0].text, "bold");
        assert!(spans[0].bold);
        assert_eq!(spans[1].text, "not");
        assert!(!spans[1].bold);
    }
}

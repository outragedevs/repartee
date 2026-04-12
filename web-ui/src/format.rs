/// A styled text segment for rendering in the web UI.
#[derive(Clone)]
pub struct StyledSpan {
    pub text: String,
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
}

impl StyledSpan {
    /// Generate a CSS `style` string for this span.
    pub fn css(&self) -> String {
        let mut s = String::new();
        let mut sep = "";
        if let Some(ref fg) = self.fg {
            s.push_str("color:");
            s.push_str(fg);
            sep = ";";
        }
        if let Some(ref bg) = self.bg {
            s.push_str(sep);
            s.push_str("background:");
            s.push_str(bg);
            sep = ";";
        }
        if self.bold {
            s.push_str(sep);
            s.push_str("font-weight:bold");
            sep = ";";
        }
        if self.italic {
            s.push_str(sep);
            s.push_str("font-style:italic");
            sep = ";";
        }
        if self.underline {
            s.push_str(sep);
            s.push_str("text-decoration:underline");
            sep = ";";
        }
        if self.dim {
            s.push_str(sep);
            s.push_str("opacity:0.5");
        }
        s
    }

    /// Returns true if this span has any styling.
    pub fn has_style(&self) -> bool {
        self.fg.is_some()
            || self.bg.is_some()
            || self.bold
            || self.italic
            || self.underline
            || self.dim
    }
}

/// Parse irssi/mIRC format strings into styled spans.
///
/// Supported formats:
/// - `%ZRRGGBB` — 24-bit hex foreground
/// - `%zRRGGBB` — 24-bit hex background
/// - `%N` / `%n` — reset all formatting
/// - `%_` — bold toggle, `%u` — underline, `%i` — italic, `%d` — dim
/// - mIRC `\x02` bold, `\x03` color, `\x04` hex color, `\x0F` reset
/// - mIRC `\x1D` italic, `\x1E` strikethrough, `\x1F` underline
pub fn parse_format(text: &str) -> Vec<StyledSpan> {
    let mut spans = Vec::new();
    let mut current = String::new();
    let mut fg: Option<String> = None;
    let mut bg: Option<String> = None;
    let mut bold = false;
    let mut italic = false;
    let mut underline = false;
    let mut dim = false;

    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    macro_rules! flush {
        () => {
            if !current.is_empty() {
                spans.push(StyledSpan {
                    text: std::mem::take(&mut current),
                    fg: fg.clone(),
                    bg: bg.clone(),
                    bold,
                    italic,
                    underline,
                    dim,
                });
            }
        };
    }

    macro_rules! reset {
        () => {
            fg = None;
            bg = None;
            bold = false;
            italic = false;
            underline = false;
            dim = false;
        };
    }

    while i < len {
        match chars[i] {
            // irssi format codes
            '%' if i + 1 < len => match chars[i + 1] {
                'Z' if i + 8 <= len => {
                    flush!();
                    let hex: String = chars[i + 2..i + 8].iter().collect();
                    fg = Some(format!("#{hex}"));
                    i += 8;
                }
                'z' if i + 8 <= len => {
                    flush!();
                    let hex: String = chars[i + 2..i + 8].iter().collect();
                    bg = Some(format!("#{hex}"));
                    i += 8;
                }
                'N' | 'n' => {
                    flush!();
                    reset!();
                    i += 2;
                }
                '_' => {
                    flush!();
                    bold = !bold;
                    i += 2;
                }
                'u' | 'U' => {
                    flush!();
                    underline = !underline;
                    i += 2;
                }
                'i' | 'I' => {
                    flush!();
                    italic = !italic;
                    i += 2;
                }
                'd' => {
                    flush!();
                    dim = !dim;
                    i += 2;
                }
                '%' => {
                    current.push('%');
                    i += 2;
                }
                c => {
                    // irssi single-letter color codes (%k %r %g %y %b %m %c %w + uppercase)
                    if let Some(hex) = irssi_color(c) {
                        flush!();
                        fg = Some(hex.to_string());
                        i += 2;
                    } else {
                        // Unknown %X — keep literal
                        current.push('%');
                        current.push(c);
                        i += 2;
                    }
                }
            },

            // mIRC bold
            '\x02' => {
                flush!();
                bold = !bold;
                i += 1;
            }

            // mIRC color: \x03[fg[,bg]]
            '\x03' => {
                flush!();
                i += 1;
                let mut fg_str = String::new();
                while i < len && chars[i].is_ascii_digit() && fg_str.len() < 2 {
                    fg_str.push(chars[i]);
                    i += 1;
                }
                if fg_str.is_empty() {
                    fg = None;
                    bg = None;
                } else {
                    fg = fg_str
                        .parse::<u8>()
                        .ok()
                        .and_then(mirc_color)
                        .map(String::from);
                    if i < len && chars[i] == ',' {
                        i += 1;
                        let mut bg_str = String::new();
                        while i < len && chars[i].is_ascii_digit() && bg_str.len() < 2 {
                            bg_str.push(chars[i]);
                            i += 1;
                        }
                        bg = bg_str
                            .parse::<u8>()
                            .ok()
                            .and_then(mirc_color)
                            .map(String::from);
                    }
                }
            }

            // mIRC hex color: \x04RRGGBB
            '\x04' => {
                flush!();
                i += 1;
                if i + 6 <= len && chars[i..i + 6].iter().all(|c| c.is_ascii_hexdigit()) {
                    let hex: String = chars[i..i + 6].iter().collect();
                    fg = Some(format!("#{hex}"));
                    i += 6;
                }
            }

            // mIRC reset
            '\x0F' => {
                flush!();
                reset!();
                i += 1;
            }

            // mIRC reverse (ignore — just skip)
            '\x16' => {
                i += 1;
            }

            // mIRC italic
            '\x1D' => {
                flush!();
                italic = !italic;
                i += 1;
            }

            // mIRC strikethrough (render as dim)
            '\x1E' => {
                flush!();
                dim = !dim;
                i += 1;
            }

            // mIRC underline
            '\x1F' => {
                flush!();
                underline = !underline;
                i += 1;
            }

            ch => {
                current.push(ch);
                i += 1;
            }
        }
    }

    flush!();
    spans
}

/// irssi single-letter color codes (%k %r %g %m etc).
fn irssi_color(code: char) -> Option<&'static str> {
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

/// Convert a mIRC color code (0-15) to a CSS hex color.
fn mirc_color(code: u8) -> Option<&'static str> {
    match code {
        0 => Some("#ffffff"),
        1 => Some("#000000"),
        2 => Some("#00007f"),
        3 => Some("#009300"),
        4 => Some("#ff0000"),
        5 => Some("#7f0000"),
        6 => Some("#9c009c"),
        7 => Some("#fc7f00"),
        8 => Some("#ffff00"),
        9 => Some("#00fc00"),
        10 => Some("#009393"),
        11 => Some("#00ffff"),
        12 => Some("#0000fc"),
        13 => Some("#ff00ff"),
        14 => Some("#7f7f7f"),
        15 => Some("#d2d2d2"),
        _ => None,
    }
}

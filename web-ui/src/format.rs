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
        let mut parts = Vec::new();
        if let Some(ref fg) = self.fg {
            parts.push(format!("color:{fg}"));
        }
        if let Some(ref bg) = self.bg {
            parts.push(format!("background:{bg}"));
        }
        if self.bold {
            parts.push("font-weight:bold".to_string());
        }
        if self.italic {
            parts.push("font-style:italic".to_string());
        }
        if self.underline {
            parts.push("text-decoration:underline".to_string());
        }
        if self.dim {
            parts.push("opacity:0.5".to_string());
        }
        parts.join(";")
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
                _ => {
                    current.push('%');
                    i += 1;
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
                    fg = fg_str.parse::<u8>().ok().and_then(mirc_color);
                    if i < len && chars[i] == ',' {
                        i += 1;
                        let mut bg_str = String::new();
                        while i < len && chars[i].is_ascii_digit() && bg_str.len() < 2 {
                            bg_str.push(chars[i]);
                            i += 1;
                        }
                        bg = bg_str.parse::<u8>().ok().and_then(mirc_color);
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

/// Convert a mIRC color code (0-15) to a CSS hex color.
fn mirc_color(code: u8) -> Option<String> {
    let hex = match code {
        0 => "ffffff",
        1 => "000000",
        2 => "00007f",
        3 => "009300",
        4 => "ff0000",
        5 => "7f0000",
        6 => "9c009c",
        7 => "fc7f00",
        8 => "ffff00",
        9 => "00fc00",
        10 => "009393",
        11 => "00ffff",
        12 => "0000fc",
        13 => "ff00ff",
        14 => "7f7f7f",
        15 => "d2d2d2",
        _ => return None,
    };
    Some(format!("#{hex}"))
}

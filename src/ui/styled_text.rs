use ratatui::text::{Line, Span};
use ratatui::style::{Color, Style, Modifier};
use crate::theme::StyledSpan;

/// Convert a slice of `StyledSpan`s (from the theme parser) into a ratatui `Line`.
pub fn styled_spans_to_line(spans: &[StyledSpan]) -> Line<'static> {
    let ratatui_spans: Vec<Span<'static>> = spans
        .iter()
        .map(|s| {
            let mut style = Style::default();
            if let Some(fg) = s.fg {
                style = style.fg(fg);
            }
            if let Some(bg) = s.bg {
                style = style.bg(bg);
            }
            let mut modifiers = Modifier::empty();
            if s.bold { modifiers |= Modifier::BOLD; }
            if s.italic { modifiers |= Modifier::ITALIC; }
            if s.underline { modifiers |= Modifier::UNDERLINED; }
            if s.dim { modifiers |= Modifier::DIM; }
            if !modifiers.is_empty() {
                style = style.add_modifier(modifiers);
            }
            Span::styled(s.text.clone(), style)
        })
        .collect();
    Line::from(ratatui_spans)
}

/// Convert a slice of `StyledSpan`s into a ratatui `Line`, using `default_fg` when a span has no fg color.
pub fn styled_spans_to_line_with_fg(spans: &[StyledSpan], default_fg: Color) -> Line<'static> {
    let ratatui_spans: Vec<Span<'static>> = spans
        .iter()
        .map(|s| {
            let mut style = Style::default().fg(s.fg.unwrap_or(default_fg));
            if let Some(bg) = s.bg {
                style = style.bg(bg);
            }
            let mut modifiers = Modifier::empty();
            if s.bold { modifiers |= Modifier::BOLD; }
            if s.italic { modifiers |= Modifier::ITALIC; }
            if s.underline { modifiers |= Modifier::UNDERLINED; }
            if s.dim { modifiers |= Modifier::DIM; }
            if !modifiers.is_empty() {
                style = style.add_modifier(modifiers);
            }
            Span::styled(s.text.clone(), style)
        })
        .collect();
    Line::from(ratatui_spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn converts_plain_span() {
        let spans = vec![StyledSpan {
            text: "hello".into(),
            fg: None, bg: None,
            bold: false, italic: false, underline: false, dim: false,
        }];
        let line = styled_spans_to_line(&spans);
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "hello");
    }

    #[test]
    fn converts_bold_colored_span() {
        let spans = vec![StyledSpan {
            text: "colored".into(),
            fg: Some(Color::Rgb(0x7a, 0xa2, 0xf7)),
            bg: None,
            bold: true, italic: false, underline: false, dim: false,
        }];
        let line = styled_spans_to_line(&spans);
        let style = line.spans[0].style;
        assert_eq!(style.fg, Some(Color::Rgb(0x7a, 0xa2, 0xf7)));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn converts_multiple_spans() {
        let spans = vec![
            StyledSpan {
                text: "a".into(), fg: None, bg: None,
                bold: false, italic: false, underline: false, dim: false,
            },
            StyledSpan {
                text: "b".into(), fg: Some(Color::Red), bg: None,
                bold: false, italic: true, underline: false, dim: false,
            },
        ];
        let line = styled_spans_to_line(&spans);
        assert_eq!(line.spans.len(), 2);
        assert!(line.spans[1].style.add_modifier.contains(Modifier::ITALIC));
    }
}

//! Image preview overlay — renders a centered popup with the preview image.
//!
//! Drawn as the last layer in the render pipeline, on top of all other UI
//! elements. The popup area is cleared before rendering the border and
//! image content.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::App;
use crate::image_preview::PreviewStatus;
use crate::theme::hex_to_color;

/// Render the image preview overlay, if active.
///
/// This should be called after all other UI elements have been rendered so
/// the popup appears on top. When the preview status is `Hidden`, this is a
/// no-op.
pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let iterm2_tmux = app.in_tmux
        && app.picker.protocol_type() == ratatui_image::picker::ProtocolType::Iterm2;

    match &mut app.image_preview {
        PreviewStatus::Hidden => {}
        PreviewStatus::Loading { url } => {
            render_loading(frame, area, &app.theme.colors, url);
        }
        PreviewStatus::Ready {
            title,
            image,
            width,
            height,
            ..
        } => {
            render_ready(
                frame, area, &app.theme.colors, title.as_deref(),
                &mut *image, *width, *height, iterm2_tmux,
            );
        }
        PreviewStatus::Error { url, message } => {
            render_error(frame, area, &app.theme.colors, url, message);
        }
    }
}

/// Center a rectangle of `width x height` within `area`.
fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

/// Truncate a string to `max_len` chars, appending "..." if truncated.
fn truncate_title(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else if max_len <= 3 {
        s.chars().take(max_len).collect()
    } else {
        let truncated: String = s.chars().take(max_len - 3).collect();
        format!("{truncated}...")
    }
}

/// Render the "Loading image..." popup.
fn render_loading(
    frame: &mut Frame,
    area: Rect,
    colors: &crate::theme::ThemeColors,
    url: &str,
) {
    let bg_alt = hex_to_color(&colors.bg_alt).unwrap_or(Color::Reset);
    let border_color = hex_to_color(&colors.border).unwrap_or(Color::DarkGray);
    let fg_muted = hex_to_color(&colors.fg_muted).unwrap_or(Color::Gray);
    let accent = hex_to_color(&colors.accent).unwrap_or(Color::Blue);

    let popup_width = 40_u16.min(area.width.saturating_sub(4));
    let popup_height = 5_u16.min(area.height.saturating_sub(2));
    let popup_area = centered_rect(area, popup_width, popup_height);

    frame.render_widget(Clear, popup_area);

    let title_text = truncate_title(url, usize::from(popup_width.saturating_sub(4)));
    let block = Block::default()
        .title(Line::from(title_text).style(Style::default().fg(accent)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(bg_alt));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let loading_text = Paragraph::new("Loading image...")
        .alignment(Alignment::Center)
        .style(Style::default().fg(fg_muted).bg(bg_alt));
    frame.render_widget(loading_text, inner);
}

/// Render the image preview popup with the decoded image.
///
/// When `iterm2_tmux` is true, the image content is skipped here — it will
/// be written directly to stdout after `terminal.draw()` completes, matching
/// how kokoirc handles iTerm2+tmux (direct `writeSync` bypassing the TUI
/// buffer). The popup border and title are still rendered through ratatui.
#[expect(clippy::too_many_arguments, reason = "render params are all needed")]
fn render_ready(
    frame: &mut Frame,
    area: Rect,
    colors: &crate::theme::ThemeColors,
    title: Option<&str>,
    image: &mut ratatui_image::protocol::StatefulProtocol,
    width: u16,
    height: u16,
    iterm2_tmux: bool,
) {
    let bg_alt = hex_to_color(&colors.bg_alt).unwrap_or(Color::Reset);
    let border_color = hex_to_color(&colors.border).unwrap_or(Color::DarkGray);
    let accent = hex_to_color(&colors.accent).unwrap_or(Color::Blue);

    let popup_area = centered_rect(area, width, height);

    frame.render_widget(Clear, popup_area);

    let title_text = title
        .map(|t| truncate_title(t, usize::from(popup_area.width.saturating_sub(4))))
        .unwrap_or_default();

    let block = Block::default()
        .title(Line::from(title_text).style(Style::default().fg(accent)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(bg_alt));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    if iterm2_tmux {
        // Skip ratatui-image widget — image will be written directly to
        // stdout after the frame completes (see App::write_iterm2_direct).
        // Fill the inner area with bg so there's no stale text behind.
        let fill = Paragraph::new("").style(Style::default().bg(bg_alt));
        frame.render_widget(fill, inner);
    } else {
        // Standard path: render via ratatui-image StatefulImage widget.
        let stateful_image = ratatui_image::StatefulImage::default();
        frame.render_stateful_widget(stateful_image, inner, image);
    }
}

/// Render an error popup.
fn render_error(
    frame: &mut Frame,
    area: Rect,
    colors: &crate::theme::ThemeColors,
    url: &str,
    message: &str,
) {
    let bg_alt = hex_to_color(&colors.bg_alt).unwrap_or(Color::Reset);
    let border_color = hex_to_color(&colors.border).unwrap_or(Color::DarkGray);
    let error_color = Color::Rgb(0xf7, 0x76, 0x8e); // soft red

    let popup_width = 50_u16.min(area.width.saturating_sub(4));
    let popup_height = 5_u16.min(area.height.saturating_sub(2));
    let popup_area = centered_rect(area, popup_width, popup_height);

    frame.render_widget(Clear, popup_area);

    let title_text = truncate_title(url, usize::from(popup_width.saturating_sub(4)));
    let block = Block::default()
        .title(
            Line::from(title_text)
                .style(Style::default().fg(error_color).add_modifier(Modifier::BOLD)),
        )
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(bg_alt));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let error_text = Paragraph::new(message.to_string())
        .alignment(Alignment::Center)
        .style(Style::default().fg(error_color).bg(bg_alt));
    frame.render_widget(error_text, inner);
}

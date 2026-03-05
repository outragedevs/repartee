use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::config::StatusbarItem;
use crate::state::buffer::{ActivityLevel, BufferType};
use crate::theme::hex_to_color;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    if !app.config.statusbar.enabled {
        return;
    }

    let colors = &app.theme.colors;
    let fg_muted = hex_to_color(&colors.fg_muted).unwrap_or(Color::DarkGray);
    let fg_dim = hex_to_color(&colors.fg_dim).unwrap_or(Color::DarkGray);
    let accent = hex_to_color(&colors.accent).unwrap_or(Color::Cyan);

    let separator = &app.config.statusbar.separator;

    // Get active buffer's connection
    let active_buf = app.state.active_buffer();
    let conn = active_buf.and_then(|b| app.state.connections.get(&b.connection_id));

    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::styled("[", Style::default().fg(fg_dim)));

    for (i, item) in app.config.statusbar.items.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                separator.clone(),
                Style::default().fg(fg_dim),
            ));
        }
        match item {
            StatusbarItem::Time => {
                let time = chrono::Local::now()
                    .format(&app.config.general.timestamp_format)
                    .to_string();
                spans.push(Span::styled(time, Style::default().fg(fg_muted)));
            }
            StatusbarItem::NickInfo => {
                let nick = conn.map(|c| c.nick.as_str()).unwrap_or("?");
                let modes = conn.map(|c| &c.user_modes).filter(|m| !m.is_empty());
                spans.push(Span::styled(nick.to_string(), Style::default().fg(accent)));
                if let Some(modes) = modes {
                    spans.push(Span::styled(
                        format!("(+{})", modes),
                        Style::default().fg(fg_muted),
                    ));
                }
            }
            StatusbarItem::ChannelInfo => {
                if let Some(buf) = active_buf {
                    let name_color = match buf.buffer_type {
                        BufferType::Channel => accent,
                        BufferType::Query => Color::Rgb(0xe0, 0xaf, 0x68), // yellow
                        _ => fg_muted,
                    };
                    spans.push(Span::styled(
                        buf.name.clone(),
                        Style::default().fg(name_color),
                    ));
                    if let Some(modes) = &buf.modes {
                        spans.push(Span::styled(
                            format!("(+{})", modes),
                            Style::default().fg(fg_muted),
                        ));
                    }
                }
            }
            StatusbarItem::Lag => {
                if let Some(lag) = conn.and_then(|c| c.lag) {
                    let secs = lag as f64 / 1000.0;
                    let lag_color = if lag > 5000 {
                        Color::Rgb(0xf7, 0x76, 0x8e)
                    } else if lag > 2000 {
                        Color::Rgb(0xe0, 0xaf, 0x68)
                    } else {
                        Color::Rgb(0x9e, 0xce, 0x6a)
                    };
                    spans.push(Span::styled("Lag: ", Style::default().fg(fg_muted)));
                    spans.push(Span::styled(
                        format!("{secs:.1}s"),
                        Style::default().fg(lag_color),
                    ));
                }
            }
            StatusbarItem::ActiveWindows => {
                let sorted_ids = app.state.sorted_buffer_ids();
                let active_id = app.state.active_buffer_id.as_deref();
                let mut activity_spans: Vec<Span> = Vec::new();

                for (idx, id) in sorted_ids.iter().enumerate() {
                    if active_id == Some(id.as_str()) {
                        continue;
                    }
                    let buf = match app.state.buffers.get(id.as_str()) {
                        Some(b) => b,
                        None => continue,
                    };
                    if buf.activity == ActivityLevel::None {
                        continue;
                    }

                    let color = match buf.activity {
                        ActivityLevel::Mention => Color::Rgb(0xbb, 0x9a, 0xf7),
                        ActivityLevel::Highlight => Color::Rgb(0xf7, 0x76, 0x8e),
                        ActivityLevel::Activity => Color::Rgb(0xe0, 0xaf, 0x68),
                        _ => Color::Rgb(0x9e, 0xce, 0x6a),
                    };

                    if !activity_spans.is_empty() {
                        activity_spans
                            .push(Span::styled(",", Style::default().fg(fg_dim)));
                    }
                    activity_spans.push(Span::styled(
                        format!("{}", idx + 1),
                        Style::default().fg(color),
                    ));
                }

                if !activity_spans.is_empty() {
                    spans.push(Span::styled("Act: ", Style::default().fg(fg_muted)));
                    spans.extend(activity_spans);
                }
            }
        }
    }

    spans.push(Span::styled("]", Style::default().fg(fg_dim)));

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}

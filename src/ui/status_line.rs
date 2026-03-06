use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::config::StatusbarItem;
use crate::state::buffer::{ActivityLevel, BufferType};
use crate::theme::hex_to_color;

#[expect(clippy::too_many_lines, reason = "single render function iterating status bar items")]
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    if !app.config.statusbar.enabled {
        return;
    }

    let colors = &app.theme.colors;
    let fg = hex_to_color(&colors.fg).unwrap_or(Color::White);
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
                let nick = conn.map_or("?", |c| c.nick.as_str());
                let modes = conn.map(|c| &c.user_modes).filter(|m| !m.is_empty());
                spans.push(Span::styled(nick.to_string(), Style::default().fg(accent)));
                if let Some(modes) = modes {
                    spans.push(Span::styled(
                        format!("(+{modes})"),
                        Style::default().fg(fg_muted),
                    ));
                }
            }
            StatusbarItem::ChannelInfo => {
                if let Some(buf) = active_buf {
                    let name_color = match buf.buffer_type {
                        BufferType::Channel => accent,
                        BufferType::Query => fg,
                        _ => fg_muted,
                    };
                    spans.push(Span::styled(
                        buf.name.clone(),
                        Style::default().fg(name_color),
                    ));
                    if let Some(modes) = &buf.modes {
                        spans.push(Span::styled(
                            format!("(+{modes})"),
                            Style::default().fg(fg_muted),
                        ));
                    }
                }
            }
            StatusbarItem::Lag => {
                if let Some(lag) = conn.and_then(|c| c.lag) {
                    #[expect(clippy::cast_precision_loss, reason = "lag in ms will never exceed f64 mantissa")]
                    let secs = lag as f64 / 1000.0;
                    let lag_color = if lag > 5000 {
                        accent
                    } else if lag > 2000 {
                        fg_muted
                    } else {
                        fg
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
                let mut win_num = 1u32; // Real buffers start at 1

                for id in &sorted_ids {
                    let Some(buf) = app.state.buffers.get(id.as_str()) else {
                        continue;
                    };
                    // Skip default Status buffer
                    if buf.connection_id == crate::app::App::DEFAULT_CONN_ID {
                        continue;
                    }
                    let current_num = win_num;
                    win_num += 1;

                    if active_id == Some(id.as_str()) {
                        continue;
                    }
                    if buf.activity == ActivityLevel::None {
                        continue;
                    }

                    let color = match buf.activity {
                        ActivityLevel::Mention | ActivityLevel::Highlight => accent,
                        ActivityLevel::Activity => fg,
                        _ => fg_muted,
                    };

                    if !activity_spans.is_empty() {
                        activity_spans
                            .push(Span::styled(",", Style::default().fg(fg_dim)));
                    }
                    activity_spans.push(Span::styled(
                        current_num.to_string(),
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

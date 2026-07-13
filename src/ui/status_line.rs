use std::collections::HashMap;
use std::time::Instant;

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::config::{StatusbarConfig, StatusbarItem};
use crate::state::AppState;
use crate::state::buffer::{ActivityLevel, BufferType};
use crate::theme::hex_to_color;

/// The four theme colours the status line paints with.
#[derive(Debug, Clone, Copy)]
struct Palette {
    fg: Color,
    fg_muted: Color,
    fg_dim: Color,
    accent: Color,
}

/// Everything the status line reads, borrowed.
///
/// [`render`] builds one from `&App`; [`status_spans`] — where all the layout
/// lives — takes only this. That split is what makes the span sequence testable:
/// `App::new` touches disk (config, theme, `SQLite`) and has no test constructor,
/// so nothing that takes an `&App` can be exercised by a unit test.
struct StatusCtx<'a> {
    statusbar: &'a StatusbarConfig,
    timestamp_format: &'a str,
    state: &'a AppState,
    shell_mgr: &'a crate::shell::ShellManager,
    lag_pings: &'a HashMap<String, Instant>,
    palette: Palette,
}

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let colors = &app.theme.colors;
    let palette = Palette {
        fg: hex_to_color(&colors.fg).unwrap_or(Color::Reset),
        fg_muted: hex_to_color(&colors.fg_muted).unwrap_or(Color::DarkGray),
        fg_dim: hex_to_color(&colors.fg_dim).unwrap_or(Color::DarkGray),
        accent: hex_to_color(&colors.accent).unwrap_or(Color::Cyan),
    };

    if app.log_browser_mode {
        // Log mode has its own layout and ignores `statusbar.enabled` — it is
        // the only thing telling the user how to get out again.
        render_log_status(
            frame,
            area,
            app,
            palette.fg,
            palette.fg_muted,
            palette.accent,
        );
        return;
    }

    let ctx = StatusCtx {
        statusbar: &app.config.statusbar,
        timestamp_format: &app.config.general.timestamp_format,
        state: &app.state,
        shell_mgr: &app.shell_mgr,
        lag_pings: &app.lag_pings,
        palette,
    };
    let Some(spans) = status_spans(&ctx) else {
        return;
    };
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The whole status line as a span sequence, or `None` when the bar is off.
///
/// **Separators are positional.** A `|` is owed only *in front of an item that
/// actually rendered something*, and only when something came before it — which
/// cannot be decided per item in isolation, because an item does not know
/// whether it will produce spans until it has tried. So each item is rendered
/// first and the separator is inserted afterwards, at the position the item
/// started. The older `if i > 0` scheme keyed off the item's *index*, which
/// emitted a leading separator when the first item rendered nothing and a
/// doubled one around any empty item in the middle.
#[expect(
    clippy::too_many_lines,
    reason = "one match arm per status bar item; splitting them would scatter the layout"
)]
fn status_spans<'a>(ctx: &StatusCtx<'a>) -> Option<Vec<Span<'a>>> {
    if !ctx.statusbar.enabled {
        return None;
    }
    let Palette {
        fg,
        fg_muted,
        fg_dim,
        accent,
    } = ctx.palette;
    let separator = ctx.statusbar.separator.as_str();

    let active_buf = ctx.state.active_buffer();
    let conn = active_buf.and_then(|b| ctx.state.connections.get(&b.connection_id));

    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::styled("[", Style::default().fg(fg_dim)));

    for item in &ctx.statusbar.items {
        let start = spans.len();
        match item {
            StatusbarItem::Time => {
                let time = chrono::Local::now()
                    .format(ctx.timestamp_format)
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
                    // Shell buffers show "shell: label" instead of channel name.
                    if buf.buffer_type == BufferType::Shell {
                        let label = ctx
                            .shell_mgr
                            .session_id_for_buffer(&buf.id)
                            .and_then(|sid| ctx.shell_mgr.label(sid))
                            .unwrap_or("shell");
                        spans.push(Span::styled(
                            format!("shell: {label}"),
                            Style::default().fg(accent),
                        ));
                    } else {
                        let name_color = match buf.buffer_type {
                            BufferType::Channel => accent,
                            BufferType::Query => fg,
                            _ => fg_muted,
                        };
                        spans.push(Span::styled(
                            buf.name.clone(),
                            Style::default().fg(name_color),
                        ));
                        if let Some(modes) = &buf.modes
                            && !modes.is_empty()
                        {
                            // Append param values for modes that have them (l=limit, k=key)
                            let param_str: String = modes
                                .chars()
                                .filter_map(|ch| {
                                    buf.mode_params
                                        .as_ref()
                                        .and_then(|mp| mp.get(&ch.to_string()))
                                        .map(String::as_str)
                                })
                                .collect::<Vec<_>>()
                                .join(" ");
                            let display = if param_str.is_empty() {
                                format!("(+{modes})")
                            } else {
                                format!("(+{modes} {param_str})")
                            };
                            spans.push(Span::styled(display, Style::default().fg(fg_muted)));
                        }
                    }
                }
            }
            StatusbarItem::Typing => {
                if let Some(buf) = active_buf {
                    let nicks = ctx.state.typing.nicks(&buf.id);
                    if let Some(phrase) = typing_phrase(&nicks) {
                        spans.push(Span::styled(phrase, Style::default().fg(fg_muted)));
                    }
                }
            }
            StatusbarItem::Lag => {
                if let Some(c) = conn {
                    if c.lag_pending {
                        // Show live elapsed time with "?" while waiting for PONG
                        if let Some(sent_at) = ctx.lag_pings.get(c.id.as_str()) {
                            #[expect(clippy::cast_precision_loss, reason = "elapsed ms fits f64")]
                            let elapsed_secs = sent_at.elapsed().as_millis() as f64 / 1000.0;
                            spans.push(Span::styled("Lag: ", Style::default().fg(fg_muted)));
                            spans.push(Span::styled(
                                format!("{elapsed_secs:.1}s (?)"),
                                Style::default().fg(accent),
                            ));
                        }
                    } else if let Some(lag) = c.lag {
                        #[expect(
                            clippy::cast_precision_loss,
                            reason = "lag in ms will never exceed f64 mantissa"
                        )]
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
            }
            StatusbarItem::ActiveWindows => {
                let sorted_ids = ctx.state.sorted_buffer_ids();
                let active_id = ctx.state.active_buffer_id.as_deref();
                let mut activity_spans: Vec<Span> = Vec::new();
                let mut win_num = 1u32; // Real buffers start at 1

                for id in &sorted_ids {
                    let Some(buf) = ctx.state.buffers.get(id.as_str()) else {
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
                        activity_spans.push(Span::styled(",", Style::default().fg(fg_dim)));
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
        // Only now, knowing whether the item produced anything, decide whether it
        // needs a separator in front of it. `start > 1` because spans[0] is the
        // opening `[` pushed before the loop.
        if spans.len() > start && start > 1 {
            spans.insert(start, Span::styled(separator, Style::default().fg(fg_dim)));
        }
    }

    spans.push(Span::styled("]", Style::default().fg(fg_dim)));
    Some(spans)
}

/// How the status line words a set of typing nicks. `None` when nobody is
/// typing — the caller then renders no item *and no separator*.
///
/// Duplicated verbatim in the web bundle
/// (`web-ui/src/components/status_line.rs`), which is a separate compilation
/// target and cannot share code with this one. The two are held in step by
/// `fixtures/typing_phrases.txt`, which BOTH crates' tests assert against:
/// reword one of them and the *other* crate's test fails.
fn typing_phrase(nicks: &[&str]) -> Option<String> {
    match nicks {
        [] => None,
        [one] => Some(format!("{one} is typing…")),
        [a, b] => Some(format!("{a} and {b} are typing…")),
        [a, b, c] => Some(format!("{a}, {b} and {c} are typing…")),
        [a, b, rest @ ..] => Some(format!("{a}, {b} and {} others are typing…", rest.len())),
    }
}

/// Status line in log-browser mode. Layout:
/// `log mode • <net>/<buf> • showing X/Y from <ts>  •  ↑/↓ scroll • / search • Q quit`
fn render_log_status(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    fg: Color,
    fg_muted: Color,
    accent: Color,
) {
    let (id_text, loaded, total, from) = app.state.active_buffer().map_or_else(
        || (String::from("(no buffer)"), 0, 0, String::from("(empty)")),
        |buf| {
            let id = buf
                .connection_id
                .strip_prefix(crate::app::App::LOG_CONN_PREFIX)
                .map_or_else(|| buf.id.clone(), |net| format!("{net}/{}", buf.name));
            let total = buf.log_total_lines.unwrap_or(0);
            // Count real DB rows only — `log_msg_id.is_some()` excludes
            // synthetic day separators and the local-event lines that
            // `/help`, `/search` results, and the slash-only hint emit.
            let loaded = buf
                .messages
                .iter()
                .filter(|m| m.log_msg_id.is_some())
                .count();
            // `from` is the timestamp of the oldest real message we've
            // loaded — that's what the user actually wants to see ("how
            // far back am I?"), not the timestamp of a synthetic
            // separator that happens to share the same date.
            let from = buf
                .messages
                .iter()
                .find(|m| m.log_msg_id.is_some())
                .map_or_else(
                    || String::from("(empty)"),
                    |m| m.timestamp.format("%Y-%m-%d %H:%M").to_string(),
                );
            (id, loaded, total, from)
        },
    );
    let sep = Span::styled("  \u{2022}  ", Style::default().fg(fg_muted));
    let line = Line::from(vec![
        Span::styled(
            "log mode",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        sep.clone(),
        Span::styled(id_text, Style::default().fg(accent)),
        sep.clone(),
        Span::styled(format!("showing {loaded}/{total}"), Style::default().fg(fg)),
        Span::styled(" from ", Style::default().fg(fg_muted)),
        Span::styled(from, Style::default().fg(fg)),
        sep,
        Span::styled(
            "↑/↓ scroll • / search • Q quit",
            Style::default().fg(fg_muted),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::buffer::{Buffer, BufferType};
    use crate::state::connection::{Connection, ConnectionStatus};

    // ── `typing_phrase`, pinned against the SHARED fixture ────────────────
    //
    // The web bundle has a verbatim copy of this function
    // (`web-ui/src/components/status_line.rs`) and cannot share code with it —
    // separate compilation targets. `fixtures/typing_phrases.txt` is the tie:
    // both crates assert against these literals, so rewording the phrase here
    // fails the WEB test, and rewording it there fails this one. The old
    // comment claimed the two were "kept in step by test"; no such test existed.

    const PHRASE_FIXTURE: &str = include_str!("../../fixtures/typing_phrases.txt");

    /// `(nicks, phrase)` for every case in the shared fixture.
    fn phrase_cases() -> Vec<(Vec<String>, String)> {
        let cases: Vec<(Vec<String>, String)> = PHRASE_FIXTURE
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|line| {
                let (nicks, phrase) = line.split_once(" => ").expect("`nicks => phrase`");
                (
                    nicks.split(',').map(str::to_string).collect(),
                    phrase.to_string(),
                )
            })
            .collect();
        assert!(!cases.is_empty(), "the shared fixture must not be empty");
        cases
    }

    #[test]
    fn phrases_match_the_shared_fixture() {
        for (nicks, expected) in phrase_cases() {
            let borrowed: Vec<&str> = nicks.iter().map(String::as_str).collect();
            assert_eq!(
                typing_phrase(&borrowed).as_deref(),
                Some(expected.as_str()),
                "{nicks:?} — the web bundle asserts this same literal"
            );
        }
    }

    #[test]
    fn no_typers_renders_nothing() {
        // Not in the fixture: there is no string here to drift.
        assert_eq!(typing_phrase(&[]), None);
    }

    // ── the span sequence, and where the separators land ─────────────────
    //
    // The separator scheme is shared by EVERY statusbar item, not just typing:
    // a `|` in front of an item that rendered something, and only if something
    // came before it. Nothing below is about typing specifically — a leading
    // separator when the first item renders nothing, a doubled one around an
    // empty item in the middle, a trailing one after an empty last item, are
    // all regressions in the pre-existing items that CI could not have seen.

    /// Owns the borrowed pieces a [`StatusCtx`] points at. `App::new` touches
    /// disk, so the ctx is assembled by hand.
    struct Fixture {
        state: AppState,
        statusbar: StatusbarConfig,
        shell_mgr: crate::shell::ShellManager,
        lag_pings: HashMap<String, Instant>,
        timestamp_format: String,
    }

    impl Fixture {
        /// One connection (`net`, nick `me`) with `#rust` active, and one idle
        /// `#tokio` alongside it.
        fn new(items: Vec<StatusbarItem>) -> Self {
            let mut state = AppState::new();
            state.add_connection(make_connection());
            state.add_buffer(make_buffer("#rust", BufferType::Channel));
            state.add_buffer(make_buffer("#tokio", BufferType::Channel));
            state.active_buffer_id = Some("net/#rust".to_string());

            let statusbar = StatusbarConfig {
                items,
                separator: "|".to_string(),
                ..StatusbarConfig::default()
            };

            let (shell_mgr, _rx) = crate::shell::ShellManager::new();
            Self {
                state,
                statusbar,
                shell_mgr,
                lag_pings: HashMap::new(),
                // Fixed, so the Time item's content is assertable.
                timestamp_format: "TIME".to_string(),
            }
        }

        fn ctx(&self) -> StatusCtx<'_> {
            StatusCtx {
                statusbar: &self.statusbar,
                timestamp_format: &self.timestamp_format,
                state: &self.state,
                shell_mgr: &self.shell_mgr,
                lag_pings: &self.lag_pings,
                palette: Palette {
                    fg: Color::Reset,
                    fg_muted: Color::DarkGray,
                    fg_dim: Color::DarkGray,
                    accent: Color::Cyan,
                },
            }
        }

        /// The rendered line as plain text, span by span.
        fn spans(&self) -> Vec<String> {
            status_spans(&self.ctx())
                .expect("the bar is enabled")
                .iter()
                .map(|s| s.content.to_string())
                .collect()
        }
    }

    fn make_buffer(name: &str, buffer_type: BufferType) -> Buffer {
        Buffer {
            id: format!("net/{name}"),
            connection_id: "net".to_string(),
            buffer_type,
            name: name.to_string(),
            messages: std::collections::VecDeque::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: chrono::Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
            last_speakers: Vec::new(),
            peer_handle: None,
            log_total_lines: None,
            log_oldest_ts: None,
            log_newest_ts: None,
            history_exhausted: false,
            log_initial_loaded: false,
            pin_backlog: false,
        }
    }

    fn make_connection() -> Connection {
        Connection {
            id: "net".to_string(),
            label: "NetServer".to_string(),
            status: ConnectionStatus::Connected,
            own_handle: None,
            nick: "me".to_string(),
            user_modes: String::new(),
            isupport: HashMap::new(),
            isupport_parsed: crate::irc::isupport::Isupport::new(),
            error: None,
            lag: None,
            lag_pending: false,
            reconnect_attempts: 0,
            reconnect_delay_secs: 30,
            next_reconnect: None,
            should_reconnect: true,
            joined_channels: Vec::new(),
            origin_config: crate::config::ServerConfig {
                label: "NetServer".to_string(),
                address: "irc.test.net".to_string(),
                port: 6697,
                tls: true,
                tls_verify: true,
                autoconnect: false,
                channels: vec![],
                nick: None,
                username: None,
                realname: None,
                password: None,
                sasl_user: None,
                sasl_pass: None,
                bind_ip: None,
                encoding: None,
                auto_reconnect: Some(true),
                reconnect_delay: None,
                reconnect_max_retries: None,
                autosendcmd: None,
                sasl_mechanism: None,
                client_cert_path: None,
            },
            local_ip: None,
            enabled_caps: std::collections::HashSet::new(),
            chathistory: crate::irc::chathistory::HistoryState::new(),
            who_token_counter: 0,
            silent_who_channels: std::collections::HashSet::new(),
            silent_banlist_channels: std::collections::HashSet::new(),
            multiline: None,
            batch_ref_counter: 0,
        }
    }

    #[test]
    fn every_default_item_renders_between_the_brackets() {
        // Everything on at once: the full default bar, one separator between
        // each pair of items and none anywhere else.
        let mut f = Fixture::new(StatusbarConfig::default().items);
        f.state
            .typing
            .set("net/#rust", "alice", crate::irc::typing::TypingState::Active, Instant::now());
        f.state.connections.get_mut("net").expect("conn").lag = Some(1234);
        f.state.buffers["net/#tokio"].activity = ActivityLevel::Activity;

        assert_eq!(
            f.spans(),
            vec![
                "[", "TIME", "|", "me", "|", "#rust", "|", "alice is typing…", "|", "Lag: ",
                "1.2s", "|", "Act: ", "2", "]",
            ]
        );
    }

    #[test]
    fn an_item_that_renders_nothing_takes_no_separator_with_it() {
        // Typing sits between nick_info and lag and nobody is typing. The
        // separator count must drop by one, not stay put — a `|` in front of an
        // item that produced no spans is a doubled separator on screen.
        let f = Fixture::new(vec![
            StatusbarItem::NickInfo,
            StatusbarItem::Typing,
            StatusbarItem::ChannelInfo,
        ]);
        assert_eq!(f.spans(), vec!["[", "me", "|", "#rust", "]"]);
    }

    #[test]
    fn an_empty_first_item_leaves_no_leading_separator() {
        // The bug the `i > 0` scheme had: it keyed the separator off the item's
        // INDEX, so a first item that rendered nothing still let the second one
        // open with `[|`.
        let f = Fixture::new(vec![StatusbarItem::Typing, StatusbarItem::NickInfo]);
        assert_eq!(f.spans(), vec!["[", "me", "]"]);
    }

    #[test]
    fn an_empty_last_item_leaves_no_trailing_separator() {
        let f = Fixture::new(vec![StatusbarItem::NickInfo, StatusbarItem::Typing]);
        assert_eq!(f.spans(), vec!["[", "me", "]"]);
    }

    #[test]
    fn a_single_item_gets_no_separator_at_all() {
        let f = Fixture::new(vec![StatusbarItem::NickInfo]);
        assert_eq!(f.spans(), vec!["[", "me", "]"]);
    }

    #[test]
    fn an_empty_item_list_renders_bare_brackets() {
        let f = Fixture::new(vec![]);
        assert_eq!(f.spans(), vec!["[", "]"]);
    }

    #[test]
    fn a_bar_where_nothing_renders_is_bare_brackets_too() {
        // Every item silent: no separators, no stray `|`.
        let f = Fixture::new(vec![
            StatusbarItem::Typing,
            StatusbarItem::Lag,
            StatusbarItem::ActiveWindows,
        ]);
        assert_eq!(f.spans(), vec!["[", "]"]);
    }

    #[test]
    fn a_disabled_bar_renders_no_line_at_all() {
        let mut f = Fixture::new(vec![StatusbarItem::NickInfo]);
        f.statusbar.enabled = false;
        assert!(status_spans(&f.ctx()).is_none());
    }

    #[test]
    fn channel_info_renders_the_server_buffers_name() {
        // A server buffer is not a channel, but it still has a name to show —
        // `channel_info` is not empty here, and must keep its separator.
        let mut f = Fixture::new(vec![StatusbarItem::NickInfo, StatusbarItem::ChannelInfo]);
        f.state
            .add_buffer(make_buffer("NetServer", BufferType::Server));
        f.state.active_buffer_id = Some("net/NetServer".to_string());
        assert_eq!(f.spans(), vec!["[", "me", "|", "NetServer", "]"]);
    }

    #[test]
    fn channel_info_renders_nothing_without_an_active_buffer() {
        let mut f = Fixture::new(vec![StatusbarItem::ChannelInfo, StatusbarItem::NickInfo]);
        f.state.active_buffer_id = None;
        // No active buffer means no connection either, hence the `?` nick.
        assert_eq!(f.spans(), vec!["[", "?", "]"]);
    }

    #[test]
    fn channel_modes_and_user_modes_ride_with_their_item() {
        // Both are extra spans pushed by an item that already rendered — they
        // must not attract a separator of their own.
        let mut f = Fixture::new(vec![StatusbarItem::NickInfo, StatusbarItem::ChannelInfo]);
        f.state
            .connections
            .get_mut("net")
            .expect("conn")
            .user_modes = "iw".to_string();
        f.state.buffers["net/#rust"].modes = Some("nt".to_string());
        assert_eq!(
            f.spans(),
            vec!["[", "me", "(+iw)", "|", "#rust", "(+nt)", "]"]
        );
    }

    #[test]
    fn items_render_in_the_configured_order() {
        // `/items move` reorders the config; the bar must follow it verbatim.
        let f = Fixture::new(vec![
            StatusbarItem::ChannelInfo,
            StatusbarItem::Time,
            StatusbarItem::NickInfo,
        ]);
        assert_eq!(f.spans(), vec!["[", "#rust", "|", "TIME", "|", "me", "]"]);
    }
}

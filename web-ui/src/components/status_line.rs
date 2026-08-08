use leptos::prelude::*;

use crate::state::AppState;

fn current_time() -> String {
    let date = js_sys::Date::new_0();
    let h = date.get_hours();
    let m = date.get_minutes();
    let s = date.get_seconds();
    format!("{h:02}:{m:02}:{s:02}")
}

#[component]
pub fn StatusLine() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();

    let (time_str, set_time_str) = signal(current_time());

    // Update the clock every second. Cancelled via on_cleanup on unmount.
    let clock_alive = StoredValue::new(true);
    on_cleanup(move || clock_alive.set_value(false));
    leptos::task::spawn_local(async move {
        loop {
            gloo_timers::future::sleep(std::time::Duration::from_secs(1)).await;
            if !clock_alive.get_value() {
                break;
            }
            set_time_str.set(current_time());
        }
    });

    let active_buf = move || {
        let active_id = state.active_buffer.get()?;
        state.buffers.get().into_iter().find(|b| b.id == active_id)
    };

    let active_conn = move || {
        let buf = active_buf()?;
        state
            .connections
            .get()
            .into_iter()
            .find(|c| c.id == buf.connection_id)
    };

    // Activity numbers — must use the exact numbering the buffer list
    // displays (position in the same sorted vec, 1-based, nothing skipped).
    // The old version skipped server buffers while counting, so with one
    // network every Act number was off by one ("[Act:3]" while the traffic
    // was on window 4), and off by N with N networks.
    let activity_items = move || {
        let active_id = state.active_buffer.get();
        state
            .buffers
            .with(|bufs| activity_numbers(bufs, active_id.as_deref()))
    };

    // One closure for the whole line, because the separators are positional:
    // a `|` is owed only *between* items that actually produced content, so
    // nothing can be decided per-item in isolation. It re-runs when the
    // statusbar config, buffers, connections or typing set change — the same
    // signals the per-item closures used to read individually. The clock is
    // deliberately NOT read here (it stays nested below), so ticking the second
    // does not rebuild the line.
    view! {
        {move || {
            let items = status_items(
                state.statusbar_enabled.get(),
                &state.statusbar_items.get(),
            );
            if items.is_empty() {
                // `statusbar.enabled = false` (or an empty item list): no line
                // at all, matching the TUI's early return.
                return None;
            }

            let mut spans: Vec<AnyView> = Vec::new();
            for item in items {
                let content: Option<AnyView> = match item {
                    StatusItem::Time => Some(
                        // Nested closure: only this text node re-renders per second.
                        view! { <span class="muted">{move || time_str.get()}</span> }.into_any(),
                    ),
                    StatusItem::NickInfo => active_conn().map(|c| {
                        let modes = if c.user_modes.is_empty() {
                            String::new()
                        } else {
                            format!("(+{})", c.user_modes)
                        };
                        view! {
                            <span class="nick">{c.nick}</span>
                            <span class="muted">{modes}</span>
                        }
                        .into_any()
                    }),
                    StatusItem::ChannelInfo => active_buf().map(|b| {
                        let modes = b.modes.as_deref()
                            .filter(|m| !m.is_empty())
                            .map(|m| format!("(+{m})"))
                            .unwrap_or_default();
                        view! {
                            <span class="nick">{b.name}</span>
                            <span class="muted">{modes}</span>
                        }
                        .into_any()
                    }),
                    StatusItem::Typing => active_buf()
                        .and_then(|buf| state.typing.get().get(&buf.id).cloned())
                        .and_then(|nicks| typing_phrase(&nicks))
                        .map(|phrase| {
                            view! { <span class="muted">{phrase}</span> }.into_any()
                        }),
                    StatusItem::Lag => active_conn().and_then(|conn| conn.lag).map(|lag| {
                        #[expect(clippy::cast_precision_loss, reason = "u64 lag ms to f64 seconds, precision loss acceptable")]
                        let secs = lag as f64 / 1000.0;
                        view! {
                            <span class="muted">"Lag: "</span>
                            <span class="nick">{format!("{secs:.1}s")}</span>
                        }
                        .into_any()
                    }),
                    StatusItem::ActiveWindows => {
                        let windows = activity_items();
                        if windows.is_empty() {
                            None
                        } else {
                            Some(view! {
                                <span class="muted">"Act: "</span>
                                {windows.into_iter().enumerate().map(|(i, (num, level, id))| {
                                    // Clamp unknown levels to the highest tier, matching
                                    // the buffer list's `activity-4` fallback.
                                    let class = match level {
                                        1 => "act-green",
                                        2 => "act-red",
                                        3 => "act-yellow",
                                        _ => "act-purple",
                                    };
                                    let sep = if i > 0 { "," } else { "" };
                                    let on_click = move |_| state.switch_to_buffer(&id);
                                    view! {
                                        <span class="sep">{sep}</span>
                                        <button type="button"
                                            class=format!("act-num {class}")
                                            title="Jump to this window"
                                            on:click=on_click
                                        >{num.to_string()}</button>
                                    }
                                }).collect::<Vec<_>>()}
                            }
                            .into_any())
                        }
                    }
                };

                // Separator only in front of an item that produced something,
                // and only if something came before it — exactly the TUI's rule
                // (`src/ui/status_line.rs`: insert the separator after the fact,
                // once the item is known to have pushed spans).
                if let Some(content) = content {
                    if !spans.is_empty() {
                        spans.push(view! { <span class="sep">"|"</span> }.into_any());
                    }
                    spans.push(content);
                }
            }

            Some(view! {
                <div class="status-line">
                    <span class="bracket">"["</span>
                    {spans}
                    <span class="bracket">"]"</span>
                </div>
            })
        }}
    }
}

/// One rendered status-line item. The wire carries these as the names `/items`
/// uses; this enum is the client's view of that vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusItem {
    Time,
    NickInfo,
    ChannelInfo,
    Typing,
    Lag,
    ActiveWindows,
}

impl StatusItem {
    /// Mirrors `parse_statusbar_item` in `src/commands/handlers_ui.rs`.
    /// `None` for a name this bundle does not know.
    fn parse(name: &str) -> Option<Self> {
        match name {
            "time" => Some(Self::Time),
            "nick_info" => Some(Self::NickInfo),
            "channel_info" => Some(Self::ChannelInfo),
            "typing" => Some(Self::Typing),
            "lag" => Some(Self::Lag),
            "active_windows" => Some(Self::ActiveWindows),
            _ => None,
        }
    }
}

/// The items to render, in the server's configured order — the DOM-free core of
/// the status line.
///
/// `enabled == false` renders nothing at all (the TUI returns early on
/// `statusbar.enabled`, and the two must agree). An unrecognised name is
/// skipped rather than fatal: an older bundle must survive a newer core that
/// has learned a new item.
#[must_use]
pub fn status_items(enabled: bool, names: &[String]) -> Vec<StatusItem> {
    if !enabled {
        return Vec::new();
    }
    names
        .iter()
        .filter_map(|name| StatusItem::parse(name))
        .collect()
}

/// How the status line words a set of typing nicks. `None` when nobody is
/// typing — the caller then renders no item *and no separator*.
///
/// Duplicated verbatim from the core (`src/ui/status_line.rs::typing_phrase`),
/// which is a separate compilation target and cannot share code with this one.
/// The two are held in step by `fixtures/typing_phrases.txt`, which BOTH crates'
/// tests assert against: reword one of them and the *other* crate's test fails.
fn typing_phrase(nicks: &[String]) -> Option<String> {
    match nicks {
        [] => None,
        [one] => Some(format!("⌨ {one} is typing…")),
        [a, b] => Some(format!("⌨ {a} and {b} are typing…")),
        [a, b, c] => Some(format!("⌨ {a}, {b} and {c} are typing…")),
        [a, b, rest @ ..] => Some(format!("⌨ {a}, {b} and {} others are typing…", rest.len())),
    }
}

/// `(display_number, activity_level, buffer_id)` for every non-active buffer
/// with pending activity. `display_number` is the buffer's 1-based position in
/// the sorted buffer vec — identical to the number the buffer list renders
/// next to it, which is the whole point: "Act: 4" must mean "window 4".
fn activity_numbers(
    buffers: &[crate::protocol::BufferMeta],
    active_id: Option<&str>,
) -> Vec<(u32, u8, String)> {
    crate::state::numbered_buffers(buffers)
        .filter(|(_, b)| b.activity != 0 && active_id != Some(b.id.as_str()))
        .map(|(num, b)| (num, b.activity, b.id.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::BufferMeta;

    fn buf(id: &str, buffer_type: &str, activity: u8) -> BufferMeta {
        BufferMeta {
            id: id.to_string(),
            connection_id: "net".to_string(),
            name: id.to_string(),
            buffer_type: buffer_type.to_string(),
            topic: None,
            unread_count: 0,
            activity,
            nick_count: 0,
            modes: None,
            e2e_enabled: false,
        }
    }

    #[test]
    fn numbers_match_buffer_list_positions() {
        // The tester's bug: activity on the 4th listed window reported as
        // "Act: 3" because server buffers were skipped while counting. The
        // number must be the 1-based position in the same vec the buffer
        // list renders.
        let buffers = vec![
            buf("mentions", "mentions", 0),
            buf("srv", "server", 0),
            buf("#a", "channel", 0),
            buf("#b", "channel", 2),
        ];
        let items = activity_numbers(&buffers, Some("#a"));
        assert_eq!(items, vec![(4, 2, "#b".to_string())]);
    }

    #[test]
    fn server_buffers_with_activity_are_listed() {
        let buffers = vec![buf("srv", "server", 1), buf("#a", "channel", 0)];
        let items = activity_numbers(&buffers, Some("#a"));
        assert_eq!(items, vec![(1, 1, "srv".to_string())]);
    }

    #[test]
    fn active_buffer_and_idle_buffers_are_excluded() {
        let buffers = vec![
            buf("#a", "channel", 3), // active — excluded even with activity
            buf("#b", "channel", 0), // idle — excluded
            buf("#c", "channel", 4),
        ];
        let items = activity_numbers(&buffers, Some("#a"));
        assert_eq!(items, vec![(3, 4, "#c".to_string())]);
    }

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn items_render_in_the_configured_order() {
        // The status line is rendered from the server's `statusbar.items`, not
        // from a sequence hardcoded here — that hardcoding is what made
        // `/items move|remove` and `statusbar.enabled` no-ops in the browser.
        let items = status_items(
            true,
            &names(&[
                "time",
                "nick_info",
                "channel_info",
                "typing",
                "lag",
                "active_windows",
            ]),
        );
        assert_eq!(
            items,
            vec![
                StatusItem::Time,
                StatusItem::NickInfo,
                StatusItem::ChannelInfo,
                StatusItem::Typing,
                StatusItem::Lag,
                StatusItem::ActiveWindows,
            ]
        );
    }

    #[test]
    fn a_reordered_config_is_honoured_verbatim() {
        let items = status_items(true, &names(&["lag", "typing", "time"]));
        assert_eq!(
            items,
            vec![StatusItem::Lag, StatusItem::Typing, StatusItem::Time]
        );
    }

    #[test]
    fn a_removed_typing_item_is_not_rendered() {
        // `/items remove typing` in the terminal must switch it off in the tab.
        let items = status_items(true, &names(&["time", "lag"]));
        assert!(!items.contains(&StatusItem::Typing));
    }

    #[test]
    fn a_disabled_statusbar_renders_nothing() {
        let items = status_items(false, &names(&["time", "typing", "lag"]));
        assert!(items.is_empty());
    }

    #[test]
    fn an_unknown_item_name_is_skipped_not_fatal() {
        // An older bundle against a newer core: a name it has never heard of
        // must not take the whole status line down with it.
        let items = status_items(true, &names(&["time", "quantum_flux", "typing"]));
        assert_eq!(items, vec![StatusItem::Time, StatusItem::Typing]);
    }

    // ── `typing_phrase`, pinned against the SHARED fixture ────────────────
    //
    // This function is a verbatim copy of the TUI's (`src/ui/status_line.rs`)
    // and cannot share code with it — separate compilation targets.
    // `fixtures/typing_phrases.txt` is the tie: both crates assert against
    // these literals, so rewording the phrase here fails the TUI test, and
    // rewording it there fails this one. `make test` runs both.
    //
    // Before this, each crate asserted its own copy against its own hard-coded
    // strings, and the comment claiming they were "kept in step by test" was
    // simply false — the web copy could silently drift.

    const PHRASE_FIXTURE: &str = include_str!("../../../fixtures/typing_phrases.txt");

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
    fn typing_phrases_match_the_tui() {
        for (nicks, expected) in phrase_cases() {
            assert_eq!(
                typing_phrase(&nicks).as_deref(),
                Some(expected.as_str()),
                "{nicks:?} — the TUI asserts this same literal"
            );
        }
    }

    #[test]
    fn no_typers_renders_nothing() {
        // Not in the fixture: there is no string here to drift.
        assert_eq!(typing_phrase(&[]), None);
    }
}

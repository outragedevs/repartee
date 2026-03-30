use leptos::prelude::*;

use super::buffer_list::BufferList;
use super::chat_view::ChatView;
use super::input::InputLine;
use super::nick_list::NickList;
use super::status_line::StatusLine;
use super::topic_bar::TopicBar;
use crate::protocol::WebCommand;
use crate::state::AppState;

/// Root layout component — renders desktop (>=768px) or mobile (<768px).
#[component]
pub fn Layout() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();
    let (left_open, set_left_open) = signal(false);
    let (right_open, set_right_open) = signal(false);

    // Auto-fetch messages and nick list whenever active buffer changes
    // or after a resync (lag recovery / reconnect clears backlog_loaded).
    //
    // Uses `backlog_loaded` (not `has_messages`) to decide whether to fetch:
    // a buffer may have live NewMessage events cached without ever having
    // its DB backlog loaded — checking messages.is_empty() would skip the
    // fetch and show an incomplete buffer.
    Effect::new(move || {
        let _sync = state.sync_version.get(); // subscribe to resync events
        if let Some(buf_id) = state.active_buffer.get() {
            let already_loaded = state
                .backlog_loaded
                .get_untracked()
                .contains(&buf_id);
            if !already_loaded {
                crate::ws::send_command(&WebCommand::FetchMessages {
                    buffer_id: buf_id.clone(),
                    limit: 100,
                    before: None,
                });
            }
            crate::ws::send_command(&WebCommand::FetchNickList {
                buffer_id: buf_id,
            });
        }
    });

    // Auto-close left panel when active buffer changes.
    Effect::new(move || {
        let _ = state.active_buffer.get();
        set_left_open.set(false);
    });

    let active_buf = move || {
        let active_id = state.active_buffer.get()?;
        state.buffers.get().into_iter().find(|b| b.id == active_id)
    };

    // Hide nick list for shell buffers (shells have no users to list).
    let is_shell_buffer = move || {
        active_buf()
            .map(|b| b.buffer_type == "shell")
            .unwrap_or(false)
    };

    let mention_count = move || state.mention_count.get();

    // Swipe gesture state.
    let (touch_start_x, set_touch_start_x) = signal(0i32);
    let (touch_start_y, set_touch_start_y) = signal(0i32);

    let on_touch_start = move |ev: web_sys::TouchEvent| {
        if let Some(touch) = ev.touches().get(0) {
            set_touch_start_x.set(touch.client_x());
            set_touch_start_y.set(touch.client_y());
        }
    };

    let on_touch_end = move |ev: web_sys::TouchEvent| {
        let Some(touch) = ev.changed_touches().get(0) else {
            return;
        };
        let dx = touch.client_x() - touch_start_x.get_untracked();
        let dy = touch.client_y() - touch_start_y.get_untracked();

        // Only horizontal swipes (|dx| > |dy|) with minimum 50px distance.
        if dx.abs() < 50 || dy.abs() > dx.abs() {
            return;
        }

        if dx > 0 {
            if right_open.get_untracked() {
                set_right_open.set(false);
            } else if !left_open.get_untracked() {
                set_left_open.set(true);
            }
        } else if dx < 0 {
            if left_open.get_untracked() {
                set_left_open.set(false);
            } else if !right_open.get_untracked() {
                set_right_open.set(true);
            }
        }
    };

    view! {
        <div class="app">
            // Desktop layout
            <div class="desktop-only">
                <TopicBar />
                <div class="main-area">
                    <BufferList />
                    <ChatView />
                    {move || (!is_shell_buffer()).then(|| view! { <NickList /> })}
                </div>
                <div class="bottom-bar">
                    <StatusLine />
                    <InputLine />
                    <ThemePicker />
                </div>
            </div>

            // Mobile layout
            <div class="mobile-only"
                on:touchstart=on_touch_start
                on:touchend=on_touch_end
            >
                <div class="mobile-topbar">
                    <span class="hamburger" on:click=move |_| set_left_open.set(true)>"\u{2630}"</span>
                    <div class="mobile-topbar-center">
                        {move || active_buf().map(|b| {
                            let modes = b.modes.as_deref()
                                .filter(|m| !m.is_empty())
                                .map(|m| format!(" (+{m})"))
                                .unwrap_or_default();
                            let topic = b.topic.as_deref().unwrap_or("");
                            let topic_end = topic.char_indices()
                                .nth(30)
                                .map_or(topic.len(), |(i, _)| i);
                            let topic_short = &topic[..topic_end];
                            view! {
                                <span class="mobile-chan">{b.name}{modes}</span>
                                {(!topic.is_empty()).then(|| view! {
                                    <span class="mobile-topic">{format!(" — {topic_short}")}</span>
                                })}
                            }
                        })}
                    </div>
                    <div class="mobile-topbar-right">
                        {move || {
                            let count = mention_count();
                            (count > 0).then(|| view! {
                                <span class="mention-badge">{count.to_string()}</span>
                            })
                        }}
                        <span class="nicklist-btn" on:click=move |_| set_right_open.set(true)>
                            "\u{1F465}"
                        </span>
                    </div>
                </div>
                <ChatView />
                <div class="bottom-bar">
                    <StatusLine />
                    <InputLine />
                </div>

                // Slide-out panels — always in DOM, toggled via CSS class.
                <div class="slide-overlay" class:visible=left_open
                    on:click=move |_| set_left_open.set(false)></div>
                <div class="slide-panel-left" class:open=left_open>
                    <div class="slide-panel-header">
                        <span style="color: var(--accent); font-weight: bold;">"Buffers"</span>
                        {move || {
                            let count = mention_count();
                            (count > 0).then(|| view! {
                                <span class="mention-badge">{format!("{count} mentions")}</span>
                            })
                        }}
                    </div>
                    <BufferList />
                    <ThemePicker />
                </div>

                <div class="slide-overlay" class:visible=right_open
                    on:click=move |_| set_right_open.set(false)></div>
                <div class="slide-panel-right" class:open=right_open>
                    <div class="slide-panel-header">
                        {move || active_buf().map(|b| {
                            view! {
                                <span style="color: var(--accent); font-weight: bold;">{b.name}</span>
                                <span style="color: var(--fg-muted); font-size: 10px; margin-left: 6px;">
                                    {format!("{} users", b.nick_count)}
                                </span>
                            }
                        })}
                    </div>
                    <NickList />
                </div>
            </div>
        </div>
    }
}

/// Theme picker — shows swatches for each theme.
#[component]
fn ThemePicker() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();

    let themes = [
        ("nightfall", "#1a1b26"),
        ("catppuccin-mocha", "#1e1e2e"),
        ("tokyo-storm", "#24283b"),
        ("gruvbox-light", "#fbf1c7"),
        ("catppuccin-latte", "#eff1f5"),
    ];

    view! {
        <div class="theme-picker">
            {themes.iter().map(|(name, color)| {
                let name_owned = (*name).to_string();
                let name_for_click = name_owned.clone();
                let is_active = move || state.theme.get() == name_owned;
                view! {
                    <div
                        class=move || if is_active() { "theme-swatch active" } else { "theme-swatch" }
                        style=format!("background: {color};")
                        title=*name
                        on:click=move |_| state.theme.set(name_for_click.clone())
                    ></div>
                }
            }).collect::<Vec<_>>()}
        </div>
    }
}

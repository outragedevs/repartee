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

    // Auto-fetch messages and nick list whenever active buffer changes.
    Effect::new(move || {
        if let Some(buf_id) = state.active_buffer.get() {
            // Only fetch from DB if we don't already have messages for this buffer.
            let has_messages = state
                .messages
                .get_untracked()
                .get(&buf_id)
                .is_some_and(|msgs| !msgs.is_empty());
            if !has_messages {
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

    let active_buf = move || {
        let active_id = state.active_buffer.get()?;
        state.buffers.get().into_iter().find(|b| b.id == active_id)
    };

    let mention_count = move || state.mention_count.get();

    view! {
        <div class="app">
            // Desktop layout
            <div class="desktop-only">
                <TopicBar />
                <div class="main-area">
                    <div class="sidebar-left">
                        <BufferList />
                        <ThemePicker />
                    </div>
                    <ChatView />
                    <NickList />
                </div>
                <div class="bottom-bar">
                    <StatusLine />
                    <InputLine />
                </div>
            </div>

            // Mobile layout
            <div class="mobile-only">
                <div class="mobile-topbar">
                    <span class="hamburger" on:click=move |_| set_left_open.set(true)>"\u{2630}"</span>
                    <div style="text-align: center; flex: 1; overflow: hidden; white-space: nowrap;">
                        {move || active_buf().map(|b| view! {
                            <span style="color: var(--accent); font-weight: bold;">{b.name}</span>
                        })}
                    </div>
                    <div style="display: flex; gap: 6px; align-items: center;">
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

                // Slide-out buffer list (left)
                {move || left_open.get().then(|| view! {
                    <div class="slide-overlay" on:click=move |_| set_left_open.set(false)></div>
                    <div class="slide-panel-left open">
                        <div style="padding: 4px 10px; border-bottom: 1px solid var(--border); display: flex; align-items: center; justify-content: space-between;">
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
                })}

                // Slide-out nick list (right)
                {move || right_open.get().then(|| view! {
                    <div class="slide-overlay" on:click=move |_| set_right_open.set(false)></div>
                    <div class="slide-panel-right open">
                        <div style="padding: 4px 10px; border-bottom: 1px solid var(--border);">
                            {move || active_buf().map(|b| {
                                let user_count = format!("{} users", b.nick_count);
                                view! {
                                    <span style="color: var(--accent); font-weight: bold;">{b.name}</span>
                                    <span style="color: var(--fg-muted); font-size: 10px; margin-left: 6px;">
                                        {user_count}
                                    </span>
                                }
                            })}
                        </div>
                        <NickList />
                    </div>
                })}
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
                let name = (*name).to_string();
                let name_for_active = name.clone();
                let name_for_click = name.clone();
                let is_active = move || state.theme.get() == name_for_active;
                view! {
                    <div
                        class=move || format!("theme-swatch{}", if is_active() { " active" } else { "" })
                        style=format!("background: {color};")
                        title=name
                        on:click=move |_| state.theme.set(name_for_click.clone())
                    ></div>
                }
            }).collect::<Vec<_>>()}
        </div>
    }
}

use leptos::prelude::*;

use super::appearance::{AppearanceButton, AppearanceModal};
use super::buffer_list::BufferList;
use super::chat_view::ChatView;
use super::emoji_picker::EmojiPicker;
use super::emote_picker::EmotePicker;
use super::input::InputLine;
use super::nick_list::NickList;
use super::status_line::StatusLine;
use super::topic_bar::TopicBar;
use super::wizard::ServerWizard;
use crate::protocol::WebCommand;
use crate::state::AppState;

/// Root layout component — renders desktop (>=768px) or mobile (<768px).
#[component]
pub fn Layout() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();

    // Auto-fetch messages and nick list whenever active buffer changes
    // or after a resync (lag recovery / reconnect clears backlog_loaded).
    //
    // Uses `backlog_loaded` (not `has_messages`) to decide whether to fetch:
    // a buffer may have live NewMessage events cached without ever having
    // its DB backlog loaded — checking messages.is_empty() would skip the
    // fetch and show an incomplete buffer.
    //
    // In-flight FetchMessages dedup keyed by (buffer_id, sync_version):
    // without it, rapid signal writes during SyncInit (or rapid clicks on
    // the same buffer within one connection epoch) re-fire this Effect
    // before backlog_loaded is updated, sending duplicate Fetch requests
    // whose responses are then both prepended → duplicated lines on screen.
    // sync_version is read untracked — it's a dedup key, not a trigger.
    let pending = StoredValue::new(std::collections::HashSet::<(String, u32)>::new());
    Effect::new(move || {
        let Some(buf_id) = state.active_buffer.get() else {
            return;
        };
        let epoch = state.sync_version.get_untracked();
        let key = (buf_id.clone(), epoch);
        let already_loaded = state.backlog_loaded.get_untracked().contains(&buf_id);
        let already_pending = pending.with_value(|s| s.contains(&key));
        if !already_loaded && !already_pending {
            pending.update_value(|s| {
                s.insert(key);
            });
            crate::ws::send_command(&WebCommand::FetchMessages {
                buffer_id: buf_id.clone(),
                limit: 100,
                before: None,
                before_id: None,
            });
        }
        crate::ws::send_command(&WebCommand::FetchNickList { buffer_id: buf_id });
    });

    let escape_handle =
        leptos::leptos_dom::helpers::window_event_listener(leptos::ev::keydown, move |event| {
            if event.key() != "Escape" {
                return;
            }
            if state.appearance_open.get_untracked() {
                state.appearance_open.set(false);
            } else if state.emoji_picker_open.get_untracked() {
                state.emoji_picker_open.set(false);
            } else if state.emote_picker_open.get_untracked() {
                state.emote_picker_open.set(false);
            } else if state.wizard_open.get_untracked() {
                state.wizard_open.set(false);
            } else {
                return;
            }
            event.prevent_default();
        });
    on_cleanup(move || escape_handle.remove());

    view! {
        <div class="app">
            // Add-server wizard modal (fixed-position overlay; rendered once).
            <ServerWizard />
            // Emote/emoji picker + appearance modals (fixed-position
            // overlays; rendered once — never inside the transformed slide
            // panels, where position:fixed would break).
            <EmotePicker />
            <EmojiPicker />
            <AppearanceModal />
            // Backend error toast — surfaces any WebEvent::Error in the
            // authenticated app (e.g. a failed wizard save, whose modal has
            // already closed optimistically). Dismissible; also cleared on the
            // next WS reconnect. Without this the error is set on state but
            // only rendered by the login screen, so it stays invisible here.
            {move || state.error.get().map(|msg| view! {
                <div class="error-toast" role="alert">
                    <span class="error-toast-msg">{msg}</span>
                    <button type="button" class="error-toast-x" aria-label="Dismiss error"
                        on:click=move |_| state.error.set(None)>"\u{2715}"</button>
                </div>
            })}
            <div class="layout-host" inert=move || {
                state.appearance_open.get()
                    || state.emoji_picker_open.get()
                    || state.emote_picker_open.get()
                    || state.wizard_open.get()
            }>
                <ResponsiveLayout />
            </div>
        </div>
    }
}

#[component]
fn ResponsiveLayout() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();
    let (left_open, set_left_open) = signal(false);
    let (right_open, set_right_open) = signal(false);
    let (touch_start_x, set_touch_start_x) = signal(0i32);
    let (touch_start_y, set_touch_start_y) = signal(0i32);
    let (tracking_single_touch, set_tracking_single_touch) = signal(false);

    let active_buf = move || {
        let active_id = state.active_buffer.get()?;
        state.buffers.with(|buffers| {
            buffers
                .iter()
                .find(|buffer| buffer.id == active_id)
                .cloned()
        })
    };
    let has_nick_list = move || active_buf().is_some_and(|buffer| buffer.buffer_type != "shell");

    Effect::new(move || {
        let _ = state.active_buffer.get();
        set_left_open.set(false);
        set_right_open.set(false);
    });

    let panel_escape_handle =
        leptos::leptos_dom::helpers::window_event_listener(leptos::ev::keydown, move |event| {
            if event.key() != "Escape"
                || event.default_prevented()
                || state.appearance_open.get_untracked()
                || state.emoji_picker_open.get_untracked()
                || state.emote_picker_open.get_untracked()
                || state.wizard_open.get_untracked()
            {
                return;
            }
            if left_open.get_untracked() || right_open.get_untracked() {
                set_left_open.set(false);
                set_right_open.set(false);
                event.prevent_default();
            }
        });
    on_cleanup(move || panel_escape_handle.remove());

    let jump_to_mentions = move |_| {
        let target = state.buffers.with_untracked(|buffers| {
            buffers
                .iter()
                .find(|buffer| buffer.buffer_type == "mentions")
                .map(|buffer| buffer.id.clone())
        });
        if let Some(id) = target {
            state.switch_to_buffer(&id);
            crate::ws::send_command(&WebCommand::FetchMentions);
        }
    };

    let on_touch_start = move |event: web_sys::TouchEvent| {
        if event.touches().length() != 1 {
            set_tracking_single_touch.set(false);
            return;
        }
        if let Some(touch) = event.touches().get(0) {
            set_tracking_single_touch.set(true);
            set_touch_start_x.set(touch.client_x());
            set_touch_start_y.set(touch.client_y());
        }
    };
    let on_touch_end = move |event: web_sys::TouchEvent| {
        if !tracking_single_touch.get_untracked() {
            return;
        }
        set_tracking_single_touch.set(false);
        let Some(touch) = event.changed_touches().get(0) else {
            return;
        };
        let dx = touch.client_x() - touch_start_x.get_untracked();
        let dy = touch.client_y() - touch_start_y.get_untracked();
        if dx.abs() < 50 || dy.abs() > dx.abs() {
            return;
        }
        if dx > 0 {
            if right_open.get_untracked() {
                set_right_open.set(false);
            } else if !left_open.get_untracked() {
                set_left_open.set(true);
            }
        } else if left_open.get_untracked() {
            set_left_open.set(false);
        } else if !right_open.get_untracked() && has_nick_list() {
            set_right_open.set(true);
        }
    };

    view! {
        <div class="responsive-layout" on:touchstart=on_touch_start on:touchend=on_touch_end>
            <div class="desktop-topic">
                <TopicBar />
            </div>
            <div class="mobile-topbar">
                <button type="button" class="hamburger" aria-label="Open buffers"
                    aria-expanded=move || left_open.get()
                    on:click=move |_| set_left_open.set(true)>"\u{2630}"</button>
                <div class="mobile-topbar-center">
                    {move || active_buf().map(|buffer| {
                        let modes = buffer.modes.as_deref()
                            .filter(|modes| !modes.is_empty())
                            .map(|modes| format!(" (+{modes})"))
                            .unwrap_or_default();
                        let topic = crate::format::strip_format(buffer.topic.as_deref().unwrap_or(""));
                        let topic_end = topic.char_indices().nth(30).map_or(topic.len(), |(i, _)| i);
                        let topic_short = &topic[..topic_end];
                        let topic_full = topic.clone();
                        view! {
                            <span class="mobile-chan">{buffer.name}{modes}</span>
                            {(!topic.is_empty()).then(|| view! {
                                <span class="mobile-topic" title=topic_full>
                                    {format!(" — {topic_short}")}
                                </span>
                            })}
                        }
                    })}
                </div>
                <div class="mobile-topbar-right">
                    {move || {
                        let count = state.mention_count.get();
                        (count > 0).then(|| view! {
                            <button type="button" class="mention-badge" title="Open mentions"
                                on:click=jump_to_mentions>{count.to_string()}</button>
                        })
                    }}
                    {move || has_nick_list().then(|| view! {
                        <button type="button" class="nicklist-btn" aria-label="Open user list"
                            aria-expanded=move || right_open.get()
                            on:click=move |_| set_right_open.set(true)>"\u{1F465}"</button>
                    })}
                </div>
            </div>
            <div class="main-area">
                <BufferList />
                <ChatView />
                {move || has_nick_list().then(|| view! { <NickList /> })}
            </div>
            <div class="bottom-bar">
                <StatusLine />
                <InputLine />
                <div class="bar-tools desktop-tools">
                    <ThemePicker />
                    <AppearanceButton />
                </div>
            </div>

            <div class="slide-overlay" class:visible=left_open aria-hidden="true"
                on:click=move |_| set_left_open.set(false)></div>
            <aside class="slide-panel-left" class:open=left_open
                aria-label="Buffers" aria-hidden=move || !left_open.get()
                inert=move || !left_open.get()>
                <div class="slide-panel-header">
                    <span class="slide-panel-title">"Buffers"</span>
                    {move || {
                        let count = state.mention_count.get();
                        (count > 0).then(|| view! {
                            <button type="button" class="mention-badge" title="Open mentions"
                                on:click=jump_to_mentions>{format!("{count} mentions")}</button>
                        })
                    }}
                </div>
                <BufferList />
                <div class="bar-tools">
                    <ThemePicker />
                    <AppearanceButton />
                </div>
            </aside>

            {move || has_nick_list().then(|| view! {
                <div class="slide-overlay" class:visible=right_open aria-hidden="true"
                    on:click=move |_| set_right_open.set(false)></div>
                <aside class="slide-panel-right" class:open=right_open
                    aria-label="Users" aria-hidden=move || !right_open.get()
                    inert=move || !right_open.get()>
                    <div class="slide-panel-header">
                        {move || active_buf().map(|buffer| view! {
                            <span class="slide-panel-title">{buffer.name}</span>
                            <span class="slide-panel-count">{format!("{} users", buffer.nick_count)}</span>
                        })}
                    </div>
                    <NickList />
                </aside>
            })}
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
        ("spring", "#1a1a2e"),
        ("gruvbox-light", "#fbf1c7"),
        ("catppuccin-latte", "#eff1f5"),
    ];

    view! {
        <div class="theme-picker">
            {themes.iter().map(|(name, color)| {
                let name_owned = (*name).to_string();
                let name_for_click = name_owned.clone();
                let name_for_class = name_owned.clone();
                let name_for_pressed = name_owned;
                view! {
                    <button
                        type="button"
                        class=move || if state.theme.get() == name_for_class {
                            "theme-swatch active"
                        } else {
                            "theme-swatch"
                        }
                        style=format!("background: {color};")
                        title=*name
                        aria-label=format!("Use {name} theme")
                        aria-pressed=move || state.theme.get() == name_for_pressed
                        on:click=move |_| state.theme.set(name_for_click.clone())
                    ></button>
                }
            }).collect::<Vec<_>>()}
        </div>
    }
}

use leptos::prelude::*;

use super::buffer_list::BufferList;
use super::chat_view::ChatView;
use super::input::InputLine;
use super::nick_list::NickList;
use super::status_line::StatusLine;
use super::topic_bar::TopicBar;

/// Root layout component — renders desktop (≥768px) or mobile (<768px).
#[component]
pub fn Layout() -> impl IntoView {
    // For now, always render the desktop layout.
    // Mobile detection + slide-out panels will be refined iteratively.
    view! {
        <div class="app">
            <TopicBar />
            <div class="main-area">
                <BufferList />
                <ChatView />
                <NickList />
            </div>
            <div class="bottom-bar">
                <StatusLine />
                <InputLine />
            </div>
        </div>
    }
}

use leptos::prelude::*;

/// Root application component — placeholder for Phase 2.
///
/// Displays connection status and a login form.
/// The full desktop/mobile layouts will be implemented in Phase 2.
#[component]
pub fn App() -> impl IntoView {
    let (connected, set_connected) = signal(false);
    let (status, set_status) = signal("Not connected".to_string());

    view! {
        <div style="
            font-family: 'JetBrains Mono', 'Fira Code', 'SF Mono', monospace;
            background: #1a1b26;
            color: #a9b1d6;
            min-height: 100vh;
            display: flex;
            align-items: center;
            justify-content: center;
            flex-direction: column;
            gap: 16px;
        ">
            <h1 style="color: #7aa2f7; font-size: 24px; margin: 0;">
                "repartee"
            </h1>
            <p style="color: #565f89; font-size: 14px; margin: 0;">
                "web frontend — phase 2 coming soon"
            </p>
            <div style="
                background: #16161e;
                border: 1px solid #292e42;
                border-radius: 8px;
                padding: 24px;
                min-width: 300px;
                text-align: center;
            ">
                <div style="margin-bottom: 12px;">
                    <span style="color: #565f89;">"Status: "</span>
                    <span style=move || {
                        if connected.get() {
                            "color: #9ece6a;"
                        } else {
                            "color: #f7768e;"
                        }
                    }>
                        {move || status.get()}
                    </span>
                </div>
                <p style="color: #565f89; font-size: 12px; margin: 0;">
                    "The full UI (desktop + mobile layouts, themes, chat) will be implemented in Phase 2."
                </p>
            </div>
        </div>
    }
}

use leptos::prelude::*;

use crate::protocol::WebCommand;
use crate::state::AppState;

#[component]
pub fn InputLine() -> impl IntoView {
    let state = use_context::<AppState>().unwrap();
    let (value, set_value) = signal(String::new());

    let submit = Callback::new(move |_: ()| {
        let text = value.get();
        if text.is_empty() {
            return;
        }
        let Some(buffer_id) = state.active_buffer.get() else {
            return;
        };

        if text.starts_with('/') {
            crate::ws::send_command(&WebCommand::RunCommand {
                buffer_id,
                text: text.clone(),
            });
        } else {
            crate::ws::send_command(&WebCommand::SendMessage {
                buffer_id,
                text: text.clone(),
            });
        }

        set_value.set(String::new());
    });

    view! {
        <div class="input-line">
            <span class="prompt">"❯"</span>
            <input
                type="text"
                placeholder="Type a message..."
                prop:value=value
                on:input=move |ev| set_value.set(event_target_value(&ev))
                on:keydown=move |ev| { if ev.key() == "Enter" { submit.run(()) } }
            />
            <button class="send-btn" on:click=move |_| submit.run(())>"Send"</button>
        </div>
    }
}

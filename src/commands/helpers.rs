use crate::app::App;
use crate::state::buffer::{Message, MessageType};
use chrono::Utc;

pub fn add_local_event(app: &mut App, text: &str) {
    let Some(active_id) = app.state.active_buffer_id.clone() else {
        return;
    };
    let id = app.state.next_message_id();
    app.state.add_message(
        &active_id,
        Message {
            id,
            timestamp: Utc::now(),
            message_type: MessageType::Event,
            nick: None,
            nick_mode: None,
            text: text.to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
        },
    );
}

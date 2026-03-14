use crate::app::App;
use crate::state::buffer::{Message, MessageType};
use chrono::Utc;

pub fn add_local_event(app: &mut App, text: &str) {
    let Some(active_id) = app.state.active_buffer_id.as_deref() else {
        return;
    };
    let active_id = active_id.to_string();
    let id = app.state.next_message_id();
    app.state.add_local_message(
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
            log_msg_id: None,
            log_ref_id: None,
            tags: std::collections::HashMap::new(),
        },
    );
}

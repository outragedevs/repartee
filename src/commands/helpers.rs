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
            log_key: None,
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
            tags: None,
            wire_origin: None,
        },
    );
}

/// Say so when the config asks for translation that this process cannot
/// provide.
///
/// The backend and the worker queues are built once in `App::new`, so turning
/// `translate.enabled` on at runtime — by `/set` or by editing the file and
/// running `/reload` — cannot materialise one. Both routes have to warn, or
/// the quieter of the two silently contradicts the documented restart
/// requirement and the user believes translation is running when it is not.
/// Shared rather than duplicated for the reason `/reload` and `/set` drifted
/// apart over typing once already.
pub fn warn_if_translate_needs_restart(app: &mut App) {
    if !app.config.translate.enabled || app.state.translate_active {
        return;
    }
    add_local_event(
        app,
        &format!(
            "{warn}translate: enabled but no backend was built \
             at startup — restart to activate{rst}",
            warn = crate::commands::types::C_ERR,
            rst = crate::commands::types::C_RST,
        ),
    );
}

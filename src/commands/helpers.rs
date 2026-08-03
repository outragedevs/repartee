use crate::app::App;
use crate::state::buffer::{Message, MessageType};
use chrono::Utc;

/// Make text safe to interpolate into an event row's format string.
///
/// An `Event` row with no `event_key` is handed to `parse_format_string`
/// whole, which reads it in two passes and each has a way to eat characters:
///
/// - `substitute_vars` consumes `$0`–`$9`, `$*` and `$[N]D`. With no params
///   they expand to nothing, so "costs $5" renders as "costs ".
/// - the format walk consumes `%N` (reset), `%_` (bold), `%Zaabbcc` (colour —
///   six further characters with it), and the rest. `printf("%i", n)` renders
///   as `printf("", n)`.
///
/// That is correct for the codes WE put in a row and wrong for everything
/// that came from a user, a peer, or a server. It matters most where the row
/// is the last copy of something: a refused outgoing message is restored to
/// the composer only while the composer is still empty (see
/// `restore_input_text_to`), so seconds after the user pressed Enter the row
/// is routinely the ONLY place their text survives, and a copy that quietly
/// drops characters is not one they can retype from.
///
/// Doubling is the escape both passes define — `$$` for a literal `$` (irssi
/// `special_vars`), `%%` for a literal `%` — and the web UI's renderer reads
/// `%%` the same way, so escaped text is correct in either front end.
#[must_use]
pub fn escape_format(text: &str) -> String {
    text.replace('%', "%%")
}

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

/// Say so when the config asks for translation that this process is not
/// doing — and say WHICH of the reasons it is.
///
/// The backend and the worker queues are built once in `App::new`, so NAMING
/// a translator at runtime — by `/set` or by editing the file and running
/// `/reload` — cannot materialise one. Both routes have to warn, or the
/// quieter of the two silently contradicts the documented restart requirement
/// and the user believes translation is running when it is not. Shared rather
/// than duplicated for the reason `/reload` and `/set` drifted apart over
/// typing once already.
///
/// Restarting is only ever the answer when a translator IS named. Told to
/// restart after `/set translate.backend none`, the user restarts into
/// exactly the same silence — and the advice is worse than useless there,
/// because it contradicts the thing they just asked for.
pub fn warn_if_translate_cannot_run(app: &mut App) {
    use crate::translate::backend::{BackendKind, backend_kind};
    if !app.config.translate.enabled || app.state.translate_active {
        return;
    }
    let warn = crate::commands::types::C_ERR;
    let rst = crate::commands::types::C_RST;
    let row = match backend_kind(&app.config.translate.backend) {
        // Not a contradiction — a config that names no translator and
        // translates nothing is consistent. Still said out loud, because
        // `translate.enabled = true` with per-buffer flags set otherwise
        // looks exactly like a working setup.
        BackendKind::None => format!(
            "{warn}translate: enabled, but translate.backend is \"none\" — no \
             translator is installed, so nothing is translated{rst}"
        ),
        BackendKind::Unknown => format!(
            "{warn}translate: unknown translate.backend \"{name}\" — no \
             translator is installed. Known: {known}{rst}",
            name = escape_format(app.config.translate.backend.trim()),
            known = crate::translate::backend::BACKEND_NAMES.join(", "),
        ),
        // Named rather than a wildcard so that adding a real broker to
        // `BackendKind` stops the compiler here and makes somebody decide
        // what this says about it, instead of inheriting the stub's wording.
        BackendKind::Stub => format!(
            "{warn}translate: {name} is configured but no backend was built \
             at startup — restart to activate{rst}",
            name = escape_format(app.config.translate.backend.trim()),
        ),
    };
    add_local_event(app, &row);
}

#[cfg(test)]
mod tests {
    /// What the browser's renderer would show. The web UI's parser lives in a
    /// separate crate, so the shape of its `%%` handling is mirrored here —
    /// the two front ends display the same rows and a copy the user is meant
    /// to retype from cannot be correct in one and wrong in the other.
    fn as_web_renders(text: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '%' if i + 1 < chars.len() && chars[i + 1] == '%' => {
                    out.push('%');
                    i += 2;
                }
                c => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        out
    }

    #[test]
    fn escaped_text_renders_back_to_itself_in_both_front_ends() {
        // `%` is the only sign that needs escaping. `$` is NOT: with no params
        // the variable pass is skipped entirely (see
        // `theme::parser::substitute_vars`), so a dollar in an event row is a
        // dollar. Escaping it was a mistake that reached the web renderer and
        // ate `echo $$` in ordinary chat.
        for original in [
            "printf(\"%i\", n)",
            "costs $5",
            "echo $$",
            "$* and $[3]0",
            "%Z112233 red %N reset %_bold",
            "100% sure",
            "literal %% and $$ signs",
            "nothing special here",
        ] {
            let escaped = super::escape_format(original);
            let tui: String = crate::theme::parser::parse_format_string(&escaped, &[])
                .iter()
                .map(|s| s.text.as_str())
                .collect();
            assert_eq!(tui, original, "TUI mangled {original:?} (escaped: {escaped:?})");
            assert_eq!(
                as_web_renders(&escaped),
                original,
                "web mangled {original:?} (escaped: {escaped:?})"
            );
        }
    }
}

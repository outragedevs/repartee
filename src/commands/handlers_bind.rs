use crate::app::App;
use crate::keybindings::{ACTIONS, Binding};

use super::helpers::{add_local_event, escape_format};

fn word(input: &str) -> (&str, &str) {
    let input = input.trim_start();
    input
        .find(char::is_whitespace)
        .map_or((input, ""), |at| (&input[..at], input[at..].trim_start()))
}

pub fn cmd_bind(app: &mut App, args: &[String]) {
    let input = args.join(" ");
    let (first, rest) = word(&input);
    if first == "-list" {
        add_local_event(app, &ACTIONS.join(", "));
        return;
    }
    let mut draft = app.config.clone();
    if matches!(first, "-delete" | "-reset") {
        let (key, extra) = word(rest);
        if key.is_empty() || !extra.is_empty() {
            add_local_event(app, "Usage: /bind -delete|-reset <key>");
            return;
        }
        if first == "-delete" {
            draft.keyboard.remove(key);
        } else {
            draft.keyboard.reset(key);
        }
    } else {
        let (action, data) = word(rest);
        if action.is_empty() {
            for (key, binding) in draft.keyboard.effective() {
                if first.is_empty() || key.contains(first) {
                    add_local_event(
                        app,
                        &escape_format(&format!("{key} {} {}", binding.action, binding.data)),
                    );
                }
            }
            return;
        }
        if first.starts_with('-') {
            add_local_event(app, "Unknown /bind option; use -list, -delete or -reset");
            return;
        }
        let binding = action.strip_prefix('/').map_or_else(
            || Binding::new(action, data),
            |command| {
                Binding::new(
                    "command",
                    &if data.is_empty() {
                        command.to_string()
                    } else {
                        format!("{command} {data}")
                    },
                )
            },
        );
        if let Err(error) = draft.keyboard.set(first, binding) {
            add_local_event(app, &escape_format(&error));
            return;
        }
    }
    if let Err(error) = draft.keyboard.compile() {
        add_local_event(app, &escape_format(&error));
        return;
    }
    if let Err(error) = crate::config::save_config(&app.config_path, &draft) {
        add_local_event(
            app,
            &escape_format(&format!("Binding was not changed: {error}")),
        );
        return;
    }
    app.config.keyboard = draft.keyboard;
    app.publish_keyboard_bindings();
    add_local_event(app, "Keyboard binding saved");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_command_persists_tombstones_and_rejects_failed_saves() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = crate::app::input::submit_typing_tests::test_app();
        app.config_path = directory.path().join("config.toml");
        cmd_bind(&mut app, &["meta-q /msg someone two  spaces".into()]);
        let config = crate::config::load_config(&app.config_path).unwrap();
        assert_eq!(
            config.keyboard.bindings["meta-q"].data,
            "msg someone two  spaces"
        );
        cmd_bind(&mut app, &["-delete meta-a".into()]);
        let config = crate::config::load_config(&app.config_path).unwrap();
        assert!(!config.keyboard.effective().contains_key("meta-a"));
        let before = app.config.keyboard.clone();
        app.config_path = directory.path().to_path_buf();
        cmd_bind(&mut app, &["-reset meta-a".into()]);
        assert_eq!(app.config.keyboard, before);
        cmd_bind(&mut app, &["meta-x unsupported_action".into()]);
        assert_eq!(app.config.keyboard, before);
    }
}

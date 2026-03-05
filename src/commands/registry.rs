use crate::app::App;

pub type CommandHandler = fn(&mut App, &[String]);

pub struct CommandDef {
    pub handler: CommandHandler,
    pub description: &'static str,
    pub usage: &'static str,
}

pub fn get_commands() -> Vec<(&'static str, CommandDef)> {
    vec![
        (
            "quit",
            CommandDef {
                handler: cmd_quit,
                description: "Quit the client",
                usage: "/quit [message]",
            },
        ),
        (
            "help",
            CommandDef {
                handler: cmd_help,
                description: "Show command list",
                usage: "/help [command]",
            },
        ),
        (
            "clear",
            CommandDef {
                handler: cmd_clear,
                description: "Clear active buffer",
                usage: "/clear",
            },
        ),
        (
            "close",
            CommandDef {
                handler: cmd_close,
                description: "Close active buffer",
                usage: "/close",
            },
        ),
    ]
}

pub fn get_command_names() -> Vec<&'static str> {
    get_commands().iter().map(|(name, _)| *name).collect()
}

fn cmd_quit(app: &mut App, _args: &[String]) {
    app.should_quit = true;
}

fn cmd_help(app: &mut App, _args: &[String]) {
    let commands = get_commands();
    let mut help_text = String::from("Available commands:");
    for (_name, def) in &commands {
        help_text.push_str(&format!("\n  {} — {}", def.usage, def.description));
    }

    super::helpers::add_local_event(app, &help_text);
}

fn cmd_clear(app: &mut App, _args: &[String]) {
    if let Some(buf) = app.state.active_buffer_mut() {
        buf.messages.clear();
    }
}

fn cmd_close(app: &mut App, _args: &[String]) {
    if let Some(active_id) = app.state.active_buffer_id.clone() {
        app.state.remove_buffer(&active_id);
    }
}

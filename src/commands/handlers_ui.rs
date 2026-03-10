#![allow(clippy::redundant_pub_crate)]

use crate::app::App;
use super::helpers::add_local_event;
use super::types::{
    divider, C_CMD, C_DIM, C_ERR, C_HEADER, C_OK, C_RST, C_TEXT, CATEGORY_ORDER,
};

pub(crate) fn cmd_quit(app: &mut App, args: &[String]) {
    if !args.is_empty() {
        app.quit_message = Some(args[0].clone());
    }
    app.should_quit = true;
    // QUIT is sent once in the post-loop cleanup (App::run) to avoid
    // double-QUIT which triggers "Excess Flood" on strict servers.
}

pub(crate) fn cmd_help(app: &mut App, args: &[String]) {
    if args.is_empty() {
        show_command_list(app);
    } else {
        let name = args[0].strip_prefix('/').unwrap_or(&args[0]).to_lowercase();
        show_command_help(app, &name);
    }
}

fn show_command_list(app: &mut App) {
    let commands = super::registry::get_commands();

    add_local_event(app, &divider("Commands"));

    for &cat in CATEGORY_ORDER {
        let cmds_in_cat: Vec<_> = commands
            .iter()
            .filter(|(_, def)| def.category == cat)
            .collect();
        if cmds_in_cat.is_empty() {
            continue;
        }

        add_local_event(app, &format!("  {C_HEADER}[{}]{C_RST}", cat.label()));
        for (name, def) in &cmds_in_cat {
            let aliases = if def.aliases.is_empty() {
                String::new()
            } else {
                format!(" {C_DIM}({}){C_RST}", def.aliases.join(", "))
            };
            add_local_event(
                app,
                &format!("    {C_CMD}/{name}{C_RST}{aliases} {C_DIM}{}{C_RST}", def.description),
            );
        }
    }

    add_local_event(app, "");
    add_local_event(
        app,
        &format!("  {C_DIM}Type {C_CMD}/help <command>{C_DIM} for detailed help.{C_RST}"),
    );
    add_local_event(app, &divider(""));
}

fn show_command_help(app: &mut App, name: &str) {
    let commands = super::registry::get_commands();

    // Find by name or alias
    let found = commands.iter().find(|(cmd_name, def)| {
        *cmd_name == name || def.aliases.contains(&name)
    });

    let Some((cmd_name, def)) = found else {
        add_local_event(
            app,
            &format!("{C_ERR}Unknown command: /{name}. Type /help for a list.{C_RST}"),
        );
        return;
    };

    // Try loading detailed help from docs/commands/*.md
    let doc = super::docs::help(cmd_name);

    add_local_event(app, &divider(&format!("/{cmd_name}")));

    // Description — prefer doc, fall back to registry
    let description = doc.map_or(def.description, |d| d.description.as_str());
    add_local_event(app, &format!("  {C_TEXT}{description}{C_RST}"));
    add_local_event(app, "");

    // Syntax from doc
    if let Some(d) = doc
        && !d.syntax.is_empty()
    {
        for line in d.syntax.lines() {
            add_local_event(app, &format!("  {C_CMD}{line}{C_RST}"));
        }
    }

    if !def.aliases.is_empty() {
        let alias_list: Vec<String> = def.aliases.iter().map(|a| format!("/{a}")).collect();
        add_local_event(
            app,
            &format!("  {C_DIM}Aliases: {}{C_RST}", alias_list.join(", ")),
        );
    }

    // Body (detailed description) from doc
    if let Some(d) = doc {
        add_local_event(app, "");
        for line in d.body.lines() {
            if line.is_empty() {
                add_local_event(app, "");
            } else {
                add_local_event(app, &format!("  {C_TEXT}{line}{C_RST}"));
            }
        }

        // Subcommands
        if !d.subcommands.is_empty() {
            add_local_event(app, "");
            add_local_event(app, &format!("  {C_HEADER}Subcommands:{C_RST}"));
            for sub in &d.subcommands {
                add_local_event(app, &format!("    {C_CMD}{}{C_RST}", sub.name));
                if !sub.description.is_empty() {
                    add_local_event(app, &format!("      {C_DIM}{}{C_RST}", sub.description));
                }
                if !sub.syntax.is_empty() {
                    add_local_event(app, &format!("      {C_CMD}{}{C_RST}", sub.syntax));
                }
            }
        }

        // Examples
        if !d.examples.is_empty() {
            add_local_event(app, "");
            add_local_event(app, &format!("  {C_HEADER}Examples:{C_RST}"));
            for example in &d.examples {
                add_local_event(app, &format!("    {C_CMD}{example}{C_RST}"));
            }
        }

        // See Also
        if !d.see_also.is_empty() {
            add_local_event(app, "");
            add_local_event(
                app,
                &format!("  {C_DIM}See also: {}{C_RST}", d.see_also.join(", ")),
            );
        }
    }

    add_local_event(app, &divider(""));
}

pub(crate) fn cmd_clear(app: &mut App, _args: &[String]) {
    if let Some(buf) = app.state.active_buffer_mut() {
        buf.messages.clear();
    }
}

pub(crate) fn cmd_close(app: &mut App, args: &[String]) {
    let Some(buf) = app.state.active_buffer() else {
        return;
    };
    let buf_id = buf.id.clone();
    let buf_type = buf.buffer_type.clone();
    let buf_name = buf.name.clone();
    let conn_id = buf.connection_id.clone();

    match buf_type {
        crate::state::buffer::BufferType::Channel => {
            // Send PART for channels
            let reason = if args.is_empty() {
                "Window closed".to_string()
            } else {
                args.join(" ")
            };
            if let Some(handle) = app.irc_handles.get(&conn_id) {
                let _ = handle
                    .sender
                    .send(irc::proto::Command::PART(buf_name, Some(reason)));
            } else {
                // Already disconnected — just remove
                app.state.remove_buffer(&buf_id);
            }
        }
        crate::state::buffer::BufferType::Query => {
            app.state.remove_buffer(&buf_id);
        }
        crate::state::buffer::BufferType::Server | crate::state::buffer::BufferType::Special => {
            let is_disconnected = app
                .state
                .connections
                .get(&conn_id)
                .is_none_or(|c| {
                    matches!(
                        c.status,
                        crate::state::connection::ConnectionStatus::Disconnected
                            | crate::state::connection::ConnectionStatus::Error
                    )
                });
            if is_disconnected {
                // Remove all buffers for this connection
                let to_remove: Vec<String> = app
                    .state
                    .buffers
                    .keys()
                    .filter(|id| {
                        app.state
                            .buffers
                            .get(id.as_str())
                            .is_some_and(|b| b.connection_id == conn_id)
                    })
                    .cloned()
                    .collect();
                for id in to_remove {
                    app.state.remove_buffer(&id);
                }
                app.state.connections.remove(&conn_id);
            } else {
                add_local_event(
                    app,
                    "Cannot close server buffer while connected. /disconnect first",
                );
            }
        }
    }

    // Recreate default Status if no real buffers remain
    app.ensure_default_status();
}

// === Alias commands ===

pub(crate) fn cmd_alias(app: &mut App, args: &[String]) {
    if args.is_empty() {
        // List all aliases
        let mut lines = vec![divider("Aliases")];
        if app.config.aliases.is_empty() {
            lines.push(format!("  {C_DIM}No aliases defined{C_RST}"));
        } else {
            let mut sorted: Vec<_> = app.config.aliases.iter().collect();
            sorted.sort_by(|(a, _), (b, _)| a.cmp(b));
            for (name, template) in sorted {
                lines.push(format!(
                    "  {C_CMD}/{name}{C_RST} = {C_TEXT}{template}{C_RST}"
                ));
            }
        }
        lines.push(divider(""));
        for line in &lines {
            add_local_event(app, line);
        }
        return;
    }

    // `/alias -name` removes the alias (irssi compat)
    if let Some(removal) = args.first()
        .and_then(|a| a.strip_prefix('-'))
        .filter(|_| args.len() == 1)
    {
        let name = removal.to_lowercase();
        if app.config.aliases.remove(&name).is_some() {
            app.cached_config_toml = None;
            let _ = crate::config::save_config(&crate::constants::config_path(), &app.config);
            add_local_event(
                app,
                &format!("{C_OK}Removed alias: /{name}{C_RST}"),
            );
        } else {
            add_local_event(
                app,
                &format!("{C_ERR}No alias named: /{name}{C_RST}"),
            );
        }
        return;
    }

    if args.len() < 2 {
        add_local_event(app, "Usage: /alias <name> <template> | /alias -<name> (to remove)");
        return;
    }

    let name = args[0].strip_prefix('/').unwrap_or(&args[0]).to_lowercase();
    let template = args[1].clone();

    // Check if it conflicts with a built-in command
    let builtins = super::registry::get_command_names();
    if builtins.contains(&name.as_str()) {
        add_local_event(
            app,
            &format!("{C_ERR}Cannot override built-in command: /{name}{C_RST}"),
        );
        return;
    }

    app.config.aliases.insert(name.clone(), template.clone());
    app.cached_config_toml = None;
    let _ = crate::config::save_config(&crate::constants::config_path(), &app.config);
    add_local_event(
        app,
        &format!("{C_OK}Alias /{name} = {template}{C_RST}"),
    );
}

pub(crate) fn cmd_unalias(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /unalias <name>");
        return;
    }

    let name = args[0].strip_prefix('/').unwrap_or(&args[0]).to_lowercase();

    if app.config.aliases.remove(&name).is_some() {
        app.cached_config_toml = None;
        let _ = crate::config::save_config(&crate::constants::config_path(), &app.config);
        add_local_event(
            app,
            &format!("{C_OK}Removed alias: /{name}{C_RST}"),
        );
    } else {
        add_local_event(
            app,
            &format!("{C_ERR}No alias named: /{name}{C_RST}"),
        );
    }
}

// === Items command ===

#[expect(clippy::too_many_lines, reason = "single match dispatching all /items subcommands")]
pub(crate) fn cmd_items(app: &mut App, args: &[String]) {
    if args.is_empty() || args[0] == "list" {
        let mut lines = vec![divider("Statusbar Items")];
        if app.config.statusbar.items.is_empty() {
            lines.push(format!("  {C_DIM}No items configured{C_RST}"));
        } else {
            for (i, item) in app.config.statusbar.items.iter().enumerate() {
                let name = statusbar_item_name(item);
                lines.push(format!("  {C_CMD}{}. {name}{C_RST}", i + 1));
            }
        }
        lines.push(format!(
            "  {C_DIM}Available: {AVAILABLE_ITEMS}{C_RST}"
        ));
        lines.push(divider(""));
        for line in &lines {
            add_local_event(app, line);
        }
        return;
    }

    match args[0].as_str() {
        "add" => {
            if args.len() < 2 {
                add_local_event(app, "Usage: /items add <item_name>");
                return;
            }
            let item_name = &args[1];
            match parse_statusbar_item(item_name) {
                Some(item) => {
                    // Check for duplicates
                    if app.config.statusbar.items.contains(&item) {
                        add_local_event(
                            app,
                            &format!("{C_ERR}{item_name} is already in the statusbar{C_RST}"),
                        );
                        return;
                    }
                    app.config.statusbar.items.push(item);
                    app.cached_config_toml = None;
                    let _ = crate::config::save_config(
                        &crate::constants::config_path(),
                        &app.config,
                    );
                    add_local_event(
                        app,
                        &format!("{C_OK}Added {item_name} to statusbar{C_RST}"),
                    );
                }
                None => {
                    add_local_event(
                        app,
                        &format!("{C_ERR}Unknown item: {item_name}. Available: {AVAILABLE_ITEMS}{C_RST}"),
                    );
                }
            }
        }
        "remove" => {
            if args.len() < 2 {
                add_local_event(app, "Usage: /items remove <item_name>");
                return;
            }
            let item_name = &args[1];
            match parse_statusbar_item(item_name) {
                Some(item) => {
                    if let Some(pos) = app.config.statusbar.items.iter().position(|i| *i == item) {
                        app.config.statusbar.items.remove(pos);
                        app.cached_config_toml = None;
                        let _ = crate::config::save_config(
                            &crate::constants::config_path(),
                            &app.config,
                        );
                        add_local_event(
                            app,
                            &format!("{C_OK}Removed {item_name} from statusbar{C_RST}"),
                        );
                    } else {
                        add_local_event(
                            app,
                            &format!("{C_ERR}{item_name} is not in the statusbar{C_RST}"),
                        );
                    }
                }
                None => {
                    add_local_event(
                        app,
                        &format!("{C_ERR}Unknown item: {item_name}. Available: {AVAILABLE_ITEMS}{C_RST}"),
                    );
                }
            }
        }
        "move" => {
            if args.len() < 3 {
                add_local_event(app, "Usage: /items move <item_name> <position>");
                return;
            }
            let item_name = &args[1];
            let Some(item) = parse_statusbar_item(item_name) else {
                add_local_event(
                    app,
                    &format!("{C_ERR}Unknown item: {item_name}. Available: {AVAILABLE_ITEMS}{C_RST}"),
                );
                return;
            };
            let Some(current_pos) = app.config.statusbar.items.iter().position(|i| *i == item)
            else {
                add_local_event(
                    app,
                    &format!("{C_ERR}{item_name} is not in the statusbar{C_RST}"),
                );
                return;
            };
            let Ok(new_pos) = args[2].parse::<usize>() else {
                add_local_event(app, &format!("{C_ERR}Invalid position: {}{C_RST}", args[2]));
                return;
            };
            if new_pos == 0 || new_pos > app.config.statusbar.items.len() {
                add_local_event(
                    app,
                    &format!(
                        "{C_ERR}Position must be 1-{}{C_RST}",
                        app.config.statusbar.items.len()
                    ),
                );
                return;
            }
            let removed = app.config.statusbar.items.remove(current_pos);
            app.config.statusbar.items.insert(new_pos - 1, removed);
            app.cached_config_toml = None;
            let _ =
                crate::config::save_config(&crate::constants::config_path(), &app.config);
            add_local_event(
                app,
                &format!("{C_OK}Moved {item_name} to position {new_pos}{C_RST}"),
            );
        }
        "format" => {
            if args.len() < 2 {
                add_local_event(app, "Usage: /items format <item_name> [format_string]");
                return;
            }
            let item_name = args[1].to_lowercase();
            if parse_statusbar_item(&item_name).is_none() {
                add_local_event(
                    app,
                    &format!("{C_ERR}Unknown item: {item_name}. Available: {AVAILABLE_ITEMS}{C_RST}"),
                );
                return;
            }
            if args.len() < 3 {
                // Show current format
                let fmt = app
                    .config
                    .statusbar
                    .item_formats
                    .get(&item_name)
                    .map_or("(default)", String::as_str);
                add_local_event(
                    app,
                    &format!("{C_CMD}{item_name}{C_RST} format: {C_TEXT}{fmt}{C_RST}"),
                );
                return;
            }
            let fmt = args[2].clone();
            app.config
                .statusbar
                .item_formats
                .insert(item_name.clone(), fmt.clone());
            app.cached_config_toml = None;
            let _ =
                crate::config::save_config(&crate::constants::config_path(), &app.config);
            add_local_event(
                app,
                &format!("{C_OK}Set {item_name} format: {fmt}{C_RST}"),
            );
        }
        "separator" => {
            if args.len() < 2 {
                add_local_event(
                    app,
                    &format!(
                        "Current separator: {C_CMD}{}{C_RST}",
                        app.config.statusbar.separator
                    ),
                );
                return;
            }
            app.config.statusbar.separator.clone_from(&args[1]);
            app.cached_config_toml = None;
            let _ =
                crate::config::save_config(&crate::constants::config_path(), &app.config);
            add_local_event(
                app,
                &format!("{C_OK}Separator set to: {}{C_RST}", args[1]),
            );
        }
        "available" => {
            add_local_event(app, &format!("Available statusbar items: {C_CMD}{AVAILABLE_ITEMS}{C_RST}"));
        }
        "reset" => {
            app.config.statusbar.items = crate::config::StatusbarConfig::default().items;
            app.config.statusbar.item_formats.clear();
            app.config.statusbar.separator = " | ".to_string();
            app.cached_config_toml = None;
            let _ =
                crate::config::save_config(&crate::constants::config_path(), &app.config);
            add_local_event(app, &format!("{C_OK}Statusbar reset to defaults{C_RST}"));
        }
        _ => {
            add_local_event(
                app,
                "Usage: /items [list|add|remove|move|format|separator|available|reset]",
            );
        }
    }
}

const AVAILABLE_ITEMS: &str = "time, nick_info, channel_info, lag, active_windows";

fn parse_statusbar_item(name: &str) -> Option<crate::config::StatusbarItem> {
    use crate::config::StatusbarItem;
    match name.to_lowercase().as_str() {
        "time" => Some(StatusbarItem::Time),
        "nick_info" => Some(StatusbarItem::NickInfo),
        "channel_info" => Some(StatusbarItem::ChannelInfo),
        "lag" => Some(StatusbarItem::Lag),
        "active_windows" => Some(StatusbarItem::ActiveWindows),
        _ => None,
    }
}

const fn statusbar_item_name(item: &crate::config::StatusbarItem) -> &'static str {
    use crate::config::StatusbarItem;
    match item {
        StatusbarItem::Time => "time",
        StatusbarItem::NickInfo => "nick_info",
        StatusbarItem::ChannelInfo => "channel_info",
        StatusbarItem::Lag => "lag",
        StatusbarItem::ActiveWindows => "active_windows",
    }
}

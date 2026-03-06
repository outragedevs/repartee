#![allow(clippy::redundant_pub_crate)]

use crate::app::App;
use crate::storage;
use super::helpers::add_local_event;
use super::types::{divider, C_CMD, C_DIM, C_ERR, C_OK, C_RST, C_TEXT};

// === Configuration ===

pub(crate) fn cmd_reload(app: &mut App, _args: &[String]) {
    // Reload config
    match crate::config::load_config(&crate::constants::config_path()) {
        Ok(new_config) => {
            app.config = new_config;
            add_local_event(app, &format!("{C_OK}Config reloaded{C_RST}"));
        }
        Err(e) => {
            add_local_event(
                app,
                &format!("{C_ERR}Failed to reload config: {e}{C_RST}"),
            );
            return;
        }
    }

    // Reload theme
    let theme_path =
        crate::constants::theme_dir().join(format!("{}.theme", app.config.general.theme));
    match crate::theme::load_theme(&theme_path) {
        Ok(new_theme) => {
            app.theme = new_theme;
            add_local_event(app, &format!("{C_OK}Theme reloaded{C_RST}"));
        }
        Err(e) => {
            add_local_event(
                app,
                &format!("{C_ERR}Failed to reload theme: {e}{C_RST}"),
            );
        }
    }
}

pub(crate) fn cmd_ignore(app: &mut App, args: &[String]) {
    if args.is_empty() {
        // List ignore rules — collect lines first to avoid borrow issues
        let mut lines = vec![divider("Ignore List")];
        if app.config.ignores.is_empty() {
            lines.push(format!("  {C_DIM}No ignore rules configured{C_RST}"));
        } else {
            for (i, entry) in app.config.ignores.iter().enumerate() {
                let level_str = if entry.levels.is_empty() {
                    "ALL".to_string()
                } else {
                    entry
                        .levels
                        .iter()
                        .map(|l| format!("{l:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                lines.push(format!(
                    "  {C_CMD}{}. {}{C_RST} {C_DIM}[{level_str}]{C_RST}",
                    i + 1,
                    entry.mask
                ));
            }
        }
        lines.push(divider(""));
        for line in &lines {
            add_local_event(app, line);
        }
        return;
    }

    let mask = args[0].clone();
    let levels: Vec<crate::config::IgnoreLevel> = args[1..]
        .iter()
        .filter_map(|s| parse_ignore_level(s))
        .collect();

    app.config.ignores.push(crate::config::IgnoreEntry {
        mask: mask.clone(),
        levels,
        channels: None,
    });

    // Save config
    let _ = crate::config::save_config(&crate::constants::config_path(), &app.config);
    add_local_event(
        app,
        &format!("{C_OK}Added ignore rule: {mask}{C_RST}"),
    );
}

pub(crate) fn cmd_unignore(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /unignore <number|mask>");
        return;
    }

    let target = &args[0];

    // Try as number first
    if let Ok(n) = target.parse::<usize>()
        && n >= 1
        && n <= app.config.ignores.len()
    {
        let removed = app.config.ignores.remove(n - 1);
        let _ = crate::config::save_config(&crate::constants::config_path(), &app.config);
        add_local_event(
            app,
            &format!("{C_OK}Removed ignore rule: {}{C_RST}", removed.mask),
        );
        return;
    }

    // Try as mask
    if let Some(pos) = app
        .config
        .ignores
        .iter()
        .position(|e| e.mask == *target)
    {
        let removed = app.config.ignores.remove(pos);
        let _ = crate::config::save_config(&crate::constants::config_path(), &app.config);
        add_local_event(
            app,
            &format!("{C_OK}Removed ignore rule: {}{C_RST}", removed.mask),
        );
    } else {
        add_local_event(
            app,
            &format!("{C_ERR}No ignore rule matching: {target}{C_RST}"),
        );
    }
}

const fn parse_ignore_level(s: &str) -> Option<crate::config::IgnoreLevel> {
    use crate::config::IgnoreLevel;
    if s.eq_ignore_ascii_case("all") {
        Some(IgnoreLevel::All)
    } else if s.eq_ignore_ascii_case("msgs") {
        Some(IgnoreLevel::Msgs)
    } else if s.eq_ignore_ascii_case("public") {
        Some(IgnoreLevel::Public)
    } else if s.eq_ignore_ascii_case("notices") {
        Some(IgnoreLevel::Notices)
    } else if s.eq_ignore_ascii_case("actions") {
        Some(IgnoreLevel::Actions)
    } else if s.eq_ignore_ascii_case("joins") {
        Some(IgnoreLevel::Joins)
    } else if s.eq_ignore_ascii_case("parts") {
        Some(IgnoreLevel::Parts)
    } else if s.eq_ignore_ascii_case("quits") {
        Some(IgnoreLevel::Quits)
    } else if s.eq_ignore_ascii_case("nicks") {
        Some(IgnoreLevel::Nicks)
    } else if s.eq_ignore_ascii_case("kicks") {
        Some(IgnoreLevel::Kicks)
    } else if s.eq_ignore_ascii_case("ctcp") || s.eq_ignore_ascii_case("ctcps") {
        Some(IgnoreLevel::Ctcps)
    } else {
        None
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_server(app: &mut App, args: &[String]) {
    if args.is_empty() || args[0] == "list" {
        let mut lines = vec![divider("Servers")];
        if app.config.servers.is_empty() {
            lines.push(format!("  {C_DIM}No servers configured{C_RST}"));
        } else {
            for (id, srv) in &app.config.servers {
                let status = app
                    .state
                    .connections
                    .get(id.as_str())
                    .map_or_else(|| "Not connected".to_string(), |c| format!("{:?}", c.status));
                let tls = if srv.tls { " [TLS]" } else { "" };
                let auto = if srv.autoconnect { " [auto]" } else { "" };
                lines.push(format!(
                    "  {C_CMD}{id}{C_RST} {C_DIM}{} {}:{}{tls}{auto} — {status}{C_RST}",
                    srv.label, srv.address, srv.port
                ));
            }
        }
        lines.push(divider(""));
        for line in &lines {
            add_local_event(app, line);
        }
        return;
    }

    match args[0].as_str() {
        "add" => {
            if args.len() < 3 {
                add_local_event(app, "Usage: /server add <id> <address> [port] [-tls] [-label=<name>]");
                return;
            }
            let id = args[1].to_lowercase();
            let address = args[2].clone();
            let mut port: u16 = 6667;
            let mut tls = false;
            let mut label = address.clone();
            let mut autoconnect = true;

            for arg in args.iter().skip(3) {
                if arg == "-tls" {
                    tls = true;
                } else if arg == "-noauto" {
                    autoconnect = false;
                } else if let Some(l) = arg.strip_prefix("-label=") {
                    label = l.to_string();
                } else if let Ok(p) = arg.parse::<u16>() {
                    port = p;
                }
            }

            if tls && port == 6667 {
                port = 6697;
            }

            app.config.servers.insert(
                id.clone(),
                crate::config::ServerConfig {
                    label,
                    address,
                    port,
                    tls,
                    tls_verify: true,
                    autoconnect,
                    channels: vec![],
                    nick: None,
                    username: None,
                    realname: None,
                    password: None,
                    sasl_user: None,
                    sasl_pass: None,
                    bind_ip: None,
                    encoding: None,
                    auto_reconnect: None,
                    reconnect_delay: None,
                    reconnect_max_retries: None,
                    autosendcmd: None,
                    sasl_mechanism: None,
                    client_cert_path: None,
                },
            );
            let _ = crate::config::save_config(&crate::constants::config_path(), &app.config);
            add_local_event(app, &format!("{C_OK}Server '{id}' added{C_RST}"));
        }
        "remove" => {
            if args.len() < 2 {
                add_local_event(app, "Usage: /server remove <id>");
                return;
            }
            let id = &args[1];
            if app.config.servers.remove(id).is_some() {
                let _ =
                    crate::config::save_config(&crate::constants::config_path(), &app.config);
                add_local_event(app, &format!("{C_OK}Server '{id}' removed{C_RST}"));
            } else {
                add_local_event(
                    app,
                    &format!("{C_ERR}No server with id '{id}'{C_RST}"),
                );
            }
        }
        _ => {
            add_local_event(app, "Usage: /server [list|add|remove] [args...]");
        }
    }
}

pub(crate) fn cmd_autoconnect(app: &mut App, args: &[String]) {
    // Find the server ID for the current connection
    let Some(conn_id) = app.active_conn_id().map(str::to_owned) else {
        add_local_event(app, "No active connection");
        return;
    };

    let Some(server) = app.config.servers.get_mut(&conn_id) else {
        add_local_event(
            app,
            &format!("{C_ERR}Server '{conn_id}' not found in config{C_RST}"),
        );
        return;
    };

    if args.is_empty() {
        // Toggle
        server.autoconnect = !server.autoconnect;
    } else {
        match args[0].to_lowercase().as_str() {
            "on" | "true" | "yes" | "1" => server.autoconnect = true,
            "off" | "false" | "no" | "0" => server.autoconnect = false,
            _ => {
                add_local_event(app, "Usage: /autoconnect [on|off]");
                return;
            }
        }
    }

    let status = if server.autoconnect { "on" } else { "off" };
    let label = server.label.clone();
    let _ = crate::config::save_config(&crate::constants::config_path(), &app.config);
    add_local_event(
        app,
        &format!("{C_OK}Autoconnect for {label}: {status}{C_RST}"),
    );
}

// === Operator commands ===

pub(crate) fn cmd_oper(app: &mut App, args: &[String]) {
    if args.len() < 2 {
        add_local_event(app, "Usage: /oper <name> <password>");
        return;
    }

    if let Some(sender) = app.active_irc_sender() {
        let _ = sender.send(irc::proto::Command::OPER(
            args[0].clone(),
            args[1].clone(),
        ));
    } else {
        add_local_event(app, "Not connected");
    }
}

pub(crate) fn cmd_kill(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /kill <nick> [reason]");
        return;
    }

    if let Some(sender) = app.active_irc_sender() {
        let reason = if args.len() > 1 {
            args[1].clone()
        } else {
            "Killed".to_string()
        };
        let _ = sender.send(irc::proto::Command::KILL(args[0].clone(), reason));
    } else {
        add_local_event(app, "Not connected");
    }
}

pub(crate) fn cmd_wallops(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /wallops <message>");
        return;
    }

    if let Some(sender) = app.active_irc_sender() {
        let _ = sender.send(irc::proto::Command::Raw(
            "WALLOPS".to_string(),
            vec![args[0].clone()],
        ));
    } else {
        add_local_event(app, "Not connected");
    }
}

pub(crate) fn cmd_stats(app: &mut App, args: &[String]) {
    if let Some(sender) = app.active_irc_sender() {
        let query = args.first().cloned();
        let server = args.get(1).cloned();
        let _ = sender.send(irc::proto::Command::STATS(
            query,
            server,
        ));
    } else {
        add_local_event(app, "Not connected");
    }
}

// === Logging ===

pub(crate) fn cmd_log(app: &mut App, args: &[String]) {
    let sub = args.first().map_or("status", String::as_str);

    match sub {
        "status" => log_status(app),
        "search" => {
            let query = args[1..].join(" ");
            if query.is_empty() {
                add_local_event(app, &format!("{C_ERR}Usage: /log search <query>{C_RST}"));
            } else {
                log_search(app, &query);
            }
        }
        _ => add_local_event(app, &format!("{C_ERR}Usage: /log [status|search <query>]{C_RST}")),
    }
}

fn log_status(app: &mut App) {
    // Collect all output lines first to avoid borrow conflicts
    let lines: Vec<String> = if let Some(ref storage) = app.storage {
        let count = storage.db.lock()
            .ok()
            .and_then(|db| storage::query::get_message_count(&db).ok())
            .unwrap_or(0);
        let encrypt_str = if storage.encrypt { "on" } else { "off" };
        let fts_str = if storage.encrypt { "unavailable (encrypted)" } else { "available" };

        let db_path = crate::constants::log_dir().join("messages.db");
        let db_size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
        #[allow(clippy::cast_precision_loss)]
        let size_str = if db_size > 1_048_576 {
            format!("{:.1} MB", db_size as f64 / 1_048_576.0)
        } else {
            format!("{:.1} KB", db_size as f64 / 1024.0)
        };

        let retention = app.config.logging.retention_days;
        let retention_str = if retention == 0 {
            "forever".to_string()
        } else {
            format!("{retention} days")
        };

        let exclude = &app.config.logging.exclude_types;
        let exclude_str = if exclude.is_empty() {
            "none".to_string()
        } else {
            exclude.join(", ")
        };

        vec![
            divider("Log Status"),
            format!("  {C_DIM}Messages:{C_RST}    {C_CMD}{count}{C_RST}"),
            format!("  {C_DIM}Database:{C_RST}    {C_CMD}{size_str}{C_RST}"),
            format!("  {C_DIM}Encryption:{C_RST}  {C_CMD}{encrypt_str}{C_RST}"),
            format!("  {C_DIM}Search:{C_RST}      {C_CMD}{fts_str}{C_RST}"),
            format!("  {C_DIM}Retention:{C_RST}   {C_CMD}{retention_str}{C_RST}"),
            format!("  {C_DIM}Excluded:{C_RST}    {C_CMD}{exclude_str}{C_RST}"),
        ]
    } else {
        vec![format!(
            "{C_DIM}Logging is {C_ERR}disabled{C_DIM} (set logging.enabled = true in config){C_RST}"
        )]
    };

    for line in &lines {
        add_local_event(app, line);
    }
}

fn log_search(app: &mut App, query: &str) {
    // Collect output lines to avoid borrow conflicts between storage and app
    let lines: Vec<String> = if let Some(ref storage) = app.storage {
        if storage.encrypt {
            vec![format!("{C_ERR}Search is not available in encrypted mode{C_RST}")]
        } else if let Ok(db) = storage.db.lock() {
            // Determine current network/buffer context for scoped search
            let (network, buffer) = if let Some(ref buf_id) = app.state.active_buffer_id {
                if let Some((conn_id, buf_name)) = buf_id.split_once('/') {
                    let net = app.state.connections.get(conn_id)
                        .map_or_else(|| conn_id.to_string(), |c| c.label.clone());
                    (Some(net), Some(buf_name.to_string()))
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            };

            match storage::query::search_messages(
                &db, query, network.as_deref(), buffer.as_deref(), 20,
            ) {
                Ok(results) if results.is_empty() => {
                    vec![format!("{C_DIM}No results for \"{C_CMD}{query}{C_DIM}\"{C_RST}")]
                }
                Ok(results) => {
                    let mut out = vec![divider(&format!("Search: {query}"))];
                    for msg in &results {
                        let ts = chrono::DateTime::from_timestamp(msg.timestamp, 0)
                            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                            .unwrap_or_default();
                        let nick = msg.nick.as_deref().unwrap_or("*");
                        out.push(format!(
                            "  {C_DIM}{ts}{C_RST} {C_CMD}<{nick}>{C_RST} {C_TEXT}{}{C_RST}",
                            msg.text
                        ));
                    }
                    out.push(format!("  {C_DIM}{} result(s){C_RST}", results.len()));
                    out
                }
                Err(e) => vec![format!("{C_ERR}Search failed: {e}{C_RST}")],
            }
        } else {
            vec![format!("{C_ERR}Failed to lock database{C_RST}")]
        }
    } else {
        vec![format!("{C_ERR}Logging is disabled{C_RST}")]
    };

    for line in &lines {
        add_local_event(app, line);
    }
}

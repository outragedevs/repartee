#![allow(clippy::redundant_pub_crate)]

use super::helpers::add_local_event;
use super::types::{C_CMD, C_DIM, C_ERR, C_OK, C_RST, C_TEXT, divider};
use crate::app::App;
use crate::storage;

// === Configuration ===

pub(crate) fn cmd_reload(app: &mut App, _args: &[String]) {
    // Reload config
    match crate::config::load_config(&crate::constants::config_path()) {
        Ok(new_config) => {
            app.config = new_config;
            app.cached_config_toml = None;
            add_local_event(app, &format!("{C_OK}Config reloaded{C_RST}"));
        }
        Err(e) => {
            add_local_event(app, &format!("{C_ERR}Failed to reload config: {e}{C_RST}"));
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
            add_local_event(app, &format!("{C_ERR}Failed to reload theme: {e}{C_RST}"));
        }
    }

    // Recompute cached wrap-indent (depends on config + theme).
    app.recompute_wrap_indent();
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
                let chan_str = entry
                    .channels
                    .as_ref()
                    .map(|chs| format!(" channels:{}", chs.join(",")))
                    .unwrap_or_default();
                lines.push(format!(
                    "  {C_CMD}{}. {}{C_RST} {C_DIM}[{level_str}]{chan_str}{C_RST}",
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
    let mut levels: Vec<crate::config::IgnoreLevel> = Vec::new();
    let mut channels: Option<Vec<String>> = None;

    let mut i = 1;
    while i < args.len() {
        if args[i] == "-channels" || args[i] == "-channel" {
            if i + 1 < args.len() {
                i += 1;
                channels = Some(
                    args[i]
                        .split(',')
                        .map(|s| s.trim().to_lowercase())
                        .collect(),
                );
            }
        } else if let Some(level) = parse_ignore_level(&args[i]) {
            levels.push(level);
        }
        i += 1;
    }

    app.config.ignores.push(crate::config::IgnoreEntry {
        mask: mask.clone(),
        levels,
        channels,
    });

    // Save config
    app.cached_config_toml = None;
    let _ = crate::config::save_config(&crate::constants::config_path(), &app.config);
    add_local_event(app, &format!("{C_OK}Added ignore rule: {mask}{C_RST}"));
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
        app.cached_config_toml = None;
        let _ = crate::config::save_config(&crate::constants::config_path(), &app.config);
        add_local_event(
            app,
            &format!("{C_OK}Removed ignore rule: {}{C_RST}", removed.mask),
        );
        return;
    }

    // Try as mask
    if let Some(pos) = app.config.ignores.iter().position(|e| e.mask == *target) {
        let removed = app.config.ignores.remove(pos);
        app.cached_config_toml = None;
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
                let status = app.state.connections.get(id.as_str()).map_or_else(
                    || "Not connected".to_string(),
                    |c| format!("{:?}", c.status),
                );
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
                add_local_event(
                    app,
                    "Usage: /server add <id> <address> [port] [-tls] [-notlsverify] [-noauto] [-label=<name>] [-nick=<nick>] [-password=<pass>] [-sasl=<user>:<pass>] [-bind=<ip>] [-autosendcmd=<cmds>]",
                );
                return;
            }
            let id = args[1].to_lowercase();
            let address = args[2].clone();
            let mut port: u16 = 6667;
            let mut tls = false;
            let mut tls_verify = true;
            let mut label = address.clone();
            let mut autoconnect = true;
            let mut nick: Option<String> = None;
            let mut password: Option<String> = None;
            let mut sasl_user: Option<String> = None;
            let mut sasl_pass: Option<String> = None;
            let mut bind_ip: Option<String> = None;
            let mut autosendcmd: Option<String> = None;

            for arg in args.iter().skip(3) {
                if arg == "-tls" {
                    tls = true;
                } else if arg == "-notlsverify" {
                    tls_verify = false;
                } else if arg == "-noauto" {
                    autoconnect = false;
                } else if let Some(l) = arg.strip_prefix("-label=") {
                    label = l.to_string();
                } else if let Some(n) = arg.strip_prefix("-nick=") {
                    nick = Some(n.to_string());
                } else if let Some(p) = arg.strip_prefix("-password=") {
                    password = Some(p.to_string());
                } else if let Some(s) = arg.strip_prefix("-sasl=") {
                    if let Some((user, pass)) = s.split_once(':') {
                        sasl_user = Some(user.to_string());
                        sasl_pass = Some(pass.to_string());
                    } else {
                        add_local_event(
                            app,
                            &format!("{C_ERR}SASL format: -sasl=user:pass{C_RST}"),
                        );
                        return;
                    }
                } else if let Some(ip) = arg.strip_prefix("-bind=") {
                    bind_ip = Some(ip.to_string());
                } else if let Some(cmd) = arg.strip_prefix("-autosendcmd=") {
                    autosendcmd = Some(cmd.to_string());
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
                    tls_verify,
                    autoconnect,
                    channels: vec![],
                    nick,
                    username: None,
                    realname: None,
                    password,
                    sasl_user,
                    sasl_pass,
                    bind_ip,
                    encoding: None,
                    auto_reconnect: None,
                    reconnect_delay: None,
                    reconnect_max_retries: None,
                    autosendcmd,
                    sasl_mechanism: None,
                    client_cert_path: None,
                },
            );
            app.cached_config_toml = None;
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
                app.cached_config_toml = None;
                let _ = crate::config::save_config(&crate::constants::config_path(), &app.config);
                add_local_event(app, &format!("{C_OK}Server '{id}' removed{C_RST}"));
            } else {
                add_local_event(app, &format!("{C_ERR}No server with id '{id}'{C_RST}"));
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
    app.cached_config_toml = None;
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
        let _ = sender.send(irc::proto::Command::OPER(args[0].clone(), args[1].clone()));
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
        let _ = sender.send(irc::proto::Command::STATS(query, server));
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
        _ => add_local_event(
            app,
            &format!("{C_ERR}Usage: /log [status|search <query>]{C_RST}"),
        ),
    }
}

fn log_status(app: &mut App) {
    // Collect all output lines first to avoid borrow conflicts
    let lines: Vec<String> = if let Some(ref storage) = app.storage {
        let count = storage
            .db
            .lock()
            .ok()
            .and_then(|db| storage::query::get_message_count(&db).ok())
            .unwrap_or(0);
        let encrypt_str = if storage.encrypt { "on" } else { "off" };
        let fts_str = if storage.encrypt {
            "unavailable (encrypted)"
        } else {
            "available"
        };

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

// === Image Preview ===

pub(crate) fn cmd_preview(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /preview <url>");
        return;
    }
    let url = &args[0];

    if !app.config.image_preview.enabled {
        add_local_event(
            app,
            &format!(
                "{C_ERR}Image preview is disabled. Use /set image_preview.enabled true{C_RST}"
            ),
        );
        return;
    }

    if crate::image_preview::detect::classify_url(url).is_none() {
        add_local_event(
            app,
            &format!("{C_ERR}URL does not appear to be a valid HTTP(S) link{C_RST}"),
        );
        return;
    }

    app.show_image_preview(url);
}

#[allow(clippy::cast_precision_loss)]
pub(crate) fn cmd_image(app: &mut App, args: &[String]) {
    let subcmd = args.first().map_or("", String::as_str);
    match subcmd {
        "stats" => match crate::image_preview::cache::stats() {
            Ok(s) => {
                let size_mb = s.total_bytes as f64 / 1_048_576.0;
                let age_days = s.oldest_age_secs / 86400;
                add_local_event(
                    app,
                    &format!(
                        "Image cache: {C_CMD}{}{C_RST} files, {C_CMD}{size_mb:.1}{C_RST} MB, oldest: {C_CMD}{age_days}{C_RST} days",
                        s.total_files
                    ),
                );
            }
            Err(e) => add_local_event(app, &format!("{C_ERR}Cache stats error: {e}{C_RST}")),
        },
        "clear" => match crate::image_preview::cache::clear() {
            Ok(count) => {
                add_local_event(app, &format!("{C_OK}Cleared {count} cached images{C_RST}"));
            }
            Err(e) => add_local_event(app, &format!("{C_ERR}Cache clear error: {e}{C_RST}")),
        },
        "cleanup" => {
            let max_mb = app.config.image_preview.cache_max_mb;
            let max_days = app.config.image_preview.cache_max_days;
            match crate::image_preview::cache::cleanup(max_mb, max_days) {
                Ok(s) => {
                    let mb = s.bytes_freed as f64 / 1_048_576.0;
                    add_local_event(
                        app,
                        &format!(
                            "{C_OK}Cleanup: removed {} files, freed {mb:.1} MB{C_RST}",
                            s.files_removed
                        ),
                    );
                }
                Err(e) => add_local_event(app, &format!("{C_ERR}Cleanup error: {e}{C_RST}")),
            }
        }
        "debug" => image_debug(app),
        _ => {
            let cfg = &app.config.image_preview;
            let lines = vec![
                divider("Image Preview"),
                format!("  {C_DIM}Enabled:{C_RST}     {C_CMD}{}{C_RST}", cfg.enabled),
                format!(
                    "  {C_DIM}Protocol:{C_RST}    {C_CMD}{}{C_RST}",
                    cfg.protocol
                ),
                format!(
                    "  {C_DIM}Max width:{C_RST}   {C_CMD}{}{C_RST}",
                    cfg.max_width
                ),
                format!(
                    "  {C_DIM}Max height:{C_RST}  {C_CMD}{}{C_RST}",
                    cfg.max_height
                ),
                format!(
                    "  {C_DIM}Cache limit:{C_RST} {C_CMD}{} MB / {} days{C_RST}",
                    cfg.cache_max_mb, cfg.cache_max_days
                ),
                divider(""),
            ];
            for line in &lines {
                add_local_event(app, line);
            }
        }
    }
}

// === Scripting ===

#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_script(app: &mut App, args: &[String]) {
    let sub = args.first().map_or("", String::as_str);

    match sub {
        "load" => {
            if args.len() < 2 {
                add_local_event(app, &format!("{C_ERR}Usage: /script load <name>{C_RST}"));
                return;
            }
            let name = &args[1];
            let Some(manager) = app.script_manager.as_mut() else {
                add_local_event(app, &format!("{C_ERR}Script manager not available{C_RST}"));
                return;
            };
            let Some(api) = app.script_api.as_ref() else {
                add_local_event(app, &format!("{C_ERR}Script API not available{C_RST}"));
                return;
            };
            match manager.load(name, api) {
                Ok(meta) => {
                    let desc = meta.description.as_deref().unwrap_or("");
                    let ver = meta.version.as_deref().unwrap_or("?");
                    add_local_event(
                        app,
                        &format!(
                            "{C_OK}Loaded script: {C_CMD}{}{C_OK} v{ver} — {desc}{C_RST}",
                            meta.name
                        ),
                    );
                }
                Err(e) => {
                    add_local_event(
                        app,
                        &format!("{C_ERR}Failed to load script '{name}': {e}{C_RST}"),
                    );
                }
            }
        }
        "unload" => {
            if args.len() < 2 {
                add_local_event(app, &format!("{C_ERR}Usage: /script unload <name>{C_RST}"));
                return;
            }
            let name = &args[1];
            let Some(manager) = app.script_manager.as_mut() else {
                add_local_event(app, &format!("{C_ERR}Script manager not available{C_RST}"));
                return;
            };
            match manager.unload(name) {
                Ok(()) => {
                    add_local_event(app, &format!("{C_OK}Unloaded script: {name}{C_RST}"));
                }
                Err(e) => {
                    add_local_event(
                        app,
                        &format!("{C_ERR}Failed to unload '{name}': {e}{C_RST}"),
                    );
                }
            }
        }
        "reload" => {
            if args.len() < 2 {
                add_local_event(app, &format!("{C_ERR}Usage: /script reload <name>{C_RST}"));
                return;
            }
            let name = &args[1];
            let Some(manager) = app.script_manager.as_mut() else {
                add_local_event(app, &format!("{C_ERR}Script manager not available{C_RST}"));
                return;
            };
            let Some(api) = app.script_api.as_ref() else {
                add_local_event(app, &format!("{C_ERR}Script API not available{C_RST}"));
                return;
            };
            match manager.reload(name, api) {
                Ok(meta) => {
                    let desc = meta.description.as_deref().unwrap_or("");
                    let ver = meta.version.as_deref().unwrap_or("?");
                    add_local_event(
                        app,
                        &format!(
                            "{C_OK}Reloaded script: {C_CMD}{}{C_OK} v{ver} — {desc}{C_RST}",
                            meta.name
                        ),
                    );
                }
                Err(e) => {
                    add_local_event(
                        app,
                        &format!("{C_ERR}Failed to reload '{name}': {e}{C_RST}"),
                    );
                }
            }
        }
        "list" | "" => {
            let Some(manager) = app.script_manager.as_ref() else {
                add_local_event(app, &format!("{C_ERR}Script manager not available{C_RST}"));
                return;
            };
            let loaded = manager.loaded_scripts();
            let available = manager.available_scripts();

            let mut lines = vec![divider("Scripts")];

            if loaded.is_empty() && available.is_empty() {
                lines.push(format!(
                    "  {C_DIM}No scripts found. Place .lua files in {}{C_RST}",
                    manager.scripts_dir().display()
                ));
            } else {
                if !loaded.is_empty() {
                    lines.push(format!("  {C_CMD}Loaded:{C_RST}"));
                    for meta in &loaded {
                        let ver = meta.version.as_deref().unwrap_or("?");
                        let desc = meta.description.as_deref().unwrap_or("");
                        lines.push(format!(
                            "    {C_OK}{}{C_RST} {C_DIM}v{ver} — {desc}{C_RST}",
                            meta.name
                        ));
                    }
                }

                let unloaded: Vec<_> = available.iter().filter(|(_, _, loaded)| !loaded).collect();
                if !unloaded.is_empty() {
                    lines.push(format!("  {C_CMD}Available:{C_RST}"));
                    for (name, _path, _) in &unloaded {
                        lines.push(format!("    {C_DIM}{name}{C_RST}"));
                    }
                }
            }
            lines.push(divider(""));
            for line in &lines {
                add_local_event(app, line);
            }
        }
        "autoload" => {
            app.autoload_scripts();
            let loaded_count = app
                .script_manager
                .as_ref()
                .map_or(0, |m| m.loaded_scripts().len());
            add_local_event(
                app,
                &format!("{C_OK}Autoloaded scripts ({loaded_count} loaded){C_RST}"),
            );
        }
        "template" => {
            add_local_event(app, &format!("{C_CMD}Lua script template:{C_RST}"));
            for line in crate::scripting::api::LUA_SCRIPT_TEMPLATE.lines() {
                add_local_event(app, &format!("  {C_DIM}{line}{C_RST}"));
            }
        }
        _ => {
            add_local_event(
                app,
                &format!(
                    "{C_ERR}Usage: /script [load|unload|reload|list|autoload|template] [name]{C_RST}"
                ),
            );
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "debug output formatter — splitting fragments the template"
)]
fn image_debug(app: &mut App) {
    // Re-detect now so debug shows current state, not stale startup state.
    app.refresh_image_protocol();

    let proto = app.picker.protocol_type();
    let font = app.picker.font_size();
    let caps = app.picker.capabilities();

    // Collect env vars — use shim's env when socket-attached, daemon's env otherwise.
    let env_override = app.shim_term_env.as_ref();
    let get_env = |key: &str| -> String {
        env_override.map_or_else(
            || std::env::var(key).unwrap_or_default(),
            |vars| vars.get(key).cloned().unwrap_or_default(),
        )
    };
    let term = get_env("TERM");
    let term_program = get_env("TERM_PROGRAM");
    let lc_terminal = get_env("LC_TERMINAL");
    let iterm_sess = get_env("ITERM_SESSION_ID");
    let ghostty_res = get_env("GHOSTTY_RESOURCES_DIR");
    let kitty_pid = get_env("KITTY_PID");
    let colorterm = get_env("COLORTERM");

    // tmux queries (only if in tmux)
    let (tmux_termtype, tmux_termname, tmux_passthrough, tmux_version) = if app.in_tmux {
        let tt = crate::app::tmux_query_raw("#{client_termtype}").unwrap_or_default();
        let tn = crate::app::tmux_query_raw("#{client_termname}").unwrap_or_default();
        let pt = std::process::Command::new("tmux")
            .args(["show", "-p", "allow-passthrough"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        let ver = std::process::Command::new("tmux")
            .args(["-V"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        (tt, tn, pt, ver)
    } else {
        (String::new(), String::new(), String::new(), String::new())
    };

    let mut lines = vec![divider("Image Debug")];

    // Detection results
    lines.push(format!(
        "  {C_DIM}Protocol:{C_RST}        {C_CMD}{proto:?}{C_RST}"
    ));
    lines.push(format!(
        "  {C_DIM}Source:{C_RST}          {C_CMD}{}{C_RST}",
        app.image_proto_source
    ));
    lines.push(format!(
        "  {C_DIM}Outer terminal:{C_RST}  {C_CMD}{}{C_RST}",
        app.outer_terminal
    ));
    lines.push(format!(
        "  {C_DIM}In tmux:{C_RST}         {C_CMD}{}{C_RST}",
        app.in_tmux
    ));
    lines.push(format!(
        "  {C_DIM}Font size:{C_RST}       {C_CMD}{}x{}{C_RST}",
        font.0, font.1
    ));
    lines.push(format!(
        "  {C_DIM}Capabilities:{C_RST}    {C_CMD}{caps:?}{C_RST}"
    ));
    lines.push(format!(
        "  {C_DIM}Config proto:{C_RST}    {C_CMD}{}{C_RST}",
        app.config.image_preview.protocol
    ));

    // tmux info
    if app.in_tmux {
        lines.push(format!(
            "  {C_DIM}tmux version:{C_RST}   {C_CMD}{tmux_version}{C_RST}"
        ));
        lines.push(format!(
            "  {C_DIM}passthrough:{C_RST}    {C_CMD}{tmux_passthrough}{C_RST}"
        ));
        lines.push(format!(
            "  {C_DIM}client_termtype:{C_RST}{C_CMD} {tmux_termtype}{C_RST}"
        ));
        lines.push(format!(
            "  {C_DIM}client_termname:{C_RST}{C_CMD} {tmux_termname}{C_RST}"
        ));
    }

    // Env vars
    lines.push(format!(
        "  {C_DIM}TERM:{C_RST}            {C_CMD}{term}{C_RST}"
    ));
    lines.push(format!(
        "  {C_DIM}TERM_PROGRAM:{C_RST}    {C_CMD}{term_program}{C_RST}"
    ));
    lines.push(format!(
        "  {C_DIM}LC_TERMINAL:{C_RST}     {C_CMD}{lc_terminal}{C_RST}"
    ));
    lines.push(format!(
        "  {C_DIM}COLORTERM:{C_RST}       {C_CMD}{colorterm}{C_RST}"
    ));
    if !iterm_sess.is_empty() {
        lines.push(format!(
            "  {C_DIM}ITERM_SESSION_ID:{C_RST}{C_CMD}{iterm_sess}{C_RST}"
        ));
    }
    if !ghostty_res.is_empty() {
        lines.push(format!(
            "  {C_DIM}GHOSTTY_RESOURCES_DIR:{C_RST}{C_CMD}{ghostty_res}{C_RST}"
        ));
    }
    if !kitty_pid.is_empty() {
        lines.push(format!(
            "  {C_DIM}KITTY_PID:{C_RST}       {C_CMD}{kitty_pid}{C_RST}"
        ));
    }

    lines.push(divider(""));
    for line in &lines {
        add_local_event(app, line);
    }
}

fn log_search(app: &mut App, query: &str) {
    // Collect output lines to avoid borrow conflicts between storage and app
    let lines: Vec<String> = if let Some(ref storage) = app.storage {
        if storage.encrypt {
            vec![format!(
                "{C_ERR}Search is not available in encrypted mode{C_RST}"
            )]
        } else if let Ok(db) = storage.db.lock() {
            // Determine current network/buffer context for scoped search
            let (network, buffer) = if let Some(ref buf_id) = app.state.active_buffer_id {
                if let Some((conn_id, buf_name)) = buf_id.split_once('/') {
                    let net = app
                        .state
                        .connections
                        .get(conn_id)
                        .map_or_else(|| conn_id.to_string(), |c| c.label.clone());
                    (Some(net), Some(buf_name.to_string()))
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            };

            match storage::query::search_messages(
                &db,
                query,
                network.as_deref(),
                buffer.as_deref(),
                20,
            ) {
                Ok(results) if results.is_empty() => {
                    vec![format!(
                        "{C_DIM}No results for \"{C_CMD}{query}{C_DIM}\"{C_RST}"
                    )]
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

// === Spell Check ===

pub(crate) fn cmd_spellcheck(app: &mut App, args: &[String]) {
    let ev = add_local_event;
    let sub = args.first().map_or("status", String::as_str);

    match sub {
        "status" => {
            ev(app, &divider("Spell Check"));
            let enabled = app.config.spellcheck.enabled;
            let status = if enabled {
                format!("{C_OK}enabled{C_RST}")
            } else {
                format!("{C_ERR}disabled{C_RST}")
            };
            ev(app, &format!("  Status: {status}"));
            ev(
                app,
                &format!(
                    "  Languages: {C_CMD}{}{C_RST}",
                    app.config.spellcheck.languages.join(", ")
                ),
            );
            let dict_dir = crate::spellcheck::SpellChecker::resolve_dict_dir(
                &app.config.spellcheck.dictionary_dir,
            );
            ev(
                app,
                &format!("  Dictionary dir: {C_CMD}{}{C_RST}", dict_dir.display()),
            );
            let loaded = app
                .spellchecker
                .as_ref()
                .map_or(0, crate::spellcheck::SpellChecker::dict_count);
            ev(
                app,
                &format!("  Loaded dictionaries: {C_CMD}{loaded}{C_RST}"),
            );
        }
        "reload" => {
            app.reload_spellchecker();
            let loaded = app
                .spellchecker
                .as_ref()
                .map_or(0, crate::spellcheck::SpellChecker::dict_count);
            if loaded > 0 {
                ev(
                    app,
                    &format!("{C_OK}Spell checker reloaded ({loaded} dictionaries){C_RST}"),
                );
            } else {
                ev(
                    app,
                    &format!(
                        "{C_ERR}No dictionaries loaded — place .dic/.aff files in {}{C_RST}",
                        crate::spellcheck::SpellChecker::resolve_dict_dir(
                            &app.config.spellcheck.dictionary_dir
                        )
                        .display()
                    ),
                );
            }
        }
        "list" => {
            ev(app, &format!("{C_DIM}Fetching dictionary list...{C_RST}"));
            let dict_dir = crate::spellcheck::SpellChecker::resolve_dict_dir(
                &app.config.spellcheck.dictionary_dir,
            );
            crate::spellcheck::spawn_fetch_manifest(
                app.http_client.clone(),
                dict_dir,
                app.dict_tx.clone(),
            );
        }
        "get" => {
            let Some(lang) = args.get(1) else {
                ev(
                    app,
                    &format!("{C_ERR}Usage: /spellcheck get <lang> (e.g. en_US, pl_PL){C_RST}"),
                );
                return;
            };
            ev(
                app,
                &format!("{C_DIM}Downloading {lang}...{C_RST}"),
            );
            let dict_dir = crate::spellcheck::SpellChecker::resolve_dict_dir(
                &app.config.spellcheck.dictionary_dir,
            );
            crate::spellcheck::spawn_download_dict(
                lang.clone(),
                app.http_client.clone(),
                dict_dir,
                app.dict_tx.clone(),
            );
        }
        _ => {
            ev(
                app,
                &format!("{C_ERR}Usage: /spellcheck [status|reload|list|get <lang>]{C_RST}"),
            );
        }
    }
}

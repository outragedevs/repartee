//! /set command — dot-notation config get/set with type coercion.
//!
//! Paths: `general.nick`, `display.nick_column_width`, `servers.libera.port`, etc.

use crate::app::App;
use crate::config::AppConfig;

use super::types::{C_CMD, C_DIM, C_ERR, C_HEADER, C_OK, C_RST, divider};

/// Result of resolving a dot-notation config path.
struct Resolved {
    value: String,
    is_credential: bool,
}

/// Get a config value by dot-notation path.
#[expect(
    clippy::too_many_lines,
    reason = "flat match dispatcher — one arm per config field"
)]
fn get_config_value(config: &AppConfig, path: &str) -> Option<Resolved> {
    let parts: Vec<&str> = path.split('.').collect();
    if parts.len() < 2 {
        return None;
    }

    match parts[0] {
        "general" => {
            let val = match parts[1] {
                "nick" => config.general.nick.clone(),
                "username" => config.general.username.clone(),
                "realname" => config.general.realname.clone(),
                "theme" => config.general.theme.clone(),
                "timestamp_format" => config.general.timestamp_format.clone(),
                "flood_protection" => config.general.flood_protection.to_string(),
                "ctcp_version" => config.general.ctcp_version.clone(),
                _ => return None,
            };
            Some(Resolved {
                value: val,
                is_credential: false,
            })
        }
        "display" => {
            let val = match parts[1] {
                "nick_column_width" => config.display.nick_column_width.to_string(),
                "nick_max_length" => config.display.nick_max_length.to_string(),
                "nick_alignment" => format!("{:?}", config.display.nick_alignment).to_lowercase(),
                "nick_truncation" => config.display.nick_truncation.to_string(),
                "show_timestamps" => config.display.show_timestamps.to_string(),
                "scrollback_lines" => config.display.scrollback_lines.to_string(),
                "backlog_lines" => config.display.backlog_lines.to_string(),
                "nick_colors" => config.display.nick_colors.to_string(),
                "nick_colors_in_nicklist" => config.display.nick_colors_in_nicklist.to_string(),
                "nick_color_saturation" => config.display.nick_color_saturation.to_string(),
                "nick_color_lightness" => config.display.nick_color_lightness.to_string(),
                "mentions_buffer" => config.display.mentions_buffer.to_string(),
                _ => return None,
            };
            Some(Resolved {
                value: val,
                is_credential: false,
            })
        }
        "sidepanel" if parts.len() >= 3 => {
            let panel = match parts[1] {
                "left" => &config.sidepanel.left,
                "right" => &config.sidepanel.right,
                _ => return None,
            };
            let val = match parts[2] {
                "width" => panel.width.to_string(),
                "visible" => panel.visible.to_string(),
                _ => return None,
            };
            Some(Resolved {
                value: val,
                is_credential: false,
            })
        }
        "statusbar" => {
            let val = match parts[1] {
                "enabled" => config.statusbar.enabled.to_string(),
                "separator" => config.statusbar.separator.clone(),
                "prompt" => config.statusbar.prompt.clone(),
                "background" => config.statusbar.background.clone(),
                "text_color" => config.statusbar.text_color.clone(),
                "accent_color" => config.statusbar.accent_color.clone(),
                "muted_color" => config.statusbar.muted_color.clone(),
                "dim_color" => config.statusbar.dim_color.clone(),
                "prompt_color" => config.statusbar.prompt_color.clone(),
                "input_color" => config.statusbar.input_color.clone(),
                "cursor_color" => config.statusbar.cursor_color.clone(),
                _ => return None,
            };
            Some(Resolved {
                value: val,
                is_credential: false,
            })
        }
        "image_preview" => {
            let val = match parts[1] {
                "enabled" => config.image_preview.enabled.to_string(),
                "max_width" => config.image_preview.max_width.to_string(),
                "max_height" => config.image_preview.max_height.to_string(),
                "cache_max_mb" => config.image_preview.cache_max_mb.to_string(),
                "cache_max_days" => config.image_preview.cache_max_days.to_string(),
                "fetch_timeout" => config.image_preview.fetch_timeout.to_string(),
                "max_file_size" => config.image_preview.max_file_size.to_string(),
                "protocol" => config.image_preview.protocol.clone(),
                "kitty_format" => config.image_preview.kitty_format.clone(),
                _ => return None,
            };
            Some(Resolved {
                value: val,
                is_credential: false,
            })
        }
        "dcc" => {
            let val = match parts[1] {
                "timeout" => config.dcc.timeout.to_string(),
                "own_ip" => config.dcc.own_ip.clone(),
                "port_range" => config.dcc.port_range.clone(),
                "autoaccept_lowports" => config.dcc.autoaccept_lowports.to_string(),
                "autochat_masks" => config.dcc.autochat_masks.join(", "),
                "max_connections" => config.dcc.max_connections.to_string(),
                _ => return None,
            };
            Some(Resolved {
                value: val,
                is_credential: false,
            })
        }
        "spellcheck" => {
            let val = match parts[1] {
                "enabled" => config.spellcheck.enabled.to_string(),
                "computing" => config.spellcheck.computing.to_string(),
                "mode" => config.spellcheck.mode.clone(),
                "languages" => config.spellcheck.languages.join(", "),
                "dictionary_dir" => config.spellcheck.dictionary_dir.clone(),
                _ => return None,
            };
            Some(Resolved {
                value: val,
                is_credential: false,
            })
        }
        "logging" => {
            let val = match parts[1] {
                "event_retention_hours" => config.logging.event_retention_hours.to_string(),
                "retention_days" => config.logging.retention_days.to_string(),
                _ => return None,
            };
            Some(Resolved {
                value: val,
                is_credential: false,
            })
        }
        "web" => {
            let is_cred = parts[1] == "password";
            let val = match parts[1] {
                "enabled" => config.web.enabled.to_string(),
                "bind_address" => config.web.bind_address.clone(),
                "port" => config.web.port.to_string(),
                "tls_cert" => config.web.tls_cert.clone(),
                "tls_key" => config.web.tls_key.clone(),
                "timestamp_format" => config.web.timestamp_format.clone(),
                "line_height" => config.web.line_height.to_string(),
                "nick_column_width" => config.web.nick_column_width.to_string(),
                "nick_max_length" => config.web.nick_max_length.to_string(),
                "theme" => config.web.theme.clone(),
                "session_hours" => config.web.session_hours.to_string(),
                "cloudflare_tunnel_name" => config.web.cloudflare_tunnel_name.clone(),
                "password" => config.web.password.clone(),
                _ => return None,
            };
            Some(Resolved {
                value: val,
                is_credential: is_cred,
            })
        }
        "servers" if parts.len() >= 3 => {
            let server = config.servers.get(parts[1])?;
            let is_cred = matches!(parts[2], "password" | "sasl_pass" | "sasl_user");
            let val = match parts[2] {
                "label" => server.label.clone(),
                "address" => server.address.clone(),
                "port" => server.port.to_string(),
                "tls" => server.tls.to_string(),
                "tls_verify" => server.tls_verify.to_string(),
                "autoconnect" => server.autoconnect.to_string(),
                "channels" => server.channels.join(", "),
                "nick" => server.nick.clone().unwrap_or_default(),
                "username" => server.username.clone().unwrap_or_default(),
                "realname" => server.realname.clone().unwrap_or_default(),
                "password" => server.password.clone().unwrap_or_default(),
                "sasl_user" => server.sasl_user.clone().unwrap_or_default(),
                "sasl_pass" => server.sasl_pass.clone().unwrap_or_default(),
                _ => return None,
            };
            Some(Resolved {
                value: val,
                is_credential: is_cred,
            })
        }
        _ => None,
    }
}

/// Set a config value by dot-notation path. Returns true on success.
#[expect(clippy::too_many_lines)]
fn set_config_value(config: &mut AppConfig, path: &str, raw: &str) -> Result<(), String> {
    let parts: Vec<&str> = path.split('.').collect();
    if parts.len() < 2 {
        return Err("Invalid path".to_string());
    }

    match parts[0] {
        "general" => match parts[1] {
            "nick" => config.general.nick = raw.to_string(),
            "username" => config.general.username = raw.to_string(),
            "realname" => config.general.realname = raw.to_string(),
            "theme" => config.general.theme = raw.to_string(),
            "timestamp_format" => config.general.timestamp_format = raw.to_string(),
            "flood_protection" => {
                config.general.flood_protection = parse_bool(raw)?;
            }
            "ctcp_version" => config.general.ctcp_version = raw.to_string(),
            _ => return Err(format!("Unknown field: {path}")),
        },
        "display" => match parts[1] {
            "nick_column_width" => {
                config.display.nick_column_width = parse_u16(raw)?;
            }
            "nick_max_length" => {
                config.display.nick_max_length = parse_u16(raw)?;
            }
            "nick_alignment" => {
                config.display.nick_alignment = match raw {
                    "left" => crate::config::NickAlignment::Left,
                    "right" => crate::config::NickAlignment::Right,
                    "center" => crate::config::NickAlignment::Center,
                    _ => return Err("Expected left, right, or center".to_string()),
                };
            }
            "nick_truncation" => {
                config.display.nick_truncation = parse_bool(raw)?;
            }
            "show_timestamps" => {
                config.display.show_timestamps = parse_bool(raw)?;
            }
            "scrollback_lines" => {
                config.display.scrollback_lines =
                    raw.parse().map_err(|_| "Expected a number".to_string())?;
            }
            "backlog_lines" => {
                config.display.backlog_lines =
                    raw.parse().map_err(|_| "Expected a number".to_string())?;
            }
            "nick_colors" => {
                config.display.nick_colors = parse_bool(raw)?;
            }
            "nick_colors_in_nicklist" => {
                config.display.nick_colors_in_nicklist = parse_bool(raw)?;
            }
            "nick_color_saturation" => {
                let v: f32 = raw.parse().map_err(|_| format!("invalid float: {raw}"))?;
                if !(0.0..=1.0).contains(&v) {
                    return Err("saturation must be 0.0–1.0".into());
                }
                config.display.nick_color_saturation = v;
            }
            "nick_color_lightness" => {
                let v: f32 = raw.parse().map_err(|_| format!("invalid float: {raw}"))?;
                if !(0.0..=1.0).contains(&v) {
                    return Err("lightness must be 0.0–1.0".into());
                }
                config.display.nick_color_lightness = v;
            }
            "mentions_buffer" => {
                config.display.mentions_buffer = parse_bool(raw)?;
            }
            _ => return Err(format!("Unknown field: {path}")),
        },
        "sidepanel" if parts.len() >= 3 => {
            let panel = match parts[1] {
                "left" => &mut config.sidepanel.left,
                "right" => &mut config.sidepanel.right,
                _ => return Err(format!("Unknown panel: {}", parts[1])),
            };
            match parts[2] {
                "width" => panel.width = parse_u16(raw)?,
                "visible" => panel.visible = parse_bool(raw)?,
                _ => return Err(format!("Unknown field: {path}")),
            }
        }
        "statusbar" => match parts[1] {
            "enabled" => config.statusbar.enabled = parse_bool(raw)?,
            "separator" => config.statusbar.separator = raw.to_string(),
            "prompt" => config.statusbar.prompt = raw.to_string(),
            "background" => config.statusbar.background = raw.to_string(),
            "text_color" => config.statusbar.text_color = raw.to_string(),
            "accent_color" => config.statusbar.accent_color = raw.to_string(),
            "muted_color" => config.statusbar.muted_color = raw.to_string(),
            "dim_color" => config.statusbar.dim_color = raw.to_string(),
            "prompt_color" => config.statusbar.prompt_color = raw.to_string(),
            "input_color" => config.statusbar.input_color = raw.to_string(),
            "cursor_color" => config.statusbar.cursor_color = raw.to_string(),
            _ => return Err(format!("Unknown field: {path}")),
        },
        "image_preview" => match parts[1] {
            "enabled" => config.image_preview.enabled = parse_bool(raw)?,
            "max_width" => {
                config.image_preview.max_width =
                    raw.parse().map_err(|_| "Expected a number".to_string())?;
            }
            "max_height" => {
                config.image_preview.max_height =
                    raw.parse().map_err(|_| "Expected a number".to_string())?;
            }
            "cache_max_mb" => {
                config.image_preview.cache_max_mb =
                    raw.parse().map_err(|_| "Expected a number".to_string())?;
            }
            "cache_max_days" => {
                config.image_preview.cache_max_days =
                    raw.parse().map_err(|_| "Expected a number".to_string())?;
            }
            "fetch_timeout" => {
                config.image_preview.fetch_timeout =
                    raw.parse().map_err(|_| "Expected a number".to_string())?;
            }
            "max_file_size" => {
                config.image_preview.max_file_size =
                    raw.parse().map_err(|_| "Expected a number".to_string())?;
            }
            "protocol" => config.image_preview.protocol = raw.to_string(),
            "kitty_format" => config.image_preview.kitty_format = raw.to_string(),
            _ => return Err(format!("Unknown field: {path}")),
        },
        "dcc" => match parts[1] {
            "timeout" => {
                config.dcc.timeout = raw.parse().map_err(|_| "Expected a number".to_string())?;
            }
            "own_ip" => config.dcc.own_ip = raw.to_string(),
            "port_range" => config.dcc.port_range = raw.to_string(),
            "autoaccept_lowports" => {
                config.dcc.autoaccept_lowports = parse_bool(raw)?;
            }
            "autochat_masks" => {
                config.dcc.autochat_masks = raw.split(',').map(|s| s.trim().to_string()).collect();
            }
            "max_connections" => {
                config.dcc.max_connections =
                    raw.parse().map_err(|_| "Expected a number".to_string())?;
            }
            _ => return Err(format!("Unknown field: {path}")),
        },
        "spellcheck" => match parts[1] {
            "enabled" => config.spellcheck.enabled = parse_bool(raw)?,
            "computing" => config.spellcheck.computing = parse_bool(raw)?,
            "mode" => {
                let mode = raw.to_lowercase();
                if mode != "replace" && mode != "highlight" {
                    return Err("Expected 'replace' or 'highlight'".to_string());
                }
                config.spellcheck.mode = mode;
            }
            "languages" => {
                config.spellcheck.languages =
                    raw.split(',').map(|s| s.trim().to_string()).collect();
            }
            "dictionary_dir" => config.spellcheck.dictionary_dir = raw.to_string(),
            _ => return Err(format!("Unknown field: {path}")),
        },
        "logging" => match parts[1] {
            "event_retention_hours" => {
                config.logging.event_retention_hours =
                    raw.parse().map_err(|_| "Expected a number".to_string())?;
            }
            "retention_days" => {
                config.logging.retention_days =
                    raw.parse().map_err(|_| "Expected a number".to_string())?;
            }
            _ => return Err(format!("Unknown field: {path}")),
        },
        "web" => match parts[1] {
            "enabled" => config.web.enabled = parse_bool(raw)?,
            "bind_address" => config.web.bind_address = raw.to_string(),
            "port" => config.web.port = parse_u16(raw)?,
            "tls_cert" => config.web.tls_cert = raw.to_string(),
            "tls_key" => config.web.tls_key = raw.to_string(),
            "timestamp_format" => config.web.timestamp_format = raw.to_string(),
            "line_height" => {
                config.web.line_height =
                    raw.parse().map_err(|_| "Expected a decimal number".to_string())?;
            }
            "nick_column_width" => {
                config.web.nick_column_width =
                    raw.parse().map_err(|_| "Expected a number".to_string())?;
            }
            "nick_max_length" => {
                config.web.nick_max_length =
                    raw.parse().map_err(|_| "Expected a number".to_string())?;
            }
            "theme" => config.web.theme = raw.to_string(),
            "session_hours" => {
                config.web.session_hours =
                    raw.parse().map_err(|_| "Expected a positive integer (hours)".to_string())?;
            }
            "cloudflare_tunnel_name" => config.web.cloudflare_tunnel_name = raw.to_string(),
            "password" => config.web.password = raw.to_string(),
            _ => return Err(format!("Unknown field: {path}")),
        },
        "servers" if parts.len() >= 3 => {
            let server = config
                .servers
                .get_mut(parts[1])
                .ok_or_else(|| format!("Unknown server: {}", parts[1]))?;
            match parts[2] {
                "label" => server.label = raw.to_string(),
                "address" => server.address = raw.to_string(),
                "port" => server.port = parse_u16(raw)?,
                "tls" => server.tls = parse_bool(raw)?,
                "tls_verify" => server.tls_verify = parse_bool(raw)?,
                "autoconnect" => server.autoconnect = parse_bool(raw)?,
                "channels" => {
                    server.channels = raw.split(',').map(|s| s.trim().to_string()).collect();
                }
                "nick" => server.nick = Some(raw.to_string()),
                "username" => server.username = Some(raw.to_string()),
                "realname" => server.realname = Some(raw.to_string()),
                "password" => server.password = Some(raw.to_string()),
                "sasl_user" => server.sasl_user = Some(raw.to_string()),
                "sasl_pass" => server.sasl_pass = Some(raw.to_string()),
                _ => return Err(format!("Unknown field: {path}")),
            }
        }
        _ => return Err(format!("Unknown section: {}", parts[0])),
    }

    Ok(())
}

fn parse_bool(raw: &str) -> Result<bool, String> {
    match raw {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err("Expected true or false".to_string()),
    }
}

fn parse_u16(raw: &str) -> Result<u16, String> {
    raw.parse().map_err(|_| "Expected a number".to_string())
}

// === Available setting paths for tab completion ===

/// Base setting paths (without server-specific ones).
const BASE_PATHS: &[&str] = &[
    "general.nick",
    "general.username",
    "general.realname",
    "general.theme",
    "general.timestamp_format",
    "general.flood_protection",
    "general.ctcp_version",
    "display.nick_column_width",
    "display.nick_max_length",
    "display.nick_alignment",
    "display.nick_truncation",
    "display.show_timestamps",
    "display.scrollback_lines",
    "display.backlog_lines",
    "display.nick_colors",
    "display.nick_colors_in_nicklist",
    "display.nick_color_saturation",
    "display.nick_color_lightness",
    "display.mentions_buffer",
    "sidepanel.left.width",
    "sidepanel.left.visible",
    "sidepanel.right.width",
    "sidepanel.right.visible",
    "statusbar.enabled",
    "statusbar.separator",
    "statusbar.prompt",
    "statusbar.background",
    "statusbar.text_color",
    "statusbar.accent_color",
    "statusbar.muted_color",
    "statusbar.dim_color",
    "statusbar.prompt_color",
    "statusbar.input_color",
    "statusbar.cursor_color",
    "image_preview.enabled",
    "image_preview.max_width",
    "image_preview.max_height",
    "image_preview.cache_max_mb",
    "image_preview.cache_max_days",
    "image_preview.fetch_timeout",
    "image_preview.max_file_size",
    "image_preview.protocol",
    "image_preview.kitty_format",
    "dcc.timeout",
    "dcc.own_ip",
    "dcc.port_range",
    "dcc.autoaccept_lowports",
    "dcc.autochat_masks",
    "dcc.max_connections",
    "logging.event_retention_hours",
    "logging.retention_days",
    "spellcheck.enabled",
    "spellcheck.computing",
    "spellcheck.mode",
    "spellcheck.languages",
    "spellcheck.dictionary_dir",
    "web.enabled",
    "web.bind_address",
    "web.port",
    "web.tls_cert",
    "web.tls_key",
    "web.timestamp_format",
    "web.line_height",
    "web.nick_column_width",
    "web.nick_max_length",
    "web.theme",
    "web.session_hours",
    "web.cloudflare_tunnel_name",
    "web.password",
];

const SERVER_FIELDS: &[&str] = &[
    "label",
    "address",
    "port",
    "tls",
    "tls_verify",
    "autoconnect",
    "channels",
    "nick",
    "username",
    "realname",
    "password",
    "sasl_user",
    "sasl_pass",
];

/// Get all valid setting paths for tab completion.
pub fn get_setting_paths(config: &AppConfig) -> Vec<String> {
    let mut paths: Vec<String> = BASE_PATHS
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    for server_id in config.servers.keys() {
        for field in SERVER_FIELDS {
            paths.push(format!("servers.{server_id}.{field}"));
        }
    }
    paths.sort();
    paths
}

// === Command handler ===

#[expect(clippy::too_many_lines, reason = "flat dispatcher with per-section side-effects")]
pub fn cmd_set(app: &mut App, args: &[String]) {
    let ev = super::helpers::add_local_event;

    if args.is_empty() {
        // List all settings
        list_all_settings(app);
        return;
    }

    let path = &args[0];

    if args.len() < 2 {
        // Show current value — or search if no exact match (irssi-style)
        if let Some(resolved) = get_config_value(&app.config, path) {
            let display = if resolved.is_credential && !resolved.value.is_empty() {
                format!("*** {C_DIM}[credential]{C_RST}")
            } else {
                format!("{C_CMD}{}{C_RST}", resolved.value.replace('%', "%%"))
            };
            ev(app, &format!("{C_HEADER}{path}{C_RST} = {display}"));
        } else {
            search_settings(app, path);
        }
        return;
    }

    // Set value
    let raw = &args[1];

    // Validate path exists first
    if get_config_value(&app.config, path).is_none() {
        ev(app, &format!("{C_ERR}Unknown setting: {path}{C_RST}"));
        return;
    }

    match set_config_value(&mut app.config, path, raw) {
        Ok(()) => {
            app.cached_config_toml = None;
            ev(app, &format!("{C_OK}{path}{C_RST} = {C_CMD}{}{C_RST}", raw.replace('%', "%%")));

            // Save config (web.password is #[serde(skip)] — saved to .env instead).
            let cfg_path = crate::constants::config_path();
            if let Err(e) = crate::config::save_config(&cfg_path, &app.config) {
                ev(app, &format!("{C_ERR}Failed to save config: {e}{C_RST}"));
            }

            // Persist credentials to .env (not config.toml).
            if path == "web.password" {
                let env_path = crate::constants::env_path();
                if let Err(e) = crate::config::set_env_value(&env_path, "WEB_PASSWORD", raw) {
                    ev(app, &format!("{C_ERR}Failed to save to .env: {e}{C_RST}"));
                } else {
                    ev(app, &format!("{C_DIM}Password saved to .env{C_RST}"));
                }
            }

            // Hot restart web server when lifecycle settings change.
            if matches!(path.as_str(),
                "web.enabled" | "web.port" | "web.bind_address" | "web.password"
                | "web.tls_cert" | "web.tls_key" | "web.session_hours"
            ) {
                app.web_restart_pending = true;
                if path.as_str() != "web.enabled" || raw == "true" {
                    ev(app, &format!("{C_DIM}Web server will restart...{C_RST}"));
                }
            }

            // Sync runtime state from config
            if path == "display.scrollback_lines" {
                app.state.scrollback_limit = app.config.display.scrollback_lines;
            }

            if path == "display.mentions_buffer" && !app.config.display.mentions_buffer {
                app.state.remove_buffer("_mentions");
            }
            // create_mentions_buffer() will be wired in Task 4 when mentions_buffer is enabled

            // Sync DCC runtime state from config
            if path.starts_with("dcc.") {
                match path.as_str() {
                    "dcc.timeout" => {
                        app.dcc.timeout_secs = app.config.dcc.timeout;
                    }
                    "dcc.own_ip" => {
                        app.dcc.own_ip = if app.config.dcc.own_ip.is_empty() {
                            None
                        } else {
                            app.config.dcc.own_ip.parse().ok()
                        };
                    }
                    "dcc.port_range" => {
                        app.dcc.port_range =
                            crate::dcc::chat::parse_port_range(&app.config.dcc.port_range);
                    }
                    "dcc.autoaccept_lowports" => {
                        app.dcc.autoaccept_lowports = app.config.dcc.autoaccept_lowports;
                    }
                    "dcc.autochat_masks" => {
                        app.dcc
                            .autochat_masks
                            .clone_from(&app.config.dcc.autochat_masks);
                    }
                    "dcc.max_connections" => {
                        app.dcc.max_connections = app.config.dcc.max_connections;
                    }
                    _ => {}
                }
            }

            // Sync spellcheck runtime state
            if path.starts_with("spellcheck.") {
                app.reload_spellchecker();
            }

            // Broadcast web settings changes to connected web clients.
            if path == "web.timestamp_format"
                || path == "web.line_height"
                || path == "web.theme"
                || path == "web.nick_column_width"
                || path == "web.nick_max_length"
                || path.starts_with("display.nick_color")
            {
                app.state.pending_web_events.push(
                    crate::web::protocol::WebEvent::SettingsChanged {
                        timestamp_format: app.config.web.timestamp_format.clone(),
                        line_height: app.config.web.line_height,
                        theme: app.config.web.theme.clone(),
                        nick_column_width: app.config.web.nick_column_width,
                        nick_max_length: app.config.web.nick_max_length,
                        nick_colors: app.config.display.nick_colors,
                        nick_colors_in_nicklist: app.config.display.nick_colors_in_nicklist,
                        nick_color_saturation: app.config.display.nick_color_saturation,
                        nick_color_lightness: app.config.display.nick_color_lightness,
                    },
                );
            }

            // Resize shells when sidebar layout changes (affects chat area dimensions).
            if path.starts_with("sidepanel.") {
                app.resize_all_shells();
            }

            // Special handling: reload theme if theme name changed
            if path == "general.theme" {
                let theme_path = crate::constants::theme_dir().join(format!("{raw}.theme"));
                match crate::theme::load_theme(&theme_path) {
                    Ok(theme) => {
                        app.theme = theme;
                        ev(app, &format!("{C_OK}Theme '{raw}' loaded{C_RST}"));
                    }
                    Err(e) => {
                        ev(app, &format!("{C_ERR}Failed to load theme: {e}{C_RST}"));
                    }
                }
            }

            // Recompute cached wrap-indent when relevant settings change.
            if path == "general.timestamp_format"
                || path == "display.nick_column_width"
                || path == "general.theme"
            {
                app.recompute_wrap_indent();
            }
        }
        Err(e) => {
            ev(app, &format!("{C_ERR}{e}{C_RST}"));
        }
    }
}

/// irssi-style substring search: `/set nick` lists all settings containing "nick".
fn search_settings(app: &mut App, needle: &str) {
    let ev = super::helpers::add_local_event;
    let lower = needle.to_lowercase();
    let all_paths = get_setting_paths(&app.config);
    let matches: Vec<&String> = all_paths
        .iter()
        .filter(|p| p.to_lowercase().contains(&lower))
        .collect();
    if matches.is_empty() {
        ev(app, &format!("{C_ERR}Unknown setting: {needle}{C_RST}"));
    } else {
        ev(app, &divider(&format!("Settings matching *{needle}*")));
        for matched_path in &matches {
            if let Some(resolved) = get_config_value(&app.config, matched_path) {
                let val = if resolved.is_credential && !resolved.value.is_empty() {
                    "***".to_string()
                } else {
                    resolved.value
                };
                ev(
                    app,
                    &format!("  {C_HEADER}{matched_path}{C_RST} = {C_CMD}{}{C_RST}", val.replace('%', "%%")),
                );
            }
        }
    }
}

fn list_all_settings(app: &mut App) {
    // Collect all lines first to avoid borrow conflicts
    let lines = build_settings_lines(&app.config);
    for line in lines {
        super::helpers::add_local_event(app, &line);
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "flat section listing — one block per config section"
)]
fn build_settings_lines(config: &AppConfig) -> Vec<String> {
    let mut lines = Vec::new();

    lines.push(divider("Settings"));

    let sections: &[(&str, &[&str])] = &[
        (
            "general",
            &[
                "nick",
                "username",
                "realname",
                "theme",
                "timestamp_format",
                "flood_protection",
                "ctcp_version",
            ],
        ),
        (
            "display",
            &[
                "nick_column_width",
                "nick_max_length",
                "nick_alignment",
                "nick_truncation",
                "show_timestamps",
                "scrollback_lines",
                "backlog_lines",
                "nick_colors",
                "nick_colors_in_nicklist",
                "nick_color_saturation",
                "nick_color_lightness",
            ],
        ),
    ];

    for &(section, fields) in sections {
        lines.push(format!("  {C_DIM}[{section}]{C_RST}"));
        for field in fields {
            let path = format!("{section}.{field}");
            if let Some(resolved) = get_config_value(config, &path) {
                let val = if resolved.is_credential && !resolved.value.is_empty() {
                    "***".to_string()
                } else {
                    resolved.value
                };
                lines.push(format!("    {C_HEADER}{path}{C_RST} = {C_CMD}{}{C_RST}", val.replace('%', "%%")));
            }
        }
    }

    // Sidepanel
    lines.push(format!("  {C_DIM}[sidepanel]{C_RST}"));
    for side in &["left", "right"] {
        for field in &["width", "visible"] {
            let path = format!("sidepanel.{side}.{field}");
            if let Some(resolved) = get_config_value(config, &path) {
                lines.push(format!(
                    "    {C_HEADER}{path}{C_RST} = {C_CMD}{}{C_RST}",
                    resolved.value
                ));
            }
        }
    }

    // Statusbar
    lines.push(format!("  {C_DIM}[statusbar]{C_RST}"));
    for field in &[
        "enabled",
        "separator",
        "prompt",
        "background",
        "text_color",
        "accent_color",
        "muted_color",
        "dim_color",
        "prompt_color",
        "input_color",
        "cursor_color",
    ] {
        let path = format!("statusbar.{field}");
        if let Some(resolved) = get_config_value(config, &path) {
            lines.push(format!(
                "    {C_HEADER}{path}{C_RST} = {C_CMD}{}{C_RST}",
                resolved.value
            ));
        }
    }

    // DCC
    lines.push(format!("  {C_DIM}[dcc]{C_RST}"));
    for field in &[
        "timeout",
        "own_ip",
        "port_range",
        "autoaccept_lowports",
        "autochat_masks",
        "max_connections",
    ] {
        let path = format!("dcc.{field}");
        if let Some(resolved) = get_config_value(config, &path) {
            lines.push(format!(
                "    {C_HEADER}{path}{C_RST} = {C_CMD}{}{C_RST}",
                resolved.value
            ));
        }
    }

    // Logging
    lines.push(format!("  {C_DIM}[logging]{C_RST}"));
    for field in &["event_retention_hours", "retention_days"] {
        let path = format!("logging.{field}");
        if let Some(resolved) = get_config_value(config, &path) {
            lines.push(format!(
                "    {C_HEADER}{path}{C_RST} = {C_CMD}{}{C_RST}",
                resolved.value
            ));
        }
    }

    // Spellcheck
    lines.push(format!("  {C_DIM}[spellcheck]{C_RST}"));
    for field in &["enabled", "computing", "mode", "languages", "dictionary_dir"] {
        let path = format!("spellcheck.{field}");
        if let Some(resolved) = get_config_value(config, &path) {
            lines.push(format!(
                "    {C_HEADER}{path}{C_RST} = {C_CMD}{}{C_RST}",
                resolved.value
            ));
        }
    }

    // Servers
    for server_id in config.servers.keys() {
        lines.push(format!("  {C_DIM}[servers.{server_id}]{C_RST}"));
        for field in SERVER_FIELDS {
            let path = format!("servers.{server_id}.{field}");
            if let Some(resolved) = get_config_value(config, &path) {
                let val = if resolved.is_credential && !resolved.value.is_empty() {
                    "***".to_string()
                } else {
                    resolved.value
                };
                lines.push(format!("    {C_HEADER}{path}{C_RST} = {C_CMD}{}{C_RST}", val.replace('%', "%%")));
            }
        }
    }

    lines.push(divider(""));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_config;

    #[test]
    fn get_general_nick() {
        let config = default_config();
        let r = get_config_value(&config, "general.nick").unwrap();
        assert_eq!(r.value, config.general.nick);
        assert!(!r.is_credential);
    }

    #[test]
    fn get_display_field() {
        let config = default_config();
        let r = get_config_value(&config, "display.nick_column_width").unwrap();
        assert_eq!(r.value, "8");
    }

    #[test]
    fn get_sidepanel_field() {
        let config = default_config();
        let r = get_config_value(&config, "sidepanel.left.width").unwrap();
        assert_eq!(r.value, "20");
    }

    #[test]
    fn get_unknown_returns_none() {
        let config = default_config();
        assert!(get_config_value(&config, "nonexistent.field").is_none());
        assert!(get_config_value(&config, "general.nonexistent").is_none());
        assert!(get_config_value(&config, "").is_none());
    }

    #[test]
    fn set_general_nick() {
        let mut config = default_config();
        set_config_value(&mut config, "general.nick", "newnick").unwrap();
        assert_eq!(config.general.nick, "newnick");
    }

    #[test]
    fn set_display_number() {
        let mut config = default_config();
        set_config_value(&mut config, "display.nick_column_width", "12").unwrap();
        assert_eq!(config.display.nick_column_width, 12);
    }

    #[test]
    fn set_bool_field() {
        let mut config = default_config();
        set_config_value(&mut config, "display.show_timestamps", "false").unwrap();
        assert!(!config.display.show_timestamps);
    }

    #[test]
    fn set_invalid_bool() {
        let mut config = default_config();
        let result = set_config_value(&mut config, "display.show_timestamps", "yes");
        assert!(result.is_err());
    }

    #[test]
    fn set_invalid_number() {
        let mut config = default_config();
        let result = set_config_value(&mut config, "display.nick_column_width", "abc");
        assert!(result.is_err());
    }

    #[test]
    fn set_alignment() {
        let mut config = default_config();
        set_config_value(&mut config, "display.nick_alignment", "left").unwrap();
        assert_eq!(
            config.display.nick_alignment,
            crate::config::NickAlignment::Left
        );
    }

    #[test]
    fn setting_paths_include_base() {
        let config = default_config();
        let paths = get_setting_paths(&config);
        assert!(paths.contains(&"general.nick".to_string()));
        assert!(paths.contains(&"display.scrollback_lines".to_string()));
        assert!(paths.contains(&"sidepanel.left.width".to_string()));
    }

    #[test]
    fn search_by_substring() {
        let config = default_config();
        let all = get_setting_paths(&config);
        let matches: Vec<&String> = all.iter().filter(|p| p.contains("nick")).collect();
        // Should find general.nick, display.nick_column_width, display.nick_max_length, etc.
        assert!(matches.len() >= 4);
        assert!(matches.iter().any(|p| *p == "general.nick"));
        assert!(matches.iter().any(|p| *p == "display.nick_column_width"));
    }

    #[test]
    fn search_no_matches() {
        let config = default_config();
        let all = get_setting_paths(&config);
        let has_match = all
            .iter()
            .any(|p| p.to_lowercase().contains("zzzznonexistent"));
        assert!(!has_match);
    }

    #[test]
    fn setting_paths_include_servers() {
        let mut config = default_config();
        config.servers.insert(
            "test".to_string(),
            crate::config::ServerConfig {
                label: "Test".to_string(),
                address: "irc.test.net".to_string(),
                port: 6667,
                tls: false,
                tls_verify: true,
                autoconnect: false,
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
        let paths = get_setting_paths(&config);
        assert!(paths.contains(&"servers.test.port".to_string()));
        assert!(paths.contains(&"servers.test.tls".to_string()));
    }

    #[test]
    fn get_set_nick_colors() {
        let mut config = default_config();
        let r = get_config_value(&config, "display.nick_colors").unwrap();
        assert_eq!(r.value, "true");
        set_config_value(&mut config, "display.nick_colors", "false").unwrap();
        assert!(!config.display.nick_colors);
    }

    #[test]
    fn set_nick_color_saturation_validates_range() {
        let mut config = default_config();
        assert!(set_config_value(&mut config, "display.nick_color_saturation", "0.7").is_ok());
        assert!(set_config_value(&mut config, "display.nick_color_saturation", "1.5").is_err());
        assert!(set_config_value(&mut config, "display.nick_color_saturation", "-0.1").is_err());
    }
}

pub mod catalog;

use super::AppConfig;

/// Result of resolving a dot-notation config path.
pub struct Resolved {
    pub value: String,
    pub is_credential: bool,
}

/// Get a config value by dot-notation path.
#[expect(
    clippy::too_many_lines,
    reason = "flat match dispatcher — one arm per config field"
)]
pub fn get_config_value(config: &AppConfig, path: &str) -> Option<Resolved> {
    if let Some(value) = catalog::extra_value(config, path) {
        return Some(Resolved {
            value,
            is_credential: path == "shrink.api_key",
        });
    }
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
                "flood_exemptions" => config.general.flood_exemptions.join(", "),
                "ctcp_version" => config.general.ctcp_version.clone(),
                "default_bind_ip" => config.general.default_bind_ip.clone().unwrap_or_default(),
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
                "inline" => config.image_preview.inline.to_string(),
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
        "shrink" => {
            // `api_key` is intentionally never exposed via /set — it
            // lives in .env and `apply_shrink_credentials` loads it on
            // startup. Reading it via /set would surface secrets in
            // command output and tab-completion.
            let val = match parts[1] {
                "enabled" => config.shrink.enabled.to_string(),
                "api_url" => config.shrink.api_url.clone(),
                "outgoing_enabled" => config.shrink.outgoing_enabled.to_string(),
                "incoming_enabled" => config.shrink.incoming_enabled.to_string(),
                "min_url_length" => config.shrink.min_url_length.to_string(),
                "outgoing_timeout_ms" => config.shrink.outgoing_timeout_ms.to_string(),
                "incoming_timeout_ms" => config.shrink.incoming_timeout_ms.to_string(),
                "cache_max_entries" => config.shrink.cache_max_entries.to_string(),
                _ => return None,
            };
            Some(Resolved {
                value: val,
                is_credential: false,
            })
        }
        "translate" => {
            // `buffers` is not exposed here: it is a per-buffer map, managed
            // by `/translate addin|delin|addout|delout`, and a dotted-path
            // setter has no sane spelling for it.
            let val = match parts.as_slice() {
                ["translate", "enabled"] => config.translate.enabled.to_string(),
                ["translate", "backend"] => config.translate.backend.clone(),
                ["translate", "my_lang"] => config.translate.my_lang.clone(),
                ["translate", "show_original_in"] => config.translate.show_original_in.to_string(),
                ["translate", "show_original_out"] => {
                    config.translate.show_original_out.to_string()
                }
                ["translate", "timeout_ms"] => config.translate.timeout_ms.to_string(),
                ["translate", "max_in_flight"] => config.translate.max_in_flight.to_string(),
                ["translate", "max_queue"] => config.translate.max_queue.to_string(),
                ["translate", "ai", "preferred_attempt_ms"] => {
                    config.translate.ai.preferred_attempt_ms.to_string()
                }
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
                "session_days" => config.web.session_days.to_string(),
                "username" => config.web.username.clone(),
                "image_previews" => config.web.image_previews.to_string(),
                "image_previews_max_per_msg" => config.web.image_previews_max_per_msg.to_string(),
                "thumbnail_cache_mb" => config.web.thumbnail_cache_mb.to_string(),
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
            let is_cred = matches!(parts[2], "password" | "sasl_pass");
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
                "bind_ip" => server.bind_ip.clone().unwrap_or_default(),
                "encoding" => server.encoding.clone().unwrap_or_default(),
                "auto_reconnect" => server
                    .auto_reconnect
                    .map_or_else(String::new, |v| v.to_string()),
                "reconnect_delay" => server
                    .reconnect_delay
                    .map_or_else(String::new, |v| v.to_string()),
                "reconnect_max_retries" => server
                    .reconnect_max_retries
                    .map_or_else(String::new, |v| v.to_string()),
                "autosendcmd" => server.autosendcmd.clone().unwrap_or_default(),
                "sasl_mechanism" => server.sasl_mechanism.clone().unwrap_or_default(),
                "client_cert_path" => server.client_cert_path.clone().unwrap_or_default(),
                "sasl_key_path" => server.sasl_key_path.clone().unwrap_or_default(),
                "bouncer_network_id" => server.bouncer_network_id.clone().unwrap_or_default(),
                "bouncer_control" => server.bouncer_control.to_string(),
                _ => return None,
            };
            Some(Resolved {
                value: val,
                is_credential: is_cred,
            })
        }
        "emotes" => {
            let val = match parts[1] {
                "enabled" => config.emotes.enabled.to_string(),
                "render" => format!("{:?}", config.emotes.render).to_lowercase(),
                "lang" => format!("{:?}", config.emotes.lang).to_lowercase(),
                _ => return None,
            };
            Some(Resolved {
                value: val,
                is_credential: false,
            })
        }
        "typing" => {
            let val = match parts[1] {
                "show" => config.typing.show.to_string(),
                "send_channels" => config.typing.send_channels.to_string(),
                "send_queries" => config.typing.send_queries.to_string(),
                _ => return None,
            };
            Some(Resolved {
                value: val,
                is_credential: false,
            })
        }
        _ => None,
    }
}

/// Set a config value by dot-notation path. Returns true on success.
#[expect(clippy::too_many_lines)]
pub fn set_config_value(config: &mut AppConfig, path: &str, raw: &str) -> Result<(), String> {
    if catalog::EXTRA_PATHS.contains(&path) {
        return catalog::set_extra(config, path, raw);
    }
    let parts: Vec<&str> = path.split('.').collect();
    if parts.len() < 2 {
        return Err("Invalid path".to_string());
    }

    if raw.trim().is_empty()
        && (matches!(path, "general.nick" | "general.username")
            || (parts[0] == "servers"
                && parts.len() >= 3
                && matches!(parts[2], "label" | "address")))
    {
        return Err("This setting cannot be empty".to_string());
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
            "flood_exemptions" => {
                config.general.flood_exemptions = split_list(raw);
            }
            "ctcp_version" => config.general.ctcp_version = raw.to_string(),
            "default_bind_ip" => {
                // Empty string clears the field (matches the
                // /set ... "" convention used elsewhere for Option<T>).
                config.general.default_bind_ip = if raw.is_empty() {
                    None
                } else {
                    Some(raw.to_string())
                };
            }
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
            "inline" => config.image_preview.inline = parse_bool(raw)?,
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
                config.dcc.autochat_masks = split_list(raw);
            }
            "max_connections" => {
                config.dcc.max_connections =
                    raw.parse().map_err(|_| "Expected a number".to_string())?;
            }
            _ => return Err(format!("Unknown field: {path}")),
        },
        "translate" => match parts.as_slice() {
            ["translate", "enabled"] => config.translate.enabled = parse_bool(raw)?,
            ["translate", "backend"] => {
                // Rejected rather than stored-and-ignored: an unknown name
                // installs nothing, and a config that asks for translation
                // and silently does none is the failure this whole setting
                // exists to make visible.
                let want = raw.trim().to_ascii_lowercase();
                if !crate::translate::backend::BACKEND_NAMES.contains(&want.as_str()) {
                    return Err(format!(
                        "translate.backend must be one of: {}",
                        crate::translate::backend::BACKEND_NAMES.join(", ")
                    ));
                }
                config.translate.backend = want;
            }
            ["translate", "my_lang"] => {
                if raw.trim().is_empty() {
                    return Err("translate.my_lang must not be empty".to_string());
                }
                config.translate.my_lang = raw.trim().to_lowercase();
            }
            ["translate", "show_original_in"] => {
                config.translate.show_original_in = parse_bool(raw)?;
            }
            ["translate", "show_original_out"] => {
                config.translate.show_original_out = parse_bool(raw)?;
            }
            ["translate", "timeout_ms"] => {
                let v: u64 = raw.parse().map_err(|_| "Expected a number".to_string())?;
                // Floor 500: below that a healthy provider would be cut off
                // mid-flight and every line would render untranslated, which
                // looks like a broken feature rather than a tight budget.
                if v < 500 {
                    return Err("translate.timeout_ms must be at least 500".to_string());
                }
                config.translate.timeout_ms = v;
            }
            ["translate", "max_in_flight"] => {
                let v: u32 = raw.parse().map_err(|_| "Expected a number".to_string())?;
                if v < 1 {
                    return Err("translate.max_in_flight must be at least 1".to_string());
                }
                config.translate.max_in_flight = v;
            }
            ["translate", "max_queue"] => {
                let v: u32 = raw.parse().map_err(|_| "Expected a number".to_string())?;
                if v < 1 {
                    return Err("translate.max_queue must be at least 1".to_string());
                }
                config.translate.max_queue = v;
            }
            ["translate", "ai", "preferred_attempt_ms"] => {
                let v: u64 = raw.parse().map_err(|_| "Expected a number".to_string())?;
                if v < 500 {
                    return Err(
                        "translate.ai.preferred_attempt_ms must be at least 500".to_string()
                    );
                }
                config.translate.ai.preferred_attempt_ms = v;
            }
            _ => return Err(format!("Unknown field: {path}")),
        },
        "shrink" => match parts[1] {
            "enabled" => config.shrink.enabled = parse_bool(raw)?,
            "api_url" => config.shrink.api_url = raw.to_string(),
            "outgoing_enabled" => config.shrink.outgoing_enabled = parse_bool(raw)?,
            "incoming_enabled" => config.shrink.incoming_enabled = parse_bool(raw)?,
            "min_url_length" => {
                let v: u32 = raw.parse().map_err(|_| "Expected a number".to_string())?;
                // Floor 25: shorter thresholds risk shortening URLs
                // that aren't actually long enough to be worth it, and
                // each shrink is an HTTP round-trip to the API.
                if v < 25 {
                    return Err("shrink.min_url_length must be at least 25".to_string());
                }
                config.shrink.min_url_length = v;
            }
            "outgoing_timeout_ms" => {
                // Floor at 100 ms. Anything lower makes
                // tokio::time::timeout fire before reqwest can
                // even open a TCP connection, so every shrink
                // returns Timeout and the user silently never
                // sees a shortened URL.
                let v: u64 = raw.parse().map_err(|_| "Expected a number".to_string())?;
                if v < 100 {
                    return Err("shrink.outgoing_timeout_ms must be at least 100".to_string());
                }
                config.shrink.outgoing_timeout_ms = v;
            }
            "incoming_timeout_ms" => {
                let v: u64 = raw.parse().map_err(|_| "Expected a number".to_string())?;
                if v < 100 {
                    return Err("shrink.incoming_timeout_ms must be at least 100".to_string());
                }
                config.shrink.incoming_timeout_ms = v;
            }
            "cache_max_entries" => {
                // Floor at 1. ShrinkCache::new internally clamps
                // 0 → 1 anyway; making /set reject 0 explicitly
                // avoids the surprise of `/set` reporting `= 0`
                // while the live cache silently uses 1.
                let v: u32 = raw.parse().map_err(|_| "Expected a number".to_string())?;
                if v == 0 {
                    return Err("shrink.cache_max_entries must be at least 1".to_string());
                }
                config.shrink.cache_max_entries = v;
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
                config.spellcheck.languages = split_list(raw);
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
                config.web.line_height = raw
                    .parse()
                    .map_err(|_| "Expected a decimal number".to_string())?;
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
            "session_days" => {
                config.web.session_days = raw
                    .parse()
                    .map_err(|_| "Expected a positive integer (days)".to_string())?;
            }
            "username" => config.web.username = raw.to_string(),
            "image_previews" => config.web.image_previews = parse_bool(raw)?,
            "image_previews_max_per_msg" => {
                config.web.image_previews_max_per_msg =
                    raw.parse().map_err(|_| "Expected a number".to_string())?;
            }
            "thumbnail_cache_mb" => {
                config.web.thumbnail_cache_mb =
                    raw.parse().map_err(|_| "Expected a number".to_string())?;
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
                    server.channels = split_list(raw);
                }
                "nick" => server.nick = (!raw.is_empty()).then(|| raw.to_string()),
                "username" => server.username = (!raw.is_empty()).then(|| raw.to_string()),
                "realname" => server.realname = (!raw.is_empty()).then(|| raw.to_string()),
                "password" => server.password = (!raw.is_empty()).then(|| raw.to_string()),
                "sasl_user" => server.sasl_user = (!raw.is_empty()).then(|| raw.to_string()),
                "sasl_pass" => server.sasl_pass = (!raw.is_empty()).then(|| raw.to_string()),
                "bind_ip" => server.bind_ip = (!raw.is_empty()).then(|| raw.to_string()),
                "encoding" => server.encoding = (!raw.is_empty()).then(|| raw.to_string()),
                "auto_reconnect" => {
                    server.auto_reconnect =
                        (!raw.is_empty()).then(|| parse_bool(raw)).transpose()?;
                }
                "reconnect_delay" => {
                    server.reconnect_delay = (!raw.is_empty())
                        .then(|| {
                            raw.parse()
                                .map_err(|_| "Expected a positive integer".to_string())
                        })
                        .transpose()?;
                }
                "reconnect_max_retries" => {
                    server.reconnect_max_retries = (!raw.is_empty())
                        .then(|| {
                            raw.parse()
                                .map_err(|_| "Expected a positive integer".to_string())
                        })
                        .transpose()?;
                }
                "autosendcmd" => server.autosendcmd = (!raw.is_empty()).then(|| raw.to_string()),
                "sasl_mechanism" => {
                    server.sasl_mechanism = (!raw.is_empty())
                        .then(|| parse_sasl_mechanism(raw))
                        .transpose()?;
                }
                "client_cert_path" => {
                    server.client_cert_path = (!raw.is_empty()).then(|| raw.to_string());
                }
                "sasl_key_path" => {
                    server.sasl_key_path = (!raw.is_empty()).then(|| raw.to_string());
                }
                "bouncer_control" => server.bouncer_control = parse_bool(raw)?,
                "bouncer_network_id" => {
                    server.bouncer_network_id = if raw.is_empty() {
                        None
                    } else {
                        Some(crate::irc::bouncer::normalize_network_id(raw)?)
                    };
                }
                _ => return Err(format!("Unknown field: {path}")),
            }
        }
        "emotes" => match parts[1] {
            "enabled" => config.emotes.enabled = parse_bool(raw)?,
            "render" => {
                config.emotes.render = match raw.to_ascii_lowercase().as_str() {
                    "graphical" => crate::config::RenderMode::Graphical,
                    "text" => crate::config::RenderMode::Text,
                    "off" => crate::config::RenderMode::Off,
                    _ => return Err("Expected graphical, text, or off".to_string()),
                };
            }
            "lang" => {
                config.emotes.lang = match raw.to_ascii_lowercase().as_str() {
                    "en" => crate::config::EmoteLang::En,
                    "pl" => crate::config::EmoteLang::Pl,
                    _ => return Err("Expected en or pl".to_string()),
                };
            }
            _ => return Err(format!("Unknown field: {path}")),
        },
        "typing" => match parts[1] {
            "show" => config.typing.show = parse_bool(raw)?,
            "send_channels" => config.typing.send_channels = parse_bool(raw)?,
            "send_queries" => config.typing.send_queries = parse_bool(raw)?,
            _ => return Err(format!("Unknown field: {path}")),
        },
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

/// Validate a `sasl_mechanism` value, normalising it to its canonical spelling.
///
/// Unvalidated, a typo here fails silently at connect time: the mechanism does
/// not resolve, SASL is skipped, and the user is left staring at an
/// unauthenticated connection with no hint that `SCRAM-SHA256` is not a name.
pub fn parse_sasl_mechanism(raw: &str) -> Result<String, String> {
    crate::irc::SaslMechanism::from_name(raw)
        .map(|m| m.name().to_string())
        .ok_or_else(|| {
            let names: Vec<&str> = crate::irc::SASL_MECHANISMS
                .iter()
                .map(|m| m.name())
                .collect();
            format!("Expected one of: {}", names.join(", "))
        })
}

fn split_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn server_password_env_key(path: &str) -> Option<String> {
    let mut parts = path.split('.');
    let (Some("servers"), Some(server_id), Some(field), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let suffix = match field {
        "password" => "PASSWORD",
        "sasl_pass" => "SASL_PASS",
        _ => return None,
    };
    Some(format!("{}_{suffix}", server_id.to_uppercase()))
}

// === Available setting paths for tab completion ===

/// Base setting paths (without server-specific ones).
pub const BASE_PATHS: &[&str] = &[
    "general.nick",
    "general.username",
    "general.realname",
    "general.theme",
    "general.timestamp_format",
    "general.flood_protection",
    "general.flood_exemptions",
    "general.ctcp_version",
    "general.default_bind_ip",
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
    "image_preview.inline",
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
    "shrink.enabled",
    "shrink.api_url",
    "shrink.outgoing_enabled",
    "shrink.incoming_enabled",
    "shrink.min_url_length",
    "shrink.outgoing_timeout_ms",
    "shrink.incoming_timeout_ms",
    "shrink.cache_max_entries",
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
    "web.session_days",
    "web.username",
    "web.image_previews",
    "web.image_previews_max_per_msg",
    "web.thumbnail_cache_mb",
    "web.cloudflare_tunnel_name",
    "web.password",
    "emotes.enabled",
    "emotes.render",
    "emotes.lang",
    "typing.show",
    "typing.send_channels",
    "typing.send_queries",
    // Per-buffer translation settings are deliberately absent: they live in
    // a map keyed by buffer id and are managed by `/translate add*|del*`,
    // which a dotted `/set` path has no sane spelling for.
    "translate.enabled",
    "translate.backend",
    "translate.my_lang",
    "translate.show_original_in",
    "translate.show_original_out",
    "translate.timeout_ms",
    "translate.max_in_flight",
    "translate.max_queue",
    "translate.ai.preferred_attempt_ms",
];

pub const SERVER_FIELDS: &[&str] = &[
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
    "bind_ip",
    "encoding",
    "auto_reconnect",
    "reconnect_delay",
    "reconnect_max_retries",
    "autosendcmd",
    "sasl_mechanism",
    "client_cert_path",
    "sasl_key_path",
    "bouncer_network_id",
    "bouncer_control",
];

/// Get all valid setting paths for tab completion.
pub fn get_setting_paths(config: &AppConfig) -> Vec<String> {
    let mut paths: Vec<String> = BASE_PATHS
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    paths.extend(catalog::EXTRA_PATHS.iter().map(|path| (*path).to_string()));
    for server_id in config.servers.keys() {
        for field in SERVER_FIELDS {
            paths.push(format!("servers.{server_id}.{field}"));
        }
    }
    paths.sort();
    paths
}

pub fn prepare_changes(config: &AppConfig, changes: &[(&str, &str)]) -> Result<AppConfig, String> {
    let paths = get_setting_paths(config);
    let mut draft = config.clone();
    for &(path, value) in changes {
        if !paths.iter().any(|known| known == path) {
            return Err(format!("Unknown setting: {path}"));
        }
        if value.contains('\0')
            || (!catalog::JSON_PATHS.contains(&path) && value.contains(['\n', '\r']))
        {
            return Err(format!(
                "{path}: values must be a single line without NUL characters"
            ));
        }
        set_config_value(&mut draft, path, value).map_err(|error| format!("{path}: {error}"))?;
    }
    Ok(draft)
}

pub fn save_changes(
    config: &AppConfig,
    changes: &[crate::settings_model::SettingChange],
    config_path: &std::path::Path,
    env_path: &std::path::Path,
) -> Result<AppConfig, String> {
    let mut seen = std::collections::HashSet::new();
    for change in changes {
        if !seen.insert(&change.path) {
            return Err(format!("Duplicate setting: {}", change.path));
        }
        let current = get_config_value(config, &change.path)
            .ok_or_else(|| format!("Unknown setting: {}", change.path))?;
        if !current.is_credential && current.value != change.original {
            return Err(format!(
                "{} changed elsewhere. Reopen Settings before saving.",
                change.path
            ));
        }
    }
    let values: Vec<_> = changes
        .iter()
        .map(|c| (c.path.as_str(), c.value.as_str()))
        .collect();
    let draft = prepare_changes(config, &values)?;
    let mut env_changes: Vec<_> = changes
        .iter()
        .filter_map(|c| {
            let key = match c.path.as_str() {
                "web.password" => Some("WEB_PASSWORD".to_string()),
                "shrink.api_key" => Some("SHRINK_API_KEY".to_string()),
                path => server_password_env_key(path),
            }?;
            Some((key, c.value.as_str()))
        })
        .collect();
    let usernames: Vec<_> = changes
        .iter()
        .filter_map(|change| {
            let id = change
                .path
                .strip_prefix("servers.")?
                .strip_suffix(".sasl_user")?;
            Some((
                format!("{}_SASL_USER", id.to_uppercase()),
                change.value.as_str(),
            ))
        })
        .collect();
    if !usernames.is_empty() {
        let existing = crate::config::load_env(env_path)
            .map_err(|error| format!("Cannot read credential file: {error}"))?;
        env_changes.extend(
            usernames
                .into_iter()
                .filter(|(key, _)| existing.contains_key(key)),
        );
    }
    let old_env = if env_changes.is_empty() {
        None
    } else {
        match std::fs::read(env_path) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("Cannot read credential file: {e}")),
        }
    };
    let result = (|| {
        for (key, value) in &env_changes {
            crate::config::set_env_value(env_path, key, value)
                .map_err(|e| format!("Cannot save credentials: {e}"))?;
        }
        crate::config::save_config(config_path, &draft)
            .map_err(|e| format!("Cannot save settings: {e}"))
    })();
    if let Err(error) = result {
        if !env_changes.is_empty() {
            let restored = old_env.map_or_else(
                || match std::fs::remove_file(env_path) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    result => result,
                },
                |bytes| crate::fs_secure::write_file(env_path, bytes, 0o600),
            );
            if let Err(rollback) = restored {
                return Err(format!("{error}; credential rollback failed: {rollback}"));
            }
        }
        return Err(error);
    }
    Ok(draft)
}

#[cfg(test)]
mod draft_tests {
    use super::*;

    #[test]
    fn invalid_batch_keeps_original_and_rejects_unknown_paths() {
        let config = AppConfig::default();
        let original = config.general.nick.clone();
        assert!(
            prepare_changes(
                &config,
                &[("general.nick", "changed"), ("web.port", "invalid")]
            )
            .is_err()
        );
        assert_eq!(config.general.nick, original);
        assert!(prepare_changes(&config, &[("general.nick.unexpected", "changed")]).is_err());
        assert!(prepare_changes(&config, &[("general.nick", "bad\nvalue")]).is_err());
    }

    #[test]
    fn draft_preserves_secrets_and_literal_text_without_mutating_source() {
        let mut config = AppConfig::default();
        config.web.password = "test-only-secret".into();
        let original = config.statusbar.prompt.clone();
        let draft = prepare_changes(
            &config,
            &[
                ("statusbar.prompt", "\"quoted\" % "),
                ("display.show_timestamps", "false"),
            ],
        )
        .unwrap();
        assert_eq!(draft.statusbar.prompt, "\"quoted\" % ");
        assert_eq!(draft.web.password, config.web.password);
        assert!(!draft.display.show_timestamps);
        assert_eq!(config.statusbar.prompt, original);
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    use crate::settings_model::SettingChange;

    #[test]
    fn save_rejects_stale_fields_and_preserves_unrelated_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let env = dir.path().join(".env");
        let mut config = AppConfig::default();
        let original = config.general.nick.clone();
        config.general.realname = "Changed elsewhere".into();
        let change = SettingChange {
            path: "general.nick".into(),
            original,
            value: "newnick".into(),
        };
        let saved = save_changes(&config, std::slice::from_ref(&change), &path, &env).unwrap();
        assert_eq!(saved.general.realname, "Changed elsewhere");
        assert_eq!(
            crate::config::load_config(&path).unwrap().general.nick,
            "newnick"
        );
        assert!(
            save_changes(&saved, &[change], &path, &env)
                .unwrap_err()
                .contains("changed elsewhere")
        );
        assert!(!env.exists());
    }

    #[test]
    fn sasl_username_edits_reject_stale_values_without_touching_passwords() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let env = dir.path().join(".env");
        let mut config = AppConfig::default();
        config.servers.insert(
            "test".into(),
            serde_json::from_value(serde_json::json!({
                "label": "Test", "address": "localhost", "port": 6697, "tls": true,
                "channels": [], "sasl_user": "first", "sasl_pass": "test-secret"
            }))
            .unwrap(),
        );
        let change = crate::settings_model::SettingChange {
            path: "servers.test.sasl_user".into(),
            original: "first".into(),
            value: "second".into(),
        };
        let saved = save_changes(&config, std::slice::from_ref(&change), &path, &env).unwrap();
        assert_eq!(saved.servers["test"].sasl_user.as_deref(), Some("second"));
        assert_eq!(
            saved.servers["test"].sasl_pass.as_deref(),
            Some("test-secret")
        );
        assert!(
            save_changes(&saved, std::slice::from_ref(&change), &path, &env)
                .unwrap_err()
                .contains("changed elsewhere")
        );
        assert!(!env.exists());
        std::fs::write(&env, "TEST_SASL_USER=first\nTEST_SASL_PASS=test-secret\n").unwrap();
        save_changes(&config, std::slice::from_ref(&change), &path, &env).unwrap();
        let mut reloaded = crate::config::load_config(&path).unwrap();
        crate::config::apply_credentials(
            &mut reloaded.servers,
            &crate::config::load_env(&env).unwrap(),
        );
        assert_eq!(
            reloaded.servers["test"].sasl_user.as_deref(),
            Some("second")
        );
        assert_eq!(
            reloaded.servers["test"].sasl_pass.as_deref(),
            Some("test-secret")
        );
    }

    #[test]
    fn failed_config_save_restores_credentials_and_does_not_mutate_source() {
        let dir = tempfile::tempdir().unwrap();
        let env = dir.path().join(".env");
        let previous = b"WEB_PASSWORD=old-test-password\nOTHER=keep\n";
        std::fs::write(&env, previous).unwrap();
        let mut config = AppConfig::default();
        config.web.password = "old-test-password".into();
        let changes = [SettingChange {
            path: "web.password".into(),
            original: String::new(),
            value: "new-test-password".into(),
        }];
        assert!(save_changes(&config, &changes, dir.path(), &env).is_err());
        assert_eq!(std::fs::read(&env).unwrap(), previous);
        assert_eq!(config.web.password, "old-test-password");
        let path = dir.path().join("config.toml");
        let saved = save_changes(&config, &changes, &path, &env).unwrap();
        assert_eq!(saved.web.password, "new-test-password");
        assert_eq!(
            crate::config::load_env(&env).unwrap()["WEB_PASSWORD"],
            "new-test-password"
        );
        assert!(
            !std::fs::read_to_string(path)
                .unwrap()
                .contains("test-password")
        );
    }
}

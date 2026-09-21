//! /set command — dot-notation config get/set with type coercion.
//!
//! Paths: `general.nick`, `display.nick_column_width`, `servers.libera.port`, etc.

use crate::app::App;
use crate::config::AppConfig;

use super::types::{C_CMD, C_DIM, C_ERR, C_HEADER, C_OK, C_RST, divider};

#[cfg(test)]
use crate::config::settings::server_password_env_key;
#[cfg(test)]
use crate::config::settings::{BASE_PATHS, set_config_value};
use crate::config::settings::{SERVER_FIELDS, get_config_value};
pub use crate::config::settings::{get_setting_paths, parse_sasl_mechanism};

// === Command handler ===

pub(super) fn decode_setting_value(raw: &str) -> Result<String, &'static str> {
    if raw.contains(['\n', '\r', '\0']) {
        return Err("Setting values must be a single line without NUL characters");
    }
    let trimmed = raw.trim_start();
    let Some(quote @ ('\'' | '"')) = trimmed.chars().next() else {
        return Ok(raw.to_string());
    };
    let mut value = String::new();
    let mut chars = trimmed[quote.len_utf8()..].char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if ch == quote {
            if !trimmed[quote.len_utf8() + index + ch.len_utf8()..]
                .trim()
                .is_empty()
            {
                return Err("Unexpected text after the closing quote");
            }
            return Ok(value);
        }
        if ch == '\\'
            && chars
                .peek()
                .is_some_and(|(_, next)| *next == quote || *next == '\\')
        {
            value.push(chars.next().expect("peeked character").1);
        } else {
            value.push(ch);
        }
    }
    Err("Unterminated quoted value")
}

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
    let value = match decode_setting_value(&args[1]) {
        Ok(value) => value,
        Err(error) => {
            ev(app, &format!("{C_ERR}{error}{C_RST}"));
            return;
        }
    };
    let raw = &value;

    let Some(current) = get_config_value(&app.config, path) else {
        ev(app, &format!("{C_ERR}Unknown setting: {path}{C_RST}"));
        return;
    };
    let change = crate::settings_model::SettingChange {
        path: path.clone(),
        original: current.value,
        value: raw.clone(),
    };
    match app.save_settings(&[change]) {
        Ok(()) => {
            let shown = if current.is_credential {
                "[credential saved]".into()
            } else {
                raw.replace('%', "%%")
            };
            ev(app, &format!("{C_OK}{path}{C_RST} = {C_CMD}{shown}{C_RST}"));
        }
        Err(error) => ev(app, &format!("{C_ERR}{error}{C_RST}")),
    }
}

#[expect(clippy::too_many_lines)]
pub fn apply_setting_runtime(app: &mut App, path: &str, raw: &str) {
    let ev = super::helpers::add_local_event;
    // Hot restart web server when lifecycle settings change.
    if matches!(
        path,
        "web.enabled"
            | "web.port"
            | "web.bind_address"
            | "web.password"
            | "web.tls_cert"
            | "web.tls_key"
            | "web.session_days"
            | "web.username"
            | "web.image_previews"
            | "web.image_previews_max_per_msg"
            | "web.thumbnail_cache_mb"
    ) {
        app.web_restart_pending = true;
        if path != "web.enabled" || raw == "true" {
            ev(app, &format!("{C_DIM}Web server will restart...{C_RST}"));
        }
    }

    // Sync runtime state from config
    if path == "general.flood_protection" {
        app.state.flood_protection = app.config.general.flood_protection;
    }
    if path == "general.flood_exemptions" {
        app.state
            .flood_exemptions
            .clone_from(&app.config.general.flood_exemptions);
    }
    if path == "display.scrollback_lines" {
        app.state.scrollback_limit = app.config.display.scrollback_lines;
    }
    if path == "display.nick_color_saturation" {
        app.state.nick_color_sat = app.config.display.nick_color_saturation;
    }
    if path == "display.nick_color_lightness" {
        app.state.nick_color_lit = app.config.display.nick_color_lightness;
    }

    // Every `typing.*` switch goes through the one sync `/reload` also
    // runs, rather than an arm per key: the sync is idempotent (it
    // re-derives from the config rather than undoing a specific switch),
    // and a per-key arm is exactly what let `/reload` drift out of step.
    if path.starts_with("typing.") {
        app.sync_typing_from_config();
    }

    if path == "display.mentions_buffer" {
        if app.config.display.mentions_buffer {
            app.create_mentions_buffer();
        } else {
            app.state.remove_buffer("_mentions");
        }
    }

    // Mirror the translate config into state so the
    // `add_message_with_activity` decision and the request payloads
    // match the freshly-set config without a restart. The backend and
    // worker queues are bound at startup, so flipping
    // `translate.enabled` from off to on at runtime cannot
    // materialise a backend — say so rather than silently doing
    // nothing.
    if path.starts_with("translate.") {
        app.sync_translate_from_config();
        if let Some(backend) = &app.translate_backend {
            backend.refresh_config(&app.config.translate);
        }
        // `backend` has the same restart caveat as `enabled` and for
        // the same reason — naming a translator cannot conjure the
        // workers that were bound at startup. Warning on only one of
        // the two switches is how the quieter one comes to lie.
        if path == "translate.enabled" || path == "translate.backend" {
            crate::commands::helpers::warn_if_translate_cannot_run(app);
        }
    }

    // Sync shrink-incoming flags into state so the
    // `add_message_with_activity` decision matches the
    // freshly-set config without restart. The shrink_client
    // and worker queue are bound at startup — flipping
    // `shrink.enabled` from off to on at runtime won't
    // materialise a client; users get a restart-required
    // notice from /set already if they hit that case.
    if path == "shrink.enabled" || path == "shrink.incoming_enabled" {
        app.state.shrink_incoming_active = app.config.shrink.enabled
            && app.config.shrink.incoming_enabled
            && app.shrink_client.is_some();
        // Warn the user when the toggle is now `true` but no
        // client exists (typically: SHRINK_API_KEY missing at
        // boot). Without this, /set replies with success but
        // shrink stays inert and the user has no diagnostic.
        if (path == "shrink.enabled" && app.config.shrink.enabled) && app.shrink_client.is_none() {
            crate::commands::helpers::add_local_event(
                app,
                &format!(
                    "{warn}shrink: enabled but no API client — set \
                 SHRINK_API_KEY in .env and restart{rst}",
                    warn = crate::commands::types::C_ERR,
                    rst = crate::commands::types::C_RST,
                ),
            );
        }
    }
    if path == "shrink.min_url_length" {
        app.state.shrink_min_url_length = app.config.shrink.min_url_length;
    }
    // Settings captured at startup by the shrink workers
    // (api_url, timeouts) or by the cache constructor
    // (cache_max_entries) cannot be propagated to running
    // tasks. Surface a restart-required notice so the user
    // knows the /set didn't take effect.
    if matches!(
        path,
        "shrink.api_url"
            | "shrink.outgoing_timeout_ms"
            | "shrink.incoming_timeout_ms"
            | "shrink.cache_max_entries"
    ) {
        crate::commands::helpers::add_local_event(
            app,
            &format!(
                "{dim}shrink: {path} change requires restart to \
             take effect{rst}",
                dim = crate::commands::types::C_DIM,
                rst = crate::commands::types::C_RST,
            ),
        );
    }

    // Sync DCC runtime state from config
    if path.starts_with("dcc.") {
        match path {
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
                app.dcc.port_range = crate::dcc::chat::parse_port_range(&app.config.dcc.port_range);
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
        || path.starts_with("emotes.")
    {
        app.state
            .pending_web_events
            .push(crate::web::protocol::WebEvent::SettingsChanged {
                timestamp_format: app.config.web.timestamp_format.clone(),
                line_height: app.config.web.line_height,
                theme: app.config.web.theme.clone(),
                nick_column_width: app.config.web.nick_column_width,
                nick_max_length: app.config.web.nick_max_length,
                nick_colors: app.config.display.nick_colors,
                nick_colors_in_nicklist: app.config.display.nick_colors_in_nicklist,
                nick_color_saturation: app.config.display.nick_color_saturation,
                nick_color_lightness: app.config.display.nick_color_lightness,
                emotes_enabled: app.config.emotes.web_enabled(),
                emotes_input_enabled: app.emotes_input_enabled(),
            });
    }

    // The web status line renders from `statusbar.items` / `.enabled`
    // too, so a `/set statusbar.…` has to reach open tabs — otherwise
    // turning the bar off in the terminal leaves it up in the browser.
    if path.starts_with("statusbar.") {
        super::handlers_ui::push_statusbar_web_event(app);
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
                    &format!(
                        "  {C_HEADER}{matched_path}{C_RST} = {C_CMD}{}{C_RST}",
                        val.replace('%', "%%")
                    ),
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
                "flood_exemptions",
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
        ("emotes", &["enabled", "render", "lang"]),
        ("typing", &["show", "send_channels", "send_queries"]),
        (
            "translate",
            &[
                "enabled",
                "backend",
                "my_lang",
                "show_original_in",
                "show_original_out",
                "timeout_ms",
                "max_in_flight",
                "max_queue",
                "ai.preferred_attempt_ms",
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
                lines.push(format!(
                    "    {C_HEADER}{path}{C_RST} = {C_CMD}{}{C_RST}",
                    val.replace('%', "%%")
                ));
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
    for field in &[
        "enabled",
        "computing",
        "mode",
        "languages",
        "dictionary_dir",
    ] {
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
                lines.push(format!(
                    "    {C_HEADER}{path}{C_RST} = {C_CMD}{}{C_RST}",
                    val.replace('%', "%%")
                ));
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
                sasl_key_path: None,
                bouncer_network_id: None,
                bouncer_control: false,
            },
        );
        let paths = get_setting_paths(&config);
        assert!(paths.contains(&"servers.test.port".to_string()));
        assert!(paths.contains(&"servers.test.tls".to_string()));
        assert!(paths.contains(&"servers.test.sasl_key_path".to_string()));
    }

    /// A server entry that `/set` can be pointed at.
    fn config_with_server() -> AppConfig {
        let mut config = default_config();
        config.servers.insert(
            "net".to_string(),
            crate::config::ServerConfig {
                label: "Net".to_string(),
                address: "irc.test.net".to_string(),
                port: 6697,
                tls: true,
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
                sasl_key_path: None,
                bouncer_network_id: None,
                bouncer_control: false,
            },
        );
        config
    }

    #[test]
    fn every_sasl_mechanism_can_be_set_and_read_back() {
        let mut config = config_with_server();
        for mech in crate::irc::SASL_MECHANISMS {
            set_config_value(&mut config, "servers.net.sasl_mechanism", mech.name())
                .unwrap_or_else(|e| panic!("{} should be settable: {e}", mech.name()));
            assert_eq!(
                get_config_value(&config, "servers.net.sasl_mechanism")
                    .unwrap()
                    .value,
                mech.name()
            );
        }

        // Lowercase input is normalised to the canonical spelling, so the
        // stored value always matches what the protocol code compares against.
        set_config_value(&mut config, "servers.net.sasl_mechanism", "scram-sha-512").unwrap();
        assert_eq!(
            get_config_value(&config, "servers.net.sasl_mechanism")
                .unwrap()
                .value,
            "SCRAM-SHA-512"
        );
    }

    #[test]
    fn server_passwords_map_to_their_env_keys() {
        for (path, expected) in [
            ("servers.libera.password", Some("LIBERA_PASSWORD")),
            ("servers.libera.sasl_user", None),
            ("servers.libera.sasl_pass", Some("LIBERA_SASL_PASS")),
            ("servers.libera.nick", None),
        ] {
            assert_eq!(server_password_env_key(path).as_deref(), expected);
        }
    }

    #[test]
    fn a_misspelled_sasl_mechanism_is_rejected_with_the_valid_names() {
        let mut config = config_with_server();
        // Unvalidated, this would be accepted and then silently skip SASL at
        // connect time — the failure mode this check exists to prevent.
        let err = set_config_value(&mut config, "servers.net.sasl_mechanism", "SCRAM-SHA256")
            .unwrap_err();
        assert!(err.contains("SCRAM-SHA-256"), "{err}");
        assert!(err.contains("PLAIN"), "{err}");
        assert!(err.contains("ECDSA-NIST256P-CHALLENGE"), "{err}");
        assert!(config.servers["net"].sasl_mechanism.is_none());

        // We do not implement channel binding, so -PLUS must not be storable.
        assert!(
            set_config_value(
                &mut config,
                "servers.net.sasl_mechanism",
                "SCRAM-SHA-256-PLUS"
            )
            .is_err()
        );
    }

    #[test]
    fn the_ecdsa_key_path_is_settable_and_survives_a_config_round_trip() {
        let mut config = config_with_server();
        set_config_value(&mut config, "servers.net.sasl_key_path", "libera.pem").unwrap();
        assert_eq!(
            get_config_value(&config, "servers.net.sasl_key_path")
                .unwrap()
                .value,
            "libera.pem"
        );
        // A path, not a credential — so unlike sasl_pass it belongs in
        // config.toml and must survive being written and read back.
        assert!(
            !get_config_value(&config, "servers.net.sasl_key_path")
                .unwrap()
                .is_credential
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        crate::config::save_config(&path, &config).unwrap();
        let reloaded = crate::config::load_config(&path).unwrap();
        assert_eq!(
            reloaded.servers["net"].sasl_key_path.as_deref(),
            Some("libera.pem")
        );
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
    fn get_set_emotes() {
        let mut config = default_config();
        assert_eq!(
            get_config_value(&config, "emotes.enabled").unwrap().value,
            "true"
        );
        assert_eq!(
            get_config_value(&config, "emotes.render").unwrap().value,
            "graphical"
        );
        set_config_value(&mut config, "emotes.enabled", "false").unwrap();
        assert!(!config.emotes.enabled);
        set_config_value(&mut config, "emotes.render", "text").unwrap();
        assert_eq!(config.emotes.render, crate::config::RenderMode::Text);
        set_config_value(&mut config, "emotes.render", "off").unwrap();
        assert_eq!(config.emotes.render, crate::config::RenderMode::Off);
        // Invalid render value is rejected.
        assert!(set_config_value(&mut config, "emotes.render", "bogus").is_err());
        // emotes.* paths are advertised as settable.
        assert!(BASE_PATHS.contains(&"translate.enabled"));
        assert!(BASE_PATHS.contains(&"translate.my_lang"));
        assert!(BASE_PATHS.contains(&"emotes.enabled"));
        assert!(BASE_PATHS.contains(&"emotes.render"));
    }

    #[test]
    fn get_set_translate_backend() {
        // The name of the translator is a setting in its own right, not
        // something `enabled` implies. A value this build has no
        // implementation for is REJECTED rather than stored: accepted-and-
        // ignored would leave the user with a config that reads like
        // translation is on and a client that translates nothing.
        let mut config = default_config();
        assert_eq!(
            get_config_value(&config, "translate.backend")
                .unwrap()
                .value,
            "none",
            "no translator is the default — the only implementation that \
             exists on this branch is a test stub"
        );
        set_config_value(&mut config, "translate.backend", "stub").unwrap();
        assert_eq!(config.translate.backend, "stub");
        // Hand-typed, so spelling is normalised the way the reader is.
        set_config_value(&mut config, "translate.backend", "  NONE ").unwrap();
        assert_eq!(config.translate.backend, "none");
        let err = set_config_value(&mut config, "translate.backend", "gogle")
            .expect_err("an unknown translator must not be accepted");
        assert!(
            err.contains("none") && err.contains("stub"),
            "the error has to say what the valid names are: {err}"
        );
        assert_eq!(config.translate.backend, "none", "and nothing was stored");
        assert!(BASE_PATHS.contains(&"translate.backend"));
    }

    #[test]
    fn get_set_translate_preferred_attempt_budget() {
        let mut config = default_config();
        let path = "translate.ai.preferred_attempt_ms";

        assert_eq!(get_config_value(&config, path).unwrap().value, "3000");
        set_config_value(&mut config, path, "4500").unwrap();
        assert_eq!(config.translate.ai.preferred_attempt_ms, 4_500);
        assert_eq!(get_config_value(&config, path).unwrap().value, "4500");
        assert!(set_config_value(&mut config, path, "499").is_err());
        assert!(BASE_PATHS.contains(&path));
    }

    #[test]
    fn get_set_emotes_lang() {
        let mut config = default_config();
        assert_eq!(
            get_config_value(&config, "emotes.lang").unwrap().value,
            "en"
        );
        set_config_value(&mut config, "emotes.lang", "pl").unwrap();
        assert_eq!(config.emotes.lang, crate::config::EmoteLang::Pl);
        assert!(set_config_value(&mut config, "emotes.lang", "fr").is_err());
        assert!(BASE_PATHS.contains(&"emotes.lang"));
    }

    #[test]
    fn get_set_typing() {
        let mut config = default_config();
        // All three keys read back "true" on a default config.
        assert_eq!(
            get_config_value(&config, "typing.show").unwrap().value,
            "true"
        );
        assert_eq!(
            get_config_value(&config, "typing.send_channels")
                .unwrap()
                .value,
            "true"
        );
        assert_eq!(
            get_config_value(&config, "typing.send_queries")
                .unwrap()
                .value,
            "true"
        );
        // Set each to false and read it back through the getter.
        set_config_value(&mut config, "typing.show", "false").unwrap();
        assert!(!config.typing.show);
        assert_eq!(
            get_config_value(&config, "typing.show").unwrap().value,
            "false"
        );
        set_config_value(&mut config, "typing.send_channels", "false").unwrap();
        assert!(!config.typing.send_channels);
        assert_eq!(
            get_config_value(&config, "typing.send_channels")
                .unwrap()
                .value,
            "false"
        );
        set_config_value(&mut config, "typing.send_queries", "false").unwrap();
        assert!(!config.typing.send_queries);
        assert_eq!(
            get_config_value(&config, "typing.send_queries")
                .unwrap()
                .value,
            "false"
        );
        // Non-boolean value is rejected by parse_bool.
        assert!(set_config_value(&mut config, "typing.show", "bogus").is_err());
        // Unknown field in the typing section is rejected / not resolvable.
        assert!(set_config_value(&mut config, "typing.nope", "true").is_err());
        assert!(get_config_value(&config, "typing.nope").is_none());
        // typing.* paths are advertised as settable.
        assert!(BASE_PATHS.contains(&"typing.show"));
        assert!(BASE_PATHS.contains(&"typing.send_channels"));
        assert!(BASE_PATHS.contains(&"typing.send_queries"));
    }

    #[test]
    fn set_nick_color_saturation_validates_range() {
        let mut config = default_config();
        assert!(set_config_value(&mut config, "display.nick_color_saturation", "0.7").is_ok());
        assert!(set_config_value(&mut config, "display.nick_color_saturation", "1.5").is_err());
        assert!(set_config_value(&mut config, "display.nick_color_saturation", "-0.1").is_err());
    }
    #[test]
    fn quoted_setting_values_preserve_content_and_round_trip() {
        for (input, expected) in [
            (r#""""#, ""),
            ("''", ""),
            (r#""❯ ""#, "❯ "),
            ("'  two words  '", "  two words  "),
            (r#""say \"hi\" \\ end""#, "say \"hi\" \\ end"),
            (r#""C:\path\file""#, r"C:\path\file"),
            ("  \"hello\"  ", "hello"),
            ("unquoted words", "unquoted words"),
            ("don't change me", "don't change me"),
        ] {
            let parsed =
                crate::commands::parser::parse_command(&format!("/set statusbar.prompt {input}"))
                    .unwrap();
            let decoded = decode_setting_value(&parsed.args[1]).unwrap();
            let mut config = AppConfig::default();
            set_config_value(&mut config, &parsed.args[0], &decoded).unwrap();
            let saved = toml::to_string(&config).unwrap();
            let reloaded: AppConfig = toml::from_str(&saved).unwrap();
            assert_eq!(reloaded.statusbar.prompt, expected, "{input}");
        }
    }

    #[test]
    fn malformed_setting_quotes_are_rejected() {
        for input in [
            "\"unfinished",
            "'unfinished",
            "\"value\" extra",
            "\"trailing\\",
            "\"one\"\"two\"",
        ] {
            assert!(decode_setting_value(input).is_err(), "{input}");
        }
    }

    #[test]
    fn empty_values_clear_lists_and_server_overrides() {
        let mut config = config_with_server();
        let id = config.servers.keys().next().unwrap().clone();
        for field in [
            "channels",
            "nick",
            "username",
            "realname",
            "bind_ip",
            "encoding",
            "autosendcmd",
            "client_cert_path",
            "sasl_key_path",
            "password",
            "sasl_pass",
            "sasl_user",
        ] {
            let path = format!("servers.{id}.{field}");
            set_config_value(&mut config, &path, "example").unwrap();
            set_config_value(&mut config, &path, "").unwrap();
        }
        for (field, value) in [
            ("sasl_mechanism", "PLAIN"),
            ("auto_reconnect", "true"),
            ("reconnect_delay", "5"),
            ("reconnect_max_retries", "3"),
        ] {
            let path = format!("servers.{id}.{field}");
            set_config_value(&mut config, &path, value).unwrap();
            set_config_value(&mut config, &path, "").unwrap();
        }
        let saved = toml::to_string(&config).unwrap();
        let reloaded: AppConfig = toml::from_str(&saved).unwrap();
        let server = &reloaded.servers[&id];
        assert!(server.sasl_mechanism.is_none());
        assert!(server.auto_reconnect.unwrap_or(true));
        assert!(server.reconnect_delay.is_none());
        assert!(server.reconnect_max_retries.is_none());
        assert!(server.channels.is_empty());
        assert!(server.nick.is_none());
        assert!(server.username.is_none());
        assert!(server.bind_ip.is_none());
        assert!(server.encoding.is_none());
        assert_eq!(
            server.nick.as_deref().unwrap_or(&reloaded.general.nick),
            reloaded.general.nick
        );
        for path in ["dcc.autochat_masks", "spellcheck.languages"] {
            set_config_value(&mut config, path, "").unwrap();
        }
        assert!(config.dcc.autochat_masks.is_empty());
        assert!(config.spellcheck.languages.is_empty());
        for path in [
            "general.nick".to_string(),
            format!("servers.{id}.address"),
            format!("servers.{id}.port"),
        ] {
            assert!(set_config_value(&mut config, &path, "").is_err());
        }
    }

    #[test]
    fn bouncer_network_setting_round_trips_and_rejects_injection() {
        let mut config: crate::config::AppConfig = toml::from_str("[servers.fixture]\nlabel = 'fixture'\naddress = 'localhost'\nport = 6697\ntls = true\nchannels = []\n").unwrap();
        let path = "servers.fixture.bouncer_network_id";
        set_config_value(&mut config, path, "00042").unwrap();
        assert_eq!(
            config.servers["fixture"].bouncer_network_id.as_deref(),
            Some("42")
        );
        assert!(set_config_value(&mut config, path, "42\r\nQUIT").is_err());
        let restored: crate::config::AppConfig =
            toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(
            restored.servers["fixture"].bouncer_network_id.as_deref(),
            Some("42")
        );
        set_config_value(&mut config, path, "").unwrap();
        assert!(config.servers["fixture"].bouncer_network_id.is_none());
    }
}

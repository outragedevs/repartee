use super::{get_config_value, get_setting_paths};
use crate::config::AppConfig;
use crate::settings_model::{SettingField, SettingKind};

pub fn fields(config: &AppConfig) -> Vec<SettingField> {
    let defaults = AppConfig::default();
    let serialized = serde_json::to_value(config).unwrap_or_default();
    let mut fields: Vec<_> = get_setting_paths(config)
        .into_iter()
        .filter_map(|path| {
            let resolved = get_config_value(config, &path)?;
            let pointer = format!("/{}", path.replace('.', "/"));
            let typed = serialized.pointer(&pointer);
            let kind = if resolved.is_credential {
                SettingKind::Secret
            } else if let Some(options) = choices(&path) {
                let mut options: Vec<_> = options.iter().map(|s| (*s).to_string()).collect();
                if !options.contains(&resolved.value) {
                    options.push(resolved.value.clone());
                }
                SettingKind::Select(options)
            } else if typed.is_some_and(serde_json::Value::is_boolean) {
                SettingKind::Toggle
            } else if typed.is_some_and(serde_json::Value::is_number)
                || path.starts_with("servers.")
                    && (path.ends_with(".reconnect_delay")
                        || path.ends_with(".reconnect_max_retries"))
            {
                SettingKind::Number
            } else if JSON_PATHS.contains(&path.as_str()) {
                SettingKind::Json
            } else {
                SettingKind::Text
            };
            let configured = !resolved.value.is_empty();
            let default_value = if resolved.is_credential || path.starts_with("servers.") {
                None
            } else {
                get_config_value(&defaults, &path).map(|v| v.value)
            };
            Some(SettingField {
                label: label(config, &path),
                section: section(&path),
                description: description(&path).to_string(),
                effect: effect(&path).to_string(),
                path,
                kind,
                value: if resolved.is_credential {
                    String::new()
                } else {
                    resolved.value
                },
                default_value,
                configured,
            })
        })
        .collect();
    fields.sort_by_key(|field| {
        (
            field.section,
            if field.path.starts_with("general.") {
                0
            } else if field.path.starts_with("servers.") {
                1
            } else if field.path.starts_with("translate.ai.") {
                3
            } else {
                2
            },
            field.path.clone(),
        )
    });
    fields
}

fn label(config: &AppConfig, path: &str) -> String {
    match path {
        "general.nick" => return "Default nickname".into(),
        "general.username" => return "Default username".into(),
        "general.realname" => return "Default real name".into(),
        "general.theme" => return "Terminal theme".into(),
        "general.timestamp_format" => return "Terminal timestamp format".into(),
        "aliases" => return "Command aliases".into(),
        "ignores" => return "Ignore rules".into(),
        "e2e.enabled" => return "Encryption: Enabled".into(),
        "e2e.default_mode" => return "Encryption: Default mode".into(),
        "e2e.ts_tolerance_secs" => return "Encryption: Replay window (seconds)".into(),
        "translate.my_lang" => return "Translation: Your language".into(),
        "translate.show_original_in" => return "Translation: Show incoming originals".into(),
        "translate.show_original_out" => return "Translation: Show outgoing originals".into(),
        "translate.timeout_ms" => return "Translation: Timeout (ms)".into(),
        "translate.max_in_flight" => return "Translation: Concurrent requests".into(),
        "translate.max_queue" => return "Translation: Queue limit per buffer".into(),
        "translate.buffers" => return "Translation: Channels and queries".into(),
        "translate.ai.easy" => return "AI translation: Primary model order".into(),
        "translate.ai.strong" => return "AI translation: Fallback model order".into(),
        "translate.ai.terminal" => return "AI translation: Final model order".into(),
        _ => {}
    }
    let (group, field) = path.rsplit_once('.').unwrap_or(("", path));
    let title = field.replace('_', " ");
    let mut chars = title.chars();
    let title = chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    });
    let group = match group {
        "display" => "Chat",
        "statusbar" => "Statusbar",
        "image_preview" => "Terminal images",
        "sidepanel.left" => "Buffer list",
        "sidepanel.right" => "Nick list",
        "dcc" => "DCC",
        "web" => "Web",
        "e2e" => "Encryption",
        "logging" => "History",
        "scripts" => "Scripts",
        "emotes" => "Emotes",
        "typing" => "Typing",
        "shrink" => "URL shortening",
        "spellcheck" => "Spellcheck",
        "translate" => "Translation",
        "translate.ai" => "AI translation",
        "general" => "General",
        group => group,
    };
    let group = group
        .strip_prefix("servers.")
        .and_then(|id| config.servers.get(id))
        .map_or(group, |server| server.label.as_str());
    format!("{group}: {title}")
}

pub const EXTRA_PATHS: &[&str] = &[
    "aliases",
    "ignores",
    "statusbar.items",
    "statusbar.item_formats",
    "scripts.autoload",
    "scripts.debug",
    "logging.enabled",
    "logging.encrypt",
    "logging.exclude_types",
    "e2e.enabled",
    "e2e.default_mode",
    "e2e.ts_tolerance_secs",
    "emotes.max_cols",
    "emotes.max_rows",
    "translate.ai.easy",
    "translate.ai.strong",
    "translate.ai.terminal",
    "translate.ai.prompt_path",
    "translate.ai.models",
    "translate.buffers",
    "shrink.api_key",
];

pub const JSON_PATHS: &[&str] = &[
    "aliases",
    "ignores",
    "statusbar.items",
    "statusbar.item_formats",
    "scripts.autoload",
    "logging.exclude_types",
    "translate.ai.easy",
    "translate.ai.strong",
    "translate.ai.terminal",
    "translate.ai.models",
    "translate.buffers",
];

pub fn extra_value(config: &AppConfig, path: &str) -> Option<String> {
    if path == "shrink.api_key" {
        return Some(config.shrink.api_key.clone());
    }
    if !EXTRA_PATHS.contains(&path) {
        return None;
    }
    let value = serde_json::to_value(config).ok()?;
    let pointer = format!("/{}", path.replace('.', "/"));
    let value = value.pointer(&pointer)?;
    Some(
        value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_string),
    )
}

pub fn set_extra(config: &mut AppConfig, path: &str, raw: &str) -> Result<(), String> {
    macro_rules! assign {
        ($field:expr) => {
            $field = serde_json::from_str(raw).map_err(|e| format!("Invalid value: {e}"))?;
        };
    }
    match path {
        "aliases" => {
            assign!(config.aliases);
        }
        "ignores" => {
            assign!(config.ignores);
        }
        "statusbar.items" => {
            assign!(config.statusbar.items);
        }
        "statusbar.item_formats" => {
            assign!(config.statusbar.item_formats);
        }
        "scripts.autoload" => {
            assign!(config.scripts.autoload);
        }
        "scripts.debug" => {
            assign!(config.scripts.debug);
        }
        "logging.enabled" => {
            assign!(config.logging.enabled);
        }
        "logging.encrypt" => {
            assign!(config.logging.encrypt);
        }
        "logging.exclude_types" => {
            assign!(config.logging.exclude_types);
        }
        "e2e.enabled" => {
            assign!(config.e2e.enabled);
        }
        "e2e.default_mode" => {
            if !["normal", "quiet", "auto-accept"].contains(&raw) {
                return Err("Expected normal, quiet, or auto-accept".into());
            }
            config.e2e.default_mode = raw.into();
        }
        "e2e.ts_tolerance_secs" => {
            let seconds: i64 = raw
                .parse()
                .map_err(|_| "Expected a positive number of seconds")?;
            if seconds <= 0 {
                return Err("Replay tolerance must be positive".into());
            }
            config.e2e.ts_tolerance_secs = seconds;
        }
        "emotes.max_cols" => {
            assign!(config.emotes.max_cols);
        }
        "emotes.max_rows" => {
            assign!(config.emotes.max_rows);
        }
        "translate.ai.easy" => {
            assign!(config.translate.ai.easy);
        }
        "translate.ai.strong" => {
            assign!(config.translate.ai.strong);
        }
        "translate.ai.terminal" => {
            assign!(config.translate.ai.terminal);
        }
        "translate.ai.prompt_path" => config.translate.ai.prompt_path = raw.into(),
        "translate.ai.models" => {
            let mut models: Vec<crate::config::TranslateAiModelConfig> =
                serde_json::from_str(raw).map_err(|e| format!("Invalid models: {e}"))?;
            for model in &mut models {
                if let Some(old) = config
                    .translate
                    .ai
                    .models
                    .iter()
                    .find(|old| old.name == model.name && old.api_key_env == model.api_key_env)
                {
                    model.api_key.clone_from(&old.api_key);
                }
            }
            config.translate.ai.models = models;
        }
        "translate.buffers" => {
            assign!(config.translate.buffers);
        }
        "shrink.api_key" => config.shrink.api_key = raw.into(),
        _ => return Err(format!("Unknown setting: {path}")),
    }
    Ok(())
}

fn section(path: &str) -> usize {
    match path {
        "aliases" => 5,
        "ignores" | "display.scrollback_lines" | "display.backlog_lines" => 4,
        "display.mentions_buffer" => 3,
        "general.nick" | "general.username" | "general.realname" | "general.default_bind_ip" => 0,
        "general.theme"
        | "general.timestamp_format"
        | "web.theme"
        | "web.timestamp_format"
        | "web.line_height"
        | "web.nick_column_width"
        | "web.nick_max_length" => 1,
        _ if path.starts_with("servers.") || path.starts_with("dcc.") => 0,
        _ if path.starts_with("display.")
            || path.starts_with("sidepanel.")
            || path.starts_with("statusbar.") =>
        {
            1
        }
        _ if path.starts_with("logging.") => 4,
        _ if path.starts_with("e2e.") => 7,
        _ if path.starts_with("translate.") => 8,
        _ if path.starts_with("scripts.") || path.starts_with("emotes.") => 6,
        _ if path.starts_with("typing.")
            || path.starts_with("shrink.")
            || path.starts_with("spellcheck.")
            || path.starts_with("image_preview.") =>
        {
            2
        }
        _ => 9,
    }
}

fn effect(path: &str) -> &'static str {
    match path {
        "web.cloudflare_tunnel_name" | "image_preview.kitty_format" => {
            "Reserved setting: saved, but not currently used at runtime."
        }
        "image_preview.cache_max_mb" | "image_preview.cache_max_days" => {
            "Used when running /image cleanup."
        }
        _ if path.starts_with("servers.")
            || matches!(
                path,
                "general.nick"
                    | "general.username"
                    | "general.realname"
                    | "general.default_bind_ip"
                    | "general.ctcp_version"
            ) =>
        {
            "Reconnect the network to apply connection changes."
        }
        _ if path.starts_with("logging.")
            || path.starts_with("e2e.")
            || path.starts_with("scripts.")
            || path.starts_with("translate.ai.")
            || matches!(
                path,
                "translate.enabled"
                    | "translate.backend"
                    | "translate.max_in_flight"
                    | "translate.max_queue"
                    | "shrink.api_url"
                    | "shrink.api_key"
                    | "shrink.enabled"
                    | "shrink.outgoing_timeout_ms"
                    | "shrink.incoming_timeout_ms"
                    | "shrink.cache_max_entries"
            ) =>
        {
            "Restart the main process to apply this setting fully."
        }
        _ if path.starts_with("web.")
            && !matches!(
                path,
                "web.theme"
                    | "web.timestamp_format"
                    | "web.line_height"
                    | "web.nick_column_width"
                    | "web.nick_max_length"
            ) =>
        {
            "The web server may restart; browser clients may need to reconnect."
        }
        "display.backlog_lines" => "Applied when history is loaded for a buffer.",
        _ => "Applied when saved.",
    }
}

fn choices(path: &str) -> Option<&'static [&'static str]> {
    match path {
        path if path.starts_with("servers.") && path.ends_with(".auto_reconnect") => {
            Some(&["", "true", "false"])
        }
        "display.nick_alignment" => Some(&["left", "right", "center"]),
        "spellcheck.mode" => Some(&["replace", "highlight"]),
        "emotes.render" => Some(&["graphical", "text", "off"]),
        "emotes.lang" => Some(&["en", "pl"]),
        "image_preview.protocol" => Some(&["auto", "kitty", "iterm2", "sixel", "halfblocks"]),
        "translate.backend" => Some(crate::translate::backend::BACKEND_NAMES),
        "e2e.default_mode" => Some(&["normal", "quiet", "auto-accept"]),
        _ => None,
    }
}

#[allow(clippy::too_many_lines)]
fn description(path: &str) -> &'static str {
    match path {
        "e2e.enabled" => {
            "Enable end-to-end encryption support. Use /e2e on in a channel or query to set up encryption; this switch alone does not encrypt messages."
        }
        "e2e.default_mode" => {
            "Default for /e2e on: normal requires /e2e accept, quiet suppresses prompts for unknown peers, auto-accept trusts peers automatically."
        }
        "e2e.ts_tolerance_secs" => {
            "Accept encrypted message timestamps within this many seconds to protect against replay. Default: 300."
        }
        "translate.enabled" => {
            "Enable translation processing. Choose a backend and configure channels or queries below; enabling this alone does not translate messages."
        }
        "translate.backend" => {
            "ai uses your configured provider models; none leaves messages unchanged. stub is for testing only and reverses word order."
        }
        "translate.my_lang" => {
            "The language you read and write, for example en, pl or de. Channels and queries can override it."
        }
        "translate.show_original_in" => "Show the original text alongside incoming translations.",
        "translate.show_original_out" => {
            "Show your original text alongside the local echo of outgoing translations."
        }
        "translate.timeout_ms" => {
            "Release a queued message untranslated after this many milliseconds (minimum 500)."
        }
        "translate.max_in_flight" => {
            "Maximum concurrent translation requests. Match this to your provider capacity."
        }
        "translate.max_queue" => {
            "Maximum queued translations per buffer. On overflow, the oldest messages are released untranslated."
        }
        "translate.ai.easy" => "Model names in preferred order for the first translation attempt.",
        "translate.ai.strong" => {
            "Model names in fallback order when the first attempt is unsuccessful."
        }
        "translate.ai.terminal" => {
            "Final model order. Automatic selects configured Groq models with available credentials; an empty custom list disables this stage."
        }
        "translate.ai.preferred_attempt_ms" => {
            "Time budget in milliseconds for the preferred model before trying a fallback (minimum 500)."
        }
        "translate.ai.prompt_path" => {
            "Path to a custom translation prompt file; leave empty to use the built-in prompt."
        }
        "aliases" => "Named shortcuts and the commands they expand to.",
        "ignores" => "Ignore rules match a user mask, message types and optional channels.",
        "statusbar.items" => {
            "Ordered statusbar items: active_windows, nick_info, channel_info, typing, lag, time."
        }
        "statusbar.item_formats" => "Custom formats for individual statusbar items.",
        "scripts.autoload" => "Lua script filenames to load at startup, in order.",
        "logging.exclude_types" => "Message types excluded from local history.",
        "translate.ai.models" => {
            "Translation models and provider limits. Reference API keys by environment variable; secret values are never displayed."
        }
        "translate.buffers" => {
            "Use connection_id/#channel or connection_id/nickname as the buffer name. Set incoming/outgoing translation and both languages; outgoing requires the channel language."
        }
        "general.nick" => "Nickname inherited by networks without their own nickname override.",
        "general.username" => "IRC username inherited by networks without their own override.",
        "general.realname" => {
            "Real-name field sent during IRC registration; it does not need to be a legal name."
        }
        "general.theme" => "Name of an installed terminal theme, without the .theme extension.",
        "general.flood_protection" => {
            "Limit outgoing message bursts to avoid server flood disconnections."
        }
        "general.flood_exemptions" => {
            "Comma-separated targets exempt from outgoing flood protection."
        }
        "general.ctcp_version" => {
            "Text returned when another IRC user requests the client version."
        }
        "display.scrollback_lines" => "Maximum messages held in memory per buffer.",
        "display.backlog_lines" => {
            "Local history messages loaded when opening a buffer; 0 disables loading."
        }
        "logging.retention_days" => "Days to retain message history; 0 keeps it indefinitely.",
        "logging.event_retention_hours" => {
            "Hours to retain join/part/quit events; 0 keeps them indefinitely."
        }
        "display.mentions_buffer" => "Show a separate buffer collecting mentions.",
        "general.default_bind_ip" => {
            "Default local IP address for outgoing IRC connections; empty uses the system route."
        }
        "typing.show" => "Display other users' typing indicators.",
        "typing.send_channels" => "Send typing indicators to channels supporting them.",
        "typing.send_queries" => "Send typing indicators in private conversations.",
        _ if path.ends_with("password")
            || path.ends_with("sasl_pass")
            || path == "shrink.api_key" =>
        {
            "Leave untouched to keep the existing secret. Replacements are stored only in .env."
        }
        _ if path.contains("timestamp_format") => {
            "Timestamp format, for example %H:%M:%S. Text is saved literally."
        }
        _ if path.contains("color") || path.starts_with("statusbar.") => {
            "Customize colors or formatting used by the interface and current theme."
        }
        _ if path.starts_with("servers.") => {
            "Network-specific connection setting. Empty optional overrides inherit the global value."
        }
        _ if path.starts_with("translate.ai.") => {
            "AI translation models, routing and provider configuration."
        }
        _ if path.starts_with("translate.") => {
            "Configure automatic translation of incoming and outgoing messages."
        }
        _ if path.starts_with("image_preview.") => {
            "Configure terminal image previews, resource limits and image protocol."
        }
        _ if path.starts_with("shrink.") => {
            "Configure URL shortening. Timeout values are in milliseconds."
        }
        _ if path.starts_with("spellcheck.") => {
            "Configure spelling dictionaries, language selection and checking behavior."
        }
        _ if path.starts_with("web.") => {
            "Shared web interface configuration; applies to all connected browsers."
        }
        _ if path.starts_with("emotes.") => {
            "Configure emote rendering, language or maximum size in terminal cells."
        }
        _ if path.starts_with("e2e.") => "Configure end-to-end encryption and replay protection.",
        _ if path.starts_with("dcc.") => {
            "Configure direct client connections. Timeout values are in seconds."
        }
        _ if path.starts_with("logging.") => "Configure local history storage and encryption.",
        _ if path.starts_with("sidepanel.") => {
            "Show or hide this sidebar and set its width in terminal columns."
        }
        _ if path.starts_with("scripts.") => "Configure Lua script loading and diagnostic logging.",
        _ if path.starts_with("display.") => {
            "Configure message and nickname layout in the terminal interface."
        }
        _ => "Global identity or messaging preference used by the client.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedicated_sections_keep_encryption_and_translation_editable() {
        let mut config = AppConfig::default();
        config.translate.backend = " AI ".into();
        let catalog = fields(&config);
        for (prefix, title) in [("e2e.", "E2E Encryption"), ("translate.", "Translation")] {
            let section = crate::settings_model::SECTIONS
                .iter()
                .position(|s| *s == title)
                .unwrap();
            let members: Vec<_> = catalog.iter().filter(|f| f.section == section).collect();
            assert!(!members.is_empty());
            assert!(members.iter().all(|f| f.path.starts_with(prefix)));
            assert!(
                catalog
                    .iter()
                    .filter(|f| f.path.starts_with(prefix))
                    .all(|f| f.section == section)
            );
        }
        let backend = catalog
            .iter()
            .find(|f| f.path == "translate.backend")
            .unwrap();
        assert!(
            matches!(&backend.kind, SettingKind::Select(options) if options.iter().any(|s| s == "ai") && options.contains(&backend.value))
        );
        let changed = super::super::prepare_changes(
            &config,
            &[
                ("e2e.enabled", "false"),
                ("e2e.default_mode", "quiet"),
                ("e2e.ts_tolerance_secs", "120"),
                ("translate.backend", "ai"),
                ("translate.my_lang", "pl"),
                (
                    "translate.buffers",
                    r#"{"fixture/#chat":{"incoming":true,"outgoing":true,"lang":"en"}}"#,
                ),
            ],
        )
        .unwrap();
        let reloaded: AppConfig = toml::from_str(&toml::to_string(&changed).unwrap()).unwrap();
        assert!(!reloaded.e2e.enabled);
        assert_eq!(reloaded.e2e.default_mode, "quiet");
        assert_eq!(reloaded.e2e.ts_tolerance_secs, 120);
        assert_eq!(reloaded.translate.backend, "ai");
        assert_eq!(reloaded.translate.my_lang, "pl");
        assert!(reloaded.translate.buffers["fixture/#chat"].outgoing);
        assert_eq!(
            reloaded.translate.buffers["fixture/#chat"].lang.as_deref(),
            Some("en")
        );
        assert!(
            super::super::prepare_changes(&config, &[("translate.backend", "unknown")]).is_err()
        );
        assert!(
            super::super::prepare_changes(&config, &[("e2e.default_mode", "unknown")]).is_err()
        );
    }

    #[test]
    fn catalog_redacts_credentials_and_preserves_inherited_connection_values() {
        let mut config = AppConfig::default();
        config.web.password = "fixture-web-secret".into();
        config.shrink.api_key = "fixture-shrink-secret".into();
        config
            .translate
            .ai
            .models
            .push(crate::config::TranslateAiModelConfig {
                api_key: "fixture-model-secret".into(),
                ..Default::default()
            });
        config.servers.insert("test".into(), serde_json::from_value(serde_json::json!({
            "label": "Test", "address": "localhost", "port": 6697, "tls": true,
            "channels": [], "password": "fixture-server-secret", "sasl_pass": "fixture-sasl-secret", "auto_reconnect": true
        })).unwrap());
        let fields = fields(&config);
        let reconnect = fields
            .iter()
            .find(|f| f.path == "servers.test.auto_reconnect")
            .unwrap();
        assert!(
            matches!(&reconnect.kind, SettingKind::Select(options) if options.contains(&String::new()))
        );
        let inherited =
            super::super::prepare_changes(&config, &[("servers.test.auto_reconnect", "")]).unwrap();
        let reloaded: AppConfig = toml::from_str(&toml::to_string(&inherited).unwrap()).unwrap();
        assert_eq!(reloaded.servers["test"].auto_reconnect, None);
        assert!(reloaded.servers["test"].auto_reconnect.unwrap_or(true));
        let wire = serde_json::to_string(&fields).unwrap();
        assert!(!wire.contains("fixture-"));
        for path in [
            "web.password",
            "shrink.api_key",
            "servers.test.password",
            "servers.test.sasl_pass",
        ] {
            let field = fields.iter().find(|f| f.path == path).unwrap();
            assert!(field.configured);
            assert_eq!(field.kind, SettingKind::Secret);
            assert!(field.value.is_empty());
            assert!(field.default_value.is_none());
        }
    }
}

pub mod defaults;
pub mod env;

use std::collections::HashMap;
use std::path::Path;

use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};

pub use defaults::default_config;
pub use env::{apply_credentials, apply_web_credentials, load_env, set_env_value};

// === Helper for serde defaults ===

const fn default_true() -> bool {
    true
}

// === Enums ===

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NickAlignment {
    Left,
    #[default]
    Right,
    Center,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusbarItem {
    ActiveWindows,
    NickInfo,
    ChannelInfo,
    Lag,
    Time,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum IgnoreLevel {
    Msgs,
    Public,
    Notices,
    Actions,
    Joins,
    Parts,
    Quits,
    Nicks,
    Kicks,
    Ctcps,
    All,
}

// === Config Structs ===

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub general: GeneralConfig,
    pub display: DisplayConfig,
    pub sidepanel: SidepanelConfig,
    pub statusbar: StatusbarConfig,
    pub image_preview: ImagePreviewConfig,
    pub servers: HashMap<String, ServerConfig>,
    pub aliases: HashMap<String, String>,
    pub ignores: Vec<IgnoreEntry>,
    pub scripts: ScriptsConfig,
    pub logging: LoggingConfig,
    pub dcc: DccConfig,
    pub spellcheck: SpellcheckConfig,
    pub web: WebConfig,
    pub e2e: E2eConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    pub nick: String,
    pub username: String,
    pub realname: String,
    pub theme: String,
    pub timestamp_format: String,
    pub flood_protection: bool,
    pub ctcp_version: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        use crate::constants::{APP_NAME, APP_VERSION};
        Self {
            nick: APP_NAME.to_string(),
            username: APP_NAME.to_lowercase(),
            realname: format!("{APP_NAME} Client"),
            theme: "default".to_string(),
            timestamp_format: "%H:%M:%S".to_string(),
            flood_protection: true,
            ctcp_version: format!("{APP_NAME} {APP_VERSION}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "config struct — each bool is an independent user setting"
)]
pub struct DisplayConfig {
    pub nick_column_width: u16,
    pub nick_max_length: u16,
    pub nick_alignment: NickAlignment,
    pub nick_truncation: bool,
    pub show_timestamps: bool,
    pub scrollback_lines: usize,
    /// Number of historical log lines to load when a buffer is first opened.
    /// 0 = disabled. Lines come from `SQLite` storage, not memory.
    pub backlog_lines: usize,
    /// Enable per-nick deterministic coloring in chat messages.
    pub nick_colors: bool,
    /// Also apply nick colors in the nick list sidebar (some users prefer a clean nick list).
    pub nick_colors_in_nicklist: bool,
    /// HSL saturation for nick colors (0.0–1.0). Only used in truecolor mode.
    pub nick_color_saturation: f32,
    /// HSL lightness for nick colors (0.0–1.0). Tune per theme: dark bg ≈ 0.65, light bg ≈ 0.40.
    pub nick_color_lightness: f32,
    /// Show the Mentions buffer at the top of the buffer list.
    pub mentions_buffer: bool,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            nick_column_width: 8,
            nick_max_length: 8,
            nick_alignment: NickAlignment::Right,
            nick_truncation: true,
            show_timestamps: true,
            scrollback_lines: 2000,
            backlog_lines: 20,
            nick_colors: true,
            nick_colors_in_nicklist: true,
            nick_color_saturation: 0.65,
            nick_color_lightness: 0.65,
            mentions_buffer: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SidepanelConfig {
    pub left: PanelConfig,
    pub right: PanelConfig,
}

impl Default for SidepanelConfig {
    fn default() -> Self {
        Self {
            left: PanelConfig {
                width: 20,
                visible: true,
            },
            right: PanelConfig {
                width: 18,
                visible: true,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelConfig {
    pub width: u16,
    pub visible: bool,
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            width: 20,
            visible: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusbarConfig {
    pub enabled: bool,
    pub items: Vec<StatusbarItem>,
    pub separator: String,
    pub item_formats: HashMap<String, String>,
    // Appearance
    pub background: String,
    pub text_color: String,
    pub accent_color: String,
    pub muted_color: String,
    pub dim_color: String,
    // Input
    pub prompt: String,
    pub prompt_color: String,
    pub input_color: String,
    pub cursor_color: String,
}

impl Default for StatusbarConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            items: vec![
                StatusbarItem::Time,
                StatusbarItem::NickInfo,
                StatusbarItem::ChannelInfo,
                StatusbarItem::Lag,
                StatusbarItem::ActiveWindows,
            ],
            separator: " | ".to_string(),
            item_formats: HashMap::new(),
            background: String::new(),
            text_color: String::new(),
            accent_color: String::new(),
            muted_color: String::new(),
            dim_color: String::new(),
            prompt: "[$server\u{2771} ".to_string(),
            prompt_color: String::new(),
            input_color: String::new(),
            cursor_color: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ImagePreviewConfig {
    pub enabled: bool,
    pub max_width: u32,
    pub max_height: u32,
    pub cache_max_mb: u32,
    pub cache_max_days: u32,
    pub fetch_timeout: u32,
    pub max_file_size: u64,
    pub protocol: String,
    pub kitty_format: String,
}

impl Default for ImagePreviewConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_width: 0,
            max_height: 0,
            cache_max_mb: 100,
            cache_max_days: 7,
            fetch_timeout: 30,
            max_file_size: 10_485_760,
            protocol: "auto".to_string(),
            kitty_format: "rgba".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub label: String,
    pub address: String,
    pub port: u16,
    pub tls: bool,
    #[serde(default = "default_true")]
    pub tls_verify: bool,
    #[serde(default)]
    pub autoconnect: bool,
    pub channels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nick: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realname: Option<String>,
    /// Server password. Loaded from `.env` (`SERVERNAME_PASSWORD`).
    /// Never written back to `config.toml` — credentials belong in `.env`.
    #[serde(default, skip_serializing)]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sasl_user: Option<String>,
    /// SASL password. Loaded from `.env` (`SERVERNAME_SASL_PASS`).
    /// Never written back to `config.toml` — credentials belong in `.env`.
    #[serde(default, skip_serializing)]
    pub sasl_pass: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    #[serde(
        default = "default_true_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub auto_reconnect: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconnect_delay: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconnect_max_retries: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autosendcmd: Option<String>,
    /// SASL mechanism to use: `"PLAIN"`, `"EXTERNAL"`, or `None` (auto-detect best).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sasl_mechanism: Option<String>,
    /// Path to a client TLS certificate (PEM) for SASL EXTERNAL / `CertFP` auth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_cert_path: Option<String>,
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "serde default requires Option<bool> return type"
)]
const fn default_true_option() -> Option<bool> {
    Some(true)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IgnoreEntry {
    pub mask: String,
    pub levels: Vec<IgnoreLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    pub enabled: bool,
    pub encrypt: bool,
    pub retention_days: u32,
    /// Hours to keep event messages (join/part/quit/nick/kick/mode) before pruning.
    /// 0 = keep forever (no automatic pruning). Default: 72.
    pub event_retention_hours: u32,
    pub exclude_types: Vec<String>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            encrypt: false,
            retention_days: 0,
            event_retention_hours: 72,
            exclude_types: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ScriptsConfig {
    pub autoload: Vec<String>,
    pub debug: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DccConfig {
    /// Seconds before unaccepted DCC requests expire.
    pub timeout: u64,
    /// Override IP address sent in DCC offers (empty = auto-detect from IRC socket).
    pub own_ip: String,
    /// Port or range for DCC listen sockets. "0" = OS-assigned, "1025 65535" = range.
    pub port_range: String,
    /// Allow auto-accepting DCC from privileged ports (< 1024).
    pub autoaccept_lowports: bool,
    /// Hostmask patterns for auto-accepting DCC CHAT (e.g. "*!*@trusted.host").
    pub autochat_masks: Vec<String>,
    /// Maximum simultaneous DCC connections.
    pub max_connections: usize,
}

impl Default for DccConfig {
    fn default() -> Self {
        Self {
            timeout: 300,
            own_ip: String::new(),
            port_range: "0".to_string(),
            autoaccept_lowports: false,
            autochat_masks: Vec::new(),
            max_connections: 10,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SpellcheckConfig {
    /// Enable/disable spell checking.
    pub enabled: bool,
    /// Enable/disable the computing/IT supplemental dictionary.
    pub computing: bool,
    /// Spell check mode: `"replace"` (auto-correct with popup) or `"highlight"` (mark red, show suggestions inline).
    pub mode: String,
    /// Active language codes (Hunspell dict file stems, e.g. `en_US`, `pl_PL`, `de_DE`).
    pub languages: Vec<String>,
    /// Directory containing `.dic`/`.aff` files. Empty = `~/.repartee/dicts`.
    pub dictionary_dir: String,
}

impl Default for SpellcheckConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            computing: true,
            mode: "replace".to_string(),
            languages: vec!["en_US".to_string()],
            dictionary_dir: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct E2eConfig {
    /// Master switch — when false, the `E2eManager` is not initialized at
    /// startup and the `/e2e` commands become no-ops.
    pub enabled: bool,
    /// Default mode applied to a channel when `/e2e on` is issued without
    /// an explicit mode. One of `auto-accept`, `normal`, `quiet`.
    pub default_mode: String,
    /// Replay-protection tolerance window for the `ts` field on incoming
    /// encrypted messages, in seconds.
    pub ts_tolerance_secs: i64,
}

impl Default for E2eConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_mode: "normal".to_string(),
            ts_tolerance_secs: 300,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebConfig {
    /// Enable the embedded web frontend.
    pub enabled: bool,
    /// Bind address for the HTTPS server.
    pub bind_address: String,
    /// Port for the HTTPS server.
    pub port: u16,
    /// Path to TLS certificate (PEM). Empty = auto-generated self-signed.
    pub tls_cert: String,
    /// Path to TLS private key (PEM). Empty = auto-generated self-signed.
    pub tls_key: String,
    /// Timestamp format for the web UI (chrono strftime syntax).
    pub timestamp_format: String,
    /// CSS line-height for chat messages.
    pub line_height: f32,
    /// Width of the nick column in characters.
    pub nick_column_width: u32,
    /// Maximum nick display length before truncation.
    pub nick_max_length: u32,
    /// Web theme name.
    pub theme: String,
    /// Session duration in hours (default 24).
    pub session_hours: u32,
    /// Cloudflare tunnel name (future use).
    pub cloudflare_tunnel_name: String,
    /// Login password — loaded from `.env` (`WEB_PASSWORD`), not serialized to TOML.
    #[serde(skip)]
    pub password: String,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_address: "127.0.0.1".to_string(),
            port: 8443,
            tls_cert: String::new(),
            tls_key: String::new(),
            timestamp_format: "%H:%M".to_string(),
            line_height: 1.35,
            nick_column_width: 12,
            nick_max_length: 9,
            theme: "nightfall".to_string(),
            session_hours: 24,
            cloudflare_tunnel_name: String::new(),
            password: String::new(),
        }
    }
}

// === Load / Save ===

/// Load config from TOML file, merging with defaults for missing fields.
/// Uses serde's `#[serde(default)]` on `AppConfig` to handle missing fields.
pub fn load_config(path: &Path) -> Result<AppConfig> {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let config: AppConfig = toml::from_str(&content)?;
            Ok(config)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(default_config()),
        Err(e) => Err(e.into()),
    }
}

/// Save config to TOML file.
pub fn save_config(path: &Path, config: &AppConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        crate::fs_secure::create_dir_all(parent, 0o700)?;
    }
    let content = toml::to_string_pretty(config)?;
    crate::fs_secure::write_file(path, content, 0o600)?;
    Ok(())
}

// === Tests ===

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_uses_app_name() {
        let config = default_config();
        assert_eq!(config.general.nick, crate::constants::APP_NAME);
        assert_eq!(
            config.general.ctcp_version,
            format!(
                "{} {}",
                crate::constants::APP_NAME,
                crate::constants::APP_VERSION
            ),
        );
    }

    #[test]
    fn parse_minimal_config() {
        let toml_str = r#"
[general]
nick = "TestNick"
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.general.nick, "TestNick");
        // Check defaults are applied for missing fields
        assert_eq!(config.display.nick_column_width, 8);
        assert!(config.statusbar.enabled);
    }

    #[test]
    fn parse_server_config() {
        let toml_str = r##"
[servers.libera]
label = "Libera"
address = "irc.libera.chat"
port = 6697
tls = true
channels = ["#rust", "#linux"]
"##;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        let server = config.servers.get("libera").unwrap();
        assert_eq!(server.label, "Libera");
        assert_eq!(server.port, 6697);
        assert!(server.tls);
        assert_eq!(
            server.channels,
            vec!["#rust".to_string(), "#linux".to_string()]
        );
        // Defaults for optional fields
        assert!(server.tls_verify);
        assert!(!server.autoconnect);
        assert!(server.nick.is_none());
    }

    #[test]
    fn parse_full_config_roundtrip() {
        let config = default_config();
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: AppConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(config.general.nick, deserialized.general.nick);
        assert_eq!(
            config.display.scrollback_lines,
            deserialized.display.scrollback_lines
        );
    }

    #[test]
    fn nick_alignment_serialization() {
        // Verify TOML serializes as lowercase strings
        let toml_str = r#"nick_alignment = "left""#;
        let display: DisplayConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(display.nick_alignment, NickAlignment::Left);

        let toml_str = r#"nick_alignment = "center""#;
        let display: DisplayConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(display.nick_alignment, NickAlignment::Center);

        // Roundtrip
        let config = default_config();
        let serialized = toml::to_string_pretty(&config.display).unwrap();
        assert!(serialized.contains("nick_alignment = \"right\""));
    }

    #[test]
    fn statusbar_item_serialization() {
        // Verify items serialize as snake_case
        let config = default_config();
        let serialized = toml::to_string_pretty(&config.statusbar).unwrap();
        assert!(serialized.contains("\"active_windows\""));
        assert!(serialized.contains("\"nick_info\""));
        assert!(serialized.contains("\"channel_info\""));
    }

    #[test]
    fn ignore_level_serialization() {
        let toml_str = r#"
mask = "*!*@spam"
levels = ["MSGS", "ALL"]
"#;
        let entry: IgnoreEntry = toml::from_str(toml_str).unwrap();
        assert_eq!(entry.levels, vec![IgnoreLevel::Msgs, IgnoreLevel::All]);

        let serialized = toml::to_string_pretty(&entry).unwrap();
        assert!(serialized.contains("\"MSGS\""));
        assert!(serialized.contains("\"ALL\""));
    }

    #[test]
    fn parse_ignore_entries() {
        let toml_str = r##"
[[ignores]]
mask = "*!*@spam.host"
levels = ["MSGS", "NOTICES"]

[[ignores]]
mask = "annoying*"
levels = ["ALL"]
channels = ["#general"]
"##;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.ignores.len(), 2);
        assert_eq!(config.ignores[0].mask, "*!*@spam.host");
        assert_eq!(
            config.ignores[0].levels,
            vec![IgnoreLevel::Msgs, IgnoreLevel::Notices]
        );
        assert!(config.ignores[0].channels.is_none());
        assert_eq!(
            config.ignores[1].channels.as_ref().unwrap(),
            &vec!["#general".to_string()]
        );
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join("repartee_test_config");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");

        let mut config = default_config();
        config.general.nick = "TestUser".to_string();
        config.servers.insert(
            "test".to_string(),
            ServerConfig {
                label: "Test".to_string(),
                address: "irc.test.net".to_string(),
                port: 6697,
                tls: true,
                tls_verify: true,
                autoconnect: false,
                channels: vec!["#test".to_string()],
                nick: None,
                username: None,
                realname: None,
                password: None,
                sasl_user: Some("user".to_string()),
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

        save_config(&path, &config).unwrap();
        let loaded = load_config(&path).unwrap();

        assert_eq!(loaded.general.nick, "TestUser");
        let server = loaded.servers.get("test").unwrap();
        assert_eq!(server.label, "Test");
        assert_eq!(server.sasl_user.as_deref(), Some("user"));
        assert!(server.sasl_pass.is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_config_missing_file() {
        let path = std::env::temp_dir().join("repartee_test_nonexistent/config.toml");
        let config = load_config(&path).unwrap();
        assert_eq!(config.general.nick, crate::constants::APP_NAME);
    }
}

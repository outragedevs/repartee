pub mod defaults;
pub mod env;

use std::collections::HashMap;
use std::path::Path;

use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};

pub use defaults::default_config;
pub use env::{
    apply_credentials, apply_shrink_credentials, apply_translate_credentials,
    apply_web_credentials, ensure_session_secret, load_env, set_env_value,
};

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
    Typing,
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

/// Schema version of the config file this build writes.
///
/// Bump whenever a change to the *defaults* has to be back-filled into config
/// files that already exist on disk (a `#[serde(default)]` only fills fields
/// the file omits — it cannot touch a list the user's file already pins).
/// Every bump needs a matching arm in [`migrate_config`].
///
/// * 1 — the `typing` statusbar item, inserted after `channel_info`.
pub const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Schema version of the file on disk, so one-time default back-fills know
    /// whether they have already run. Must stay **first**: TOML requires scalar
    /// values before any table, and every other field of this struct is one.
    ///
    /// The field-level `#[serde(default)]` is load-bearing and deliberately
    /// shadows the container-level one: it resolves to `u32::default()` (0),
    /// not to `AppConfig::default().config_version` (the current version). A
    /// file written before this field existed must read back as 0, otherwise
    /// nothing is ever migrated.
    #[serde(default)]
    pub config_version: u32,
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
    pub shrink: ShrinkConfig,
    pub emotes: EmotesConfig,
    #[serde(default)]
    pub typing: TypingConfig,
    #[serde(default)]
    pub translate: TranslateConfig,
}

/// Hand-written (not derived) for one reason: `config_version` must be
/// [`CONFIG_VERSION`] here, so a config born from the defaults is already
/// current and no migration ever touches it. Everything else is its own
/// `Default`. The derived impl would hand out version 0 and make first run
/// look like a stale file.
impl Default for AppConfig {
    fn default() -> Self {
        Self {
            config_version: CONFIG_VERSION,
            general: GeneralConfig::default(),
            display: DisplayConfig::default(),
            sidepanel: SidepanelConfig::default(),
            statusbar: StatusbarConfig::default(),
            image_preview: ImagePreviewConfig::default(),
            servers: HashMap::new(),
            aliases: HashMap::new(),
            ignores: Vec::new(),
            scripts: ScriptsConfig::default(),
            logging: LoggingConfig::default(),
            dcc: DccConfig::default(),
            spellcheck: SpellcheckConfig::default(),
            web: WebConfig::default(),
            e2e: E2eConfig::default(),
            shrink: ShrinkConfig::default(),
            emotes: EmotesConfig::default(),
            typing: TypingConfig::default(),
            translate: TranslateConfig::default(),
        }
    }
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
    pub flood_exemptions: Vec<String>,
    pub ctcp_version: String,
    /// Fallback local IP to bind outgoing IRC sockets to, used when a
    /// server's per-server `bind_ip` is unset. Useful on hosts with
    /// multiple addresses where you want a default source IP without
    /// duplicating it on every `[servers.*]` entry.
    ///
    /// Precedence (highest first):
    ///   1. `servers.<id>.bind_ip` (config or `/server set ... -bind=`)
    ///   2. `repartee -h <ip>` CLI override (runtime only, not persisted)
    ///   3. `general.default_bind_ip` (this field)
    ///   4. OS default (kernel picks via routing table)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_bind_ip: Option<String>,
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
            flood_exemptions: Vec::new(),
            ctcp_version: format!("{APP_NAME} {APP_VERSION}"),
            default_bind_ip: None,
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
                StatusbarItem::Typing,
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
    /// SASL mechanism to use — `"PLAIN"`, `"EXTERNAL"`, `"SCRAM-SHA-1"`,
    /// `"SCRAM-SHA-256"`, `"SCRAM-SHA-512"`, `"ECDSA-NIST256P-CHALLENGE"` — or
    /// `None` to auto-detect the strongest the server offers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sasl_mechanism: Option<String>,
    /// Path to a client TLS certificate (PEM) for SASL EXTERNAL / `CertFP` auth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_cert_path: Option<String>,
    /// Path to a PEM NIST P-256 private key for SASL
    /// `ECDSA-NIST256P-CHALLENGE`. Distinct from `client_cert_path`, which is
    /// the TLS client certificate: this key is never presented to TLS, only
    /// used to sign the server's challenge.
    ///
    /// A path, not key material, so it is written to `config.toml` like
    /// `client_cert_path` and unlike `sasl_pass`. Relative paths resolve
    /// against `~/.repartee/certs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sasl_key_path: Option<String>,
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

/// URL shortener integration. Shortens long URLs in outgoing and/or
/// incoming chat messages via a shrink-compatible API (default
/// `https://shr.al`). The API key is loaded from `.env`
/// (`SHRINK_API_KEY`) and never serialized to `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ShrinkConfig {
    /// Master switch — when false, no shortening happens in either
    /// direction even if outgoing/incoming flags are true.
    pub enabled: bool,
    /// Base URL of the shrink API (no trailing slash).
    pub api_url: String,
    /// API key. Always populated from `.env` (`SHRINK_API_KEY`); the
    /// `#[serde(skip)]` ensures `/save` never writes it to disk.
    #[serde(skip)]
    pub api_key: String,
    /// Shorten URLs in messages we send.
    pub outgoing_enabled: bool,
    /// Shorten URLs in incoming live messages (NOT in backlog).
    pub incoming_enabled: bool,
    /// URLs at least this many characters long are candidates. Length
    /// includes the scheme — `https://x` counts as 9. Floor 25
    /// enforced in `/set`.
    pub min_url_length: u32,
    /// Per-URL shorten timeout for outgoing messages. The user is
    /// blocked on this; default 2 s.
    pub outgoing_timeout_ms: u64,
    /// Per-URL shorten timeout for incoming messages. Runs in the
    /// background, so a longer budget is OK but kept symmetric for
    /// predictability.
    pub incoming_timeout_ms: u64,
    /// LRU cache size — bounded so RAM usage stays predictable.
    /// At ~150 bytes per entry, 500 ≈ 75 KB.
    pub cache_max_entries: u32,
}

impl Default for ShrinkConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_url: "https://shr.al".to_string(),
            api_key: String::new(),
            outgoing_enabled: true,
            incoming_enabled: true,
            min_url_length: 50,
            outgoing_timeout_ms: 2000,
            incoming_timeout_ms: 2000,
            cache_max_entries: 500,
        }
    }
}

/// Near-real-time channel translation.
///
/// This configures the *mechanism* — what is eligible, how long a line may
/// wait, how it is displayed. Everything about how translation is actually
/// performed lives behind the seam in `src/translate/backend.rs` and is
/// configured separately.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TranslateConfig {
    /// Master switch — when false nothing is translated in either
    /// direction, even for buffers with per-buffer flags set.
    ///
    /// On its own it translates NOTHING: it says the mechanism may run, not
    /// that there is anything to run it with. See `backend`.
    pub enabled: bool,
    /// Which implementation behind the seam to install, by name.
    ///
    /// `"none"` (the default) means there is no translator: the mechanism
    /// stays wired but every line is delivered as it arrived. `"ai"` selects
    /// the OpenAI-compatible provider policy configured under `translate.ai`.
    /// `"stub"` selects the built-in test backend, which does not translate —
    /// it REVERSES word order — and exists so the mechanism can be exercised
    /// end to end with no API key.
    ///
    /// Deliberately not implied by `enabled`. A user who turns translation
    /// on expects a translator; installing the stub would publish reversed
    /// sentences on a real channel, under their nick, as if they had typed
    /// them. Naming the stub is the difference between testing the mechanism
    /// and being handed it by surprise.
    pub backend: String,
    /// The language YOU read and write, unless a buffer overrides it.
    ///
    /// Named `my_lang` rather than `target_lang` deliberately: "target" is
    /// ambiguous about WHOSE language it means, and reading it as "the
    /// language to translate into" is what produced an inverted outgoing
    /// direction during development.
    pub my_lang: String,
    /// Append ` [original]` to incoming translated lines.
    pub show_original_in: bool,
    /// Append ` [original]` to the local echo of outgoing translated lines.
    pub show_original_out: bool,
    /// How long a line may sit in the reorder queue before it is released
    /// untranslated. The default sits above the worst case measured during
    /// research (~4.2 s), so a healthy provider never trips it.
    pub timeout_ms: u64,
    /// Concurrent in-flight translations. Raising this past a provider's
    /// measured concurrency limit makes throughput worse, not better.
    pub max_in_flight: u32,
    /// Per-buffer queue ceiling. On overflow the oldest pending entries are
    /// released untranslated, so a dead provider cannot turn the queue into
    /// an unbounded memory leak with a frozen channel behind it.
    pub max_queue: u32,
    pub ai: TranslateAiConfig,
    /// Per-buffer settings, keyed by buffer id (`<connection_id>/<target>`).
    pub buffers: HashMap<String, TranslateBufferConfig>,
}

/// Which directions are translated for one channel or query.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TranslateBufferConfig {
    pub incoming: bool,
    pub outgoing: bool,
    /// The language spoken in THIS channel or query.
    ///
    /// A buffer has a language PAIR, not a per-direction setting: this one
    /// and ours. The directions swap them —
    ///
    /// | direction | source | target |
    /// |---|---|---|
    /// | incoming | `lang` | `my_lang` |
    /// | outgoing | `my_lang` | `lang` |
    ///
    /// — which is why both call sites resolve the pair through the single
    /// [`crate::translate::resolve_langs`] rather than reading these fields
    /// directly. Reading them per direction is exactly how the outgoing
    /// direction ended up inverted during development.
    ///
    /// `None` is allowed for incoming, where it means "let the broker detect
    /// it". Outgoing cannot autodetect a TARGET — there is nothing to detect
    /// which language to write in from — so it requires this to be set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// Per-buffer override of [`TranslateConfig::my_lang`].
    ///
    /// For reading one channel in a different language than the rest — say
    /// the machine translation into your own language is poor for that
    /// source, and English reads better.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub my_lang: Option<String>,
}

impl Default for TranslateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: "none".to_string(),
            my_lang: "en".to_string(),
            show_original_in: true,
            show_original_out: true,
            timeout_ms: 15_000,
            max_in_flight: 4,
            max_queue: 200,
            ai: TranslateAiConfig::default(),
            buffers: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TranslateAiConfig {
    pub easy: Vec<String>,
    pub strong: Vec<String>,
    #[serde(default)]
    pub terminal: Option<Vec<String>>,
    pub preferred_attempt_ms: u64,
    pub prompt_path: String,
    pub models: Vec<TranslateAiModelConfig>,
}

impl Default for TranslateAiConfig {
    fn default() -> Self {
        Self {
            easy: vec![
                "openrouter-gemma4-31b".to_string(),
                "ollama-gemma4-31b".to_string(),
                "groq-gptoss-120b".to_string(),
                "groq-qwen36-27b".to_string(),
            ],
            strong: vec![
                "gemini-36-flash".to_string(),
                "openrouter-gemma4-31b".to_string(),
                "ollama-gemma4-31b".to_string(),
                "groq-qwen36-27b".to_string(),
                "groq-gptoss-120b".to_string(),
            ],
            terminal: Some(vec![
                "groq-qwen36-27b".to_string(),
                "groq-gptoss-120b".to_string(),
            ]),
            preferred_attempt_ms: 3_000,
            prompt_path: String::new(),
            models: default_translate_ai_models(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TranslateAiModelConfig {
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub api_key_env: String,
    #[serde(skip)]
    pub api_key: String,
    pub rpm: u32,
    pub tpm: u32,
    pub max_retries: u32,
    pub max_output_tokens: u32,
    pub reasoning_effort: Option<String>,
    pub provider: Option<TranslateAiProviderConfig>,
}

impl Default for TranslateAiModelConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            base_url: String::new(),
            model: String::new(),
            api_key_env: String::new(),
            api_key: String::new(),
            rpm: 0,
            tpm: 0,
            max_retries: 2,
            max_output_tokens: 256,
            reasoning_effort: None,
            provider: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TranslateAiProviderConfig {
    pub only: Vec<String>,
    pub quantizations: Vec<String>,
    pub allow_fallbacks: bool,
}

fn default_translate_ai_models() -> Vec<TranslateAiModelConfig> {
    vec![
        TranslateAiModelConfig {
            name: "openrouter-gemma4-31b".to_string(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            model: "google/gemma-4-31b-it".to_string(),
            api_key_env: "OPENROUTER_API".to_string(),
            rpm: 60,
            provider: Some(TranslateAiProviderConfig {
                only: vec![
                    "OpenInference".to_string(),
                    "Venice".to_string(),
                    "CoreWeave".to_string(),
                ],
                quantizations: vec!["bf16".to_string(), "fp16".to_string()],
                allow_fallbacks: false,
            }),
            ..TranslateAiModelConfig::default()
        },
        TranslateAiModelConfig {
            name: "ollama-gemma4-31b".to_string(),
            base_url: "https://ollama.com/v1".to_string(),
            model: "gemma4:31b".to_string(),
            api_key_env: "OLLAMA_API".to_string(),
            rpm: 20,
            ..TranslateAiModelConfig::default()
        },
        TranslateAiModelConfig {
            name: "gemini-36-flash".to_string(),
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai".to_string(),
            model: "gemini-3.6-flash".to_string(),
            api_key_env: "GEMINI_API".to_string(),
            rpm: 15,
            ..TranslateAiModelConfig::default()
        },
        TranslateAiModelConfig {
            name: "groq-qwen36-27b".to_string(),
            base_url: "https://api.groq.com/openai/v1".to_string(),
            model: "qwen/qwen3.6-27b".to_string(),
            api_key_env: "GROQ_API_KEY".to_string(),
            rpm: 30,
            tpm: 8_000,
            reasoning_effort: Some("none".to_string()),
            ..TranslateAiModelConfig::default()
        },
        TranslateAiModelConfig {
            name: "groq-gptoss-120b".to_string(),
            base_url: "https://api.groq.com/openai/v1".to_string(),
            model: "openai/gpt-oss-120b".to_string(),
            api_key_env: "GROQ_API_KEY".to_string(),
            rpm: 30,
            tpm: 8_000,
            reasoning_effort: Some("low".to_string()),
            ..TranslateAiModelConfig::default()
        },
    ]
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
    /// Session lifetime in days (default 90).
    /// Sessions persist to disk; cookie carries `Max-Age=session_days*86400`.
    pub session_days: u32,
    /// Username pre-filled in the login form (default `"repartee"`).
    /// The server only validates the password — the username exists so password
    /// managers (1Password, iCloud Keychain, Bitwarden) recognise the form.
    pub username: String,
    /// Enable server-side image previews under chat messages (default false).
    pub image_previews: bool,
    /// Maximum number of preview thumbnails per message (default 4).
    pub image_previews_max_per_msg: u32,
    /// Maximum total size of the thumbnail cache in megabytes (default 200).
    pub thumbnail_cache_mb: u32,
    /// Cloudflare tunnel name (future use).
    pub cloudflare_tunnel_name: String,
    /// Login password — loaded from `.env` (`WEB_PASSWORD`), not serialized to TOML.
    #[serde(skip)]
    pub password: String,
    /// 32-byte HMAC key for hashing session tokens at rest. Loaded from
    /// `.env` (`WEB_SESSION_SECRET`); auto-generated on first start if absent.
    /// Rotating this value invalidates every persisted session (deliberate
    /// "log everyone out" knob).
    #[serde(skip)]
    pub session_secret: Vec<u8>,
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
            session_days: 90,
            username: "repartee".to_string(),
            image_previews: false,
            image_previews_max_per_msg: 4,
            thumbnail_cache_mb: 200,
            cloudflare_tunnel_name: String::new(),
            password: String::new(),
            session_secret: Vec::new(),
        }
    }
}

/// How `:name:` emote tokens are rendered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderMode {
    /// Render as an inline image where the surface supports it; fall back to text.
    #[default]
    Graphical,
    /// Always render the literal `:name:` text.
    Text,
    /// Do not treat `:name:` as an emote at all.
    Off,
}

/// Picker / autocomplete-insert preview language for emotes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmoteLang {
    /// English aliases (`:smile:`).
    #[default]
    En,
    /// Polish stems (`:usmiech:`).
    Pl,
}

impl EmoteLang {
    /// Map to the registry's language enum.
    #[must_use]
    pub const fn to_registry(self) -> crate::emotes::Lang {
        match self {
            Self::En => crate::emotes::Lang::En,
            Self::Pl => crate::emotes::Lang::Pl,
        }
    }
}

/// `[emotes]` configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmotesConfig {
    /// Enable built-in `:name:` emotes.
    pub enabled: bool,
    /// How emotes are rendered.
    pub render: RenderMode,
    /// Picker / insert preview language.
    pub lang: EmoteLang,
    /// Maximum width, in terminal cells, an inline emote may occupy. Wide GIFs
    /// scale up to this many columns (preserving aspect, never past native size)
    /// instead of being crushed into a fixed 2-cell box.
    pub max_cols: u16,
    /// Maximum height, in terminal rows, an inline emote may occupy. Tall GIFs
    /// grow up to this many rows (the chat reserves blank rows below the line so
    /// the emote does not overlap following text). `1` keeps every emote strictly
    /// inline (no reserved rows).
    pub max_rows: u16,
}

impl EmotesConfig {
    /// Whether the web UI should render `:name:` as inline images: enabled and
    /// in graphical mode. Pushed to the web on connect (`SyncInit`) and change
    /// (`SettingsChanged`).
    #[must_use]
    pub fn web_enabled(&self) -> bool {
        self.enabled && self.render == RenderMode::Graphical
    }
}

impl Default for EmotesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            render: RenderMode::Graphical,
            lang: EmoteLang::En,
            max_cols: 8,
            max_rows: 3,
        }
    }
}

/// `IRCv3` `+typing`. Split three ways because the spec asks clients to "provide
/// appropriate privacy controls": you may watch without broadcasting, or
/// broadcast in DMs but not in public channels.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TypingConfig {
    /// Receive and display other people's typing indicators. Gates ingestion,
    /// not just rendering (spec §5).
    pub show: bool,
    /// Send `+typing` in channels.
    pub send_channels: bool,
    /// Send `+typing` in private queries.
    pub send_queries: bool,
}

impl Default for TypingConfig {
    fn default() -> Self {
        Self {
            show: true,
            send_channels: true,
            send_queries: true,
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

/// Startup config load: parse and migrate **in memory**. Writes nothing.
///
/// Deliberately not persisted. A startup write-back would rewrite the config of
/// every user upgrading into this version, none of whom asked for a write, and
/// the rewrite is lossy: `AppConfig` has no `deny_unknown_fields` and
/// `toml::to_string_pretty` emits only the fields this build knows, so every
/// comment and every unknown key in a hand-maintained `config.toml` would be
/// destroyed on first launch.
///
/// Persistence rides the next save the user actually triggers, which is enough
/// to make a deliberate removal stick, because [`save_config`] serializes
/// `config_version` along with everything else:
///
/// * existing user who never touches a setting → the migration re-runs in memory
///   on every start. Harmless: no file write, comments intact.
/// * user runs `/items remove typing` → `save_config` writes the list without
///   `typing` **and** `config_version = CONFIG_VERSION` → the next start sees a
///   current file, [`migrate_config`] returns early, and the removal sticks.
///
/// A missing file yields the defaults — creating the initial `config.toml` is
/// `constants::ensure_config_dir`'s job, not the migration's.
pub fn load_and_migrate(path: &Path) -> Result<AppConfig> {
    let mut config = load_config(path)?;
    if migrate_config(&mut config) {
        tracing::info!(
            path = %path.display(),
            version = CONFIG_VERSION,
            "config migrated in memory; will persist on the next save"
        );
    }
    Ok(config)
}

/// Bring a freshly-parsed config up to [`CONFIG_VERSION`], returning `true`
/// if anything changed — the caller then persists it (see `App::new_with_mode`).
///
/// Pure and side-effect-free on purpose: the file I/O lives at the call site so
/// this stays unit-testable and so `load_config` (also used by tests and by
/// `/reload`) keeps its current semantics.
///
/// Why a version gate rather than "append the item if it is missing": the
/// blind version resurrects an item the user deliberately dropped with
/// `/items remove typing` on the very next start. The version — persisted with
/// the config — is what makes the back-fill happen exactly once.
///
/// Placement is relative to whatever the user already has; their order is never
/// reshuffled. `typing` goes right after `channel_info`, else right before
/// `lag`, else at the end.
pub fn migrate_config(config: &mut AppConfig) -> bool {
    if config.config_version >= CONFIG_VERSION {
        return false;
    }

    // v0 → v1: the `typing` statusbar item. Existing files pin an explicit
    // `items` list, so `#[serde(default)]` cannot reach them.
    if !config.statusbar.items.contains(&StatusbarItem::Typing) {
        let items = &mut config.statusbar.items;
        let at = items
            .iter()
            .position(|i| *i == StatusbarItem::ChannelInfo)
            .map_or_else(
                || {
                    items
                        .iter()
                        .position(|i| *i == StatusbarItem::Lag)
                        .unwrap_or(items.len())
                },
                |channel| channel + 1,
            );
        items.insert(at, StatusbarItem::Typing);
        tracing::info!(position = at, "config migration: added the typing statusbar item");
    }

    config.config_version = CONFIG_VERSION;
    true
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

/// Validate config + selected theme **before** the fork-detach split, so
/// TOML parse errors (typos like `autoconnect = fals`, malformed strings,
/// truncated themes) surface on the parent's TTY instead of vanishing
/// into the daemon's `/dev/null` stderr.
///
/// Returns the parsed `AppConfig` so the caller can reuse the theme name
/// and validate the matching `*.theme` file in one pass. Missing config
/// resolves to `default_config()` (first run). Missing theme is fine —
/// `theme::load_theme` returns the built-in fallback.
///
/// The child process re-parses the same files via `App::new`; this
/// validation is a fast pre-check, not a substitute. The narrow race
/// where the user edits between this call and `App::new` is harmless —
/// `waitpid` will surface the child's parse error then.
pub fn validate_startup_files(config_path: &Path, theme_dir: &Path) -> Result<AppConfig> {
    let config = match std::fs::read_to_string(config_path) {
        Ok(content) => toml::from_str::<AppConfig>(&content).map_err(|e| {
            color_eyre::eyre::eyre!("Invalid TOML in {}\n{e}", config_path.display())
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => default_config(),
        Err(e) => {
            return Err(color_eyre::eyre::eyre!(
                "Could not read config {}: {e}",
                config_path.display()
            ));
        }
    };

    let theme_path = theme_dir.join(format!("{}.theme", config.general.theme));
    if theme_path.exists() {
        let content = std::fs::read_to_string(&theme_path).map_err(|e| {
            color_eyre::eyre::eyre!("Could not read theme {}: {e}", theme_path.display())
        })?;
        toml::from_str::<toml::Value>(&content).map_err(|e| {
            color_eyre::eyre::eyre!("Invalid TOML in theme {}\n{e}", theme_path.display())
        })?;
    }

    Ok(config)
}

// === Tests ===

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emotes_lang_default_and_parse() {
        assert_eq!(AppConfig::default().emotes.lang, EmoteLang::En);
        let p: AppConfig = toml::from_str("[emotes]\nlang = \"pl\"\n").unwrap();
        assert_eq!(p.emotes.lang, EmoteLang::Pl);
    }

    #[test]
    fn emotes_config_defaults_and_roundtrip() {
        let cfg = AppConfig::default();
        assert!(cfg.emotes.enabled);
        assert_eq!(cfg.emotes.render, RenderMode::Graphical);
        assert_eq!(cfg.emotes.max_cols, 8);
        assert_eq!(cfg.emotes.max_rows, 3);

        // TOML round-trip preserves the section.
        let toml_str = toml::to_string(&cfg).expect("serialize");
        let back: AppConfig = toml::from_str(&toml_str).expect("deserialize");
        assert_eq!(back.emotes.render, RenderMode::Graphical);
        assert_eq!(back.emotes.max_cols, 8);
        assert_eq!(back.emotes.max_rows, 3);

        // Parsing an explicit section. An older config without the new sizing
        // fields must still deserialize, falling back to the defaults.
        let parsed: AppConfig =
            toml::from_str("[emotes]\nenabled = false\nrender = \"text\"\n").unwrap();
        assert!(!parsed.emotes.enabled);
        assert_eq!(parsed.emotes.render, RenderMode::Text);
        assert_eq!(parsed.emotes.max_cols, 8, "missing max_cols falls back to default");
        assert_eq!(parsed.emotes.max_rows, 3, "missing max_rows falls back to default");
    }

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
                sasl_key_path: None,
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

    #[test]
    fn validate_startup_files_typo_returns_clear_error() {
        // Regression for the silent-fork-death bug: a typo like
        //   autoconnect = fals
        // makes the daemon child crash with /dev/null stderr, leaving the
        // user staring at "No session found for PID X" 5 seconds later.
        // The pre-fork validator must surface the underlying TOML error
        // verbatim so the parent's TTY shows it before any fork happens.
        let dir = std::env::temp_dir().join("repartee_validate_typo");
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.toml");
        std::fs::write(
            &cfg,
            "[general]\nnick = \"x\"\n\
             [servers.libera]\nlabel = \"L\"\naddress = \"a\"\nport = 6697\n\
             tls = true\nautoconnect = fals\n",
        )
        .unwrap();

        let theme_dir = dir.join("themes");
        std::fs::create_dir_all(&theme_dir).unwrap();

        let err = validate_startup_files(&cfg, &theme_dir).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("Invalid TOML"),
            "expected 'Invalid TOML' prefix in {msg}"
        );
        assert!(
            msg.contains("autoconnect") || msg.contains("fals") || msg.contains("boolean"),
            "error must point at the typo, got: {msg}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn validate_startup_files_missing_config_uses_defaults() {
        let dir = std::env::temp_dir().join("repartee_validate_missing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.toml"); // does not exist
        let theme_dir = dir.join("themes");
        std::fs::create_dir_all(&theme_dir).unwrap();

        let config = validate_startup_files(&cfg, &theme_dir).unwrap();
        assert_eq!(config.general.nick, crate::constants::APP_NAME);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn validate_startup_files_broken_theme_returns_clear_error() {
        let dir = std::env::temp_dir().join("repartee_validate_theme");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.toml");
        std::fs::write(&cfg, "[general]\ntheme = \"broken\"\n").unwrap();
        let theme_dir = dir.join("themes");
        std::fs::create_dir_all(&theme_dir).unwrap();
        // Truly malformed TOML in the selected theme.
        std::fs::write(theme_dir.join("broken.theme"), "[meta\nname = \"x\"").unwrap();

        let err = validate_startup_files(&cfg, &theme_dir).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("Invalid TOML in theme"),
            "expected theme error, got: {msg}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn typing_defaults_are_on() {
        let config = AppConfig::default();
        assert!(config.typing.show);
        assert!(config.typing.send_channels);
        assert!(config.typing.send_queries);
    }

    #[test]
    fn typing_item_sits_between_channel_and_lag() {
        let config = AppConfig::default();
        let items = &config.statusbar.items;
        let channel = items.iter().position(|i| *i == StatusbarItem::ChannelInfo);
        let typing = items.iter().position(|i| *i == StatusbarItem::Typing);
        let lag = items.iter().position(|i| *i == StatusbarItem::Lag);
        assert!(channel < typing, "typing must come after the buffer name");
        assert!(typing < lag, "typing must come before lag");
    }

    #[test]
    fn typing_config_round_trips_through_toml() {
        let toml = "[typing]\nshow = false\nsend_channels = false\nsend_queries = true\n";
        let config: AppConfig = toml::from_str(toml).expect("parses");
        assert!(!config.typing.show);
        assert!(!config.typing.send_channels);
        assert!(config.typing.send_queries);
    }

    // === Config migration (statusbar `typing` item) ===

    /// The list every pre-`typing` install has pinned in its `config.toml`.
    const LEGACY_STATUSBAR: &str = r#"
[statusbar]
items = ["time", "nick_info", "channel_info", "lag", "active_windows"]
"#;

    #[test]
    fn legacy_config_deserializes_at_version_zero() {
        // The whole migration hinges on this: a file written before the
        // schema-version field existed must read back as 0, NOT as
        // `AppConfig::default().config_version`.
        let config: AppConfig = toml::from_str(LEGACY_STATUSBAR).expect("parses");
        assert_eq!(config.config_version, 0);
    }

    #[test]
    fn migration_inserts_typing_after_channel_info() {
        let mut config: AppConfig = toml::from_str(LEGACY_STATUSBAR).expect("parses");
        assert!(migrate_config(&mut config), "legacy config must be migrated");
        assert_eq!(
            config.statusbar.items,
            vec![
                StatusbarItem::Time,
                StatusbarItem::NickInfo,
                StatusbarItem::ChannelInfo,
                StatusbarItem::Typing,
                StatusbarItem::Lag,
                StatusbarItem::ActiveWindows,
            ]
        );
        assert_eq!(config.config_version, CONFIG_VERSION);
    }

    #[test]
    fn migration_is_idempotent() {
        let mut config: AppConfig = toml::from_str(LEGACY_STATUSBAR).expect("parses");
        assert!(migrate_config(&mut config));
        assert!(
            !migrate_config(&mut config),
            "a second run must report no change"
        );
        assert_eq!(
            config
                .statusbar
                .items
                .iter()
                .filter(|i| **i == StatusbarItem::Typing)
                .count(),
            1,
            "typing must not be inserted twice"
        );
    }

    #[test]
    fn migration_does_not_resurrect_a_deliberately_removed_item() {
        // `/items remove typing` saves the list without it — at the current
        // version. The migration must leave that alone forever.
        let toml = format!(
            "config_version = {CONFIG_VERSION}\n{LEGACY_STATUSBAR}"
        );
        let mut config: AppConfig = toml::from_str(&toml).expect("parses");
        assert!(!migrate_config(&mut config), "already migrated");
        assert!(!config.statusbar.items.contains(&StatusbarItem::Typing));
    }

    #[test]
    fn migration_falls_back_to_before_lag_without_channel_info() {
        let mut config: AppConfig =
            toml::from_str("[statusbar]\nitems = [\"time\", \"lag\"]\n").expect("parses");
        assert!(migrate_config(&mut config));
        assert_eq!(
            config.statusbar.items,
            vec![StatusbarItem::Time, StatusbarItem::Typing, StatusbarItem::Lag]
        );
    }

    #[test]
    fn migration_appends_when_neither_anchor_is_present() {
        let mut config: AppConfig =
            toml::from_str("[statusbar]\nitems = [\"time\", \"active_windows\"]\n")
                .expect("parses");
        assert!(migrate_config(&mut config));
        assert_eq!(
            config.statusbar.items,
            vec![
                StatusbarItem::Time,
                StatusbarItem::ActiveWindows,
                StatusbarItem::Typing,
            ]
        );
    }

    #[test]
    fn migration_appends_to_an_empty_item_list_without_panicking() {
        let mut config: AppConfig =
            toml::from_str("[statusbar]\nitems = []\n").expect("parses");
        assert!(migrate_config(&mut config));
        assert_eq!(config.statusbar.items, vec![StatusbarItem::Typing]);
    }

    #[test]
    fn migration_preserves_a_customised_order() {
        // A user who moved things around keeps their order — typing is only
        // *inserted*, never a reshuffle.
        let mut config: AppConfig = toml::from_str(
            "[statusbar]\nitems = [\"active_windows\", \"channel_info\", \"lag\", \"time\"]\n",
        )
        .expect("parses");
        assert!(migrate_config(&mut config));
        assert_eq!(
            config.statusbar.items,
            vec![
                StatusbarItem::ActiveWindows,
                StatusbarItem::ChannelInfo,
                StatusbarItem::Typing,
                StatusbarItem::Lag,
                StatusbarItem::Time,
            ]
        );
    }

    #[test]
    fn default_config_is_already_current_and_needs_no_migration() {
        let mut config = default_config();
        assert_eq!(config.config_version, CONFIG_VERSION);
        assert_eq!(config.statusbar.items.get(3), Some(&StatusbarItem::Typing));
        assert!(
            !migrate_config(&mut config),
            "a fresh config must not be migrated"
        );
    }

    #[test]
    fn load_and_migrate_never_rewrites_the_users_file() {
        // THE REGRESSION. The migration used to write the result back at startup,
        // for a user who never asked for a write. `AppConfig` has no
        // `deny_unknown_fields` and `to_string_pretty` emits only known fields, so
        // that rewrite silently destroyed every comment and every unknown key in a
        // hand-maintained config — on the first launch after an upgrade.
        //
        // Migrating in memory costs nothing: it re-runs on each start until a save
        // the user actually triggered persists the new version.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let original = format!(
            "# my hand-written config — keep me\n\
             a_key_this_build_does_not_know = 42\n\
             {LEGACY_STATUSBAR}"
        );
        std::fs::write(&path, &original).expect("write legacy config");

        let config = load_and_migrate(&path).expect("loads");
        assert_eq!(
            config.statusbar.items.get(3),
            Some(&StatusbarItem::Typing),
            "the migration still applies — in memory"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("re-read"),
            original,
            "startup must not touch the user's file: comments and unknown keys survive"
        );
    }

    #[test]
    fn a_removal_sticks_because_save_config_writes_the_current_version() {
        // What makes the in-memory migration safe. The user runs
        // `/items remove typing`; `save_config` serializes `AppConfig`, which
        // carries `config_version`, so the next start sees a current file and the
        // back-fill does not run — the removal sticks without startup ever writing.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, LEGACY_STATUSBAR).expect("write legacy config");

        // Start once: migrated in memory, nothing written.
        let mut config = load_and_migrate(&path).expect("loads");
        assert!(config.statusbar.items.contains(&StatusbarItem::Typing));

        // `/items remove typing` — a save the user actually asked for.
        config.statusbar.items.retain(|i| *i != StatusbarItem::Typing);
        save_config(&path, &config).expect("saves");
        assert_eq!(
            load_config(&path).expect("reloads").config_version,
            CONFIG_VERSION,
            "save_config must persist the version, or the back-fill runs forever"
        );

        // Restart: the removal survives.
        let after_restart = load_and_migrate(&path).expect("loads");
        assert!(
            !after_restart.statusbar.items.contains(&StatusbarItem::Typing),
            "a deliberate `/items remove typing` must not be resurrected"
        );
    }

    #[test]
    fn load_and_migrate_leaves_a_current_file_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        save_config(&path, &default_config()).expect("saves");
        let before = std::fs::metadata(&path).expect("stat").modified().ok();
        let config = load_and_migrate(&path).expect("loads");
        assert_eq!(config.config_version, CONFIG_VERSION);
        let after = std::fs::metadata(&path).expect("stat").modified().ok();
        assert_eq!(before, after, "a current config must not be rewritten");
    }

    #[test]
    fn load_and_migrate_on_a_missing_file_yields_defaults_and_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("does-not-exist.toml");
        let config = load_and_migrate(&path).expect("loads defaults");
        assert_eq!(config.config_version, CONFIG_VERSION);
        assert!(
            !path.exists(),
            "first-run config creation belongs to ensure_config_dir, not the migration"
        );
    }

    #[test]
    fn migrated_config_version_survives_a_save_load_round_trip() {
        // If the version does not persist, the migration re-runs forever and
        // `/items remove typing` never sticks.
        let mut config: AppConfig = toml::from_str(LEGACY_STATUSBAR).expect("parses");
        assert!(migrate_config(&mut config));
        let serialized = toml::to_string_pretty(&config).expect("serializes");
        let reloaded: AppConfig = toml::from_str(&serialized).expect("parses back");
        assert_eq!(reloaded.config_version, CONFIG_VERSION);
        assert!(reloaded.statusbar.items.contains(&StatusbarItem::Typing));
    }

    #[test]
    fn documented_configuration_example_parses() {
        let markdown = include_str!("../../docs/src/content/configuration.md");
        let (_, after_fence) = markdown
            .split_once("```toml\n")
            .expect("documented TOML example");
        let (example, _) = after_fence
            .split_once("\n```")
            .expect("closed TOML example");
        let _: AppConfig = toml::from_str(example).expect("documented configuration must parse");
    }
}

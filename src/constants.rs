pub const APP_NAME: &str = "repartee";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// WHOX field selector string.
/// Fields requested: t=token, c=channel, u=user, i=ip, h=host,
/// n=nick, f=flags, a=account, r=realname.
/// Note: `s` (server), `d` (hopcount), `l` (idle) are omitted because
/// `IRCnet` ircd 2.12 silently drops unsupported fields, causing arg count
/// mismatches in the parser.
pub const WHOX_FIELDS: &str = "%tcuihnfar";

/// Bundled default theme shipped with the binary.
const DEFAULT_THEME: &str = include_str!("../themes/default.theme");

use std::path::PathBuf;

pub fn home_dir() -> PathBuf {
    let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(format!(".{APP_NAME}"))
}

pub fn config_path() -> PathBuf {
    home_dir().join("config.toml")
}

pub fn theme_dir() -> PathBuf {
    home_dir().join("themes")
}

pub fn env_path() -> PathBuf {
    home_dir().join(".env")
}

pub fn log_dir() -> PathBuf {
    home_dir().join("logs")
}

pub fn scripts_dir() -> PathBuf {
    home_dir().join("scripts")
}

/// Create config directory and write default files on first run.
pub fn ensure_config_dir() {
    let home = home_dir();
    if let Err(e) = std::fs::create_dir_all(theme_dir()) {
        tracing::warn!("failed to create theme dir: {e}");
    }
    if let Err(e) = std::fs::create_dir_all(log_dir()) {
        tracing::warn!("failed to create log dir: {e}");
    }
    if let Err(e) = std::fs::create_dir_all(scripts_dir()) {
        tracing::warn!("failed to create scripts dir: {e}");
    }

    // Write default config if missing
    let cfg = config_path();
    if !cfg.exists() {
        let default_cfg = crate::config::default_config();
        if let Err(e) = crate::config::save_config(&cfg, &default_cfg) {
            tracing::warn!("failed to write default config: {e}");
        } else {
            tracing::info!("Created default config at {}", cfg.display());
        }
    }

    // Write default theme if missing
    let theme = home.join("themes/default.theme");
    if !theme.exists() {
        if let Err(e) = std::fs::write(&theme, DEFAULT_THEME) {
            tracing::warn!("failed to write default theme: {e}");
        } else {
            tracing::info!("Created default theme at {}", theme.display());
        }
    }
}

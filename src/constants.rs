pub const APP_NAME: &str = "rustirc";
#[allow(dead_code)]
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// WHOX field selector string.
/// Fields requested: t=token, c=channel, u=user, i=ip, h=host, s=server,
/// n=nick, f=flags, d=hopcount, l=idle, a=account, r=realname.
pub const WHOX_FIELDS: &str = "%tcuihsnfdlar";

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

#[allow(dead_code)]
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
    let _ = std::fs::create_dir_all(theme_dir());
    let _ = std::fs::create_dir_all(log_dir());
    let _ = std::fs::create_dir_all(scripts_dir());

    // Write default config if missing
    let cfg = config_path();
    if !cfg.exists() {
        let default_cfg = crate::config::default_config();
        let _ = crate::config::save_config(&cfg, &default_cfg);
        tracing::info!("Created default config at {}", cfg.display());
    }

    // Write default theme if missing
    let theme = home.join("themes/default.theme");
    if !theme.exists() {
        let _ = std::fs::write(&theme, DEFAULT_THEME);
        tracing::info!("Created default theme at {}", theme.display());
    }
}

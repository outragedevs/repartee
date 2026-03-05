pub const APP_NAME: &str = "rustirc";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

use std::path::PathBuf;

pub fn home_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(format!(".{}", APP_NAME))
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

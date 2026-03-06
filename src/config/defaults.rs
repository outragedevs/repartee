use super::AppConfig;

/// Returns the default configuration, matching kokoirc's defaults but using `APP_NAME`.
pub fn default_config() -> AppConfig {
    AppConfig::default()
}

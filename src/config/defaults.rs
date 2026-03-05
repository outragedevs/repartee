use std::collections::HashMap;

use crate::constants::APP_NAME;

use super::{
    AppConfig, DisplayConfig, GeneralConfig, ImagePreviewConfig, LoggingConfig, NickAlignment,
    PanelConfig, ScriptsConfig, SidepanelConfig, StatusbarConfig, StatusbarItem,
};

/// Returns the default configuration, matching kokoirc's defaults but using APP_NAME.
pub fn default_config() -> AppConfig {
    AppConfig {
        general: GeneralConfig {
            nick: APP_NAME.to_string(),
            username: APP_NAME.to_lowercase(),
            realname: format!("{APP_NAME} Client"),
            theme: "default".to_string(),
            timestamp_format: "%H:%M:%S".to_string(),
            flood_protection: true,
            ctcp_version: APP_NAME.to_string(),
        },
        display: DisplayConfig {
            nick_column_width: 8,
            nick_max_length: 8,
            nick_alignment: NickAlignment::Right,
            nick_truncation: true,
            show_timestamps: true,
            scrollback_lines: 2000,
        },
        sidepanel: SidepanelConfig {
            left: PanelConfig {
                width: 20,
                visible: true,
            },
            right: PanelConfig {
                width: 18,
                visible: true,
            },
        },
        statusbar: StatusbarConfig {
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
        },
        image_preview: ImagePreviewConfig {
            enabled: true,
            max_width: 0,
            max_height: 0,
            cache_max_mb: 100,
            cache_max_days: 7,
            fetch_timeout: 30,
            max_file_size: 10_485_760,
            protocol: "auto".to_string(),
            kitty_format: "rgba".to_string(),
        },
        servers: HashMap::new(),
        aliases: HashMap::new(),
        ignores: Vec::new(),
        scripts: ScriptsConfig {
            autoload: Vec::new(),
            debug: false,
        },
        logging: LoggingConfig {
            enabled: true,
            encrypt: false,
            retention_days: 0,
            exclude_types: Vec::new(),
        },
    }
}

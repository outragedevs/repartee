#[path = "settings_collection.rs"]
pub mod collection;

use serde::{Deserialize, Serialize};

pub const SECTIONS: [&str; 8] = [
    "Networks & Connections",
    "Appearance",
    "Messages",
    "Notifications",
    "History & Privacy",
    "Keyboard",
    "Extensions",
    "Advanced",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettingKind {
    Text,
    Number,
    Toggle,
    Select(Vec<String>),
    Json,
    Secret,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingField {
    pub path: String,
    pub label: String,
    pub section: usize,
    pub description: String,
    pub effect: String,
    pub kind: SettingKind,
    pub value: String,
    pub default_value: Option<String>,
    pub configured: bool,
}

impl SettingField {
    pub fn network(&self) -> Option<&str> {
        self.path
            .strip_prefix("servers.")?
            .rsplit_once('.')
            .map(|(id, _)| id)
    }

    pub fn is_collection(&self) -> bool {
        self.kind == SettingKind::Json
            || matches!(
                self.path.as_str(),
                "dcc.autochat_masks" | "general.flood_exemptions" | "spellcheck.languages"
            )
            || self.path.starts_with("servers.") && self.path.ends_with(".channels")
    }

    pub fn group(&self) -> &str {
        self.label.rsplit_once(": ").map_or_else(
            || match self.path.as_str() {
                "aliases" => "Commands",
                "ignores" => "Ignore rules",
                _ => "General",
            },
            |(group, _)| group,
        )
    }

    pub fn short_label(&self) -> &str {
        self.label
            .rsplit_once(": ")
            .map_or(self.label.as_str(), |(_, label)| label)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingChange {
    pub path: String,
    pub original: String,
    pub value: String,
}

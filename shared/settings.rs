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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingChange {
    pub path: String,
    pub original: String,
    pub value: String,
}

use crate::app::App;

pub type CommandHandler = fn(&mut App, &[String]);

pub struct CommandDef {
    pub handler: CommandHandler,
    pub description: &'static str,
    pub aliases: &'static [&'static str],
    pub category: CommandCategory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CommandCategory {
    Connection,
    Channel,
    Messaging,
    Configuration,
    Info,
    Other,
}

impl CommandCategory {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Connection => "Connection",
            Self::Channel => "Channel",
            Self::Messaging => "Messaging",
            Self::Configuration => "Configuration",
            Self::Info => "Info",
            Self::Other => "Other",
        }
    }
}

/// Category display order.
pub const CATEGORY_ORDER: &[CommandCategory] = &[
    CommandCategory::Connection,
    CommandCategory::Channel,
    CommandCategory::Messaging,
    CommandCategory::Configuration,
    CommandCategory::Info,
    CommandCategory::Other,
];

// Color constants matching kokoirc's Nightfall theme palette
pub const C_HEADER: &str = "%Z7aa2f7"; // accent — headers, dividers
pub const C_CMD: &str = "%Zc0caf5"; // light blue — command names, values, syntax
pub const C_DIM: &str = "%Z565f89"; // blue-gray — descriptions, aliases, secondary
pub const C_TEXT: &str = "%Za9b1d6"; // gold — main description text
pub const C_OK: &str = "%Z9ece6a"; // green — success messages
pub const C_ERR: &str = "%Zf7768e"; // red — error messages
pub const C_RST: &str = "%N"; // reset

pub fn divider(title: &str) -> String {
    let pad = 45usize.saturating_sub(title.len() + 7);
    format!("{C_HEADER}───── {title} {}{C_RST}", "─".repeat(pad))
}

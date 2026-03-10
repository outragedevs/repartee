use std::sync::LazyLock;

use super::types::{CommandDef, CommandCategory};
use super::handlers_ui::{
    cmd_quit, cmd_help, cmd_clear, cmd_close, cmd_alias, cmd_unalias, cmd_items,
};
use super::handlers_irc::{
    cmd_connect, cmd_disconnect, cmd_join, cmd_part, cmd_cycle, cmd_topic, cmd_kick, cmd_invite,
    cmd_mode, cmd_op, cmd_deop, cmd_voice, cmd_devoice, cmd_ban, cmd_unban, cmd_kickban,
    cmd_except, cmd_unexcept, cmd_invex, cmd_uninvex, cmd_reop, cmd_unreop,
    cmd_names, cmd_msg, cmd_query, cmd_me, cmd_notice, cmd_nick, cmd_whois, cmd_wii, cmd_version,
    cmd_quote, cmd_away, cmd_list, cmd_who, cmd_whowas,
    cmd_info, cmd_admin, cmd_lusers, cmd_time, cmd_links,
};
use super::handlers_admin::{
    cmd_reload, cmd_ignore, cmd_unignore, cmd_server, cmd_autoconnect,
    cmd_oper, cmd_kill, cmd_wallops, cmd_stats, cmd_log,
    cmd_preview, cmd_image, cmd_script,
};

static COMMANDS: LazyLock<Vec<(&'static str, CommandDef)>> = LazyLock::new(|| vec![
        // === Connection ===
        ("connect",    CommandDef { handler: cmd_connect,    description: "Connect to a server",              aliases: &["c"],          category: CommandCategory::Connection }),
        ("disconnect", CommandDef { handler: cmd_disconnect, description: "Disconnect from current server",   aliases: &[],             category: CommandCategory::Connection }),
        ("quit",       CommandDef { handler: cmd_quit,       description: "Quit the client",                  aliases: &["exit"],       category: CommandCategory::Connection }),
        ("server",     CommandDef { handler: cmd_server,     description: "Manage server configurations",     aliases: &[],             category: CommandCategory::Connection }),
        // === Channel ===
        ("join",       CommandDef { handler: cmd_join,       description: "Join a channel",                   aliases: &["j"],          category: CommandCategory::Channel }),
        ("part",       CommandDef { handler: cmd_part,       description: "Leave a channel",                  aliases: &["leave"],      category: CommandCategory::Channel }),
        ("cycle",      CommandDef { handler: cmd_cycle,      description: "Part and rejoin a channel",        aliases: &["rejoin"],     category: CommandCategory::Channel }),
        ("topic",      CommandDef { handler: cmd_topic,      description: "View or set channel topic",        aliases: &["t"],          category: CommandCategory::Channel }),
        ("kick",       CommandDef { handler: cmd_kick,       description: "Kick a user from channel",         aliases: &[],             category: CommandCategory::Channel }),
        ("invite",     CommandDef { handler: cmd_invite,     description: "Invite a user to a channel",       aliases: &[],             category: CommandCategory::Channel }),
        ("mode",       CommandDef { handler: cmd_mode,       description: "Query or set modes",               aliases: &[],             category: CommandCategory::Channel }),
        ("op",         CommandDef { handler: cmd_op,         description: "Give operator status",             aliases: &[],             category: CommandCategory::Channel }),
        ("deop",       CommandDef { handler: cmd_deop,       description: "Remove operator status",           aliases: &[],             category: CommandCategory::Channel }),
        ("voice",      CommandDef { handler: cmd_voice,      description: "Give voice status",                aliases: &["v"],          category: CommandCategory::Channel }),
        ("devoice",    CommandDef { handler: cmd_devoice,    description: "Remove voice status",              aliases: &[],             category: CommandCategory::Channel }),
        ("ban",        CommandDef { handler: cmd_ban,        description: "Add ban or show ban list",         aliases: &["b"],          category: CommandCategory::Channel }),
        ("unban",      CommandDef { handler: cmd_unban,      description: "Remove ban(s) by number, mask, or wildcard (* = all)", aliases: &[], category: CommandCategory::Channel }),
        ("kb",         CommandDef { handler: cmd_kickban,    description: "Kickban a user (*!*ident@host)",   aliases: &["kickban"],    category: CommandCategory::Channel }),
        ("except",     CommandDef { handler: cmd_except,     description: "Add exception or show list",       aliases: &[],             category: CommandCategory::Channel }),
        ("unexcept",   CommandDef { handler: cmd_unexcept,   description: "Remove an exception",              aliases: &[],             category: CommandCategory::Channel }),
        ("invex",      CommandDef { handler: cmd_invex,      description: "Add invite exception or show list",aliases: &[],             category: CommandCategory::Channel }),
        ("uninvex",    CommandDef { handler: cmd_uninvex,    description: "Remove an invite exception",       aliases: &[],             category: CommandCategory::Channel }),
        ("reop",       CommandDef { handler: cmd_reop,       description: "Add reop entry or show list",      aliases: &[],             category: CommandCategory::Channel }),
        ("unreop",     CommandDef { handler: cmd_unreop,     description: "Remove a reop entry",              aliases: &[],             category: CommandCategory::Channel }),
        ("names",      CommandDef { handler: cmd_names,      description: "Request NAMES list",               aliases: &[],             category: CommandCategory::Channel }),
        // === Messaging ===
        ("msg",        CommandDef { handler: cmd_msg,        description: "Send a private message",           aliases: &["m"],          category: CommandCategory::Messaging }),
        ("query",      CommandDef { handler: cmd_query,      description: "Open a query with a user",         aliases: &["q"],          category: CommandCategory::Messaging }),
        ("me",         CommandDef { handler: cmd_me,         description: "Send a CTCP ACTION",               aliases: &["action"],     category: CommandCategory::Messaging }),
        ("notice",     CommandDef { handler: cmd_notice,     description: "Send a notice",                    aliases: &[],             category: CommandCategory::Messaging }),
        ("nick",       CommandDef { handler: cmd_nick,       description: "Change nickname",                  aliases: &[],             category: CommandCategory::Messaging }),
        ("away",       CommandDef { handler: cmd_away,       description: "Set or clear away status",         aliases: &[],             category: CommandCategory::Messaging }),
        // === Info ===
        ("help",       CommandDef { handler: cmd_help,       description: "Show help for commands",           aliases: &["?"],          category: CommandCategory::Info }),
        ("whois",      CommandDef { handler: cmd_whois,      description: "WHOIS query on a user",            aliases: &["wi"],         category: CommandCategory::Info }),
        ("wii",        CommandDef { handler: cmd_wii,        description: "WHOIS with idle time",             aliases: &[],             category: CommandCategory::Info }),
        ("version",    CommandDef { handler: cmd_version,    description: "CTCP VERSION or server version",   aliases: &["ver"],        category: CommandCategory::Info }),
        ("list",       CommandDef { handler: cmd_list,       description: "List channels on server",          aliases: &[],             category: CommandCategory::Info }),
        ("who",        CommandDef { handler: cmd_who,        description: "WHO query",                        aliases: &[],             category: CommandCategory::Info }),
        ("whowas",     CommandDef { handler: cmd_whowas,     description: "WHOWAS query on a nick",           aliases: &[],             category: CommandCategory::Info }),
        ("info",       CommandDef { handler: cmd_info,       description: "Request server info",              aliases: &[],             category: CommandCategory::Info }),
        ("admin",      CommandDef { handler: cmd_admin,      description: "Request server admin info",        aliases: &[],             category: CommandCategory::Info }),
        ("lusers",     CommandDef { handler: cmd_lusers,     description: "Request server user statistics",   aliases: &[],             category: CommandCategory::Info }),
        ("time",       CommandDef { handler: cmd_time,       description: "Request server time",              aliases: &[],             category: CommandCategory::Info }),
        ("links",      CommandDef { handler: cmd_links,      description: "List server links",                aliases: &[],             category: CommandCategory::Info }),
        // === Configuration ===
        ("set",        CommandDef { handler: super::settings::cmd_set, description: "View or change settings",aliases: &[],             category: CommandCategory::Configuration }),
        ("reload",     CommandDef { handler: cmd_reload,     description: "Reload theme and config",          aliases: &[],             category: CommandCategory::Configuration }),
        ("ignore",     CommandDef { handler: cmd_ignore,     description: "Add or list ignore rules",         aliases: &[],             category: CommandCategory::Configuration }),
        ("unignore",   CommandDef { handler: cmd_unignore,   description: "Remove an ignore rule",            aliases: &[],             category: CommandCategory::Configuration }),
        ("alias",      CommandDef { handler: cmd_alias,      description: "Define or list command aliases",   aliases: &[],             category: CommandCategory::Configuration }),
        ("unalias",    CommandDef { handler: cmd_unalias,    description: "Remove a command alias",           aliases: &[],             category: CommandCategory::Configuration }),
        ("autoconnect",CommandDef { handler: cmd_autoconnect,description: "Toggle server autoconnect",        aliases: &[],             category: CommandCategory::Configuration }),
        ("items",      CommandDef { handler: cmd_items,      description: "Manage statusbar items",           aliases: &[],             category: CommandCategory::Configuration }),
        ("log",        CommandDef { handler: cmd_log,        description: "Log status and search",            aliases: &[],             category: CommandCategory::Configuration }),
        ("script",     CommandDef { handler: cmd_script,     description: "Manage Lua scripts",               aliases: &[],             category: CommandCategory::Configuration }),
        // === Other ===
        ("oper",       CommandDef { handler: cmd_oper,       description: "Authenticate as IRC operator",     aliases: &[],             category: CommandCategory::Other }),
        ("kill",       CommandDef { handler: cmd_kill,       description: "Disconnect a user (oper only)",    aliases: &[],             category: CommandCategory::Other }),
        ("wallops",    CommandDef { handler: cmd_wallops,    description: "Send message to all opers",        aliases: &[],             category: CommandCategory::Other }),
        ("stats",      CommandDef { handler: cmd_stats,      description: "Request server statistics",        aliases: &[],             category: CommandCategory::Other }),
        ("clear",      CommandDef { handler: cmd_clear,      description: "Clear active buffer",              aliases: &[],             category: CommandCategory::Other }),
        ("close",      CommandDef { handler: cmd_close,      description: "Close active buffer",              aliases: &["wc"],         category: CommandCategory::Other }),
        ("quote",      CommandDef { handler: cmd_quote,      description: "Send a raw IRC command",           aliases: &["raw"],        category: CommandCategory::Other }),
        // === Media ===
        ("preview",    CommandDef { handler: cmd_preview,    description: "Preview an image URL",             aliases: &[],             category: CommandCategory::Other }),
        ("image",      CommandDef { handler: cmd_image,      description: "Image cache management",           aliases: &["img"],        category: CommandCategory::Other }),
    ]);

/// Command table — handler + short description + aliases + category.
/// Detailed help lives in docs/commands/*.md, accessed via the docs module.
pub fn get_commands() -> &'static [(&'static str, CommandDef)] {
    &COMMANDS
}

/// Get all command names including aliases.
pub fn get_command_names() -> &'static [&'static str] {
    static NAMES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
        let commands = get_commands();
        let mut names: Vec<&'static str> = Vec::new();
        for &(name, ref def) in commands {
            names.push(name);
            for alias in def.aliases {
                names.push(alias);
            }
        }
        names.sort_unstable();
        names.dedup();
        names
    });
    &NAMES
}

/// Resolve an alias to its canonical command name.
#[allow(dead_code)]
pub fn resolve_alias(name: &str) -> Option<&'static str> {
    let commands = get_commands();
    for &(cmd_name, ref def) in commands {
        if cmd_name == name {
            return Some(cmd_name);
        }
        for alias in def.aliases {
            if *alias == name {
                return Some(cmd_name);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_commands_have_description() {
        for &(name, ref def) in get_commands() {
            assert!(!def.description.is_empty(), "/{name} missing description");
        }
    }

    #[test]
    fn command_names_includes_aliases() {
        let names = get_command_names();
        assert!(names.contains(&"connect"));
        assert!(names.contains(&"c"));
        assert!(names.contains(&"j"));
        assert!(names.contains(&"?"));
        assert!(names.contains(&"exit"));
    }

    #[test]
    fn resolve_alias_works() {
        assert_eq!(resolve_alias("c"), Some("connect"));
        assert_eq!(resolve_alias("j"), Some("join"));
        assert_eq!(resolve_alias("?"), Some("help"));
        assert_eq!(resolve_alias("connect"), Some("connect"));
        assert_eq!(resolve_alias("nonexistent"), None);
    }

    #[test]
    fn categories_cover_all_commands() {
        for &(name, ref def) in get_commands() {
            assert!(
                !def.category.label().is_empty(),
                "/{name} has empty category label"
            );
        }
    }

    #[test]
    fn server_query_commands_registered() {
        let commands = get_commands();
        let names: Vec<&str> = commands.iter().map(|(name, _)| *name).collect();
        assert!(names.contains(&"info"), "/info command not registered");
        assert!(names.contains(&"admin"), "/admin command not registered");
        assert!(names.contains(&"lusers"), "/lusers command not registered");
        assert!(names.contains(&"time"), "/time command not registered");
        assert!(names.contains(&"stats"), "/stats command not registered");
        assert!(names.contains(&"links"), "/links command not registered");
    }

    #[test]
    fn server_query_commands_in_correct_category() {
        let commands = get_commands();
        for &(name, ref def) in commands {
            match name {
                "info" | "admin" | "lusers" | "time" | "links" => {
                    assert!(
                        matches!(def.category, CommandCategory::Info),
                        "/{name} should be in Info category, got {:?}",
                        def.category
                    );
                }
                _ => {}
            }
        }
    }

    #[test]
    fn no_duplicate_aliases() {
        let mut all_names: Vec<&str> = Vec::new();
        for &(name, ref def) in get_commands() {
            assert!(!all_names.contains(&name), "Duplicate command name: {name}");
            all_names.push(name);
            for alias in def.aliases {
                assert!(
                    !all_names.contains(alias),
                    "Duplicate alias: {alias}"
                );
                all_names.push(alias);
            }
        }
    }
}

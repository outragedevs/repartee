//! Command documentation parser — reads docs/commands/*.md into structured help.
//! Single source of truth for /help output and subcommand tab completion.

use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Debug, Clone)]
pub struct CommandHelp {
    pub description: String,
    pub syntax: String,
    pub body: String,
    pub subcommands: Vec<SubcommandHelp>,
    pub examples: Vec<String>,
    pub see_also: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SubcommandHelp {
    pub name: String,
    pub description: String,
    pub syntax: String,
}

static HELP_CACHE: LazyLock<HashMap<String, CommandHelp>> = LazyLock::new(load_all_docs);

/// Get parsed help for a command by name.
pub fn help(name: &str) -> Option<&'static CommandHelp> {
    HELP_CACHE.get(name)
}

/// Get subcommand names for a command (for tab completion).
/// Returns empty slice if the command has no subcommands.
pub fn get_subcommand_names(cmd: &str) -> Vec<&'static str> {
    HELP_CACHE
        .get(cmd)
        .map(|h| h.subcommands.iter().map(|s| s.name.as_str()).collect())
        .unwrap_or_default()
}

/// Load all command docs from the embedded directory.
fn load_all_docs() -> HashMap<String, CommandHelp> {
    let mut map = HashMap::new();
    let docs_dir = find_docs_dir();

    if let Ok(entries) = std::fs::read_dir(&docs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "md")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                && let Ok(content) = std::fs::read_to_string(&path)
            {
                map.insert(stem.to_string(), parse_doc(&content));
            }
        }
    }
    map
}

/// Find the docs/commands directory relative to the binary or manifest.
fn find_docs_dir() -> std::path::PathBuf {
    // Try relative to CARGO_MANIFEST_DIR (dev builds)
    let manifest_dir = option_env!("CARGO_MANIFEST_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let from_manifest = manifest_dir.join("docs/commands");
    if from_manifest.is_dir() {
        return from_manifest;
    }

    // Try relative to executable
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let from_exe = parent.join("docs/commands");
        if from_exe.is_dir() {
            return from_exe;
        }
        if let Some(grandparent) = parent.parent() {
            let from_grandparent = grandparent.join("docs/commands");
            if from_grandparent.is_dir() {
                return from_grandparent;
            }
            // Two levels up (target/release -> project root)
            if let Some(great_grandparent) = grandparent.parent() {
                let from_great_grandparent = great_grandparent.join("docs/commands");
                if from_great_grandparent.is_dir() {
                    return from_great_grandparent;
                }
            }
        }
    }

    // Try current directory
    let cwd = std::path::PathBuf::from("docs/commands");
    if cwd.is_dir() {
        return cwd;
    }

    from_manifest
}

/// Parse a single command doc from markdown content.
fn parse_doc(raw: &str) -> CommandHelp {
    let (meta, body) = parse_frontmatter(raw);
    let sections = split_sections(&body);

    let description = meta.get("description").cloned().unwrap_or_default();
    let syntax = sections
        .get("syntax")
        .map(|s| extract_indented(s))
        .unwrap_or_default();
    let body_text = sections.get("description").cloned().unwrap_or_default();

    let subcommands = sections
        .get("subcommands")
        .map(|s| parse_subcommands(s))
        .unwrap_or_default();

    let examples = sections
        .get("examples")
        .map(|s| extract_indented_lines(s))
        .unwrap_or_default();

    let see_also = sections
        .get("see also")
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default();

    CommandHelp {
        description,
        syntax,
        body: body_text,
        subcommands,
        examples,
        see_also,
    }
}

fn parse_frontmatter(raw: &str) -> (HashMap<String, String>, String) {
    let mut meta = HashMap::new();
    if !raw.starts_with("---") {
        return (meta, raw.to_string());
    }
    let Some(end) = raw[3..].find("---") else {
        return (meta, raw.to_string());
    };
    let block = &raw[3..3 + end];
    for line in block.lines() {
        if let Some(idx) = line.find(':') {
            let key = line[..idx].trim().to_string();
            let val = line[idx + 1..].trim().to_string();
            if !key.is_empty() {
                meta.insert(key, val);
            }
        }
    }
    let body = raw[3 + end + 3..].trim().to_string();
    (meta, body)
}

fn split_sections(body: &str) -> HashMap<String, String> {
    let mut sections = HashMap::new();
    // Match ## headings — handle both start-of-body and mid-body positions
    let mut current_heading: Option<String> = None;
    let mut current_content = String::new();

    for line in body.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            // Save previous section
            if let Some(h) = current_heading.take() {
                sections.insert(h, trim_newlines(&current_content));
            }
            current_heading = Some(heading.trim().to_lowercase());
            current_content = String::new();
        } else if line.starts_with("# ") && current_heading.is_none() {
            // Skip the title line (# /command)
        } else if current_heading.is_some() {
            if !current_content.is_empty() {
                current_content.push('\n');
            }
            current_content.push_str(line);
        }
    }
    // Save last section
    if let Some(h) = current_heading {
        sections.insert(h, trim_newlines(&current_content));
    }
    sections
}

/// Trim leading/trailing newlines but preserve inner indentation.
fn trim_newlines(s: &str) -> String {
    s.trim_matches('\n').to_string()
}

fn extract_indented(text: &str) -> String {
    text.lines()
        .filter(|l| l.starts_with("    ") || l.starts_with('\t'))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_indented_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| l.starts_with("    ") || l.starts_with('\t'))
        .map(|l| l.trim().to_string())
        .collect()
}

fn parse_subcommands(text: &str) -> Vec<SubcommandHelp> {
    let mut subs = Vec::new();
    // Normalize: ensure we can always split on "\n### "
    let normalized = if text.starts_with("### ") {
        format!("\n{text}")
    } else {
        text.to_string()
    };
    let parts: Vec<&str> = normalized.split("\n### ").collect();
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            continue;
        }
        let trimmed = part.trim();
        let (name, body) = trimmed.find('\n').map_or((trimmed, ""), |idx| {
            (trimmed[..idx].trim(), trimmed[idx + 1..].trim())
        });
        let syntax = extract_indented(body);
        // First non-indented paragraph is description
        let description = body
            .lines()
            .take_while(|l| !l.starts_with("    ") && !l.starts_with('\t'))
            .collect::<Vec<_>>()
            .join(" ");

        subs.push(SubcommandHelp {
            name: name.to_string(),
            description,
            syntax,
        });
    }
    subs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_doc() {
        let doc = r"---
category: Channel
description: Join a channel
---

# /join

## Syntax

    /join <channel> [key]

## Description

Join an IRC channel. A `#` prefix is auto-added if missing.

## Examples

    /join #linux
    /join linux

## See Also

/part, /close";

        let help = parse_doc(doc);
        assert_eq!(help.description, "Join a channel");
        assert_eq!(help.syntax, "/join <channel> [key]");
        assert!(help.body.contains("auto-added"));
        assert_eq!(help.examples.len(), 2);
        assert_eq!(help.see_also, vec!["/part", "/close"]);
    }

    #[test]
    fn parse_doc_with_subcommands() {
        let doc = r"---
category: Connection
description: Manage servers
---

# /server

## Syntax

    /server [list|add|remove]

## Description

Manage IRC server configurations.

## Subcommands

### list

List all configured servers.

    /server list

### add

Add a new server.

    /server add <id> <address>

## Examples

    /server list
    /server add libera irc.libera.chat";

        let help = parse_doc(doc);
        assert_eq!(help.subcommands.len(), 2);
        assert_eq!(help.subcommands[0].name, "list");
        assert_eq!(help.subcommands[1].name, "add");
        assert!(help.subcommands[1].syntax.contains("/server add"));
    }

    #[test]
    fn load_docs_dir_exists() {
        let docs = load_all_docs();
        // Should find at least a few docs
        assert!(!docs.is_empty(), "No command docs found");
        assert!(docs.contains_key("join"), "Missing join doc");
        assert!(docs.contains_key("quit"), "Missing quit doc");
    }

    #[test]
    fn help_function_works() {
        // Force LazyLock init
        let join = help("join");
        assert!(join.is_some());
        assert_eq!(join.unwrap().description, "Join a channel");
    }

    #[test]
    fn get_subcommand_names_returns_names() {
        let names = get_subcommand_names("server");
        assert!(
            names.contains(&"list"),
            "server should have 'list' subcommand"
        );
        assert!(
            names.contains(&"add"),
            "server should have 'add' subcommand"
        );
    }

    #[test]
    fn get_subcommand_names_empty_for_no_subcommands() {
        let names = get_subcommand_names("join");
        assert!(names.is_empty(), "join should have no subcommands");
    }
}

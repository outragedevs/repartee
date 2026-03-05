pub struct ParsedCommand {
    pub name: String,
    pub args: Vec<String>,
}

/// Greedy commands: the last arg consumes the rest of the line.
/// - me, quit, close: single arg (entire rest)
/// - msg, notice, topic, kick, kb, disconnect, set, alias: two args (first word, rest)
const GREEDY_COMMANDS: &[&str] = &[
    "msg", "notice", "me", "quit", "topic", "kick", "kb", "close", "disconnect", "set", "alias",
];

pub fn parse_command(input: &str) -> Option<ParsedCommand> {
    if !input.starts_with('/') {
        return None;
    }
    let trimmed = &input[1..];
    let (command, rest) = match trimmed.find(' ') {
        Some(idx) => (trimmed[..idx].to_lowercase(), trimmed[idx + 1..].trim()),
        None => {
            return Some(ParsedCommand {
                name: trimmed.to_lowercase(),
                args: vec![],
            });
        }
    };

    if GREEDY_COMMANDS.contains(&command.as_str()) {
        if matches!(command.as_str(), "me" | "quit" | "close") {
            return Some(ParsedCommand {
                name: command,
                args: vec![rest.to_string()],
            });
        }
        return match rest.find(' ') {
            Some(idx) => Some(ParsedCommand {
                name: command,
                args: vec![rest[..idx].to_string(), rest[idx + 1..].to_string()],
            }),
            None => Some(ParsedCommand {
                name: command,
                args: vec![rest.to_string()],
            }),
        };
    }

    let args: Vec<String> = rest.split_whitespace().map(String::from).collect();
    Some(ParsedCommand {
        name: command,
        args,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quit_no_args() {
        let cmd = parse_command("/quit").unwrap();
        assert_eq!(cmd.name, "quit");
        assert!(cmd.args.is_empty());
    }

    #[test]
    fn msg_greedy_two_args() {
        let cmd = parse_command("/msg nick hello world").unwrap();
        assert_eq!(cmd.name, "msg");
        assert_eq!(cmd.args, vec!["nick", "hello world"]);
    }

    #[test]
    fn me_greedy_single_arg() {
        let cmd = parse_command("/me does a thing").unwrap();
        assert_eq!(cmd.name, "me");
        assert_eq!(cmd.args, vec!["does a thing"]);
    }

    #[test]
    fn join_single_channel() {
        let cmd = parse_command("/join #channel").unwrap();
        assert_eq!(cmd.name, "join");
        assert_eq!(cmd.args, vec!["#channel"]);
    }

    #[test]
    fn join_multiple_channels() {
        let cmd = parse_command("/join #a #b #c").unwrap();
        assert_eq!(cmd.name, "join");
        assert_eq!(cmd.args, vec!["#a", "#b", "#c"]);
    }

    #[test]
    fn non_command_returns_none() {
        assert!(parse_command("hello world").is_none());
        assert!(parse_command("").is_none());
    }

    #[test]
    fn case_insensitive() {
        let cmd = parse_command("/QUIT").unwrap();
        assert_eq!(cmd.name, "quit");
    }
}

pub struct ParsedCommand {
    pub name: String,
    pub args: Vec<String>,
}

/// Greedy commands: the last arg consumes the rest of the line.
/// - me, quit, quote: single arg (entire rest)
/// - msg, query, notice, topic, kb, disconnect, set, alias: two args (first word, rest)
///
/// `kick` is intentionally NOT greedy: it accepts multiple nicks plus an
/// optional `:reason` (everything from the first `:`-prefixed token onward).
/// The handler reconstructs the reason from the tokenised args.
///
/// `close` is likewise NOT greedy. It used to be, back when its whole
/// argument was a free-text part reason — but it now takes an optional window
/// number or range and a `-YES` flag ahead of that reason, and greedy parsing
/// handed the handler one blob it could not split (`/close 22 see you` closed
/// the active window instead of window 22). Its `/wc` alias was always
/// tokenised, since greediness is decided by the name as typed and runs
/// before alias resolution; dropping `close` here is what makes the two
/// agree. `cmd_close` rejoins the tail into the reason.
const GREEDY_COMMANDS: &[&str] = &[
    "msg",
    "query",
    "notice",
    "me",
    "quit",
    "topic",
    "kb",
    "disconnect",
    "set",
    "alias",
    "quote",
    "shell",
    "sh",
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
        if matches!(command.as_str(), "me" | "quit" | "quote") {
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
    fn close_is_tokenised_like_its_wc_alias() {
        // Greediness is decided by the name as typed, before aliases resolve,
        // so putting `close` back on GREEDY_COMMANDS would silently break
        // `/close <number> [reason]` while leaving `/wc` working.
        for line in ["/close 22 see you", "/wc 22 see you"] {
            let cmd = parse_command(line).unwrap();
            assert_eq!(cmd.args, vec!["22", "see", "you"], "{line}");
        }
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

    #[test]
    fn connect_with_flags() {
        let cmd = parse_command("/connect irc.example.com 6697 -tls").unwrap();
        assert_eq!(cmd.name, "connect");
        assert_eq!(cmd.args, vec!["irc.example.com", "6697", "-tls"]);
    }

    #[test]
    fn connect_with_bind() {
        let cmd = parse_command("/connect mynet -bind=192.168.1.1").unwrap();
        assert_eq!(cmd.name, "connect");
        assert_eq!(cmd.args, vec!["mynet", "-bind=192.168.1.1"]);
    }

    #[test]
    fn connect_address_port_colon() {
        let cmd = parse_command("/connect irc.example.com:6697 -tls").unwrap();
        assert_eq!(cmd.name, "connect");
        assert_eq!(cmd.args, vec!["irc.example.com:6697", "-tls"]);
    }
}

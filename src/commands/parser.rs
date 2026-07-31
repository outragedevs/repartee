pub struct ParsedCommand {
    pub name: String,
    pub args: Vec<String>,
}

/// Greedy commands: the last arg consumes the rest of the line.
/// - me, quit, quote: single arg (entire rest)
/// - msg, query, notice, topic, disconnect, set, alias: two args (first word, rest)
///
/// `kick` is intentionally NOT greedy: it accepts multiple nicks plus an
/// optional `:reason` (everything from the first `:`-prefixed token onward).
/// The handler reconstructs the reason from the tokenised args.
///
/// `kb` is NOT greedy for the same reason. It takes `[#channel] <nick>
/// [reason]` — three parts, where two-arg greedy parsing only ever yields
/// two: `/kb #chan nick be nice` collapsed into `["#chan", "nick be nice"]`,
/// so `cmd_kickban` took the entire tail as the nick and banned
/// `nick be nice!*@*`. It joins the reason from the tokenised args itself.
///
/// `close` is likewise NOT greedy. It used to be, back when its whole
/// argument was a free-text part reason — but it now takes an optional window
/// number or range and a `-YES` flag ahead of that reason, and greedy parsing
/// handed the handler one blob it could not split (`/close 22 see you` closed
/// the active window instead of window 22). `cmd_close` rejoins the tail into
/// the reason.
///
/// Entries are CANONICAL command names. The typed name is resolved through
/// the registry first, so a built-in alias is parsed exactly like the command
/// it stands for — `/m` splits like `/msg`, `/t` like `/topic`. Matching the
/// typed name instead used to silently mangle every aliased greedy command:
/// `/m nick hello world` tokenised into three args, and `cmd_msg` reads only
/// `args[1]`, so the message went out as "hello" with "world" dropped.
const GREEDY_COMMANDS: &[&str] = &[
    "msg",
    "query",
    "notice",
    "me",
    "quit",
    "topic",
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

    // Greediness is a property of the command, not of the spelling used to
    // reach it, so resolve built-in aliases before deciding.
    let canonical = super::registry::resolve_alias(&command).unwrap_or(command.as_str());

    if GREEDY_COMMANDS.contains(&canonical) {
        if matches!(canonical, "me" | "quit" | "quote") {
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

    /// Every built-in alias must parse its arguments exactly like the command
    /// it stands for. Driven off the registry rather than a hand-written list,
    /// so a newly added alias is covered the moment it lands.
    #[test]
    fn aliases_parse_like_their_canonical_command() {
        let tail = "one two three four";
        for &(name, ref def) in crate::commands::registry::get_commands() {
            let canonical = parse_command(&format!("/{name} {tail}")).unwrap();
            for alias in def.aliases {
                let aliased = parse_command(&format!("/{alias} {tail}")).unwrap();
                assert_eq!(
                    aliased.args, canonical.args,
                    "/{alias} must split like /{name}"
                );
                // The typed name still reaches dispatch unchanged — only the
                // argument split is canonicalised.
                assert_eq!(aliased.name, *alias);
            }
        }
    }

    #[test]
    fn greedy_aliases_keep_the_tail_intact() {
        // /m used to tokenise into ["nick","hello","world"], and cmd_msg reads
        // args[1] only — the message went out as "hello".
        let cmd = parse_command("/m nick hello world").unwrap();
        assert_eq!(cmd.args, vec!["nick", "hello world"]);

        let cmd = parse_command("/t #chan a new topic").unwrap();
        assert_eq!(cmd.args, vec!["#chan", "a new topic"]);

        let cmd = parse_command("/action waves at everyone").unwrap();
        assert_eq!(cmd.args, vec!["waves at everyone"]);

        let cmd = parse_command("/raw PRIVMSG #chan :hello there").unwrap();
        assert_eq!(cmd.args, vec!["PRIVMSG #chan :hello there"]);
    }

    #[test]
    fn kickban_keeps_channel_nick_and_reason_separable() {
        // cmd_kickban reads `[#channel] <nick> [reason]` and joins the reason
        // from the tail itself, exactly like /kick. Two-arg greedy parsing
        // gave it "nick be nice" as one token, so it banned that whole string
        // as the nick.
        for line in ["/kb #chan nick be nice", "/kickban #chan nick be nice"] {
            let cmd = parse_command(line).unwrap();
            assert_eq!(cmd.args, vec!["#chan", "nick", "be", "nice"], "{line}");
        }
    }

    #[test]
    fn non_greedy_aliases_are_unaffected() {
        let cmd = parse_command("/j #a #b #c").unwrap();
        assert_eq!(cmd.args, vec!["#a", "#b", "#c"]);
    }

    #[test]
    fn close_is_tokenised_like_its_wc_alias() {
        // `close` takes a window selector ahead of its reason, so it must stay
        // off GREEDY_COMMANDS — putting it back would break both spellings.
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

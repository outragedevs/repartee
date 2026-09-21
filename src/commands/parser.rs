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
    "bouncer",
    "quote",
    "setname",
    "shell",
    "sh",
];

pub fn parse_command(input: &str) -> Option<ParsedCommand> {
    if !input.starts_with('/') {
        return None;
    }
    let trimmed = &input[1..];
    let (command, rest) = match trimmed.find(' ') {
        Some(idx) => (trimmed[..idx].to_lowercase(), &trimmed[idx + 1..]),
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
    let rest = if canonical == "shell" {
        rest.trim_start()
    } else {
        rest.trim()
    };

    if canonical == "items" {
        let head_count = match rest.split_whitespace().next() {
            Some("format") => 2,
            Some("separator") => 1,
            _ => 0,
        };
        if head_count > 0 {
            let mut tail = rest;
            let mut args = Vec::new();
            for _ in 0..head_count {
                if let Some(index) = tail.find(char::is_whitespace) {
                    args.push(tail[..index].to_string());
                    tail = tail[index..].trim_start();
                } else {
                    if !tail.is_empty() { args.push(tail.to_string()); }
                    tail = "";
                    break;
                }
            }
            if !tail.is_empty() { args.push(tail.to_string()); }
            return Some(ParsedCommand { name: command, args });
        }
    }

    if canonical == "upload" {
        let args = shell_words::split(&rest.replace('#', "\0"))
            .map(|words| words.into_iter().map(|word| word.replace('\0', "#")).collect())
            .unwrap_or_default();
        return Some(ParsedCommand { name: command, args });
    }

    if GREEDY_COMMANDS.contains(&canonical) {
        if matches!(canonical, "me" | "quit" | "quote" | "setname") {
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
    fn shell_command_tail_preserves_quoting_and_trailing_escaped_space() {
        for name in ["shell", "sh"] {
            let command = parse_command(&format!("/{name} cmd echo 'two words' tail\\ ")).unwrap();
            assert_eq!(command.args, ["cmd", "echo 'two words' tail\\ "]);
            let direct = parse_command(&format!("/{name} '/path with spaces/tool' 'two words'")).unwrap();
            assert_eq!(direct.args.join(" "), "'/path with spaces/tool' 'two words'");
        }
    }

    #[test]
    fn upload_paths_preserve_quoted_spaces_and_literal_shell_characters() {
        let command = parse_command(r#"/upload "/tmp/my document.pdf" application/pdf"#).unwrap();
        assert_eq!(command.args, ["/tmp/my document.pdf", "application/pdf"]);
        let command = parse_command(r"/upload /tmp/escaped\ space.txt text/plain").unwrap();
        assert_eq!(command.args, ["/tmp/escaped space.txt", "text/plain"]);
        let command = parse_command(r"/upload '/tmp/#file $HOME ; name.txt'").unwrap();
        assert_eq!(command.args, ["/tmp/#file $HOME ; name.txt"]);
        assert!(parse_command("/upload \"unterminated").unwrap().args.is_empty());
    }

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

#[cfg(test)]
mod items_value_tests {
    #[test]
    fn item_text_tails_preserve_spaces_and_decode_quotes() {
        for command in ["/items format time", "/items separator"] {
            for (value, expected) in [
                ("one  two", "one  two"),
                ("\"  %H:%M  text  \"", "  %H:%M  text  "),
                ("' | '", " | "),
                ("\"\"", ""),
            ] {
                let parsed = super::parse_command(&format!("{command} {value}")).unwrap();
                let decoded = crate::commands::settings::decode_setting_value(parsed.args.last().unwrap()).unwrap();
                assert_eq!(decoded, expected);
            }
            let parsed = super::parse_command(&format!("{command} \"unfinished")).unwrap();
            assert!(crate::commands::settings::decode_setting_value(parsed.args.last().unwrap()).is_err());
        }
        assert_eq!(super::parse_command("/items move time 2").unwrap().args, ["move", "time", "2"]);
        assert_eq!(super::parse_command("/items format time").unwrap().args, ["format", "time"]);
        assert_eq!(super::parse_command("/items separator").unwrap().args, ["separator"]);
    }
}

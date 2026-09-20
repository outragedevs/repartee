use std::collections::BTreeMap;

use irc::proto::Command;

pub struct Mutation {
    pub operation: &'static str,
    pub network_id: Option<String>,
    pub command: Command,
}

pub fn parse(
    action: &str,
    tail: &str,
    secret: impl Fn(&str) -> Option<String>,
) -> Result<Mutation, String> {
    if tail.contains('\0') {
        return Err("Invalid command text".into());
    }
    let words = shell_words::split(&tail.replace('#', "\0"))
        .map_err(|_| "Unclosed quote or invalid escaping".to_string())?
        .into_iter()
        .map(|word| word.replace('\0', "#"))
        .collect::<Vec<_>>();
    let operation = match action {
        "add" => "ADDNETWORK",
        "change" => "CHANGENETWORK",
        "delete" => "DELNETWORK",
        _ => return Err("Unknown network operation".into()),
    };
    let (network_id, attributes) = if action == "add" {
        (None, words.as_slice())
    } else {
        let id = words.first().ok_or("A network ID is required")?;
        (Some(super::normalize_network_id(id)?), &words[1..])
    };
    let mut params = vec![operation.to_string()];
    if let Some(id) = &network_id {
        params.push(id.clone());
    }
    if action == "delete" {
        if !attributes.is_empty() {
            return Err("Usage: /bouncer delete ID".into());
        }
    } else {
        let mut values = BTreeMap::new();
        for word in attributes {
            let (key, value) = word
                .split_once('=')
                .ok_or("Use attribute=value arguments")?;
            if !matches!(
                key,
                "host"
                    | "port"
                    | "tls"
                    | "name"
                    | "nickname"
                    | "username"
                    | "realname"
                    | "pass-env"
                    | "pass"
            ) {
                return Err("Unsupported network attribute".into());
            }
            let (key, value) = if key == "pass-env" {
                if value.is_empty()
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    return Err("Invalid password environment key".into());
                }
                (
                    "pass",
                    secret(value).ok_or("Password key is not present in the credentials file")?,
                )
            } else {
                if key == "pass" && !value.is_empty() {
                    return Err(
                        "Use pass-env=KEY from the credentials file, or pass= to clear".into(),
                    );
                }
                (key, value.to_string())
            };
            if value.chars().any(char::is_control) {
                return Err("Control characters are not allowed in attributes".into());
            }
            if values.insert(key, value).is_some() {
                return Err("Duplicate network attribute".into());
            }
        }
        if values.is_empty() {
            return Err("At least one network attribute is required".into());
        }
        if action == "add" && values.get("host").is_none_or(String::is_empty) {
            return Err("A host attribute is required".into());
        }
        params.push(
            values
                .iter()
                .map(|(key, value)| format!("{key}={}", escape(value)))
                .collect::<Vec<_>>()
                .join(";"),
        );
    }
    let command = Command::Raw("BOUNCER".into(), params);
    if irc::proto::Message::from(command.clone()).to_string().len() > 512 {
        return Err("Network operation exceeds the IRC line limit".into());
    }
    Ok(Mutation {
        operation,
        network_id,
        command,
    })
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(';', "\\:")
        .replace(' ', "\\s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_round_trip_through_irc_tag_encoding() {
        let mutation = parse(
            "add",
            r#"host=irc.example name="Work #1; Group" realname='A\B'"#,
            |_| None,
        )
        .unwrap();
        let Command::Raw(_, args) = mutation.command else {
            panic!("expected raw command");
        };
        let message: irc::proto::Message = format!("@{} TAGMSG *", args[1]).parse().unwrap();
        let tags = message
            .tags
            .unwrap()
            .into_iter()
            .map(|tag| (tag.0, tag.1.unwrap_or_default()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(tags["name"], "Work #1; Group");
        assert_eq!(tags["realname"], "A\\B");
        assert_eq!(tags["host"], "irc.example");
    }

    #[test]
    fn credentials_resolve_only_by_reference_and_errors_hide_values() {
        let mutation = parse("change", "0007 pass-env=FIXTURE", |key| {
            (key == "FIXTURE").then(|| "fixture; password\\value".into())
        })
        .unwrap();
        assert_eq!(mutation.network_id.as_deref(), Some("7"));
        let wire = irc::proto::Message::from(mutation.command).to_string();
        assert!(wire.contains(r"pass=fixture\:\spassword\\value"));
        for tail in [
            "7 pass=private",
            "7 pass-env=MISSING",
            "7 pass-env=bad-key",
            "7 pass= pass-env=FIXTURE",
        ] {
            let error = parse("change", tail, |_| None).err().unwrap();
            assert!(!error.contains("private"));
        }
        assert!(parse("change", "7 pass=", |_| None).is_ok());
    }

    #[test]
    fn malformed_or_oversized_mutations_are_rejected() {
        for (action, tail) in [
            ("add", "name=x"),
            ("change", "1"),
            ("delete", "1 extra"),
            ("delete", "0"),
            ("add", "host=x host=y"),
            ("change", "1 state=connected"),
            ("add", "host='unterminated"),
            ("add", "host='x\r\nQUIT'"),
        ] {
            assert!(parse(action, tail, |_| None).is_err(), "{action}: {tail}");
        }
        assert!(parse("add", &format!("host=x name={}", "x".repeat(512)), |_| None).is_err());
    }

    #[test]
    fn command_parser_preserves_quoted_attribute_whitespace() {
        let parsed =
            crate::commands::parser::parse_command(r#"/bouncer change 1 name="two   words""#)
                .unwrap();
        let mutation = parse(&parsed.args[0], &parsed.args[1], |_| None).unwrap();
        assert!(
            irc::proto::Message::from(mutation.command)
                .to_string()
                .contains(r"name=two\s\s\swords")
        );
    }
}

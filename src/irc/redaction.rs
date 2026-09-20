use irc::proto::{Command, Message};

pub(crate) fn is_redaction(message: &Message) -> bool {
    matches!(&message.command, Command::Raw(command, _) if command.eq_ignore_ascii_case("REDACT"))
}

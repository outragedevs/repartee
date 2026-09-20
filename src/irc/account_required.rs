use color_eyre::eyre::Result;
use irc::proto::{Command, Message};

pub const CAP: &str = "soju.im/account-required";

#[derive(Debug, thiserror::Error)]
#[error("Account authentication is required. Configure SASL credentials, a client certificate, or a supported server password (PASS).")]
pub struct Required;

pub fn is_failure(message: &Message) -> bool {
    matches!(&message.command, Command::Raw(command, args)
        if command.eq_ignore_ascii_case("FAIL")
            && args.first().is_some_and(|arg| arg == "*")
            && args.get(1).is_some_and(|arg| arg.eq_ignore_ascii_case("ACCOUNT_REQUIRED")))
}

pub fn check(message: &Message) -> Result<()> {
    if is_failure(message) { return Err(Required.into()); }
    Ok(())
}

#[cfg(test)]
#[path = "account_required_tests.rs"]
mod tests;

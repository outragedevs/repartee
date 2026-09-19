use color_eyre::eyre::{Result, eyre};
use futures::StreamExt;
use irc::proto::{Command, Message, Response};

pub const NETWORKS_CAP: &str = "soju.im/bouncer-networks";

pub fn normalize_network_id(value: &str) -> Result<String, String> {
    if !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && let Ok(id) = value.parse::<i64>()
        && id > 0
    {
        return Ok(id.to_string());
    }
    Err("Bouncer network ID must be a positive decimal integer".to_string())
}

pub(super) async fn confirm_binding(
    stream: &mut irc::client::ClientStream,
    expected: &str,
    early_messages: &mut Vec<Message>,
) -> Result<()> {
    let mut confirmed = false;
    while let Some(result) = stream.next().await {
        let message = result?;
        match &message.command {
            Command::Response(Response::RPL_ISUPPORT, args) => {
                for token in args.iter().skip(1).take(args.len().saturating_sub(2)) {
                    if let Some(value) = token.strip_prefix("BOUNCER_NETID=") {
                        if normalize_network_id(value).as_deref() != Ok(expected) {
                            return Err(eyre!(
                                "Bouncer selected a different network than requested"
                            ));
                        }
                        confirmed = true;
                    }
                }
            }
            Command::Raw(command, args)
                if command.eq_ignore_ascii_case("FAIL")
                    && args
                        .first()
                        .is_some_and(|arg| arg.eq_ignore_ascii_case("BOUNCER")) =>
            {
                return Err(eyre!("Bouncer rejected the requested network binding"));
            }
            Command::ERROR(_) => return Err(eyre!("Bouncer closed registration")),
            Command::Response(Response::RPL_ENDOFMOTD | Response::ERR_NOMOTD, _) => {
                if !confirmed {
                    return Err(eyre!("Bouncer did not confirm the requested network ID"));
                }
                early_messages.push(message);
                return Ok(());
            }
            _ => {}
        }
        early_messages.push(message);
    }
    Err(eyre!(
        "Bouncer disconnected before confirming the network ID"
    ))
}

#[cfg(test)]
mod tests;

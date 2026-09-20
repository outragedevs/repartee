use color_eyre::eyre::{Result, eyre};
use futures::StreamExt;
use irc::proto::{Command, Message, Response};

pub const NETWORKS_CAP: &str = "soju.im/bouncer-networks";
pub const NETWORKS_NOTIFY_CAP: &str = "soju.im/bouncer-networks-notify";

pub mod mutations;
mod networks;
pub use networks::{Network, NetworkRegistry, RegistryEvent};

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

pub(super) async fn confirm_registration(
    stream: &mut irc::client::ClientStream,
    expected: Option<&str>,
    early_messages: &mut Vec<Message>,
) -> Result<()> {
    let mut confirmed = expected.is_none();
    let mut tracker = super::batch::BatchTracker::default();
    let mut welcomed = false;
    for message in early_messages.iter() {
        if !welcomed { super::account_required::check(message)?; }
        welcomed |= matches!(message.command, Command::Response(Response::RPL_WELCOME, _));
        confirm_identity(message, &mut tracker, expected, &mut confirmed)?;
    }
    while let Some(result) = stream.next().await {
        let message = result?;
        if !welcomed { super::account_required::check(&message)?; }
        welcomed |= matches!(message.command, Command::Response(Response::RPL_WELCOME, _));
        confirm_identity(&message, &mut tracker, expected, &mut confirmed)?;
        match &message.command {
            Command::Raw(command, args)
                if command.eq_ignore_ascii_case("FAIL")
                    && args
                        .first()
                        .is_some_and(|arg| arg.eq_ignore_ascii_case("BOUNCER")) =>
            {
                if args.get(1).is_some_and(|code| code.eq_ignore_ascii_case("ACCOUNT_REQUIRED")) {
                    return Err(super::account_required::Required.into());
                }
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

fn confirm_identity(message: &Message, tracker: &mut super::batch::BatchTracker, expected: Option<&str>, confirmed: &mut bool) -> Result<()> {
    let completed;
    let tokens = match &message.command {
        Command::BATCH(reference, kind, params) => {
            tracker.invalidate_isupport_parent(message);
            if let Some(reference) = reference.strip_prefix('+') {
                tracker.start_batch(reference, kind.as_ref().map_or("", |kind| kind.to_str()), params.clone().unwrap_or_default(), message.tags.clone());
                return Ok(());
            }
            let Some(batch) = reference.strip_prefix('-').and_then(|reference| tracker.end_batch(reference)) else { return Ok(()); };
            if batch.batch_type != "DRAFT/ISUPPORT" || batch.parent_ref().is_some_and(|parent| !tracker.is_open(parent)) { return Ok(()); }
            completed = batch;
            completed.isupport_tokens(true)
        }
        _ if super::batch::BatchTracker::get_batch_tag_owned(message).is_some() => {
            tracker.add_message(message.clone());
            return Ok(());
        }
        Command::Response(Response::RPL_ISUPPORT, args) => super::isupport::response_tokens(args),
        _ => return Ok(()),
    };
    for token in tokens.unwrap_or_default() {
        if token == "-BOUNCER_NETID" { *confirmed = expected.is_none(); }
        if let Some(value) = token.strip_prefix("BOUNCER_NETID=") {
            let Some(expected) = expected else {
                return Err(eyre!("Bouncer control login selected a network; remove the network selector from the login"));
            };
            if normalize_network_id(value).as_deref() != Ok(expected) {
                return Err(eyre!("Bouncer selected a different network than requested"));
            }
            *confirmed = true;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

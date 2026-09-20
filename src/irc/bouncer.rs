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

pub(super) fn legacy_child_username(config: &crate::config::ServerConfig, username: &str, network: &str) -> Result<Option<String>> {
    if config.bouncer_network_id.is_none() || config.password.as_deref().is_none_or(str::is_empty)
        || [&config.sasl_user, &config.sasl_pass, &config.sasl_mechanism,
            &config.sasl_key_path, &config.client_cert_path].iter().any(|value| value.is_some())
    {
        return Ok(None);
    }
    if network.is_empty() || network.chars().any(char::is_whitespace) || network.contains(['\0', '\r', '\n']) {
        return Err(eyre!("This network name cannot be selected using PASS; configure SASL authentication"));
    }
    let account = username.split(['/', '@']).next().unwrap_or_default();
    let client = username.split_once('@').map_or("", |(_, client)| client.split('/').next().unwrap_or_default());
    Ok(Some(format!("{account}/{network}@{client}")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    Control,
    Network(String),
}

#[derive(Clone, Copy)]
pub(super) enum Selection<'a> {
    Automatic,
    Control,
    Network(&'a str),
}

pub(super) async fn confirm_registration(
    stream: &mut irc::client::ClientStream,
    expected: Selection<'_>,
    early_messages: &mut Vec<Message>,
) -> Result<Identity> {
    let mut confirmed = match expected {
        Selection::Network(_) => None,
        Selection::Automatic | Selection::Control => Some(Identity::Control),
    };
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
                let identity = confirmed.ok_or_else(|| eyre!("Bouncer did not confirm the requested network ID"))?;
                early_messages.push(message);
                return Ok(identity);
            }
            _ => {}
        }
        early_messages.push(message);
    }
    Err(eyre!(
        "Bouncer disconnected before confirming the network ID"
    ))
}

fn confirm_identity(message: &Message, tracker: &mut super::batch::BatchTracker, expected: Selection<'_>, confirmed: &mut Option<Identity>) -> Result<()> {
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
        if token == "-BOUNCER_NETID" {
            *confirmed = match expected {
                Selection::Network(_) => None,
                Selection::Automatic | Selection::Control => Some(Identity::Control),
            };
        }
        if let Some(value) = token.strip_prefix("BOUNCER_NETID=") {
            let network = normalize_network_id(value).map_err(|error| eyre!(error))?;
            match expected {
                Selection::Control => return Err(eyre!("Bouncer control login selected a network; remove the network selector from the login")),
                Selection::Network(expected) if network != expected => return Err(eyre!("Bouncer selected a different network than requested")),
                Selection::Automatic | Selection::Network(_) => {},
            }
            *confirmed = Some(Identity::Network(network));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "bouncer/pass_tests.rs"]
mod pass_tests;

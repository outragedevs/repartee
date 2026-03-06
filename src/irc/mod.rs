pub mod batch;
pub mod cap;
pub mod events;
pub mod flood;
pub mod formatting;
pub mod ignore;
pub mod isupport;
pub mod netsplit;

use std::collections::HashSet;

use base64::Engine as _;
use color_eyre::eyre::{Result, eyre};
use futures::StreamExt;
use irc::client::prelude::*;
use tokio::sync::mpsc;

use crate::irc::cap::{DESIRED_CAPS, ServerCaps};

/// An IRC event forwarded from the reader task to the main loop.
#[derive(Debug)]
pub enum IrcEvent {
    /// A raw IRC protocol message from the server.
    Message(String, Box<irc::proto::Message>),
    /// The connection has been established and identified.
    Connected(String, HashSet<String>),
    /// The connection was lost, optionally with an error description.
    Disconnected(String, Option<String>),
    /// An IRC handle (sender) is ready after async connection completes.
    HandleReady(String, irc::client::Sender),
}

/// Handle to a connected IRC client, holding the connection ID and send-side.
pub struct IrcHandle {
    pub conn_id: String,
    pub sender: irc::client::Sender,
}

/// Connect to an IRC server, returning a handle and the event receiver.
///
/// Always performs capability negotiation (CAP LS 302), requesting all
/// supported capabilities from [`DESIRED_CAPS`].  If SASL credentials are
/// configured and the server supports SASL, performs SASL PLAIN
/// authentication during negotiation.
///
/// Spawns a tokio task that reads from the message stream and forwards
/// events over an unbounded channel.
pub async fn connect_server(
    conn_id: &str,
    server_config: &crate::config::ServerConfig,
    general: &crate::config::GeneralConfig,
) -> Result<(IrcHandle, mpsc::UnboundedReceiver<IrcEvent>)> {
    let nick = server_config
        .nick
        .as_deref()
        .unwrap_or(&general.nick);
    let username = server_config
        .username
        .as_deref()
        .unwrap_or(&general.username);
    let realname = server_config
        .realname
        .as_deref()
        .unwrap_or(&general.realname);

    let irc_config = Config {
        nickname: Some(nick.to_string()),
        username: Some(username.to_string()),
        realname: Some(realname.to_string()),
        server: Some(server_config.address.clone()),
        port: Some(server_config.port),
        use_tls: Some(server_config.tls),
        dangerously_accept_invalid_certs: Some(!server_config.tls_verify),
        password: server_config.password.clone(),
        channels: server_config.channels.clone(),
        encoding: server_config.encoding.clone(),
        version: Some(general.ctcp_version.clone()),
        ..Config::default()
    };

    let mut client = Client::from_config(irc_config).await?;
    let sender = client.sender();
    let mut stream = client.stream()?;

    let sasl_user = server_config.sasl_user.as_deref();
    let sasl_pass = server_config.sasl_pass.as_deref();

    let enabled_caps = negotiate_caps(
        &sender,
        &mut stream,
        nick,
        username,
        realname,
        server_config.password.as_deref(),
        sasl_user,
        sasl_pass,
    )
    .await?;

    let (tx, rx) = mpsc::unbounded_channel();
    let id = conn_id.to_string();
    let id2 = id.clone();

    // Spawn reader task
    tokio::spawn(async move {
        let _ = tx.send(IrcEvent::Connected(id.clone(), enabled_caps));
        let mut error = None;
        while let Some(result) = stream.next().await {
            match result {
                Ok(message) => {
                    if tx.send(IrcEvent::Message(id.clone(), Box::new(message))).is_err() {
                        return; // receiver dropped, no disconnect event needed
                    }
                }
                Err(e) => {
                    error = Some(e.to_string());
                    break;
                }
            }
        }
        // Stream ended — send disconnect with error if any
        let _ = tx.send(IrcEvent::Disconnected(id, error));
    });

    Ok((IrcHandle { conn_id: id2, sender }, rx))
}

/// Negotiate `IRCv3` capabilities and perform connection registration.
///
/// 1. Send `CAP LS 302`
/// 2. Collect all `CAP LS` reply lines (handle multiline `*` continuation)
/// 3. Parse via [`ServerCaps::parse`]
/// 4. Compute the intersection of desired and available capabilities
/// 5. Send `CAP REQ` for all supported caps (sasl last if present)
/// 6. Wait for `ACK`/`NAK`
/// 7. If `sasl` was ACK'd and credentials exist, run SASL PLAIN flow
/// 8. Send `CAP END`, `PASS` (if set), `NICK`, `USER`
/// 9. Return the set of enabled capability names
#[expect(clippy::too_many_arguments, reason = "IRC registration requires many parameters")]
async fn negotiate_caps(
    sender: &irc::client::Sender,
    stream: &mut irc::client::ClientStream,
    nick: &str,
    username: &str,
    realname: &str,
    password: Option<&str>,
    sasl_user: Option<&str>,
    sasl_pass: Option<&str>,
) -> Result<HashSet<String>> {
    use irc::proto::command::CapSubCommand;

    // Step 1: Request capability listing (version 302 for values)
    sender.send(Command::CAP(None, CapSubCommand::LS, Some("302".to_string()), None))?;

    // Step 2: Collect all CAP LS reply lines.
    // The server may send caps across multiple lines (CAP LS 302 multiline).
    // Continuation line: field3 = "*", caps in field4
    // Final/single line: caps in field3, field4 = None
    let mut server_caps = ServerCaps::default();
    while let Some(result) = stream.next().await {
        let msg = result?;
        if let Command::CAP(_, CapSubCommand::LS, ref field3, ref field4) = msg.command {
            let is_continuation = field3.as_deref() == Some("*");
            let caps_str = if is_continuation {
                field4.as_deref().unwrap_or("")
            } else {
                field3.as_deref().unwrap_or("")
            };
            server_caps.merge(caps_str);
            // Only break on the final line (not a continuation)
            if !is_continuation {
                break;
            }
        }
    }

    // Step 3: Compute capabilities to request
    let mut caps_to_request = server_caps.negotiate(DESIRED_CAPS);

    // If we have no SASL credentials, don't request sasl even if advertised
    let has_sasl_creds = sasl_user.is_some() && sasl_pass.is_some();
    if !has_sasl_creds {
        caps_to_request.retain(|c| c != "sasl");
    }

    // Put sasl LAST so other caps are ACK'd regardless of SASL outcome
    let sasl_requested = caps_to_request.iter().position(|c| c == "sasl").is_some_and(|pos| {
        caps_to_request.remove(pos);
        caps_to_request.push("sasl".to_string());
        true
    });

    let mut enabled_caps: HashSet<String> = HashSet::new();

    // Step 4: Send CAP REQ if there are any caps to request
    if !caps_to_request.is_empty() {
        let req_str = caps_to_request.join(" ");
        tracing::info!("requesting capabilities: {req_str}");
        sender.send(Command::CAP(
            None,
            CapSubCommand::REQ,
            None,
            Some(req_str),
        ))?;

        // Step 5: Wait for ACK/NAK
        while let Some(result) = stream.next().await {
            let msg = result?;
            if let Command::CAP(_, CapSubCommand::ACK, _, ref acked) = msg.command {
                if let Some(ref acked_str) = *acked {
                    for cap in acked_str.split_whitespace() {
                        enabled_caps.insert(cap.to_ascii_lowercase());
                    }
                }
                tracing::info!("capabilities ACK'd: {}", enabled_caps.iter().cloned().collect::<Vec<_>>().join(" "));
                break;
            }
            if let Command::CAP(_, CapSubCommand::NAK, _, ref naked) = msg.command {
                tracing::warn!(
                    "server NAK'd capabilities: {}",
                    naked.as_deref().unwrap_or("(unknown)")
                );
                break;
            }
        }
    }

    // Step 6: If sasl was ACK'd and we have credentials, run SASL PLAIN
    let sasl_acked = enabled_caps.contains("sasl");
    if sasl_requested && sasl_acked {
        if let (Some(user), Some(pass)) = (sasl_user, sasl_pass) {
            match run_sasl_plain(sender, stream, user, pass).await {
                Ok(()) => {
                    tracing::info!("SASL PLAIN authentication successful");
                }
                Err(e) => {
                    tracing::warn!("SASL authentication failed: {e}");
                    // Remove sasl from enabled since auth failed
                    enabled_caps.remove("sasl");
                }
            }
        }
    } else if sasl_requested && !sasl_acked {
        tracing::warn!("server did not ACK sasl capability");
    }

    // Step 7: Finish registration
    sender.send(Command::CAP(None, CapSubCommand::END, None, None))?;
    if let Some(pass) = password {
        sender.send(Command::PASS(pass.to_string()))?;
    }
    sender.send(Command::NICK(nick.to_string()))?;
    sender.send(Command::USER(
        username.to_string(),
        "0".to_string(),
        realname.to_string(),
    ))?;

    Ok(enabled_caps)
}

/// Execute the SASL PLAIN authentication handshake.
///
/// Assumes SASL has already been ACK'd.  Sends `AUTHENTICATE PLAIN`,
/// waits for `+`, sends base64-encoded credentials, waits for 903/904.
async fn run_sasl_plain(
    sender: &irc::client::Sender,
    stream: &mut irc::client::ClientStream,
    sasl_user: &str,
    sasl_pass: &str,
) -> Result<()> {
    // Send AUTHENTICATE PLAIN
    sender.send(Command::AUTHENTICATE("PLAIN".to_string()))?;

    // Wait for AUTHENTICATE + from server
    while let Some(result) = stream.next().await {
        let msg = result?;
        if let Command::AUTHENTICATE(ref param) = msg.command
            && param == "+"
        {
            break;
        }
    }

    // Send base64-encoded credentials: authzid\0authcid\0password
    let auth_string = format!("{sasl_user}\x00{sasl_user}\x00{sasl_pass}");
    let encoded = base64::engine::general_purpose::STANDARD.encode(auth_string);
    sender.send(Command::AUTHENTICATE(encoded))?;

    // Wait for 903 (success) or 904/905/906 (failure)
    while let Some(result) = stream.next().await {
        let msg = result?;
        if let Command::Response(response, _) = &msg.command {
            match response {
                Response::RPL_SASLSUCCESS => return Ok(()),
                Response::ERR_SASLFAIL => return Err(eyre!("SASL authentication failed")),
                Response::ERR_SASLTOOLONG => return Err(eyre!("SASL message too long")),
                Response::ERR_SASLABORT => return Err(eyre!("SASL authentication aborted")),
                _ => {}
            }
        }
    }

    Err(eyre!("SASL authentication: connection closed unexpectedly"))
}

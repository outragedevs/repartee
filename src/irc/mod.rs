pub mod events;
pub mod flood;
pub mod formatting;
pub mod ignore;
pub mod isupport;
pub mod netsplit;

use base64::Engine as _;
use color_eyre::eyre::{Result, eyre};
use futures::StreamExt;
use irc::client::prelude::*;
use tokio::sync::mpsc;

/// An IRC event forwarded from the reader task to the main loop.
#[derive(Debug)]
pub enum IrcEvent {
    /// A raw IRC protocol message from the server.
    Message(String, Box<irc::proto::Message>),
    /// The connection has been established and identified.
    Connected(String),
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
/// If SASL credentials are configured, performs SASL PLAIN authentication
/// during capability negotiation before registering with NICK/USER.
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

    let has_sasl = server_config.sasl_user.is_some() && server_config.sasl_pass.is_some();

    if has_sasl {
        // Manual registration with SASL PLAIN handshake
        sasl_authenticate(
            &sender,
            &mut stream,
            nick,
            username,
            realname,
            server_config.password.as_deref(),
            server_config.sasl_user.as_deref().unwrap_or_default(),
            server_config.sasl_pass.as_deref().unwrap_or_default(),
        )
        .await?;
    } else {
        // Standard registration (CAP END → PASS → NICK → USER)
        client.identify()?;
    }

    let (tx, rx) = mpsc::unbounded_channel();
    let id = conn_id.to_string();
    let id2 = id.clone();

    // Spawn reader task
    tokio::spawn(async move {
        let _ = tx.send(IrcEvent::Connected(id.clone()));
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

/// Perform SASL PLAIN authentication during IRC capability negotiation.
///
/// Flow: CAP LS 302 → CAP REQ :sasl → AUTHENTICATE PLAIN → base64 creds → CAP END → NICK/USER
#[expect(clippy::too_many_arguments, reason = "IRC registration requires many parameters")]
async fn sasl_authenticate(
    sender: &irc::client::Sender,
    stream: &mut irc::client::ClientStream,
    nick: &str,
    username: &str,
    realname: &str,
    password: Option<&str>,
    sasl_user: &str,
    sasl_pass: &str,
) -> Result<()> {
    use irc::proto::command::CapSubCommand;

    // Step 1: Request capabilities
    sender.send(Command::CAP(None, CapSubCommand::LS, Some("302".to_string()), None))?;

    // Step 2: Wait for CAP LS reply, then request SASL.
    // The server may send caps across multiple lines (CAP LS 302 multiline).
    // Single-line:     CAP(Some("*"), LS, Some("caps..."), None)
    // Multi cont:      CAP(Some("*"), LS, Some("*"), Some("caps..."))
    // Multi final:     CAP(Some("*"), LS, Some("caps..."), None)
    let mut sasl_supported = false;
    while let Some(result) = stream.next().await {
        let msg = result?;
        if let Command::CAP(_, CapSubCommand::LS, ref field3, ref field4) = msg.command {
            // Continuation line: field3 = "*", caps in field4
            // Final/single line: caps in field3, field4 = None
            let is_continuation = field3.as_deref() == Some("*");
            let caps_str = if is_continuation {
                field4.as_deref().unwrap_or("")
            } else {
                field3.as_deref().unwrap_or("")
            };
            if caps_str.split_whitespace().any(|c| c.eq_ignore_ascii_case("sasl")) {
                sasl_supported = true;
            }
            // Only break on the final line (not a continuation)
            if !is_continuation {
                break;
            }
        }
    }

    if !sasl_supported {
        // Server doesn't support SASL — fall back to normal registration
        tracing::warn!("server does not support SASL, proceeding without authentication");
        if let Some(pass) = password {
            sender.send(Command::PASS(pass.to_string()))?;
        }
        sender.send(Command::CAP(None, CapSubCommand::END, None, None))?;
        sender.send(Command::NICK(nick.to_string()))?;
        sender.send(Command::USER(username.to_string(), "0".to_string(), realname.to_string()))?;
        return Ok(());
    }

    sender.send(Command::CAP(None, CapSubCommand::REQ, None, Some("sasl".to_string())))?;

    // Step 3: Wait for CAP ACK
    while let Some(result) = stream.next().await {
        let msg = result?;
        if let Command::CAP(_, CapSubCommand::ACK, _, _) = msg.command {
            break;
        }
        if let Command::CAP(_, CapSubCommand::NAK, _, _) = msg.command {
            tracing::warn!("server NAK'd SASL capability, proceeding without authentication");
            if let Some(pass) = password {
                sender.send(Command::PASS(pass.to_string()))?;
            }
            sender.send(Command::CAP(None, CapSubCommand::END, None, None))?;
            sender.send(Command::NICK(nick.to_string()))?;
            sender.send(Command::USER(username.to_string(), "0".to_string(), realname.to_string()))?;
            return Ok(());
        }
    }

    // Step 4: Send AUTHENTICATE PLAIN
    sender.send(Command::AUTHENTICATE("PLAIN".to_string()))?;

    // Step 5: Wait for AUTHENTICATE + from server
    while let Some(result) = stream.next().await {
        let msg = result?;
        if let Command::AUTHENTICATE(ref param) = msg.command {
            if param == "+" {
                break;
            }
        }
    }

    // Step 6: Send base64-encoded credentials: \0username\0password
    let auth_string = format!("{sasl_user}\x00{sasl_user}\x00{sasl_pass}");
    let encoded = base64::engine::general_purpose::STANDARD.encode(auth_string);
    sender.send(Command::AUTHENTICATE(encoded))?;

    // Step 7: Wait for 903 (success) or 904 (failure)
    while let Some(result) = stream.next().await {
        let msg = result?;
        if let Command::Response(response, _) = &msg.command {
            match response {
                Response::RPL_SASLSUCCESS => break,
                Response::ERR_SASLFAIL => return Err(eyre!("SASL authentication failed")),
                Response::ERR_SASLTOOLONG => return Err(eyre!("SASL message too long")),
                Response::ERR_SASLABORT => return Err(eyre!("SASL authentication aborted")),
                _ => {}
            }
        }
    }

    // Step 8: Finish registration
    if let Some(pass) = password {
        sender.send(Command::PASS(pass.to_string()))?;
    }
    sender.send(Command::CAP(None, CapSubCommand::END, None, None))?;
    sender.send(Command::NICK(nick.to_string()))?;
    sender.send(Command::USER(username.to_string(), "0".to_string(), realname.to_string()))?;

    Ok(())
}

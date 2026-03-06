pub mod batch;
pub mod cap;
pub mod events;
pub mod extban;
pub mod flood;
pub mod formatting;
pub mod ignore;
pub mod isupport;
pub mod netsplit;
pub mod sasl_scram;

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
    /// Registration complete (`RPL_WELCOME` received).
    Connected(String, HashSet<String>),
    /// The connection was lost, optionally with an error description.
    Disconnected(String, Option<String>),
    /// An IRC handle (sender) is ready after async connection completes.
    HandleReady(String, irc::client::Sender),
    /// Diagnostic messages from CAP/SASL negotiation (fires immediately).
    NegotiationInfo(String, Vec<String>),
}

/// Result of `IRCv3` capability negotiation.
struct NegotiateResult {
    /// Capabilities successfully enabled via `CAP REQ` / `CAP ACK`.
    enabled_caps: HashSet<String>,
    /// Human-readable diagnostic messages for the status buffer.
    diagnostics: Vec<String>,
    /// Messages consumed during negotiation that must be replayed
    /// (e.g. `RPL_WELCOME` from non-`IRCv3` servers, pre-registration `NOTICE`s).
    early_messages: Vec<irc::proto::Message>,
}

/// Handle to a connected IRC client, holding the connection ID and send-side.
pub struct IrcHandle {
    pub conn_id: String,
    pub sender: irc::client::Sender,
}

/// SASL authentication mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaslMechanism {
    /// SASL PLAIN — username + password, base64-encoded.
    Plain,
    /// SASL EXTERNAL — client TLS certificate (`CertFP`) based.
    External,
    /// SASL SCRAM-SHA-256 — challenge-response (RFC 5802 / RFC 7677).
    ScramSha256,
}

impl std::fmt::Display for SaslMechanism {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plain => write!(f, "PLAIN"),
            Self::External => write!(f, "EXTERNAL"),
            Self::ScramSha256 => write!(f, "SCRAM-SHA-256"),
        }
    }
}

/// Select the best SASL mechanism given server capabilities and local config.
///
/// Priority when `sasl_mechanism` is `None` (auto-detect):
/// 1. `EXTERNAL` — only if a `client_cert_path` is configured and the server advertises it.
/// 2. `SCRAM-SHA-256` — if `sasl_user` + `sasl_pass` are configured and the server advertises it.
/// 3. `PLAIN` — if `sasl_user` + `sasl_pass` are configured and the server advertises it.
///
/// When `sasl_mechanism` is explicitly set, that mechanism is used if the server supports it.
/// Returns `None` if no suitable mechanism can be selected.
#[must_use]
pub fn select_sasl_mechanism(
    server_mechanisms: &[String],
    sasl_mechanism_override: Option<&str>,
    has_client_cert: bool,
    has_credentials: bool,
) -> Option<SaslMechanism> {
    let server_has = |mech: &str| {
        server_mechanisms.iter().any(|m| m.eq_ignore_ascii_case(mech))
    };

    // Explicit override from config
    if let Some(override_mech) = sasl_mechanism_override {
        return match override_mech.to_ascii_uppercase().as_str() {
            "EXTERNAL" if server_has("EXTERNAL") && has_client_cert => Some(SaslMechanism::External),
            "SCRAM-SHA-256" if server_has("SCRAM-SHA-256") && has_credentials => Some(SaslMechanism::ScramSha256),
            "PLAIN" if server_has("PLAIN") && has_credentials => Some(SaslMechanism::Plain),
            _ => {
                tracing::warn!(
                    "configured SASL mechanism '{override_mech}' not available \
                     (server offers: {}, cert={has_client_cert}, creds={has_credentials})",
                    server_mechanisms.join(",")
                );
                None
            }
        };
    }

    // Auto-detect: prefer EXTERNAL, then SCRAM-SHA-256, then PLAIN
    if has_client_cert && server_has("EXTERNAL") {
        return Some(SaslMechanism::External);
    }
    if has_credentials && server_has("SCRAM-SHA-256") {
        return Some(SaslMechanism::ScramSha256);
    }
    if has_credentials && server_has("PLAIN") {
        return Some(SaslMechanism::Plain);
    }

    None
}

/// Connect to an IRC server, returning a handle and the event receiver.
///
/// Always performs capability negotiation (CAP LS 302), requesting all
/// supported capabilities from [`DESIRED_CAPS`].  If SASL credentials or
/// a client certificate are configured and the server supports SASL,
/// performs the appropriate SASL authentication during negotiation.
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

    // Generate alt nicks so the irc crate can retry on ERR_NICKNAMEINUSE
    // instead of fatally erroring with NoUsableNick.
    let alt_nicks: Vec<String> = (1..=4)
        .map(|i| format!("{nick}{}", "_".repeat(i)))
        .collect();

    let irc_config = Config {
        nickname: Some(nick.to_string()),
        alt_nicks,
        username: Some(username.to_string()),
        realname: Some(realname.to_string()),
        server: Some(server_config.address.clone()),
        port: Some(server_config.port),
        use_tls: Some(server_config.tls),
        dangerously_accept_invalid_certs: Some(!server_config.tls_verify),
        password: server_config.password.clone(),
        // Do NOT pass channels here — the irc crate auto-joins after ENDOFMOTD
        // which combined with our own join logic causes Excess Flood.
        // Our app handles channel joining with throttling via IrcEvent::Connected.
        channels: vec![],
        encoding: server_config.encoding.clone(),
        version: Some(general.ctcp_version.clone()),
        client_cert_path: server_config.client_cert_path.clone(),
        ..Config::default()
    };

    let mut client = Client::from_config(irc_config).await?;
    let sender = client.sender();
    let mut stream = client.stream()?;

    let reg_params = RegistrationParams {
        nick,
        username,
        realname,
        password: server_config.password.as_deref(),
        sasl_user: server_config.sasl_user.as_deref(),
        sasl_pass: server_config.sasl_pass.as_deref(),
        sasl_mechanism_override: server_config.sasl_mechanism.as_deref(),
        has_client_cert: server_config.client_cert_path.is_some(),
    };

    let neg = negotiate_caps(&sender, &mut stream, &reg_params).await?;

    let (tx, rx) = mpsc::unbounded_channel();
    let id = conn_id.to_string();
    let id2 = id.clone();

    // Spawn reader task
    tokio::spawn(async move {
        // Send negotiation diagnostics immediately so they're visible even if
        // registration fails (e.g. server requires SASL but auth didn't complete).
        let _ = tx.send(IrcEvent::NegotiationInfo(id.clone(), neg.diagnostics));

        let mut sent_connected = false;
        let mut error = None;

        // Replay messages consumed during capability negotiation.
        // Non-IRCv3 servers that silently ignore CAP send RPL_WELCOME during
        // negotiation; pre-registration NOTICEs may also be collected here.
        for message in neg.early_messages {
            if !sent_connected
                && let Command::Response(Response::RPL_WELCOME, _) = &message.command
            {
                sent_connected = true;
                let _ = tx.send(IrcEvent::Connected(
                    id.clone(),
                    neg.enabled_caps.clone(),
                ));
            }
            if tx.send(IrcEvent::Message(id.clone(), Box::new(message))).is_err() {
                return;
            }
        }

        // Continue reading from the stream.
        while let Some(result) = stream.next().await {
            match result {
                Ok(message) => {
                    if !sent_connected
                        && let Command::Response(Response::RPL_WELCOME, _) = &message.command
                    {
                        sent_connected = true;
                        let _ = tx.send(IrcEvent::Connected(
                            id.clone(),
                            neg.enabled_caps.clone(),
                        ));
                    }
                    if tx.send(IrcEvent::Message(id.clone(), Box::new(message))).is_err() {
                        return;
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

/// Parameters for IRC connection registration, bundled to avoid long argument lists.
struct RegistrationParams<'a> {
    nick: &'a str,
    username: &'a str,
    realname: &'a str,
    password: Option<&'a str>,
    sasl_user: Option<&'a str>,
    sasl_pass: Option<&'a str>,
    sasl_mechanism_override: Option<&'a str>,
    has_client_cert: bool,
}

/// Negotiate `IRCv3` capabilities and perform connection registration.
///
/// Sends `CAP LS 302` + `NICK` + `USER` together (the standard irssi/weechat
/// approach).  `IRCv3` servers enter negotiation mode upon receiving `CAP LS`
/// and **suspend** `NICK`/`USER` processing until `CAP END`.  Non-`IRCv3`
/// servers either reply with `421 ERR_UNKNOWNCOMMAND` or silently ignore `CAP`
/// and process `NICK`/`USER` immediately — detected by `RPL_WELCOME`.
///
/// Messages consumed during negotiation (pre-registration `NOTICE`s,
/// `RPL_WELCOME` from non-`IRCv3` servers) are collected in
/// [`NegotiateResult::early_messages`] for replay by the reader task.
#[expect(clippy::too_many_lines, reason = "single linear negotiation flow")]
async fn negotiate_caps(
    sender: &irc::client::Sender,
    stream: &mut irc::client::ClientStream,
    params: &RegistrationParams<'_>,
) -> Result<NegotiateResult> {
    use irc::proto::command::CapSubCommand;
    let mut diag: Vec<String> = Vec::new();
    let mut early_messages: Vec<irc::proto::Message> = Vec::new();

    // Step 1: Send CAP LS 302 + PASS + NICK + USER together.
    //
    // Per the IRCv3 specification, a server that receives CAP LS enters
    // capability negotiation mode and suspends registration — NICK/USER are
    // held until CAP END.  This means the registration timer does NOT start
    // even if hostname lookup takes 30+ seconds (Libera IPv6 reverse DNS).
    //
    // Non-IRCv3 servers ignore CAP and process NICK/USER immediately,
    // producing RPL_WELCOME (or 421 + RPL_WELCOME).
    sender.send(Command::CAP(None, CapSubCommand::LS, Some("302".to_string()), None))?;
    if let Some(pass) = params.password {
        sender.send(Command::PASS(pass.to_string()))?;
    }
    sender.send(Command::NICK(params.nick.to_string()))?;
    sender.send(Command::USER(
        params.username.to_string(),
        "0".to_string(),
        params.realname.to_string(),
    ))?;

    // Step 2: Wait for the server's response to determine IRCv3 support.
    //
    // Three possible outcomes:
    // - CAP LS response      → IRCv3 server, proceed with full negotiation
    // - 421 ERR_UNKNOWNCOMMAND → non-IRCv3 server, skip CAP (NICK/USER already sent)
    // - RPL_WELCOME (001)    → non-IRCv3 that silently ignored CAP, already registered
    let mut server_caps = ServerCaps::default();
    let mut cap_supported = false;

    while let Some(result) = stream.next().await {
        let msg = result?;

        // 421 ERR_UNKNOWNCOMMAND for CAP → non-IRCv3 server
        if let Command::Response(Response::ERR_UNKNOWNCOMMAND, ref args) = msg.command
            && args.iter().any(|a| a.eq_ignore_ascii_case("CAP"))
        {
            diag.push("CAP: server does not support IRCv3 capabilities".to_string());
            break;
        }

        // RPL_WELCOME → server ignored CAP, already registered us
        if let Command::Response(Response::RPL_WELCOME, _) = &msg.command {
            diag.push("CAP: server does not support IRCv3 (registered without CAP)".to_string());
            early_messages.push(msg);
            break;
        }

        // CAP LS response → IRCv3 server
        if let Command::CAP(_, CapSubCommand::LS, ref field3, ref field4) = msg.command {
            let is_continuation = field3.as_deref() == Some("*");
            let caps_str = if is_continuation {
                field4.as_deref().unwrap_or("")
            } else {
                field3.as_deref().unwrap_or("")
            };
            server_caps.merge(caps_str);
            if !is_continuation {
                cap_supported = true;
                break;
            }
        }

        // Other messages (NOTICE about hostname/ident, PING, etc.) — save for replay
        early_messages.push(msg);
    }

    let mut enabled_caps: HashSet<String> = HashSet::new();

    if cap_supported {
        // Determine whether we can authenticate via SASL
        let has_credentials = params.sasl_user.is_some() && params.sasl_pass.is_some();
        let server_mechanisms = server_caps.sasl_mechanisms();
        let selected_mechanism = select_sasl_mechanism(
            &server_mechanisms,
            params.sasl_mechanism_override,
            params.has_client_cert,
            has_credentials,
        );
        let want_sasl = selected_mechanism.is_some();

        diag.push(format!(
            "CAP: server advertises sasl={}, has_credentials={has_credentials}, mechanism={:?}",
            server_caps.has("sasl"),
            selected_mechanism,
        ));

        // Compute capabilities to request
        let mut caps_to_request = server_caps.negotiate(DESIRED_CAPS);

        if !want_sasl {
            caps_to_request.retain(|c| c != "sasl");
        }

        // Put sasl LAST so other caps are ACK'd regardless of SASL outcome
        let sasl_requested = caps_to_request.iter().position(|c| c == "sasl").is_some_and(|pos| {
            caps_to_request.remove(pos);
            caps_to_request.push("sasl".to_string());
            true
        });

        // Send CAP REQ if there are any caps to request
        if caps_to_request.is_empty() {
            diag.push("CAP: no capabilities to request".to_string());
        } else {
            let req_str = caps_to_request.join(" ");
            diag.push(format!("CAP REQ: {req_str}"));
            sender.send(Command::CAP(
                None,
                CapSubCommand::REQ,
                None,
                Some(req_str),
            ))?;

            // Wait for ACK/NAK
            while let Some(result) = stream.next().await {
                let msg = result?;
                if let Command::CAP(_, CapSubCommand::ACK, ref acked, _) = msg.command {
                    if let Some(ref acked_str) = *acked {
                        for cap in acked_str.split_whitespace() {
                            enabled_caps.insert(cap.to_ascii_lowercase());
                        }
                    }
                    diag.push(format!(
                        "CAP ACK: {}",
                        enabled_caps.iter().cloned().collect::<Vec<_>>().join(" "),
                    ));
                    break;
                }
                if let Command::CAP(_, CapSubCommand::NAK, ref naked, _) = msg.command {
                    diag.push(format!(
                        "CAP NAK: {}",
                        naked.as_deref().unwrap_or("(unknown)"),
                    ));
                    break;
                }
            }
        }

        // If sasl was ACK'd, run the selected SASL flow
        let sasl_acked = enabled_caps.contains("sasl");
        if sasl_requested && sasl_acked {
            if let Some(mechanism) = selected_mechanism {
                let result = match mechanism {
                    SaslMechanism::External => {
                        diag.push("SASL: authenticating via EXTERNAL".to_string());
                        run_sasl_external(sender, stream).await
                    }
                    SaslMechanism::ScramSha256 => {
                        if let (Some(user), Some(pass)) = (params.sasl_user, params.sasl_pass) {
                            diag.push(format!("SASL: authenticating via SCRAM-SHA-256 as {user}"));
                            run_sasl_scram(sender, stream, user, pass).await
                        } else {
                            Err(eyre!("SASL SCRAM-SHA-256 selected but credentials missing"))
                        }
                    }
                    SaslMechanism::Plain => {
                        if let (Some(user), Some(pass)) = (params.sasl_user, params.sasl_pass) {
                            diag.push(format!("SASL: authenticating via PLAIN as {user}"));
                            run_sasl_plain(sender, stream, user, pass).await
                        } else {
                            Err(eyre!("SASL PLAIN selected but credentials missing"))
                        }
                    }
                };
                match result {
                    Ok(()) => {
                        diag.push(format!("SASL: {mechanism} authentication successful"));
                    }
                    Err(e) => {
                        diag.push(format!("SASL: {mechanism} authentication FAILED: {e}"));
                        enabled_caps.remove("sasl");
                    }
                }
            }
        } else if sasl_requested && !sasl_acked {
            diag.push("SASL: requested but server did not ACK".to_string());
        } else if !sasl_requested && has_credentials {
            diag.push("SASL: credentials available but server does not advertise sasl".to_string());
        }

        // Send CAP END to finish capability negotiation.
        // The server will now process the held NICK/USER commands.
        sender.send(Command::CAP(None, CapSubCommand::END, None, None))?;
    }

    // NICK/USER were already sent in step 1 — no need to send them again.

    Ok(NegotiateResult {
        enabled_caps,
        diagnostics: diag,
        early_messages,
    })
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

/// Execute the SASL SCRAM-SHA-256 authentication handshake.
///
/// Assumes SASL has already been ACK'd.  Performs the three-step
/// challenge-response protocol:
///
/// 1. Send `AUTHENTICATE SCRAM-SHA-256`, wait for `+`
/// 2. Send base64-encoded client-first message, receive server-first
/// 3. Send base64-encoded client-final message, receive server-final
/// 4. Verify server signature and wait for 903/904
async fn run_sasl_scram(
    sender: &irc::client::Sender,
    stream: &mut irc::client::ClientStream,
    sasl_user: &str,
    sasl_pass: &str,
) -> Result<()> {
    use base64::Engine as _;

    let b64 = &base64::engine::general_purpose::STANDARD;

    // Step 1: Initiate SCRAM-SHA-256
    sender.send(Command::AUTHENTICATE("SCRAM-SHA-256".to_string()))?;

    // Wait for AUTHENTICATE + from server
    while let Some(result) = stream.next().await {
        let msg = result?;
        if let Command::AUTHENTICATE(ref param) = msg.command
            && param == "+"
        {
            break;
        }
    }

    // Step 2: Send client-first message
    let (client_first_bare, client_first_full, client_nonce) =
        sasl_scram::client_first(sasl_user);
    let encoded = b64.encode(&client_first_full);
    for chunk in sasl_scram::chunk_authenticate(&encoded) {
        sender.send(Command::AUTHENTICATE(chunk))?;
    }

    // Step 3: Receive server-first message
    let server_first = loop {
        if let Some(result) = stream.next().await {
            let msg = result?;
            match &msg.command {
                Command::AUTHENTICATE(param) if param != "+" => {
                    // Decode base64 server-first
                    let decoded = b64
                        .decode(param)
                        .map_err(|e| eyre!("SCRAM: invalid base64 in server-first: {e}"))?;
                    break String::from_utf8(decoded)
                        .map_err(|e| eyre!("SCRAM: non-UTF-8 server-first: {e}"))?;
                }
                Command::Response(response, _) => match response {
                    irc::proto::Response::ERR_SASLFAIL => {
                        return Err(eyre!("SASL SCRAM-SHA-256 authentication failed"));
                    }
                    irc::proto::Response::ERR_SASLABORT => {
                        return Err(eyre!("SASL SCRAM-SHA-256 authentication aborted"));
                    }
                    _ => {}
                },
                _ => {}
            }
        } else {
            return Err(eyre!(
                "SASL SCRAM-SHA-256: connection closed waiting for server-first"
            ));
        }
    };

    // Step 4: Compute and send client-final message
    let (client_final_msg, expected_server_sig) =
        sasl_scram::client_final(&server_first, &client_first_bare, &client_nonce, sasl_pass)?;
    let encoded_final = b64.encode(&client_final_msg);
    for chunk in sasl_scram::chunk_authenticate(&encoded_final) {
        sender.send(Command::AUTHENTICATE(chunk))?;
    }

    // Step 5: Receive server-final and verify, then wait for 903/904
    let mut server_verified = false;
    while let Some(result) = stream.next().await {
        let msg = result?;
        match &msg.command {
            Command::AUTHENTICATE(param) if !server_verified && param != "+" => {
                let decoded = b64
                    .decode(param)
                    .map_err(|e| eyre!("SCRAM: invalid base64 in server-final: {e}"))?;
                let server_final = String::from_utf8(decoded)
                    .map_err(|e| eyre!("SCRAM: non-UTF-8 server-final: {e}"))?;
                if !sasl_scram::verify_server(&server_final, &expected_server_sig) {
                    return Err(eyre!(
                        "SCRAM: server signature verification failed — possible MITM"
                    ));
                }
                server_verified = true;
            }
            Command::Response(response, _) => match response {
                irc::proto::Response::RPL_SASLSUCCESS => return Ok(()),
                irc::proto::Response::ERR_SASLFAIL => {
                    return Err(eyre!("SASL SCRAM-SHA-256 authentication failed"));
                }
                irc::proto::Response::ERR_SASLTOOLONG => {
                    return Err(eyre!("SASL SCRAM-SHA-256 message too long"));
                }
                irc::proto::Response::ERR_SASLABORT => {
                    return Err(eyre!("SASL SCRAM-SHA-256 authentication aborted"));
                }
                _ => {}
            },
            _ => {}
        }
    }

    Err(eyre!(
        "SASL SCRAM-SHA-256: connection closed unexpectedly"
    ))
}

/// Execute the SASL EXTERNAL authentication handshake.
///
/// SASL EXTERNAL authenticates via the client TLS certificate already
/// presented during the TLS handshake.  The flow is:
///
/// 1. Send `AUTHENTICATE EXTERNAL`
/// 2. Wait for server's `AUTHENTICATE +`
/// 3. Send `AUTHENTICATE +` (base64 of empty string — literal `+`)
/// 4. Wait for `RPL_SASLSUCCESS` (903) or `ERR_SASLFAIL` (904)
async fn run_sasl_external(
    sender: &irc::client::Sender,
    stream: &mut irc::client::ClientStream,
) -> Result<()> {
    // Send AUTHENTICATE EXTERNAL
    sender.send(Command::AUTHENTICATE("EXTERNAL".to_string()))?;

    // Wait for AUTHENTICATE + from server
    while let Some(result) = stream.next().await {
        let msg = result?;
        if let Command::AUTHENTICATE(ref param) = msg.command
            && param == "+"
        {
            break;
        }
    }

    // Send AUTHENTICATE + (base64 encoding of an empty string is "+")
    sender.send(Command::AUTHENTICATE("+".to_string()))?;

    // Wait for 903 (success) or 904/905/906 (failure)
    while let Some(result) = stream.next().await {
        let msg = result?;
        if let Command::Response(response, _) = &msg.command {
            match response {
                Response::RPL_SASLSUCCESS => return Ok(()),
                Response::ERR_SASLFAIL => return Err(eyre!("SASL EXTERNAL authentication failed")),
                Response::ERR_SASLTOOLONG => return Err(eyre!("SASL EXTERNAL message too long")),
                Response::ERR_SASLABORT => return Err(eyre!("SASL EXTERNAL authentication aborted")),
                _ => {}
            }
        }
    }

    Err(eyre!("SASL EXTERNAL: connection closed unexpectedly"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_external_when_cert_configured() {
        let server_mechs = vec!["PLAIN".to_string(), "EXTERNAL".to_string()];
        let result = select_sasl_mechanism(&server_mechs, None, true, true);
        // EXTERNAL is preferred over PLAIN when cert is available
        assert_eq!(result, Some(SaslMechanism::External));
    }

    #[test]
    fn select_plain_when_only_credentials() {
        let server_mechs = vec!["PLAIN".to_string(), "EXTERNAL".to_string()];
        let result = select_sasl_mechanism(&server_mechs, None, false, true);
        assert_eq!(result, Some(SaslMechanism::Plain));
    }

    #[test]
    fn explicit_override_plain() {
        let server_mechs = vec!["PLAIN".to_string(), "EXTERNAL".to_string()];
        // Even though cert is available, explicit override to PLAIN
        let result = select_sasl_mechanism(&server_mechs, Some("PLAIN"), true, true);
        assert_eq!(result, Some(SaslMechanism::Plain));
    }

    #[test]
    fn explicit_override_external() {
        let server_mechs = vec!["PLAIN".to_string(), "EXTERNAL".to_string()];
        let result = select_sasl_mechanism(&server_mechs, Some("EXTERNAL"), true, false);
        assert_eq!(result, Some(SaslMechanism::External));
    }

    #[test]
    fn explicit_override_unavailable_mechanism() {
        let server_mechs = vec!["PLAIN".to_string()];
        // Server doesn't offer EXTERNAL
        let result = select_sasl_mechanism(&server_mechs, Some("EXTERNAL"), true, true);
        assert_eq!(result, None);
    }

    #[test]
    fn no_credentials_no_cert() {
        let server_mechs = vec!["PLAIN".to_string(), "EXTERNAL".to_string()];
        let result = select_sasl_mechanism(&server_mechs, None, false, false);
        assert_eq!(result, None);
    }

    #[test]
    fn server_no_mechanisms() {
        let server_mechs: Vec<String> = vec![];
        let result = select_sasl_mechanism(&server_mechs, None, true, true);
        assert_eq!(result, None);
    }

    #[test]
    fn external_only_when_server_supports() {
        // Server only offers PLAIN, but we have a cert
        let server_mechs = vec!["PLAIN".to_string()];
        let result = select_sasl_mechanism(&server_mechs, None, true, true);
        // Falls through to PLAIN since server doesn't advertise EXTERNAL
        assert_eq!(result, Some(SaslMechanism::Plain));
    }

    #[test]
    fn case_insensitive_override() {
        let server_mechs = vec!["PLAIN".to_string(), "EXTERNAL".to_string()];
        let result = select_sasl_mechanism(&server_mechs, Some("external"), true, false);
        assert_eq!(result, Some(SaslMechanism::External));
    }

    #[test]
    fn sasl_mechanism_display() {
        assert_eq!(SaslMechanism::Plain.to_string(), "PLAIN");
        assert_eq!(SaslMechanism::External.to_string(), "EXTERNAL");
        assert_eq!(SaslMechanism::ScramSha256.to_string(), "SCRAM-SHA-256");
    }

    #[test]
    fn scram_preferred_over_plain() {
        // When both SCRAM-SHA-256 and PLAIN are available, SCRAM wins
        let server_mechs = vec![
            "PLAIN".to_string(),
            "SCRAM-SHA-256".to_string(),
        ];
        let result = select_sasl_mechanism(&server_mechs, None, false, true);
        assert_eq!(result, Some(SaslMechanism::ScramSha256));
    }

    #[test]
    fn scram_falls_back_to_plain() {
        // Server only offers PLAIN, no SCRAM-SHA-256
        let server_mechs = vec!["PLAIN".to_string()];
        let result = select_sasl_mechanism(&server_mechs, None, false, true);
        assert_eq!(result, Some(SaslMechanism::Plain));
    }

    #[test]
    fn explicit_override_scram() {
        let server_mechs = vec![
            "PLAIN".to_string(),
            "SCRAM-SHA-256".to_string(),
        ];
        let result = select_sasl_mechanism(&server_mechs, Some("SCRAM-SHA-256"), false, true);
        assert_eq!(result, Some(SaslMechanism::ScramSha256));
    }

    #[test]
    fn scram_override_unavailable() {
        // Server doesn't offer SCRAM-SHA-256, override fails
        let server_mechs = vec!["PLAIN".to_string()];
        let result = select_sasl_mechanism(&server_mechs, Some("SCRAM-SHA-256"), false, true);
        assert_eq!(result, None);
    }

    #[test]
    fn external_still_preferred_over_scram() {
        // EXTERNAL > SCRAM-SHA-256 when cert is available
        let server_mechs = vec![
            "PLAIN".to_string(),
            "SCRAM-SHA-256".to_string(),
            "EXTERNAL".to_string(),
        ];
        let result = select_sasl_mechanism(&server_mechs, None, true, true);
        assert_eq!(result, Some(SaslMechanism::External));
    }
}

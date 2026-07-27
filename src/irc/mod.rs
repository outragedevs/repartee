pub mod batch;
pub mod cap;
pub mod chathistory;
pub mod events;
pub mod extban;
pub mod flood;
pub mod formatting;
pub mod handle;
pub mod ignore;
pub mod isupport;
pub mod multiline;
pub mod netsplit;
pub mod sasl_scram;
pub mod typing;

use std::collections::HashSet;

use base64::Engine as _;
use color_eyre::eyre::{Result, eyre};
use futures::StreamExt;
use irc::client::prelude::*;
use tokio::sync::mpsc;

use crate::irc::cap::{DESIRED_CAPS, ServerCaps};
pub use crate::irc::handle::{IrcHandle, IrcSender};
use crate::irc::handle::FLOOD_PENALTY_THRESHOLD_MS;

const IRC_PING_TIMEOUT_SECS: u32 = 60;

/// An IRC event forwarded from the reader task to the main loop.
#[derive(Debug)]
pub enum IrcEvent {
    /// A raw IRC protocol message from the server.
    Message(String, Box<irc::proto::Message>),
    /// Registration complete (`RPL_WELCOME` received). Carries the negotiated
    /// caps and the parsed `draft/multiline` limits (when the cap is active).
    Connected(
        String,
        HashSet<String>,
        Option<crate::irc::multiline::MultilineLimits>,
    ),
    /// The connection was lost, optionally with an error description.
    Disconnected(String, Option<String>),
    /// An IRC handle is ready after async connection completes. Boxed because
    /// it is by far the largest variant, and it carries the connection's flood
    /// budget — the raw `irc::client::Sender` never leaves `crate::irc`.
    HandleReady(Box<IrcHandle>),
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
    /// Parsed `draft/multiline` limits when the cap was enabled, else `None`.
    multiline_limits: Option<crate::irc::multiline::MultilineLimits>,
}

/// SASL authentication mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaslMechanism {
    /// SASL EXTERNAL — client TLS certificate (`CertFP`) based.
    External,
    /// `ECDSA-NIST256P-CHALLENGE` — sign a server challenge with a P-256
    /// private key. No secret leaves the client at all.
    EcdsaNist256pChallenge,
    /// SCRAM over one of SHA-1 / SHA-256 / SHA-512 — challenge-response
    /// (RFC 5802 / RFC 7677); the password is never sent.
    Scram(sasl_scram::ScramHash),
    /// SASL PLAIN — username + password, base64-encoded. The password crosses
    /// the wire, so this ranks last.
    Plain,
}

/// Every mechanism we implement, **strongest first**.
///
/// This is the order auto-detection walks and the order the wizards list.
/// Certificate and key mechanisms put no secret on the wire at all; SCRAM never
/// sends the password; `PLAIN` does, so it comes last. `SCRAM-SHA-1` still
/// outranks `PLAIN` — SHA-1's collision weakness does not touch its use inside
/// HMAC and PBKDF2 here, and it beats a cleartext password.
pub const SASL_MECHANISMS: &[SaslMechanism] = &[
    SaslMechanism::External,
    SaslMechanism::EcdsaNist256pChallenge,
    SaslMechanism::Scram(sasl_scram::ScramHash::Sha512),
    SaslMechanism::Scram(sasl_scram::ScramHash::Sha256),
    SaslMechanism::Scram(sasl_scram::ScramHash::Sha1),
    SaslMechanism::Plain,
];

impl SaslMechanism {
    /// The mechanism name as it appears in the `sasl` capability and in
    /// `AUTHENTICATE`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::External => "EXTERNAL",
            Self::EcdsaNist256pChallenge => "ECDSA-NIST256P-CHALLENGE",
            Self::Scram(hash) => hash.mechanism(),
            Self::Plain => "PLAIN",
        }
    }

    /// Parse a mechanism name, case-insensitively.
    ///
    /// Whole-token match, so `SCRAM-SHA-256-PLUS` resolves to nothing: we do not
    /// implement channel binding and must not answer a `-PLUS` offer with a
    /// plain exchange.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        SASL_MECHANISMS
            .iter()
            .copied()
            .find(|m| m.name().eq_ignore_ascii_case(name))
    }

    /// Does this connection hold the credential this mechanism needs?
    #[must_use]
    const fn prerequisite_met(self, have: SaslCapabilities) -> bool {
        match self {
            Self::External => have.client_cert,
            Self::EcdsaNist256pChallenge => have.sasl_key,
            Self::Scram(_) | Self::Plain => have.password,
        }
    }
}

impl std::fmt::Display for SaslMechanism {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// What this connection can actually authenticate *with* — the local half of
/// mechanism selection, as opposed to what the server offers.
///
/// A struct rather than three positional `bool` arguments: the three are easy
/// to transpose at a call site and impossible to tell apart in a stack trace.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SaslCapabilities {
    /// A TLS client certificate is configured — `EXTERNAL` / `CertFP`.
    pub client_cert: bool,
    /// An ECDSA private key is configured — `ECDSA-NIST256P-CHALLENGE`.
    pub sasl_key: bool,
    /// Both `sasl_user` and `sasl_pass` are set — `PLAIN` and every SCRAM.
    pub password: bool,
}

/// Select the best SASL mechanism given what the server offers and what we hold.
///
/// `advertised` is the mechanism list from the `sasl` capability, or `None` when
/// the server advertised `sasl` without naming one (or did not advertise it —
/// callers check that separately).
///
/// With an explicit `sasl_mechanism_override`, that mechanism is used if we hold
/// its credential **and** the server either advertises it or named nothing at
/// all. Attempting a configured mechanism against a silent server beats
/// authenticating with nothing; an override that cannot be satisfied returns
/// `None` rather than quietly falling back to something weaker.
///
/// Without an override, auto-detection walks [`SASL_MECHANISMS`] and takes the
/// first mechanism we hold the credential for *and* the server named. A server
/// that named nothing gets `PLAIN` when credentials exist — the only mechanism
/// a server that predates mechanism advertisement is likely to speak.
#[must_use]
pub fn select_sasl_mechanism(
    advertised: Option<&[String]>,
    sasl_mechanism_override: Option<&str>,
    have: SaslCapabilities,
) -> Option<SaslMechanism> {
    let named = |list: &[String], mech: SaslMechanism| {
        list.iter().any(|m| m.eq_ignore_ascii_case(mech.name()))
    };

    if let Some(override_mech) = sasl_mechanism_override {
        let Some(mech) = SaslMechanism::from_name(override_mech) else {
            tracing::warn!(
                "unknown SASL mechanism '{override_mech}' in config — not authenticating"
            );
            return None;
        };
        // `is_none_or`: a server that named no mechanisms cannot contradict us.
        if mech.prerequisite_met(have) && advertised.is_none_or(|list| named(list, mech)) {
            return Some(mech);
        }
        tracing::warn!(
            "configured SASL mechanism '{override_mech}' not available (server offers: {}, {have:?})",
            advertised.map_or_else(|| "(unspecified)".to_string(), |l| l.join(","))
        );
        return None;
    }

    // Auto-detect against a server that named nothing: guessing a
    // challenge-response mechanism would burn the one exchange we get.
    let Some(list) = advertised else {
        return have.password.then_some(SaslMechanism::Plain);
    };

    SASL_MECHANISMS
        .iter()
        .copied()
        .find(|m| m.prerequisite_met(have) && named(list, *m))
}

/// Timeout in seconds for SASL authentication steps.
const SASL_TIMEOUT_SECS: u64 = 30;

/// Maximum size of a single `AUTHENTICATE` chunk, per the `IRCv3` SASL spec.
/// A chunk of exactly this length means "more follows".
const AUTHENTICATE_CHUNK_BYTES: usize = 400;

/// Cap on a reassembled inbound `AUTHENTICATE` payload.
///
/// An order of magnitude above any real SCRAM or challenge message, so it only
/// bites a server that streams 400-byte chunks forever.
const MAX_AUTHENTICATE_BYTES: usize = 8192;

/// Map a SASL failure numeric to its message, if this response is one.
const fn sasl_failure(response: Response) -> Option<&'static str> {
    match response {
        Response::ERR_SASLFAIL => Some("SASL authentication failed"),
        Response::ERR_SASLTOOLONG => Some("SASL message too long"),
        Response::ERR_SASLABORT => Some("SASL authentication aborted"),
        Response::ERR_SASLALREADY => Some("already authenticated with SASL"),
        _ => None,
    }
}

/// Concatenate `AUTHENTICATE` chunks and base64-decode the result.
///
/// A bare `+` carries no data: it is both the empty payload and the terminator
/// that follows a chunk of exactly [`AUTHENTICATE_CHUNK_BYTES`], so it is
/// skipped rather than appended.
fn reassemble_authenticate(chunks: &[String]) -> Result<Vec<u8>> {
    let mut payload = String::new();
    for chunk in chunks {
        if chunk == "+" {
            continue;
        }
        if payload.len() + chunk.len() > MAX_AUTHENTICATE_BYTES {
            return Err(eyre!(
                "SASL: AUTHENTICATE payload exceeds {MAX_AUTHENTICATE_BYTES} bytes"
            ));
        }
        payload.push_str(chunk);
    }
    if payload.is_empty() {
        return Ok(Vec::new());
    }
    base64::engine::general_purpose::STANDARD
        .decode(&payload)
        .map_err(|e| eyre!("SASL: invalid base64 in AUTHENTICATE: {e}"))
}

/// Read one inbound `AUTHENTICATE` payload, reassembling chunked continuations.
///
/// The server may split a payload into 400-byte chunks; a chunk shorter than
/// that ends the payload. Without this, a long server-first message decodes as
/// truncated base64 and the exchange dies for no visible reason.
async fn await_authenticate_payload(stream: &mut irc::client::ClientStream) -> Result<Vec<u8>> {
    let result = tokio::time::timeout(std::time::Duration::from_secs(SASL_TIMEOUT_SECS), async {
        let mut chunks: Vec<String> = Vec::new();
        let mut total = 0usize;
        while let Some(msg_result) = stream.next().await {
            let msg = msg_result?;
            match &msg.command {
                Command::AUTHENTICATE(param) => {
                    total += param.len();
                    if total > MAX_AUTHENTICATE_BYTES {
                        return Err(eyre!(
                            "SASL: AUTHENTICATE payload exceeds {MAX_AUTHENTICATE_BYTES} bytes"
                        ));
                    }
                    let is_final = param.len() < AUTHENTICATE_CHUNK_BYTES;
                    chunks.push(param.clone());
                    if is_final {
                        return reassemble_authenticate(&chunks);
                    }
                }
                Command::Response(response, _) => {
                    if let Some(err) = sasl_failure(*response) {
                        return Err(eyre!("{err}"));
                    }
                    // Success where a challenge was due. For SCRAM this means
                    // the server never sent its server-final message, so its
                    // signature cannot be checked — and an unverified 903 is
                    // exactly what a man in the middle would send. Refuse it
                    // rather than sit here until the timeout.
                    if *response == Response::RPL_SASLSUCCESS {
                        return Err(eyre!(
                            "SASL: server reported success without sending the challenge \
                             response we need to verify it"
                        ));
                    }
                }
                _ => {}
            }
        }
        Err(eyre!("connection closed waiting for AUTHENTICATE payload"))
    })
    .await;

    result.unwrap_or_else(|_| Err(eyre!("SASL authentication timed out waiting for a challenge")))
}

/// Wait for the server's `AUTHENTICATE +` reply with a timeout.
///
/// Handles SASL error numerics and connection closure. Used by every SASL
/// mechanism implementation to avoid duplicating the timeout + error handling
/// logic. Mechanisms that expect *data* back use
/// [`await_authenticate_payload`] instead.
async fn await_authenticate_plus(stream: &mut irc::client::ClientStream) -> Result<()> {
    let result = tokio::time::timeout(std::time::Duration::from_secs(SASL_TIMEOUT_SECS), async {
        while let Some(msg_result) = stream.next().await {
            let msg = msg_result?;
            match &msg.command {
                Command::AUTHENTICATE(param) if param == "+" => return Ok(()),
                Command::Response(response, _) => {
                    if let Some(err) = sasl_failure(*response) {
                        return Err(eyre!("{err}"));
                    }
                }
                _ => {}
            }
        }
        Err(eyre!("connection closed waiting for AUTHENTICATE +"))
    })
    .await;

    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(eyre!(
            "SASL authentication timed out waiting for AUTHENTICATE +"
        )),
    }
}

/// Wait for the terminal `903` / `904` of a SASL exchange.
async fn await_sasl_result(stream: &mut irc::client::ClientStream) -> Result<()> {
    while let Some(result) = stream.next().await {
        let msg = result?;
        if let Command::Response(response, _) = &msg.command {
            if *response == Response::RPL_SASLSUCCESS {
                return Ok(());
            }
            if let Some(err) = sasl_failure(*response) {
                return Err(eyre!("{err}"));
            }
        }
    }
    Err(eyre!("SASL authentication: connection closed unexpectedly"))
}

/// Default maximum message body length in bytes.
///
/// IRC protocol limits total message length to 512 bytes including `\r\n`.
/// The server prepends `:nick!user@host ` when relaying, which can consume
/// up to ~160 bytes. Using 350 bytes for the body (matching irc-framework)
/// leaves safe headroom.
pub const MESSAGE_MAX_BYTES: usize = 350;

/// Conservative fallback for `draft/multiline` `max-lines` when the server
/// advertises the cap without that key (the spec marks it RECOMMENDED, not
/// required).
pub const MULTILINE_DEFAULT_MAX_LINES: usize = 24;

/// Runaway backstop for INBOUND `draft/multiline` reassembly: a reassembled
/// message is truncated to this many lines (well below `MAX_BATCH_MESSAGES` in
/// `batch.rs`) so a hostile or buggy server cannot materialise a single message
/// whose wrapped output is thousands of visual lines (an OOM-class regression;
/// see the v0.8.4 render-budget fix).
pub const MULTILINE_MAX_INBOUND_LINES: usize = 100;

/// Split a message into chunks that each fit within `max_bytes` of UTF-8.
///
/// Prefers word boundaries; falls back to character boundaries for very
/// long words. Matches the behaviour of irc-framework's `lineBreak()`.
#[must_use]
pub fn split_irc_message(text: &str, max_bytes: usize) -> Vec<String> {
    if text.len() <= max_bytes {
        return vec![text.to_string()];
    }

    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();

    for word in WordSplitter::new(text) {
        let combined_len = if current.is_empty() {
            word.len()
        } else {
            current.len() + word.len()
        };

        if combined_len <= max_bytes {
            current.push_str(word);
            continue;
        }

        // Word alone fits on a new line — flush current, start new line.
        if word.trim().len() <= max_bytes {
            if !current.is_empty() {
                lines.push(current);
                current = String::new();
            }
            // Don't carry leading whitespace onto a continuation line.
            current.push_str(word.trim_start());
            continue;
        }

        // Word is too long even for a line — break at char boundaries.
        for ch in word.chars() {
            if current.len() + ch.len_utf8() > max_bytes && !current.is_empty() {
                lines.push(current);
                current = String::new();
            }
            current.push(ch);
        }
    }

    if !current.is_empty() {
        lines.push(current);
    }

    lines
}

/// Iterator that yields "word + trailing whitespace" chunks from a string,
/// keeping whitespace attached to the preceding word so the caller can
/// decide where to break.
struct WordSplitter<'a> {
    remaining: &'a str,
}

impl<'a> WordSplitter<'a> {
    const fn new(s: &'a str) -> Self {
        Self { remaining: s }
    }
}

impl<'a> Iterator for WordSplitter<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        if self.remaining.is_empty() {
            return None;
        }

        // Find end of non-whitespace (the word).
        let word_end = self
            .remaining
            .find(char::is_whitespace)
            .unwrap_or(self.remaining.len());

        // Find end of trailing whitespace.
        let ws_end = self.remaining[word_end..]
            .find(|c: char| !c.is_whitespace())
            .map_or(self.remaining.len(), |pos| word_end + pos);

        let chunk = &self.remaining[..ws_end];
        self.remaining = &self.remaining[ws_end..];
        Some(chunk)
    }
}

/// Resolve the effective local bind IP for an outgoing IRC socket.
///
/// Precedence (highest first):
///   1. `server.bind_ip` — explicit per-server config (or `/connect
///      -bind=` runtime override).
///   2. `cli_override` — `repartee -h <ip>` CLI flag (runtime only).
///   3. `general.default_bind_ip` — host-wide fallback from
///      `[general]` in `config.toml`.
///   4. `None` — let the kernel choose via the routing table.
///
/// Pulled into a free function with explicit args so it's trivially
/// testable and behaves identically across the `/connect` and
/// autoconnect/reconnect spawn paths.
pub fn resolve_bind_ip(
    server: &crate::config::ServerConfig,
    cli_override: Option<&str>,
    general: &crate::config::GeneralConfig,
) -> Option<String> {
    server
        .bind_ip
        .clone()
        .or_else(|| cli_override.map(str::to_string))
        .or_else(|| general.default_bind_ip.clone())
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
#[expect(clippy::too_many_lines, reason = "IRC config builder with many fields")]
pub async fn connect_server(
    conn_id: &str,
    server_config: &crate::config::ServerConfig,
    general: &crate::config::GeneralConfig,
) -> Result<(IrcHandle, mpsc::Receiver<IrcEvent>)> {
    let nick = server_config.nick.as_deref().unwrap_or(&general.nick);
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

    // Baked into this connection's `Outgoing` for its whole life — a later
    // `/flood off` only affects connections opened after the toggle, so the
    // handle's own budget carries the same number.
    let penalty_threshold = if general.flood_protection {
        FLOOD_PENALTY_THRESHOLD_MS
    } else {
        0 // disabled: the crate applies no penalty at all
    };

    // Pass channels to the irc crate so it handles batched autojoin
    // on ENDOFMOTD. Entries like "#channel key" are split into the
    // channels vec (name only) and channel_keys map.
    //
    // Hoisted out of the `Config` literal because `CrateEcho` has to predict the
    // JOIN batch the crate will build from exactly these two values — see
    // `crate::irc::handle`. Two copies of this could drift; one cannot.
    let autojoin_channels: Vec<String> = server_config
        .channels
        .iter()
        .map(|e| {
            e.split_once(' ')
                .map_or_else(|| e.clone(), |(c, _)| c.to_string())
        })
        .collect();
    let autojoin_keys: std::collections::HashMap<String, String> = server_config
        .channels
        .iter()
        .filter_map(|e| {
            e.split_once(' ')
                .map(|(c, k)| (c.to_string(), k.to_string()))
        })
        .collect();

    // Everything the crate will auto-send from, mirrored so the flood budget can
    // charge frames that never pass through `IrcSender`.
    let echo_config = crate::irc::handle::CrateEchoConfig {
        ctcp_version: general.ctcp_version.clone(),
        username: username.to_string(),
        realname: realname.to_string(),
        channels: autojoin_channels.clone(),
        channel_keys: autojoin_keys.clone(),
        alt_nicks: alt_nicks.clone(),
    };

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
        channels: autojoin_channels,
        channel_keys: autojoin_keys,
        encoding: server_config.encoding.clone(),
        version: Some(general.ctcp_version.clone()),
        client_cert_path: server_config.client_cert_path.clone(),
        bind_address: server_config.bind_ip.clone(),
        ping_timeout: Some(IRC_PING_TIMEOUT_SECS),
        flood_penalty_threshold: Some(penalty_threshold),
        ..Config::default()
    };

    let mut client = Client::from_config(irc_config).await?;
    let local_ip = client.local_addr().map(|a| a.ip());
    // The connection's budget starts here, not at `IrcHandle::new`: registration
    // itself (NICK + USER, ~7000ms) is charged by the crate's `Outgoing` like
    // any other traffic, and the handle must inherit that — see `IrcHandle::new`.
    let sender = IrcSender::new(client.sender(), u64::from(penalty_threshold));
    let mut stream = client.stream()?;
    // Extract the outgoing task handle so we can abort it on disconnect.
    // Without this, the Pinger inside Outgoing holds a tx_outgoing clone
    // that keeps the write half of the TCP socket alive (CLOSE-WAIT leak).
    let outgoing_handle = client.outgoing_handle.take();

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

    let (tx, rx) = mpsc::channel(4096);
    let id = conn_id.to_string();
    let id2 = id.clone();

    // The crate emits frames of its own from `ClientState::handle_message`,
    // which runs inside `stream.poll_next` — i.e. exactly when a message pops
    // out of the loop below. Those frames go on the same charged lane as ours
    // but never pass through `IrcSender`, so the budget has to book them here or
    // it under-reads and we hand typing to a queue that is already throttling.
    // The clone shares the budget: it IS this connection. See `crate::irc::handle`.
    let echo_sender = sender.clone();
    let mut echo = crate::irc::handle::CrateEcho::new(echo_config);

    // Spawn reader task
    tokio::spawn(async move {
        // Send negotiation diagnostics immediately so they're visible even if
        // registration fails (e.g. server requires SASL but auth didn't complete).
        let _ = tx
            .send(IrcEvent::NegotiationInfo(id.clone(), neg.diagnostics))
            .await;

        let mut sent_connected = false;
        let mut error = None;

        // Replay messages consumed during capability negotiation.
        // Non-IRCv3 servers that silently ignore CAP send RPL_WELCOME during
        // negotiation; pre-registration NOTICEs may also be collected here.
        //
        // These already went through the crate's `handle_message` (negotiation
        // reads the same stream), so anything they made it send has already been
        // charged to the real counter — the mirror has to catch up on them too.
        for message in neg.early_messages {
            for frame in echo.frames_for(&message) {
                echo_sender.charge(&frame, std::time::Instant::now());
            }
            if !sent_connected && let Command::Response(Response::RPL_WELCOME, _) = &message.command
            {
                sent_connected = true;
                let _ = tx
                    .send(IrcEvent::Connected(
                        id.clone(),
                        neg.enabled_caps.clone(),
                        neg.multiline_limits,
                    ))
                    .await;
            }
            if tx
                .send(IrcEvent::Message(id.clone(), Box::new(message)))
                .await
                .is_err()
            {
                return;
            }
        }

        // Continue reading from the stream.
        while let Some(result) = stream.next().await {
            match result {
                Ok(message) => {
                    // The crate has just handled this message inside `poll_next`
                    // and may already have queued a CTCP reply, the autojoin
                    // batch or a NICK retry. Book them before anything else can
                    // ask this connection for typing headroom.
                    for frame in echo.frames_for(&message) {
                        echo_sender.charge(&frame, std::time::Instant::now());
                    }
                    if !sent_connected
                        && let Command::Response(Response::RPL_WELCOME, _) = &message.command
                    {
                        sent_connected = true;
                        let _ = tx
                            .send(IrcEvent::Connected(
                        id.clone(),
                        neg.enabled_caps.clone(),
                        neg.multiline_limits,
                    ))
                            .await;
                    }
                    if tx
                        .send(IrcEvent::Message(id.clone(), Box::new(message)))
                        .await
                        .is_err()
                    {
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
        let _ = tx.send(IrcEvent::Disconnected(id, error)).await;
    });

    Ok((IrcHandle::new(id2, sender, local_ip, outgoing_handle), rx))
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

/// The frames that open a connection, in order: `CAP LS 302`, optional `PASS`,
/// `NICK`, `USER`.
///
/// A free function so the flood cost of registration can be asserted without a
/// socket: the crate charges these like any other traffic (`NICK` 3000 + 1000
/// length, `USER` 2000 + 1000 — `PASS` and `CAP` are exempt), and the budget
/// the connection is handed to the app must already reflect them.
fn registration_frames(params: &RegistrationParams<'_>) -> Vec<irc::proto::Message> {
    use irc::proto::command::CapSubCommand;

    let mut frames = vec![
        Command::CAP(None, CapSubCommand::LS, Some("302".to_string()), None).into(),
    ];
    if let Some(pass) = params.password {
        frames.push(Command::PASS(pass.to_string()).into());
    }
    frames.push(Command::NICK(params.nick.to_string()).into());
    frames.push(
        Command::USER(
            params.username.to_string(),
            "0".to_string(),
            params.realname.to_string(),
        )
        .into(),
    );
    frames
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
    sender: &IrcSender,
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
    for frame in registration_frames(params) {
        sender.send(frame)?;
    }

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
        let have = SaslCapabilities {
            client_cert: params.has_client_cert,
            sasl_key: false,
            password: params.sasl_user.is_some() && params.sasl_pass.is_some(),
        };
        let advertised = server_caps.sasl_mechanisms_advertised();
        let selected_mechanism = select_sasl_mechanism(
            advertised.as_deref(),
            params.sasl_mechanism_override,
            have,
        );
        let want_sasl = selected_mechanism.is_some();

        diag.push(format!(
            "CAP: server advertises sasl={} ({}), {have:?}, mechanism={}",
            server_caps.has("sasl"),
            advertised.as_ref().map_or_else(
                || "mechanisms unspecified".to_string(),
                |list| list.join(",")
            ),
            selected_mechanism.map_or_else(|| "none".to_string(), |m| m.to_string()),
        ));

        // Compute capabilities to request
        let mut caps_to_request = server_caps.negotiate(DESIRED_CAPS);

        if !want_sasl {
            caps_to_request.retain(|c| c != "sasl");
        }

        // Put sasl LAST so other caps are ACK'd regardless of SASL outcome
        let sasl_requested = caps_to_request
            .iter()
            .position(|c| c == "sasl")
            .is_some_and(|pos| {
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
            sender.send(Command::CAP(None, CapSubCommand::REQ, None, Some(req_str)))?;

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
                    SaslMechanism::EcdsaNist256pChallenge => Err(eyre!(
                        "SASL ECDSA-NIST256P-CHALLENGE selected but no key is configured"
                    )),
                    SaslMechanism::Scram(hash) => {
                        if let (Some(user), Some(pass)) = (params.sasl_user, params.sasl_pass) {
                            diag.push(format!("SASL: authenticating via {hash} as {user}"));
                            run_sasl_scram(sender, stream, hash, user, pass).await
                        } else {
                            Err(eyre!("SASL {hash} selected but credentials missing"))
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
        } else if !sasl_requested && have.password {
            diag.push("SASL: credentials available but server does not advertise sasl".to_string());
        }

        // Send CAP END to finish capability negotiation.
        // The server will now process the held NICK/USER commands.
        sender.send(Command::CAP(None, CapSubCommand::END, None, None))?;
    }

    // NICK/USER were already sent in step 1 — no need to send them again.

    // The cap value (max-bytes/max-lines) lives on `server_caps`; the ACK loop
    // only records bare names in `enabled_caps`, so read the limits from there.
    let multiline_limits = if enabled_caps.contains("draft/multiline") {
        crate::irc::multiline::parse_limits(server_caps.value("draft/multiline"))
    } else {
        None
    };

    Ok(NegotiateResult {
        enabled_caps,
        diagnostics: diag,
        early_messages,
        multiline_limits,
    })
}

/// Execute the SASL PLAIN authentication handshake.
///
/// Assumes SASL has already been ACK'd.  Sends `AUTHENTICATE PLAIN`,
/// waits for `+`, sends base64-encoded credentials, waits for 903/904.
async fn run_sasl_plain(
    sender: &IrcSender,
    stream: &mut irc::client::ClientStream,
    sasl_user: &str,
    sasl_pass: &str,
) -> Result<()> {
    // Send AUTHENTICATE PLAIN
    sender.send(Command::AUTHENTICATE(
        SaslMechanism::Plain.name().to_string(),
    ))?;

    // Wait for AUTHENTICATE + from server (with timeout and error handling)
    await_authenticate_plus(stream).await?;

    // Send base64-encoded credentials: authzid\0authcid\0password.
    // RFC 4616 requires SASLprep on both the authcid and the password.
    let user = sasl_scram::saslprep(sasl_user);
    let pass = sasl_scram::saslprep(sasl_pass);
    let auth_string = format!("{user}\x00{user}\x00{pass}");
    let encoded = base64::engine::general_purpose::STANDARD.encode(auth_string);
    for chunk in sasl_scram::chunk_authenticate(&encoded) {
        sender.send(Command::AUTHENTICATE(chunk))?;
    }

    // Wait for 903 (success) or 904/905/906/907 (failure)
    await_sasl_result(stream).await
}

/// Execute a SASL SCRAM authentication handshake over the given hash.
///
/// Assumes SASL has already been ACK'd.  Performs the three-step
/// challenge-response protocol:
///
/// 1. Send `AUTHENTICATE SCRAM-SHA-<n>`, wait for `+`
/// 2. Send base64-encoded client-first message, receive server-first
/// 3. Send base64-encoded client-final message, receive server-final
/// 4. Verify server signature and wait for 903/904
async fn run_sasl_scram(
    sender: &IrcSender,
    stream: &mut irc::client::ClientStream,
    hash: sasl_scram::ScramHash,
    sasl_user: &str,
    sasl_pass: &str,
) -> Result<()> {
    use base64::Engine as _;

    let b64 = &base64::engine::general_purpose::STANDARD;

    // Step 1: Initiate SCRAM over the selected hash
    sender.send(Command::AUTHENTICATE(hash.mechanism().to_string()))?;

    // Wait for AUTHENTICATE + from server (with timeout and error handling)
    await_authenticate_plus(stream).await?;

    // Step 2: Send client-first message
    let (client_first_bare, client_first_full, client_nonce) = sasl_scram::client_first(sasl_user);
    let encoded = b64.encode(&client_first_full);
    for chunk in sasl_scram::chunk_authenticate(&encoded) {
        sender.send(Command::AUTHENTICATE(chunk))?;
    }

    // Step 3: Receive server-first message, reassembling chunked continuations
    let server_first_bytes = await_authenticate_payload(stream).await?;
    let server_first = String::from_utf8(server_first_bytes)
        .map_err(|e| eyre!("SCRAM: non-UTF-8 server-first: {e}"))?;

    // Step 4: Compute and send client-final message
    let (client_final_msg, expected_server_sig) = sasl_scram::client_final(
        hash,
        &server_first,
        &client_first_bare,
        &client_nonce,
        sasl_pass,
    )?;
    let encoded_final = b64.encode(&client_final_msg);
    for chunk in sasl_scram::chunk_authenticate(&encoded_final) {
        sender.send(Command::AUTHENTICATE(chunk))?;
    }

    // Step 5: Receive server-final and verify it before accepting 903.
    //
    // The verification is the point of SCRAM: it proves the peer knows the
    // stored key, so a 903 from an attacker who intercepted the exchange is
    // worthless without it.
    let server_final_bytes = await_authenticate_payload(stream).await?;
    let server_final = String::from_utf8(server_final_bytes)
        .map_err(|e| eyre!("SCRAM: non-UTF-8 server-final: {e}"))?;
    if !sasl_scram::verify_server(&server_final, &expected_server_sig) {
        return Err(eyre!(
            "SCRAM: server signature verification failed — possible MITM"
        ));
    }

    await_sasl_result(stream).await
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
    sender: &IrcSender,
    stream: &mut irc::client::ClientStream,
) -> Result<()> {
    // Send AUTHENTICATE EXTERNAL
    sender.send(Command::AUTHENTICATE(
        SaslMechanism::External.name().to_string(),
    ))?;

    // Wait for AUTHENTICATE + from server (with timeout and error handling)
    await_authenticate_plus(stream).await?;

    // Send AUTHENTICATE + (base64 encoding of an empty string is "+")
    sender.send(Command::AUTHENTICATE("+".to_string()))?;

    // Wait for 903 (success) or 904/905/906/907 (failure)
    await_sasl_result(stream).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn reg_params(password: Option<&str>) -> RegistrationParams<'_> {
        RegistrationParams {
            nick: "bob",
            username: "bob",
            realname: "Bob",
            password,
            sasl_user: None,
            sasl_pass: None,
            sasl_mechanism_override: None,
            has_client_cert: false,
        }
    }

    #[test]
    fn registration_charges_the_connection_budget_before_the_app_sees_it() {
        // Regression: the handle used to mint a *zeroed* budget after
        // registration had already spent ~7000ms of the crate's real counter
        // (NICK 3000+1000, USER 2000+1000). A `/query` in the first seconds of a
        // connection then read 0, let a TAGMSG out, and the user's first real
        // PRIVMSG landed on a counter near the threshold and was delayed.
        // Registration now runs through the *same* `IrcSender` the handle
        // carries, so the budget it hands the app is already charged.
        let now = Instant::now();
        let sender = IrcSender::capturing(u64::from(FLOOD_PENALTY_THRESHOLD_MS));
        assert!(
            sender.has_typing_headroom_at(now),
            "a fresh connection starts with headroom"
        );

        for frame in registration_frames(&reg_params(None)) {
            sender.send_at(frame, now).unwrap();
        }

        // NICK 3000+1000, USER 2000+1000; CAP LS is exempt.
        assert_eq!(
            sender.penalty_ms(),
            7000,
            "registration must be charged, not free"
        );
        // 7000 of the 10_000 threshold is spent, and the gate only lets a
        // TAGMSG (3000) through at or below 3000 — so typing is shut out for
        // the first seconds of the connection, which is the whole point.
        assert!(
            !sender.has_typing_headroom_at(now),
            "registration must leave no typing headroom at t=0"
        );

        // And the handle the app gets inherits that budget rather than a fresh
        // zeroed one — which is what made the bug reachable.
        let handle = IrcHandle::new("net".to_string(), sender, None, None);
        assert_eq!(
            handle.sender().penalty_ms(),
            7000,
            "the handle must inherit the charged budget, not a zeroed one"
        );

        // It is a penalty, not a ban: it drains like any other, so typing opens
        // up once enough of it has bled off (7000 - 4000ms of drain = 3000).
        assert!(
            !handle
                .sender()
                .has_typing_headroom_at(now + Duration::from_secs(3))
        );
        assert!(
            handle
                .sender()
                .has_typing_headroom_at(now + Duration::from_secs(4))
        );
    }

    #[test]
    fn a_password_does_not_change_the_registration_cost() {
        // PASS is exempt in the crate's table — charged nothing, not even its
        // length penalty — so a password-protected server costs the same 7000ms.
        let now = Instant::now();
        let with_pass = IrcSender::capturing(u64::from(FLOOD_PENALTY_THRESHOLD_MS));
        for frame in registration_frames(&reg_params(Some("hunter2"))) {
            with_pass.send_at(frame, now).unwrap();
        }
        assert_eq!(with_pass.captured().len(), 4, "CAP, PASS, NICK, USER");
        assert_eq!(with_pass.penalty_ms(), 7000);
    }

    #[test]
    fn registration_is_not_charged_when_flood_protection_is_off() {
        // Threshold 0: the crate applies no penalty at all, so the mirror must
        // not invent one — and typing is never gated.
        let now = Instant::now();
        let sender = IrcSender::capturing(0);
        for frame in registration_frames(&reg_params(None)) {
            sender.send_at(frame, now).unwrap();
        }
        assert_eq!(sender.penalty_ms(), 0);
        assert!(sender.has_typing_headroom_at(now));
    }

    // ── SASL mechanism selection ────────────────────────────

    use crate::irc::sasl_scram::ScramHash;

    /// Server-advertised mechanism list, as `select_sasl_mechanism` takes it.
    fn offers(mechs: &[&str]) -> Vec<String> {
        mechs.iter().map(|m| (*m).to_string()).collect()
    }

    /// Everything configured: cert, ECDSA key, and user+pass.
    const ALL_CREDS: SaslCapabilities = SaslCapabilities {
        client_cert: true,
        sasl_key: true,
        password: true,
    };
    /// Only a password — the common case.
    const PASSWORD_ONLY: SaslCapabilities = SaslCapabilities {
        client_cert: false,
        sasl_key: false,
        password: true,
    };

    #[test]
    fn auto_detect_walks_the_ladder_strongest_first() {
        // With every credential and every mechanism on offer, the strongest
        // wins; strike the winner off the server's list and the next one does.
        let ladder = [
            (
                vec![
                    "PLAIN",
                    "SCRAM-SHA-1",
                    "SCRAM-SHA-256",
                    "SCRAM-SHA-512",
                    "ECDSA-NIST256P-CHALLENGE",
                    "EXTERNAL",
                ],
                SaslMechanism::External,
            ),
            (
                vec![
                    "PLAIN",
                    "SCRAM-SHA-1",
                    "SCRAM-SHA-256",
                    "SCRAM-SHA-512",
                    "ECDSA-NIST256P-CHALLENGE",
                ],
                SaslMechanism::EcdsaNist256pChallenge,
            ),
            (
                vec!["PLAIN", "SCRAM-SHA-1", "SCRAM-SHA-256", "SCRAM-SHA-512"],
                SaslMechanism::Scram(ScramHash::Sha512),
            ),
            (
                vec!["PLAIN", "SCRAM-SHA-1", "SCRAM-SHA-256"],
                SaslMechanism::Scram(ScramHash::Sha256),
            ),
            (
                vec!["PLAIN", "SCRAM-SHA-1"],
                SaslMechanism::Scram(ScramHash::Sha1),
            ),
            (vec!["PLAIN"], SaslMechanism::Plain),
            (vec![], SaslMechanism::Plain),
        ];

        for (server, expected) in ladder {
            let list = offers(&server);
            let got = select_sasl_mechanism(Some(&list), None, ALL_CREDS);
            if server.is_empty() {
                // A server that offers nothing gets nothing, credentials or not.
                assert_eq!(got, None, "empty offer list must select nothing");
            } else {
                assert_eq!(got, Some(expected), "server offering {server:?}");
            }
        }
    }

    #[test]
    fn every_mechanism_needs_its_own_credential() {
        let all = offers(&[
            "PLAIN",
            "SCRAM-SHA-256",
            "ECDSA-NIST256P-CHALLENGE",
            "EXTERNAL",
        ]);
        // No cert → EXTERNAL is skipped even though the server offers it.
        let no_cert = SaslCapabilities {
            client_cert: false,
            ..ALL_CREDS
        };
        assert_eq!(
            select_sasl_mechanism(Some(&all), None, no_cert),
            Some(SaslMechanism::EcdsaNist256pChallenge)
        );
        // No key either → falls to SCRAM.
        let password_only = PASSWORD_ONLY;
        assert_eq!(
            select_sasl_mechanism(Some(&all), None, password_only),
            Some(SaslMechanism::Scram(ScramHash::Sha256))
        );
        // Nothing configured → no SASL at all.
        assert_eq!(
            select_sasl_mechanism(Some(&all), None, SaslCapabilities::default()),
            None
        );
        // An ECDSA key alone is enough for the key mechanism.
        let key_only = SaslCapabilities {
            client_cert: false,
            sasl_key: true,
            password: false,
        };
        assert_eq!(
            select_sasl_mechanism(Some(&all), None, key_only),
            Some(SaslMechanism::EcdsaNist256pChallenge)
        );
    }

    #[test]
    fn channel_binding_variants_are_never_selected() {
        // We do not implement channel binding; answering a -PLUS offer with a
        // plain exchange would be a downgrade the server cannot detect.
        let plus_only = offers(&["SCRAM-SHA-256-PLUS", "SCRAM-SHA-512-PLUS"]);
        assert_eq!(select_sasl_mechanism(Some(&plus_only), None, ALL_CREDS), None);
        assert_eq!(
            select_sasl_mechanism(Some(&plus_only), Some("SCRAM-SHA-256"), ALL_CREDS),
            None
        );
        assert_eq!(SaslMechanism::from_name("SCRAM-SHA-256-PLUS"), None);
    }

    #[test]
    fn an_override_outranks_the_ladder() {
        let all = offers(&["PLAIN", "SCRAM-SHA-512", "EXTERNAL"]);
        // PLAIN would never win auto-detection here — the override forces it.
        assert_eq!(
            select_sasl_mechanism(Some(&all), Some("PLAIN"), ALL_CREDS),
            Some(SaslMechanism::Plain)
        );
        // Case-insensitively.
        assert_eq!(
            select_sasl_mechanism(Some(&all), Some("scram-sha-512"), ALL_CREDS),
            Some(SaslMechanism::Scram(ScramHash::Sha512))
        );
    }

    #[test]
    fn an_unsatisfiable_override_authenticates_with_nothing() {
        let plain_only = offers(&["PLAIN"]);
        // Server does not offer it → no silent downgrade to PLAIN.
        assert_eq!(
            select_sasl_mechanism(Some(&plain_only), Some("SCRAM-SHA-512"), ALL_CREDS),
            None
        );
        // We do not hold its credential → likewise.
        let all = offers(&["PLAIN", "EXTERNAL", "ECDSA-NIST256P-CHALLENGE"]);
        assert_eq!(
            select_sasl_mechanism(Some(&all), Some("EXTERNAL"), PASSWORD_ONLY),
            None
        );
        assert_eq!(
            select_sasl_mechanism(Some(&all), Some("ECDSA-NIST256P-CHALLENGE"), PASSWORD_ONLY),
            None
        );
        // A name we do not implement at all.
        assert_eq!(
            select_sasl_mechanism(Some(&all), Some("OAUTHBEARER"), ALL_CREDS),
            None
        );
    }

    #[test]
    fn a_server_that_names_no_mechanisms_still_honours_an_override() {
        // `sasl` advertised bare: we know nothing, so a configured mechanism is
        // attempted rather than dropped. This is the case that used to lose a
        // configured SCRAM to the "assume PLAIN" default.
        assert_eq!(
            select_sasl_mechanism(None, Some("SCRAM-SHA-512"), ALL_CREDS),
            Some(SaslMechanism::Scram(ScramHash::Sha512))
        );
        assert_eq!(
            select_sasl_mechanism(None, Some("EXTERNAL"), ALL_CREDS),
            Some(SaslMechanism::External)
        );
        // But the credential still has to be there.
        assert_eq!(
            select_sasl_mechanism(None, Some("EXTERNAL"), PASSWORD_ONLY),
            None
        );
        // And auto-detect does not guess a challenge-response mechanism — it
        // falls back to the one such a server is likely to speak.
        assert_eq!(
            select_sasl_mechanism(None, None, ALL_CREDS),
            Some(SaslMechanism::Plain)
        );
        assert_eq!(
            select_sasl_mechanism(None, None, SaslCapabilities::default()),
            None
        );
    }

    #[test]
    fn mechanism_names_round_trip() {
        for mech in SASL_MECHANISMS.iter().copied() {
            assert_eq!(SaslMechanism::from_name(mech.name()), Some(mech));
            assert_eq!(
                SaslMechanism::from_name(&mech.name().to_lowercase()),
                Some(mech)
            );
            assert_eq!(mech.to_string(), mech.name());
        }
        assert_eq!(SaslMechanism::Plain.to_string(), "PLAIN");
        assert_eq!(SaslMechanism::External.to_string(), "EXTERNAL");
        assert_eq!(
            SaslMechanism::Scram(ScramHash::Sha256).to_string(),
            "SCRAM-SHA-256"
        );
        assert_eq!(
            SaslMechanism::EcdsaNist256pChallenge.to_string(),
            "ECDSA-NIST256P-CHALLENGE"
        );
    }

    // ── inbound AUTHENTICATE reassembly ─────────────────────

    fn b64(data: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(data)
    }

    #[test]
    fn a_single_short_chunk_is_the_whole_payload() {
        let encoded = b64(b"r=nonce,s=c2FsdA==,i=4096");
        assert_eq!(
            reassemble_authenticate(&[encoded]).unwrap(),
            b"r=nonce,s=c2FsdA==,i=4096"
        );
    }

    #[test]
    fn chunks_are_concatenated_before_decoding() {
        // A payload long enough that the server has to split it. Decoding the
        // chunks individually would fail or truncate — the whole point.
        let payload = vec![b'x'; 700];
        let encoded = b64(&payload);
        assert!(encoded.len() > AUTHENTICATE_CHUNK_BYTES);
        let chunks: Vec<String> = encoded
            .as_bytes()
            .chunks(AUTHENTICATE_CHUNK_BYTES)
            .map(|c| String::from_utf8(c.to_vec()).unwrap())
            .collect();
        assert!(chunks.len() > 1, "payload must actually be split");
        assert_eq!(reassemble_authenticate(&chunks).unwrap(), payload);
    }

    #[test]
    fn a_bare_plus_carries_no_data() {
        assert_eq!(reassemble_authenticate(&["+".to_string()]).unwrap(), Vec::<u8>::new());

        // A payload that lands on exactly 400 base64 bytes is terminated by a
        // trailing "+", which must not be appended to the base64.
        let exact = "A".repeat(AUTHENTICATE_CHUNK_BYTES);
        let with_terminator = vec![exact.clone(), "+".to_string()];
        assert_eq!(
            reassemble_authenticate(&with_terminator).unwrap(),
            reassemble_authenticate(&[exact]).unwrap()
        );
    }

    #[test]
    fn an_endless_stream_of_chunks_is_refused() {
        let flood: Vec<String> = (0..40)
            .map(|_| "A".repeat(AUTHENTICATE_CHUNK_BYTES))
            .collect();
        let err = reassemble_authenticate(&flood).unwrap_err().to_string();
        assert!(err.contains("exceeds"), "unexpected error: {err}");
    }

    #[test]
    fn undecodable_base64_is_reported_not_silently_dropped() {
        let err = reassemble_authenticate(&["not base64!!!".to_string()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("base64"), "unexpected error: {err}");
    }

    // ── split_irc_message tests ─────────────────────────────

    #[test]
    fn short_message_not_split() {
        let result = split_irc_message("hello world", 350);
        assert_eq!(result, vec!["hello world"]);
    }

    #[test]
    fn split_at_word_boundary() {
        // 10-byte limit: "hello " is 6 bytes, "world" is 5 bytes — total 11 > 10
        let result = split_irc_message("hello world", 10);
        assert_eq!(result, vec!["hello ", "world"]);
    }

    #[test]
    fn split_long_text_multiple_chunks() {
        let text = "aaa bbb ccc ddd eee fff";
        let result = split_irc_message(text, 12);
        // "aaa bbb ccc " = 12, "ddd eee fff" = 11
        assert_eq!(result.len(), 2);
        // Each chunk should be <= 12 bytes
        for chunk in &result {
            assert!(chunk.len() <= 12, "chunk too long: '{chunk}'");
        }
    }

    #[test]
    fn very_long_word_split_at_chars() {
        let text = "abcdefghij";
        let result = split_irc_message(text, 5);
        assert_eq!(result, vec!["abcde", "fghij"]);
    }

    #[test]
    fn empty_message() {
        let result = split_irc_message("", 350);
        assert_eq!(result, vec![""]);
    }

    #[test]
    fn unicode_message_split() {
        // Each emoji is 4 bytes. 3 emojis = 12 bytes.
        let text = "🦀🦀🦀";
        let result = split_irc_message(text, 8);
        assert_eq!(result, vec!["🦀🦀", "🦀"]);
    }

    #[test]
    fn lorem_ipsum_split() {
        let text = "Lorem ipsum dolor sit amet, consetetur sadipscing elitr, \
                    sed diam nonumy eirmod tempor invidunt ut labore et dolore \
                    magna aliquyam erat, sed diam voluptua.";
        let result = split_irc_message(text, MESSAGE_MAX_BYTES);
        // The full text is ~170 bytes, well under 350 — should not be split.
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn lorem_ipsum_long_split() {
        let text = "Lorem ipsum dolor sit amet, consetetur sadipscing elitr, \
                    sed diam nonumy eirmod tempor invidunt ut labore et dolore \
                    magna aliquyam erat, sed diam voluptua. At vero eos et accusam \
                    et justo duo dolores et ea rebum. Stet clita kasd gubergren, \
                    no sea takimata sanctus est Lorem ipsum dolor sit amet. Lorem \
                    ipsum dolor sit amet, consetetur sadipscing elitr, sed diam \
                    nonumy eirmod tempor invidunt ut labore et dolore magna \
                    aliquyam erat, sed diam voluptua.";
        let result = split_irc_message(text, MESSAGE_MAX_BYTES);
        assert!(
            result.len() >= 2,
            "expected multiple chunks, got {}",
            result.len()
        );
        // Every chunk must fit within the limit.
        for (i, chunk) in result.iter().enumerate() {
            assert!(
                chunk.len() <= MESSAGE_MAX_BYTES,
                "chunk {i} is {} bytes (max {MESSAGE_MAX_BYTES}): '{chunk}'",
                chunk.len(),
            );
        }
        // Reassembled text should equal original (minus whitespace trimming at break points).
        let reassembled: String = result.join(" ");
        // Word-level comparison (whitespace at break points may differ).
        let orig_words: Vec<&str> = text.split_whitespace().collect();
        let re_words: Vec<&str> = reassembled.split_whitespace().collect();
        assert_eq!(orig_words, re_words);
    }

    fn server_with(bind: Option<&str>) -> crate::config::ServerConfig {
        crate::config::ServerConfig {
            label: "test".into(),
            address: "irc.example".into(),
            port: 6697,
            tls: true,
            tls_verify: true,
            autoconnect: false,
            channels: vec![],
            nick: None,
            username: None,
            realname: None,
            password: None,
            sasl_user: None,
            sasl_pass: None,
            bind_ip: bind.map(str::to_string),
            encoding: None,
            auto_reconnect: None,
            reconnect_delay: None,
            reconnect_max_retries: None,
            autosendcmd: None,
            sasl_mechanism: None,
            client_cert_path: None,
        }
    }

    fn general_with(default_bind: Option<&str>) -> crate::config::GeneralConfig {
        crate::config::GeneralConfig {
            default_bind_ip: default_bind.map(str::to_string),
            ..crate::config::GeneralConfig::default()
        }
    }

    #[test]
    fn resolve_bind_ip_prefers_per_server() {
        let s = server_with(Some("10.0.0.1"));
        let g = general_with(Some("10.0.0.99"));
        let r = resolve_bind_ip(&s, Some("10.0.0.50"), &g);
        // Per-server wins over both CLI and default.
        assert_eq!(r.as_deref(), Some("10.0.0.1"));
    }

    #[test]
    fn resolve_bind_ip_falls_back_to_cli() {
        let s = server_with(None);
        let g = general_with(Some("10.0.0.99"));
        let r = resolve_bind_ip(&s, Some("10.0.0.50"), &g);
        // No per-server bind: CLI override beats general default.
        assert_eq!(r.as_deref(), Some("10.0.0.50"));
    }

    #[test]
    fn resolve_bind_ip_falls_back_to_default() {
        let s = server_with(None);
        let g = general_with(Some("10.0.0.99"));
        let r = resolve_bind_ip(&s, None, &g);
        assert_eq!(r.as_deref(), Some("10.0.0.99"));
    }

    #[test]
    fn resolve_bind_ip_returns_none_when_nothing_set() {
        let s = server_with(None);
        let g = general_with(None);
        let r = resolve_bind_ip(&s, None, &g);
        assert_eq!(r, None);
    }

    #[test]
    fn resolve_bind_ip_empty_string_treated_as_value() {
        // Defensive: an empty string IS Some("") and would propagate
        // — make the precedence rule explicit so a future regression
        // (e.g. someone normalises empty -> None at a different layer)
        // is caught here rather than at runtime.
        let s = server_with(Some(""));
        let g = general_with(Some("10.0.0.99"));
        let r = resolve_bind_ip(&s, None, &g);
        assert_eq!(r.as_deref(), Some(""));
    }
}

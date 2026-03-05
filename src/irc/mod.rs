pub mod events;
pub mod flood;
pub mod formatting;
pub mod ignore;
pub mod netsplit;

use color_eyre::eyre::Result;
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
        password: server_config.password.clone(),
        channels: server_config.channels.clone(),
        ..Config::default()
    };

    let mut client = Client::from_config(irc_config).await?;
    let sender = client.sender();
    let mut stream = client.stream()?;
    client.identify()?;

    let (tx, rx) = mpsc::unbounded_channel();
    let id = conn_id.to_string();
    let id2 = id.clone();

    // Spawn reader task
    tokio::spawn(async move {
        let _ = tx.send(IrcEvent::Connected(id.clone()));
        while let Some(result) = stream.next().await {
            match result {
                Ok(message) => {
                    if tx.send(IrcEvent::Message(id.clone(), Box::new(message))).is_err() {
                        break; // receiver dropped
                    }
                }
                Err(e) => {
                    let _ = tx.send(IrcEvent::Disconnected(
                        id.clone(),
                        Some(e.to_string()),
                    ));
                    break;
                }
            }
        }
        // Stream ended — send final disconnect if channel is still open
        let _ = tx.send(IrcEvent::Disconnected(id, None));
    });

    Ok((IrcHandle { conn_id: id2, sender }, rx))
}

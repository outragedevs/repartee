use base64::Engine as _;
use color_eyre::eyre::{Result, eyre};
use futures::StreamExt;
use irc::proto::Command;

use super::{AuthenticateAck, IrcSender, await_authenticate_plus, sasl_failure, sasl_success};

fn initial_response(user: &str, token: &str) -> Result<String> {
    if user.is_empty() || user.chars().any(char::is_control) || token.is_empty()
        || !token.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-._~+/=".contains(&byte))
    {
        return Err(eyre!("Invalid OAUTHBEARER account or token"));
    }
    let identity = user.replace('=', "=3D").replace(',', "=2C");
    Ok(format!("n,a={identity},\x01auth=Bearer {token}\x01\x01"))
}

pub async fn run(sender: &IrcSender, stream: &mut irc::client::ClientStream, user: &str, token: &str) -> Result<()> {
    let payload = initial_response(user, token)?;
    sender.send(Command::AUTHENTICATE("OAUTHBEARER".into()))?;
    if await_authenticate_plus(stream).await? == AuthenticateAck::AlreadyAuthenticated { return Ok(()); }
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
    for chunk in super::sasl_scram::chunk_authenticate(&encoded) {
        sender.send(Command::AUTHENTICATE(chunk))?;
    }
    tokio::time::timeout(std::time::Duration::from_secs(super::SASL_TIMEOUT_SECS), async {
        let mut challenge_bytes = 0;
        let mut error_acknowledged = false;
        while let Some(message) = stream.next().await {
            let message = message?;
            super::account_required::check(&message)?;
            match message.command {
                Command::Response(response, _) => {
                    if let Some(error) = sasl_failure(response) { return Err(eyre!(error)); }
                    if sasl_success(response) {
                        return if error_acknowledged || challenge_bytes != 0 {
                            Err(eyre!("OAUTHBEARER rejected by server"))
                        } else { Ok(()) };
                    }
                }
                Command::AUTHENTICATE(chunk) => {
                    if error_acknowledged || chunk.len() > super::AUTHENTICATE_CHUNK_BYTES {
                        return Err(eyre!("Invalid OAUTHBEARER error exchange"));
                    }
                    challenge_bytes += chunk.len();
                    if challenge_bytes > super::MAX_AUTHENTICATE_BYTES {
                        return Err(eyre!("OAUTHBEARER error response is too large"));
                    }
                    if chunk.len() < super::AUTHENTICATE_CHUNK_BYTES {
                        sender.send(Command::AUTHENTICATE("AQ==".into()))?;
                        error_acknowledged = true;
                    }
                }
                Command::PING(server, token) => sender.send(Command::PONG(server, token))?,
                _ => {}
            }
        }
        Err(eyre!("Connection closed during OAUTHBEARER authentication"))
    }).await.map_err(|_| eyre!("OAUTHBEARER authentication timed out"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_frame_preserves_token_and_escapes_authorization_identity() {
        assert_eq!(initial_response("a=b,c", "test._~+/==").unwrap(),
            "n,a=a=3Db=2Cc,\x01auth=Bearer test._~+/==\x01\x01");
    }

    #[test]
    fn rejects_empty_or_injected_credentials_without_echoing_them() {
        for (user, token) in [("", "token"), ("a\x01b", "token"), ("user", ""), ("user", "secret\x01host=other")] {
            assert_eq!(initial_response(user, token).unwrap_err().to_string(), "Invalid OAUTHBEARER account or token");
        }
    }
    #[tokio::test]
    async fn socket_exchange_chunks_token_and_acknowledges_rejection() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        for mode in 0..4 {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let token = "x".repeat(700);
            let expected = initial_response("account", &token).unwrap();
            let server = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let (read, mut write) = socket.into_split();
                let mut lines = BufReader::new(read).lines();
                assert_eq!(lines.next_line().await.unwrap().unwrap(), "AUTHENTICATE OAUTHBEARER");
                write.write_all(b"AUTHENTICATE +\r\n").await.unwrap();
                let mut encoded = String::new();
                loop {
                    let line = lines.next_line().await.unwrap().unwrap();
                    let chunk = line.strip_prefix("AUTHENTICATE ").unwrap();
                    assert!(chunk.len() <= 400);
                    if chunk != "+" { encoded.push_str(chunk); }
                    if chunk.len() < 400 { break; }
                }
                assert_eq!(base64::engine::general_purpose::STANDARD.decode(encoded).unwrap(), expected.as_bytes());
                if mode == 2 {
                    write.write_all(format!("AUTHENTICATE {}\r\n", "x".repeat(401)).as_bytes()).await.unwrap();
                } else if mode != 0 {
                    write.write_all(b"AUTHENTICATE eyJzdGF0dXMiOiI0MDEifQ==\r\n").await.unwrap();
                    assert_eq!(lines.next_line().await.unwrap().unwrap(), "AUTHENTICATE AQ==");
                    let terminal = if mode == 3 { b"AUTHENTICATE +\r\n".as_slice() } else { b":server 904 account :Failed\r\n".as_slice() };
                    write.write_all(terminal).await.unwrap();
                } else {
                    write.write_all(b":server 903 account :Success\r\n").await.unwrap();
                }
            });
            let mut client = irc::client::Client::from_config(irc::client::data::Config {
                nickname: Some("account".into()), server: Some("127.0.0.1".into()),
                port: Some(port), use_tls: Some(false), flood_penalty_threshold: Some(0),
                ..Default::default()
            }).await.unwrap();
            let sender = IrcSender::new(client.sender(), 0);
            let mut stream = client.stream().unwrap();
            let result = tokio::time::timeout(std::time::Duration::from_secs(5), run(&sender, &mut stream, "account", &token)).await.unwrap();
            assert_eq!(result.is_err(), mode != 0);
            server.await.unwrap();
            if let Some(handle) = client.outgoing_handle.take() { handle.abort(); }
        }
    }

    #[tokio::test]
    async fn already_authenticated_does_not_send_a_token() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (read, mut write) = socket.into_split();
            let mut lines = BufReader::new(read).lines();
            assert_eq!(lines.next_line().await.unwrap().unwrap(), "AUTHENTICATE OAUTHBEARER");
            write.write_all(b":server 907 account :Already authenticated\r\n").await.unwrap();
            assert!(tokio::time::timeout(std::time::Duration::from_millis(100), lines.next_line()).await.is_err());
        });
        let mut client = irc::client::Client::from_config(irc::client::data::Config {
            nickname: Some("account".into()), server: Some("127.0.0.1".into()),
            port: Some(port), use_tls: Some(false), flood_penalty_threshold: Some(0),
            ..Default::default()
        }).await.unwrap();
        let sender = IrcSender::new(client.sender(), 0);
        let mut stream = client.stream().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), run(&sender, &mut stream, "account", "unused-token")).await.unwrap().unwrap();
        server.await.unwrap();
        if let Some(handle) = client.outgoing_handle.take() { handle.abort(); }
    }

}

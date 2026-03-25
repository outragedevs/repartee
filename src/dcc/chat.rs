//! DCC CHAT async TCP tasks: listener, connector, and line reader/writer.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::DccEvent;

// ─── Public entry points ──────────────────────────────────────────────────────

/// Accept one incoming connection on an already-bound TCP listener.
///
/// The caller binds the listener and extracts the local port to include in
/// the CTCP DCC CHAT offer *before* calling this function.  We then wait up
/// to `timeout` for the remote peer to connect.
pub async fn listen_for_chat(
    id: String,
    listener: TcpListener,
    timeout_dur: Duration,
    event_tx: mpsc::Sender<DccEvent>,
    line_rx: mpsc::Receiver<String>,
) {
    let accept_result = timeout(timeout_dur, listener.accept()).await;

    match accept_result {
        Ok(Ok((stream, _peer_addr))) => {
            // Notify: TCP handshake done.
            let _ = event_tx.send(DccEvent::ChatConnected { id: id.clone() }).await;
            run_chat_session(id, stream, event_tx, line_rx).await;
        }
        Ok(Err(e)) => {
            let _ = event_tx.send(DccEvent::ChatError {
                id,
                error: format!("DCC CHAT accept error: {e}"),
            }).await;
        }
        Err(_elapsed) => {
            let _ = event_tx.send(DccEvent::ChatError {
                id,
                error: "DCC CHAT request timed out".to_owned(),
            }).await;
        }
    }
}

/// Connect to a remote peer for an outgoing DCC CHAT session.
///
/// Used when we are the initiator (active DCC) or when we accept a passive
/// DCC offer and must connect back to the peer.
pub async fn connect_for_chat(
    id: String,
    addr: SocketAddr,
    timeout_dur: Duration,
    event_tx: mpsc::Sender<DccEvent>,
    line_rx: mpsc::Receiver<String>,
) {
    let connect_result = timeout(timeout_dur, TcpStream::connect(addr)).await;

    match connect_result {
        Ok(Ok(stream)) => {
            let _ = event_tx.send(DccEvent::ChatConnected { id: id.clone() }).await;
            run_chat_session(id, stream, event_tx, line_rx).await;
        }
        Ok(Err(e)) => {
            let _ = event_tx.send(DccEvent::ChatError {
                id,
                error: format!("DCC CHAT connect error: {e}"),
            }).await;
        }
        Err(_elapsed) => {
            let _ = event_tx.send(DccEvent::ChatError {
                id,
                error: "DCC CHAT connect timed out".to_owned(),
            }).await;
        }
    }
}

// ─── Session loop ─────────────────────────────────────────────────────────────

/// Drive a live DCC CHAT session over an established TCP stream.
///
/// Spawns two sub-tasks: one reads lines from the peer and converts them to
/// [`DccEvent`]s; the other drains `line_rx` and writes those lines to the
/// peer.  When either sub-task finishes (EOF, write error, or channel close)
/// the other is aborted.
async fn run_chat_session(
    id: String,
    stream: TcpStream,
    event_tx: mpsc::Sender<DccEvent>,
    mut line_rx: mpsc::Receiver<String>,
) {
    // Split the stream so reader and writer can run concurrently.
    let (read_half, write_half) = stream.into_split();

    // ── Reader task ───────────────────────────────────────────────────────────
    let reader_id = id.clone();
    let reader_tx = event_tx.clone();
    let mut reader_handle = tokio::spawn(async move {
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();

        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    // EOF — peer closed connection cleanly.
                    let _ = reader_tx.send(DccEvent::ChatClosed {
                        id: reader_id,
                        reason: None,
                    }).await;
                    break;
                }
                Ok(_) => {
                    // Strip the trailing newline (and any carriage-return).
                    let text = line.trim_end_matches(['\n', '\r']).to_owned();

                    // Empty lines after stripping are silently discarded —
                    // they carry no content and can arise from \r\n line endings.
                    if text.is_empty() {
                        continue;
                    }

                    // Detect CTCP ACTION: \x01ACTION text\x01
                    if let Some(action_text) = text
                        .strip_prefix("\x01ACTION ")
                        .and_then(|s| s.strip_suffix('\x01'))
                    {
                        let _ = reader_tx.send(DccEvent::ChatAction {
                            id: reader_id.clone(),
                            text: action_text.to_owned(),
                        }).await;
                    } else {
                        let _ = reader_tx.send(DccEvent::ChatMessage {
                            id: reader_id.clone(),
                            text,
                        }).await;
                    }
                }
                Err(e) => {
                    let _ = reader_tx.send(DccEvent::ChatClosed {
                        id: reader_id,
                        reason: Some(format!("read error: {e}")),
                    }).await;
                    break;
                }
            }
        }
    });

    // ── Writer task ───────────────────────────────────────────────────────────
    let writer_id = id.clone();
    let writer_tx = event_tx.clone();
    let mut writer_handle = tokio::spawn(async move {
        let mut write_half = write_half;

        while let Some(line) = line_rx.recv().await {
            // Append a newline; the DCC CHAT protocol uses bare LF.
            let mut data = line;
            data.push('\n');

            if let Err(e) = write_half.write_all(data.as_bytes()).await {
                let _ = writer_tx.send(DccEvent::ChatClosed {
                    id: writer_id,
                    reason: Some(format!("write error: {e}")),
                }).await;
                return;
            }

            if let Err(e) = write_half.flush().await {
                let _ = writer_tx.send(DccEvent::ChatClosed {
                    id: writer_id,
                    reason: Some(format!("flush error: {e}")),
                }).await;
                return;
            }
        }
        // line_rx was dropped/closed: the user closed this DCC window.
        // No event needed; the reader will observe EOF shortly.
    });

    // ── Wait for the first task to finish, then cancel the other ─────────────
    //
    // Pinning + biased ensures each arm sees the same handle binding after
    // the select completes.  We abort the still-running sibling so we do not
    // leak tasks when one side terminates early.
    tokio::select! {
        biased;
        _ = &mut reader_handle => {
            writer_handle.abort();
        }
        _ = &mut writer_handle => {
            reader_handle.abort();
        }
    }
}

// ─── Port-range helper ────────────────────────────────────────────────────────

/// Parse a DCC port-range string into a `(low, high)` pair.
///
/// Accepted formats:
/// - `""` or `"0"` → `(0, 0)` (OS assigns a free port)
/// - `"5000"`      → `(5000, 5000)` (fixed single port)
/// - `"1025 65535"` or `"1025-65535"` → `(1025, 65535)`
///
/// Any value that cannot be parsed returns `(0, 0)`.
pub fn parse_port_range(s: &str) -> (u16, u16) {
    let s = s.trim();

    if s.is_empty() || s == "0" {
        return (0, 0);
    }

    // Try separator: space first, then hyphen.
    let maybe_pair = if let Some((lo, hi)) = s.split_once(' ') {
        Some((lo.trim(), hi.trim()))
    } else {
        s.split_once('-').map(|(lo, hi)| (lo.trim(), hi.trim()))
    };

    if let Some((lo_str, hi_str)) = maybe_pair {
        match (lo_str.parse::<u16>(), hi_str.parse::<u16>()) {
            (Ok(lo), Ok(hi)) => return (lo, hi),
            _ => return (0, 0),
        }
    }

    // Single port number.
    s.parse::<u16>().map_or((0, 0), |p| (p, p))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use tokio::net::TcpListener;

    // ── parse_port_range ─────────────────────────────────────────────────────

    #[test]
    fn parse_port_range_zero() {
        assert_eq!(parse_port_range("0"), (0, 0));
    }

    #[test]
    fn parse_port_range_empty() {
        assert_eq!(parse_port_range(""), (0, 0));
    }

    #[test]
    fn parse_port_range_single() {
        assert_eq!(parse_port_range("5000"), (5000, 5000));
    }

    #[test]
    fn parse_port_range_space() {
        assert_eq!(parse_port_range("1025 65535"), (1025, 65535));
    }

    #[test]
    fn parse_port_range_dash() {
        assert_eq!(parse_port_range("1025-65535"), (1025, 65535));
    }

    // ── Integration: connect, exchange, close ─────────────────────────────────

    /// Bind a listener on localhost, connect to it, exchange a plain message
    /// and a /me action in both directions, then close the sender and verify
    /// `ChatClosed` is emitted.
    #[tokio::test]
    async fn chat_session_connect_and_exchange() {
        // 1. Bind a listener on an OS-assigned port.
        let server_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind listener");
        let server_addr = server_listener.local_addr().expect("local_addr");

        // Channels for the client side (connect_for_chat).
        let (client_event_tx, mut client_event_rx) = mpsc::channel::<DccEvent>(256);
        let (client_line_tx, client_line_rx) = mpsc::channel::<String>(256);

        // Channels for the server side (run_chat_session on the accepted stream).
        let (server_event_tx, mut server_event_rx) = mpsc::channel::<DccEvent>(256);
        let (server_line_tx, server_line_rx) = mpsc::channel::<String>(256);

        // 2. Spawn connect_for_chat — connects to the server listener.
        tokio::spawn(connect_for_chat(
            "client".to_owned(),
            server_addr,
            Duration::from_secs(5),
            client_event_tx,
            client_line_rx,
        ));

        // 3. Accept the incoming connection on the server side.
        let (server_stream, _) = server_listener.accept().await.expect("server accept");

        // 4. Spawn run_chat_session on the server side.
        tokio::spawn(run_chat_session(
            "server".to_owned(),
            server_stream,
            server_event_tx,
            server_line_rx,
        ));

        // 5a. Client should have received ChatConnected.
        let event = client_event_rx.recv().await.expect("ChatConnected");
        assert!(
            matches!(event, DccEvent::ChatConnected { ref id } if id == "client"),
            "expected ChatConnected, got {event:?}",
        );

        // 5b. Server side: send a plain text line to the client.
        server_line_tx
            .send("hello from server".to_owned())
            .await
            .expect("send plain");

        // Client should receive ChatMessage.
        let event = client_event_rx.recv().await.expect("ChatMessage");
        assert!(
            matches!(&event, DccEvent::ChatMessage { id, text }
                if id == "client" && text == "hello from server"),
            "expected ChatMessage, got {event:?}",
        );

        // 6. Client side: send a CTCP ACTION to the server.
        client_line_tx
            .send("\x01ACTION waves\x01".to_owned())
            .await
            .expect("send action");

        // Server should receive ChatAction.
        let event = server_event_rx.recv().await.expect("ChatAction");
        assert!(
            matches!(&event, DccEvent::ChatAction { id, text }
                if id == "server" && text == "waves"),
            "expected ChatAction, got {event:?}",
        );

        // 7. Drop the server sender — the writer task exits; client observes EOF.
        drop(server_line_tx);

        // Client should receive ChatClosed (EOF, no error reason).
        let event = client_event_rx.recv().await.expect("ChatClosed");
        assert!(
            matches!(event, DccEvent::ChatClosed { ref id, reason: None } if id == "client"),
            "expected ChatClosed with no reason, got {event:?}",
        );
    }
}

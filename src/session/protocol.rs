use std::collections::HashMap;

use color_eyre::eyre::{Result, eyre};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Shim → Main (upstream) messages.
#[derive(Debug, Serialize, Deserialize)]
pub enum ShimMessage {
    /// Keyboard, mouse, paste event from the shim's terminal.
    TermEvent(crossterm::event::Event),
    /// Terminal dimensions (sent on connect + SIGWINCH).
    Resize { cols: u16, rows: u16 },
    /// User requested detach (chord or /detach typed in shim).
    Detach,
    /// Bytes of `MainMessage::Output` written to the terminal. Sent only after
    /// the daemon asked for acknowledgements with `MainMessage::OutputAcks`.
    OutputWritten(u64),
}

/// Main → Shim (downstream) messages.
#[derive(Debug, Serialize, Deserialize)]
pub enum MainMessage {
    /// Raw terminal escape sequences (ratatui frame + image direct-write).
    Output(Vec<u8>),
    /// Confirmation that detach completed — shim should restore terminal and exit.
    Detached,
    /// Main process is shutting down — shim should exit.
    Quit,
    /// Acknowledge every following `Output` once it is written to the
    /// terminal. Sent only to shims that announced `TerminalEnv::output_acks`,
    /// so older shims never see this variant.
    OutputAcks,
}

/// Terminal environment snapshot sent by the shim on connect.
///
/// The main process uses these values (instead of its own env vars, which
/// may point to a now-closed terminal after detach) for image protocol
/// detection and font size when rendering through the socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalEnv {
    pub cols: u16,
    pub rows: u16,
    /// Font size in pixels `(width, height)` — derived from the terminal's
    /// pixel dimensions divided by cell count. `None` if the terminal
    /// doesn't report pixel dimensions.
    pub font_size: Option<(u16, u16)>,
    /// Key terminal env vars from the shim's environment.
    pub env_vars: HashMap<String, String>,
    /// The shim can acknowledge written output. Kept last: older daemons stop
    /// decoding before it, and `read_terminal_env` accepts payloads without it.
    pub output_acks: bool,
}

/// `TerminalEnv` as sent by shims that predate output acknowledgements.
#[derive(Deserialize)]
struct LegacyTerminalEnv {
    cols: u16,
    rows: u16,
    font_size: Option<(u16, u16)>,
    env_vars: HashMap<String, String>,
}

impl TerminalEnv {
    /// Capture the current terminal environment.
    pub fn capture() -> Self {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));

        // Query pixel dimensions to compute font size.
        let font_size = crossterm::terminal::window_size().ok().and_then(|ws| {
            if ws.width > 0 && ws.height > 0 && ws.columns > 0 && ws.rows > 0 {
                Some((ws.width / ws.columns, ws.height / ws.rows))
            } else {
                None
            }
        });

        let keys = [
            "TERM",
            "TERM_PROGRAM",
            "TERM_PROGRAM_VERSION",
            "LC_TERMINAL",
            "LC_TERMINAL_VERSION",
            "ITERM_SESSION_ID",
            "KITTY_PID",
            "GHOSTTY_RESOURCES_DIR",
            "WT_SESSION",
            "WEZTERM_EXECUTABLE",
            "COLORTERM",
            "TMUX",
        ];
        let mut env_vars = HashMap::new();
        for key in keys {
            if let Ok(val) = std::env::var(key)
                && !val.is_empty()
            {
                env_vars.insert(key.to_string(), val);
            }
        }
        Self {
            cols,
            rows,
            font_size,
            env_vars,
            output_acks: true,
        }
    }
}

/// Maximum message size: 64 MiB.
///
/// Kitty protocol encodes images as raw RGBA + base64, which can produce
/// multi-MB frame outputs for large terminals. 64 MiB provides headroom.
const MAX_MESSAGE_SIZE: u32 = 64 * 1024 * 1024;

/// Write a length-prefixed bincode message to an async writer.
pub async fn write_message<W, M>(writer: &mut W, msg: &M) -> Result<()>
where
    W: AsyncWriteExt + Unpin + Send,
    M: Serialize + Sync,
{
    let payload = postcard::to_stdvec(msg)?;
    let len = u32::try_from(payload.len())
        .map_err(|_| eyre!("message too large: {} bytes", payload.len()))?;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn write_output<W>(writer: &mut W, bytes: &[u8]) -> Result<()>
where
    W: AsyncWriteExt + Unpin + Send,
{
    for chunk in bytes.chunks(super::writer::SOCKET_OUTPUT_CHUNK_BYTES) {
        write_message(writer, &MainMessage::Output(chunk.to_vec())).await?;
    }
    Ok(())
}

async fn read_frame<R>(reader: &mut R) -> Result<Vec<u8>>
where
    R: AsyncReadExt + Unpin,
{
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_MESSAGE_SIZE {
        return Err(eyre!("message too large: {len} bytes"));
    }
    let mut payload = vec![0u8; len as usize];
    reader.read_exact(&mut payload).await?;
    Ok(payload)
}

/// Read a length-prefixed bincode message from an async reader.
pub async fn read_message<R, M>(reader: &mut R) -> Result<M>
where
    R: AsyncReadExt + Unpin,
    M: for<'de> Deserialize<'de>,
{
    let payload = read_frame(reader).await?;
    let msg: M = postcard::from_bytes(&payload)?;
    Ok(msg)
}

/// Read the shim's initial `TerminalEnv`, accepting shims that predate
/// output acknowledgements.
pub async fn read_terminal_env<R>(reader: &mut R) -> Result<TerminalEnv>
where
    R: AsyncReadExt + Unpin,
{
    let payload = read_frame(reader).await?;
    decode_terminal_env(&payload)
}

fn decode_terminal_env(payload: &[u8]) -> Result<TerminalEnv> {
    if let Ok(env) = postcard::from_bytes::<TerminalEnv>(payload) {
        return Ok(env);
    }
    let legacy: LegacyTerminalEnv = postcard::from_bytes(payload)?;
    Ok(TerminalEnv {
        cols: legacy.cols,
        rows: legacy.rows,
        font_size: legacy.font_size,
        env_vars: legacy.env_vars,
        output_acks: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct LegacyEnv {
        cols: u16,
        rows: u16,
        font_size: Option<(u16, u16)>,
        env_vars: HashMap<String, String>,
    }

    #[test]
    fn terminal_env_stays_compatible_with_shims_and_daemons_without_acks() {
        let env_vars = HashMap::from([("TERM".to_string(), "xterm".to_string())]);
        let legacy = postcard::to_stdvec(&LegacyEnv {
            cols: 100,
            rows: 30,
            font_size: Some((8, 16)),
            env_vars: env_vars.clone(),
        })
        .unwrap();
        let decoded = decode_terminal_env(&legacy).unwrap();
        assert_eq!((decoded.cols, decoded.rows, decoded.font_size), (100, 30, Some((8, 16))));
        assert_eq!(decoded.env_vars, env_vars);
        assert!(!decoded.output_acks);

        let current = postcard::to_stdvec(&TerminalEnv {
            cols: 120,
            rows: 40,
            font_size: None,
            env_vars: env_vars.clone(),
            output_acks: true,
        })
        .unwrap();
        assert!(decode_terminal_env(&current).unwrap().output_acks);
        let old_daemon_view: LegacyTerminalEnv = postcard::from_bytes(&current).unwrap();
        assert_eq!((old_daemon_view.cols, old_daemon_view.rows), (120, 40));
        assert_eq!(old_daemon_view.env_vars, env_vars);
    }

    #[tokio::test]
    async fn output_chunks_reassemble_through_a_small_socket_buffer() {
        let (mut sender, mut receiver) = tokio::io::duplex(128);
        let expected = b"\x1b_Gm=1;image-data\x1b\\".repeat(150_000);
        let original = expected.clone();
        let writer = tokio::spawn(async move {
            write_output(&mut sender, &original).await.unwrap();
            write_message(&mut sender, &MainMessage::Detached).await.unwrap();
        });
        let mut reconstructed = Vec::new();
        let mut chunks = 0;
        loop {
            match read_message::<_, MainMessage>(&mut receiver).await.unwrap() {
                MainMessage::Output(bytes) => {
                    assert!(bytes.len() <= super::super::writer::SOCKET_OUTPUT_CHUNK_BYTES);
                    reconstructed.extend(bytes);
                    chunks += 1;
                    tokio::task::yield_now().await;
                }
                MainMessage::Detached => break,
                MainMessage::Quit | MainMessage::OutputAcks => panic!("unexpected control message"),
            }
        }
        writer.await.unwrap();
        assert!(chunks > 1);
        assert_eq!(reconstructed, expected);
    }
}

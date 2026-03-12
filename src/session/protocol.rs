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
    let payload = bincode::serialize(msg)?;
    let len = u32::try_from(payload.len())
        .map_err(|_| eyre!("message too large: {} bytes", payload.len()))?;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

/// Read a length-prefixed bincode message from an async reader.
pub async fn read_message<R, M>(reader: &mut R) -> Result<M>
where
    R: AsyncReadExt + Unpin,
    M: for<'de> Deserialize<'de>,
{
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_MESSAGE_SIZE {
        return Err(eyre!("message too large: {len} bytes"));
    }
    let mut payload = vec![0u8; len as usize];
    reader.read_exact(&mut payload).await?;
    let msg = bincode::deserialize(&payload)?;
    Ok(msg)
}

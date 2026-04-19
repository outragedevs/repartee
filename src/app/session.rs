use chrono::Utc;

use color_eyre::eyre::Result;

use crate::state::buffer::{Message, MessageType};
use crate::ui;

use super::App;

impl App {
    /// Start the Unix socket listener for shim connections.
    pub(crate) fn start_socket_listener(&mut self) -> Result<()> {
        if self.socket_listener.is_some() {
            return Ok(());
        }
        let dir = crate::constants::sessions_dir();
        crate::fs_secure::create_dir_all(&dir, 0o700)?;
        let path = crate::session::socket_path(std::process::id());
        // Remove stale socket from a previous run.
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path)?;
        crate::fs_secure::restrict_path(&path, 0o600)?;
        tracing::info!("session socket listening at {}", path.display());
        self.socket_listener = Some(listener);
        Ok(())
    }

    /// Clean up own socket file.
    pub fn remove_own_socket() {
        let path = crate::session::socket_path(std::process::id());
        let _ = std::fs::remove_file(&path);
    }

    /// Handle a new shim connection from the socket listener.
    #[expect(
        clippy::too_many_lines,
        reason = "flat init sequence, splitting adds indirection"
    )]
    pub(crate) async fn handle_shim_connect(
        &mut self,
        stream: tokio::net::UnixStream,
    ) -> Result<()> {
        use crate::session::protocol::{self, MainMessage, ShimMessage};
        use crate::session::writer::SocketWriter;
        use tokio::sync::mpsc;

        if !same_user_peer(&stream)? {
            tracing::warn!("rejecting shim connection from different local user");
            return Ok(());
        }

        // If a shim is already connected, disconnect it first.
        if self.is_socket_attached {
            tracing::info!("new shim connecting, disconnecting existing shim");
        }
        self.disconnect_shim();

        let (read_half, write_half) = tokio::io::split(stream);
        let mut read_half = tokio::io::BufReader::new(read_half);

        // Read the initial TerminalEnv message to get dimensions + env vars.
        let term_env =
            match protocol::read_message::<_, protocol::TerminalEnv>(&mut read_half).await {
                Ok(env) => env,
                Err(e) => {
                    tracing::warn!("failed to read initial shim message: {e}");
                    return Ok(());
                }
            };
        let cols = term_env.cols;
        let rows = term_env.rows;

        // Set up output channel: SocketWriter → mpsc → write_half.
        let (output_tx, mut output_rx) = mpsc::unbounded_channel::<MainMessage>();
        let output_handle = tokio::spawn(async move {
            let mut write_half = write_half;
            while let Some(msg) = output_rx.recv().await {
                if protocol::write_message(&mut write_half, &msg)
                    .await
                    .is_err()
                {
                    tracing::warn!("shim output write failed, closing output task");
                    break;
                }
            }
            tracing::debug!("shim output task exiting");
        });

        // Create socket-backed terminal.
        let socket_writer = SocketWriter::new(output_tx.clone());
        let terminal = ui::setup_socket_terminal(Box::new(socket_writer), cols, rows)?;

        // Set up input reader: read ShimMessages from socket → mpsc.
        let (shim_tx, shim_rx) = mpsc::channel::<ShimMessage>(1024);
        let input_handle = tokio::spawn(async move {
            let mut reader = read_half;
            loop {
                match protocol::read_message::<_, ShimMessage>(&mut reader).await {
                    Ok(msg) => {
                        if shim_tx.send(msg).await.is_err() {
                            tracing::debug!("shim input channel closed");
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::debug!("shim input read error: {e}");
                        break;
                    }
                }
            }
            tracing::debug!("shim input reader task exiting");
        });

        self.terminal = Some(terminal);
        self.socket_output_tx = Some(output_tx);
        self.shim_event_rx = Some(shim_rx);
        self.shim_output_handle = Some(output_handle);
        self.shim_input_handle = Some(input_handle);
        self.detached = false;
        self.is_socket_attached = true;
        self.needs_full_redraw = true;
        self.cached_term_cols = cols;
        self.cached_term_rows = rows;
        self.buffer_list_scroll = 0;
        self.nick_list_scroll = 0;

        // Store shim's terminal env for protocol detection.
        self.shim_term_env = Some(term_env.env_vars);

        // Update picker font_size — reattaching shim may have different cell
        // pixel dimensions than the terminal we started with.
        if let Some(font_size) = term_env.font_size {
            tracing::info!(
                old_font = ?self.picker.font_size(),
                new_font = ?font_size,
                "updating picker font_size from shim terminal"
            );
            #[expect(deprecated, reason = "only API to set font dimensions")]
            let mut new_picker = ratatui_image::picker::Picker::from_fontsize(font_size);
            new_picker.set_protocol_type(self.picker.protocol_type());
            self.picker = new_picker;
        }

        // Re-detect image protocol using the shim's terminal env.
        self.refresh_image_protocol();

        // Add system message to the active buffer.
        let buf_id = self.state.active_buffer_id.clone().unwrap_or_default();
        let id = self.state.next_message_id();
        self.state.add_message(
            &buf_id,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: "Terminal attached".to_string(),
                highlight: false,
                event_key: None,
                event_params: None,
                log_msg_id: None,
                log_ref_id: None,
                tags: None,
            },
        );

        tracing::info!(cols, rows, "shim attached");
        Ok(())
    }

    /// Send a control `MainMessage` through the shim output channel.
    pub(crate) fn send_shim_control(&self, msg: crate::session::protocol::MainMessage) {
        if let Some(ref tx) = self.socket_output_tx
            && let Err(e) = tx.send(msg)
        {
            tracing::warn!("shim control channel full or closed: {e}");
        }
    }

    /// Tear down the shim connection (terminal, tasks, channels).
    pub(crate) fn teardown_shim(&mut self) {
        self.terminal = None;
        self.socket_output_tx = None;
        self.shim_event_rx = None;
        self.is_socket_attached = false;
        self.shim_term_env = None;
        self.shim_output_handle.take();
        if let Some(h) = self.shim_input_handle.take() {
            h.abort();
        }
    }

    /// Disconnect the current shim (if any).
    pub(crate) fn disconnect_shim(&mut self) {
        self.send_shim_control(crate::session::protocol::MainMessage::Detached);
        self.teardown_shim();
    }

    /// Perform detach: save state, drop terminal, start socket listener.
    pub(crate) fn perform_detach(&mut self) {
        self.should_detach = false;

        if self.is_socket_attached {
            self.send_shim_control(crate::session::protocol::MainMessage::Detached);
            self.teardown_shim();
        }

        self.detached = true;

        tracing::info!(pid = std::process::id(), "detached");
    }

    /// Send Quit to the connected shim before shutdown.
    pub(crate) fn notify_shim_quit(&self) {
        self.send_shim_control(crate::session::protocol::MainMessage::Quit);
    }
}

fn same_user_peer(stream: &tokio::net::UnixStream) -> Result<bool> {
    #[cfg(unix)]
    {
        let peer = stream.peer_cred()?;
        let uid = peer.uid();
        let expected = unsafe { libc::geteuid() };
        Ok(uid == expected)
    }

    #[cfg(not(unix))]
    {
        let _ = stream;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::same_user_peer;

    #[tokio::test]
    async fn same_user_peer_accepts_current_uid() {
        let (left, _right) = tokio::net::UnixStream::pair().unwrap();
        assert!(same_user_peer(&left).unwrap());
    }
}

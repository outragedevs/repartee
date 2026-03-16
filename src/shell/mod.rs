pub mod types;

use std::collections::HashMap;
use std::io::{Read, Write};

use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use tokio::sync::mpsc;

pub use types::ShellEvent;

/// A single shell session backed by a PTY and a vt100 terminal emulator.
struct ShellSession {
    parser: vt100::Parser,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send>,
    /// Master PTY handle — kept alive for resize (SIGWINCH) propagation.
    master: Box<dyn portable_pty::MasterPty + Send>,
    /// Buffer ID this session is attached to (e.g. "shell/zsh").
    buffer_id: String,
    /// Display label for the shell (e.g. "zsh", "htop").
    label: String,
}

/// Manages all active shell sessions and their PTY connections.
pub struct ShellManager {
    sessions: HashMap<String, ShellSession>,
    event_tx: mpsc::UnboundedSender<ShellEvent>,
    next_id: u32,
}

impl ShellManager {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<ShellEvent>) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        (
            Self {
                sessions: HashMap::new(),
                event_tx,
                next_id: 0,
            },
            event_rx,
        )
    }

    /// Open a new shell session. Returns `(shell_id, label)` on success.
    ///
    /// `command` defaults to `$SHELL` if `None`.
    /// The PTY is sized to `cols × rows` and a reader thread is spawned
    /// to forward output into the event channel.
    pub fn open(
        &mut self,
        cols: u16,
        rows: u16,
        command: Option<&str>,
        buffer_id: &str,
    ) -> Result<(String, String), String> {
        let id = format!("shell-{}", self.next_id);
        self.next_id += 1;

        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("Failed to open PTY: {e}"))?;

        let full_command = command
            .map(String::from)
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "/bin/sh".to_string());

        // Split command into program + arguments for multi-word commands
        // (e.g. "vim /etc/hosts" → program="vim", args=["/etc/hosts"]).
        let mut parts = full_command.split_whitespace();
        let program = parts.next().unwrap_or("/bin/sh");

        let label = std::path::Path::new(program)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("shell")
            .to_string();

        let mut cmd = CommandBuilder::new(program);
        for arg in parts {
            cmd.arg(arg);
        }
        // TERM tells programs what escape sequences the terminal supports.
        // xterm-256color is the standard for modern 256-color terminals.
        cmd.env("TERM", "xterm-256color");
        cmd.cwd(std::env::current_dir().unwrap_or_else(|_| "/".into()));

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("Failed to spawn shell: {e}"))?;

        // Drop the slave side — only the master is needed after spawn.
        drop(pair.slave);

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("Failed to get PTY writer: {e}"))?;

        // 1000 lines of scrollback.
        let parser = vt100::Parser::new(rows, cols, 1000);

        // Spawn a dedicated reader thread (blocking I/O — not safe for tokio thread pool).
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("Failed to clone PTY reader: {e}"))?;
        let tx = self.event_tx.clone();
        // Arc<str> avoids a String clone per read in the hot output path.
        let reader_id: std::sync::Arc<str> = id.as_str().into();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        let _ = tx.send(ShellEvent::Exited {
                            id: reader_id,
                            status: None,
                        });
                        break;
                    }
                    Ok(n) => {
                        if tx
                            .send(ShellEvent::Output {
                                id: std::sync::Arc::clone(&reader_id),
                                bytes: buf[..n].to_vec(),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });

        let label_out = label.clone();
        self.sessions.insert(
            id.clone(),
            ShellSession {
                parser,
                writer,
                child,
                master: pair.master,
                buffer_id: buffer_id.to_string(),
                label,
            },
        );

        Ok((id, label_out))
    }

    /// Close a shell session, killing the child process.
    pub fn close(&mut self, id: &str) {
        if let Some(mut session) = self.sessions.remove(id) {
            let _ = session.child.kill();
        }
    }

    /// Write raw bytes to a shell's PTY master.
    pub fn write(&mut self, id: &str, data: &[u8]) {
        if let Some(session) = self.sessions.get_mut(id) {
            let _ = session.writer.write_all(data);
            let _ = session.writer.flush();
        }
    }

    /// Resize a shell's PTY and vt100 parser.
    /// Sends SIGWINCH to the child process via the OS PTY resize call.
    pub fn resize(&mut self, id: &str, cols: u16, rows: u16) {
        if let Some(session) = self.sessions.get_mut(id) {
            session.parser.screen_mut().set_size(rows, cols);
            // OS PTY resize — sends SIGWINCH to the child process.
            let _ = session.master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    /// Feed raw output bytes into the vt100 terminal emulator.
    ///
    /// Rewrites `CSI f` (HVP) to `CSI H` (CUP) before parsing, because the
    /// vt100 crate does not handle HVP. Both are functionally identical
    /// (absolute cursor positioning). Programs like btop use HVP exclusively.
    pub fn process_output(&mut self, id: &str, bytes: &[u8]) {
        let Some(session) = self.sessions.get_mut(id) else {
            return;
        };
        // Fast path: no 'f' byte means no HVP sequences to rewrite.
        if !bytes.contains(&b'f') {
            session.parser.process(bytes);
            return;
        }
        // Rewrite CSI...f → CSI...H for vt100 compatibility.
        let patched = rewrite_hvp_to_cup(bytes);
        session.parser.process(&patched);
    }

    /// Get the vt100 screen for rendering.
    pub fn screen(&self, id: &str) -> Option<&vt100::Screen> {
        self.sessions.get(id).map(|s| s.parser.screen())
    }

    /// Get the buffer ID associated with a shell session.
    pub fn buffer_id(&self, id: &str) -> Option<&str> {
        self.sessions.get(id).map(|s| s.buffer_id.as_str())
    }

    /// Get the display label for a shell session (e.g. "zsh", "htop").
    pub fn label(&self, id: &str) -> Option<&str> {
        self.sessions.get(id).map(|s| s.label.as_str())
    }

    /// Find a shell session ID by its buffer ID.
    pub fn session_id_for_buffer(&self, buffer_id: &str) -> Option<&str> {
        self.sessions
            .iter()
            .find(|(_, s)| s.buffer_id == buffer_id)
            .map(|(id, _)| id.as_str())
    }

    /// Return the number of active shell sessions.
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// List all sessions as `(shell_id, buffer_id, label)` tuples.
    pub fn list_sessions(&self) -> Vec<(&str, &str, &str)> {
        self.sessions
            .iter()
            .map(|(id, s)| (id.as_str(), s.buffer_id.as_str(), s.label.as_str()))
            .collect()
    }

    /// Kill all shell processes. Called on app shutdown.
    pub fn kill_all(&mut self) {
        for session in self.sessions.values_mut() {
            let _ = session.child.kill();
        }
        self.sessions.clear();
    }
}

/// Rewrite `CSI ... f` (HVP) sequences to `CSI ... H` (CUP).
///
/// The vt100 crate handles CUP but not HVP, despite them being functionally
/// identical (ECMA-48 §8.3.25 / §8.3.21). This performs a single pass over
/// the byte stream, tracking CSI state to only replace `f` when it is the
/// final byte of a CSI sequence.
///
/// CSI format: `\x1b[` followed by parameter bytes (0x30..=0x3F),
/// then optional intermediate bytes (0x20..=0x2F), then a final byte (0x40..=0x7E).
fn rewrite_hvp_to_cup(input: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let mut i = 0;

    while i < input.len() {
        // Look for ESC [ (CSI introducer).
        if input[i] == 0x1b && i + 1 < input.len() && input[i + 1] == b'[' {
            // Start of CSI sequence. Copy the introducer.
            output.push(0x1b);
            output.push(b'[');
            i += 2;

            // Skip parameter bytes (0x30..=0x3F: digits, semicolons, etc.)
            // and intermediate bytes (0x20..=0x2F).
            while i < input.len() && (0x20..=0x3F).contains(&input[i]) {
                output.push(input[i]);
                i += 1;
            }

            // Final byte (0x40..=0x7E).
            if i < input.len() && (0x40..=0x7E).contains(&input[i]) {
                if input[i] == b'f' {
                    output.push(b'H'); // HVP → CUP
                } else {
                    output.push(input[i]);
                }
                i += 1;
            }
        } else {
            output.push(input[i]);
            i += 1;
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_manager_new_has_no_sessions() {
        let (mgr, _rx) = ShellManager::new();
        assert_eq!(mgr.session_count(), 0);
    }

    #[test]
    fn shell_manager_open_creates_session() {
        let (mut mgr, _rx) = ShellManager::new();
        let result = mgr.open(80, 24, Some("/bin/sh"), "shell/sh");
        assert!(result.is_ok(), "open failed: {:?}", result.err());
        assert_eq!(mgr.session_count(), 1);
        mgr.kill_all();
    }

    #[test]
    fn shell_manager_close_removes_session() {
        let (mut mgr, _rx) = ShellManager::new();
        let (id, _) = mgr.open(80, 24, Some("/bin/sh"), "shell/sh").unwrap();
        assert_eq!(mgr.session_count(), 1);
        mgr.close(&id);
        assert_eq!(mgr.session_count(), 0);
    }

    #[test]
    fn shell_manager_session_id_for_buffer() {
        let (mut mgr, _rx) = ShellManager::new();
        let (id, _) = mgr.open(80, 24, Some("/bin/sh"), "shell/sh").unwrap();
        assert_eq!(mgr.session_id_for_buffer("shell/sh"), Some(id.as_str()));
        assert_eq!(mgr.session_id_for_buffer("nonexistent"), None);
        mgr.kill_all();
    }

    #[test]
    fn shell_manager_buffer_id_and_label() {
        let (mut mgr, _rx) = ShellManager::new();
        let (id, label) = mgr.open(80, 24, Some("/bin/sh"), "shell/sh").unwrap();
        assert_eq!(mgr.buffer_id(&id), Some("shell/sh"));
        assert_eq!(label, "sh");
        assert_eq!(mgr.label(&id), Some("sh"));
        mgr.kill_all();
    }

    #[test]
    fn shell_manager_list_sessions() {
        let (mut mgr, _rx) = ShellManager::new();
        let _ = mgr.open(80, 24, Some("/bin/sh"), "shell/sh").unwrap();
        let sessions = mgr.list_sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].1, "shell/sh");
        assert_eq!(sessions[0].2, "sh");
        mgr.kill_all();
    }

    #[test]
    fn shell_manager_kill_all_clears_sessions() {
        let (mut mgr, _rx) = ShellManager::new();
        let _ = mgr.open(80, 24, Some("/bin/sh"), "shell/sh1").unwrap();
        let _ = mgr.open(80, 24, Some("/bin/sh"), "shell/sh2").unwrap();
        assert_eq!(mgr.session_count(), 2);
        mgr.kill_all();
        assert_eq!(mgr.session_count(), 0);
    }

    #[test]
    fn shell_manager_process_output_updates_screen() {
        let (mut mgr, _rx) = ShellManager::new();
        let (id, _) = mgr.open(80, 24, Some("/bin/sh"), "shell/sh").unwrap();

        // Feed known text directly into the vt100 parser.
        mgr.process_output(&id, b"hello world");

        let screen = mgr.screen(&id).unwrap();
        let contents = screen.contents();
        assert!(
            contents.contains("hello world"),
            "screen should contain 'hello world', got: {contents:?}"
        );
        mgr.kill_all();
    }

    #[test]
    fn shell_manager_resize_updates_parser() {
        let (mut mgr, _rx) = ShellManager::new();
        let (id, _) = mgr.open(80, 24, Some("/bin/sh"), "shell/sh").unwrap();

        let screen = mgr.screen(&id).unwrap();
        assert_eq!(screen.size(), (24, 80));

        mgr.resize(&id, 120, 40);

        let screen = mgr.screen(&id).unwrap();
        assert_eq!(screen.size(), (40, 120));
        mgr.kill_all();
    }

    #[test]
    fn shell_manager_screen_returns_none_for_unknown_id() {
        let (mgr, _rx) = ShellManager::new();
        assert!(mgr.screen("nonexistent").is_none());
    }

    #[test]
    fn shell_manager_open_returns_label_from_command() {
        let (mut mgr, _rx) = ShellManager::new();
        let (_, label) = mgr.open(80, 24, Some("/usr/bin/env"), "shell/env").unwrap();
        assert_eq!(label, "env");
        mgr.kill_all();
    }

    #[test]
    fn shell_manager_multiple_opens_get_unique_ids() {
        let (mut mgr, _rx) = ShellManager::new();
        let (id1, _) = mgr.open(80, 24, Some("/bin/sh"), "shell/sh1").unwrap();
        let (id2, _) = mgr.open(80, 24, Some("/bin/sh"), "shell/sh2").unwrap();
        assert_ne!(id1, id2);
        assert_eq!(mgr.session_count(), 2);
        mgr.kill_all();
    }

    // ── rewrite_hvp_to_cup tests ──────────────────────────────────────────

    #[test]
    fn hvp_rewrite_converts_csi_f_to_csi_h() {
        // CSI 5;10 f → CSI 5;10 H
        let input = b"\x1b[5;10f";
        let output = rewrite_hvp_to_cup(input);
        assert_eq!(output, b"\x1b[5;10H");
    }

    #[test]
    fn hvp_rewrite_preserves_plain_text_with_f() {
        let input = b"hello from foo";
        let output = rewrite_hvp_to_cup(input);
        assert_eq!(output, b"hello from foo");
    }

    #[test]
    fn hvp_rewrite_preserves_other_csi_sequences() {
        // CSI 2 J (clear screen) should not be modified.
        let input = b"\x1b[2J";
        let output = rewrite_hvp_to_cup(input);
        assert_eq!(output, b"\x1b[2J");
    }

    #[test]
    fn hvp_rewrite_handles_mixed_content() {
        // Text, then HVP, then more text, then a normal CSI.
        let input = b"abc\x1b[1;1fxyz\x1b[0m";
        let output = rewrite_hvp_to_cup(input);
        assert_eq!(output, b"abc\x1b[1;1Hxyz\x1b[0m");
    }

    #[test]
    fn hvp_rewrite_handles_bare_csi_f() {
        // CSI f with no params → CSI H (default position 1;1).
        let input = b"\x1b[f";
        let output = rewrite_hvp_to_cup(input);
        assert_eq!(output, b"\x1b[H");
    }

    #[test]
    fn hvp_rewrite_multiple_hvp_in_sequence() {
        let input = b"\x1b[1;1f\x1b[2;5f\x1b[10;20f";
        let output = rewrite_hvp_to_cup(input);
        assert_eq!(output, b"\x1b[1;1H\x1b[2;5H\x1b[10;20H");
    }

    #[test]
    fn hvp_rewrite_does_not_touch_sgr_with_f_in_params() {
        // CSI 38;2;255;0;0 m — the '0' params contain no 'f', final byte is 'm'.
        let input = b"\x1b[38;2;255;0;0m";
        let output = rewrite_hvp_to_cup(input);
        assert_eq!(output, b"\x1b[38;2;255;0;0m");
    }

    #[test]
    fn hvp_process_output_applies_cursor_position() {
        let (mut mgr, _rx) = ShellManager::new();
        let (id, _) = mgr.open(80, 24, Some("/bin/sh"), "shell/sh").unwrap();

        // Feed HVP sequence to move cursor to row 5, col 10 (1-based).
        mgr.process_output(&id, b"\x1b[5;10f");

        let screen = mgr.screen(&id).unwrap();
        // vt100 cursor_position() returns 0-based (row, col).
        assert_eq!(screen.cursor_position(), (4, 9));
        mgr.kill_all();
    }
}


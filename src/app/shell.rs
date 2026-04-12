use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use crate::config;
use crate::state::buffer::{ActivityLevel, Buffer, BufferType, make_buffer_id};
use crate::state::connection::{Connection, ConnectionStatus};

use super::App;

impl App {
    /// Connection ID used for the synthetic "Shell" sidebar group.
    pub const SHELL_CONN_ID: &'static str = "_shell";

    /// Handle an event from a shell PTY reader thread.
    pub(crate) fn handle_shell_event(&mut self, ev: crate::shell::ShellEvent) {
        match ev {
            crate::shell::ShellEvent::Output { id, bytes } => {
                if self.shell_mgr.is_web_session(&id) {
                    self.shell_mgr.process_output_web(&id, &bytes);
                    self.maybe_broadcast_web_shell_screen(&id);
                } else {
                    self.shell_mgr.process_output(&id, &bytes);
                    // Broadcast TUI shell screen to web clients (throttled).
                    self.maybe_broadcast_shell_screen(&id);
                }
            }
            crate::shell::ShellEvent::Exited { id, status } => {
                tracing::info!(shell_id = %id, ?status, "shell process exited");
                if self.shell_mgr.is_web_session(&id) {
                    self.shell_mgr.close_web(&id);
                } else if let Some(buffer_id) =
                    self.shell_mgr.buffer_id(&id).map(ToString::to_string)
                {
                    self.shell_mgr.close(&id);
                    self.state.remove_buffer(&buffer_id);
                    self.maybe_remove_shell_connection();
                    if self
                        .state
                        .active_buffer()
                        .is_none_or(|b| b.buffer_type != BufferType::Shell)
                    {
                        self.shell_input_active = false;
                    }
                } else {
                    self.shell_mgr.close(&id);
                }
            }
        }
    }

    /// Close a shell buffer (called from /close command handler).
    pub fn close_shell_buffer(&mut self, buf_id: &str) {
        if let Some(sid) = self
            .shell_mgr
            .session_id_for_buffer(buf_id)
            .map(ToString::to_string)
        {
            self.shell_mgr.close(&sid);
        }
        self.state.remove_buffer(buf_id);
        self.maybe_remove_shell_connection();
    }

    /// Add the synthetic "Shell" connection header if not already present.
    pub fn ensure_shell_connection(&mut self) {
        if self.state.connections.contains_key(Self::SHELL_CONN_ID) {
            return;
        }
        self.state.add_connection(Connection {
            id: Self::SHELL_CONN_ID.to_string(),
            label: "Shell".to_string(),
            status: ConnectionStatus::Connected,
            nick: String::new(),
            user_modes: String::new(),
            isupport: HashMap::new(),
            isupport_parsed: crate::irc::isupport::Isupport::new(),
            error: None,
            lag: None,
            lag_pending: false,
            reconnect_attempts: 0,
            reconnect_delay_secs: 0,
            next_reconnect: None,
            should_reconnect: false,
            joined_channels: Vec::new(),
            origin_config: config::ServerConfig {
                label: String::new(),
                address: String::new(),
                port: 0,
                tls: false,
                tls_verify: true,
                autoconnect: false,
                channels: vec![],
                nick: None,
                username: None,
                realname: None,
                password: None,
                sasl_user: None,
                sasl_pass: None,
                bind_ip: None,
                encoding: None,
                auto_reconnect: Some(false),
                reconnect_delay: None,
                reconnect_max_retries: None,
                autosendcmd: None,
                sasl_mechanism: None,
                client_cert_path: None,
            },
            local_ip: None,
            enabled_caps: HashSet::new(),
            who_token_counter: 0,
            silent_who_channels: HashSet::new(),
        });
        // Add a Server-type buffer so the sidebar header renders.
        let header_id = make_buffer_id(Self::SHELL_CONN_ID, "Shell");
        self.state.add_buffer(Buffer {
            id: header_id,
            connection_id: Self::SHELL_CONN_ID.to_string(),
            buffer_type: BufferType::Server,
            name: "Shell".to_string(),
            messages: VecDeque::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: chrono::Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
            last_speakers: Vec::new(),
            peer_handle: None,
        });
    }

    /// Remove the synthetic "Shell" connection if no shell sessions remain.
    pub fn maybe_remove_shell_connection(&mut self) {
        if self.shell_mgr.session_count() > 0 {
            return;
        }
        let header_id = make_buffer_id(Self::SHELL_CONN_ID, "Shell");
        self.state.remove_buffer(&header_id);
        self.state.connections.remove(Self::SHELL_CONN_ID);
        self.shell_input_active = false;
    }

    /// Resize all active shell PTYs to match the current chat area dimensions.
    pub fn resize_all_shells(&mut self) {
        if self.shell_mgr.session_count() == 0 {
            return;
        }
        let (cols, rows) = crate::ui::layout::compute_chat_area_size(
            self.cached_term_cols,
            self.cached_term_rows,
            self.config.sidepanel.left.visible,
            self.config.sidepanel.left.width,
            false,
            0,
        );
        let ids: Vec<String> = self
            .shell_mgr
            .list_sessions()
            .iter()
            .map(|(id, _, _)| (*id).to_string())
            .collect();
        for id in &ids {
            self.shell_mgr.resize(id, cols, rows);
        }
    }

    /// TUI shell screen broadcast — no-op now that web has its own PTY.
    #[expect(
        clippy::unused_self,
        clippy::missing_const_for_fn,
        reason = "stub — will gain a body when TUI shell broadcast is implemented"
    )]
    pub(crate) fn maybe_broadcast_shell_screen(&self, _shell_id: &str) {}

    /// TUI shell screen broadcast — used as initial fallback when web client
    /// switches to a shell buffer before the web PTY is created.
    pub(crate) fn force_broadcast_shell_screen(&self, shell_id: &str) {
        let Some(buffer_id) = self.shell_mgr.buffer_id(shell_id).map(ToString::to_string) else {
            return;
        };
        let Some((rows, cursor_row, cursor_col, cursor_visible)) =
            self.shell_mgr.screen_to_web(shell_id)
        else {
            return;
        };
        let cols = self.shell_mgr.screen_cols(shell_id);
        self.broadcast_web(crate::web::protocol::WebEvent::ShellScreen {
            buffer_id,
            cols,
            rows,
            cursor_row,
            cursor_col,
            cursor_visible,
        });
    }

    /// Broadcast web shell screen (throttled).
    pub(crate) fn maybe_broadcast_web_shell_screen(&mut self, web_id: &str) {
        let now = Instant::now();
        if now
            .duration_since(self.last_shell_web_broadcast)
            .as_millis()
            < 100
        {
            self.shell_broadcast_pending = Some(web_id.to_string());
            return;
        }
        self.shell_broadcast_pending = None;
        self.force_broadcast_web_shell_screen(web_id);
    }

    /// Broadcast web shell screen immediately.
    pub(crate) fn force_broadcast_web_shell_screen(&mut self, web_id: &str) {
        self.last_shell_web_broadcast = Instant::now();

        let Some((rows, cursor_row, cursor_col, cursor_visible)) =
            self.shell_mgr.screen_to_web_session(web_id)
        else {
            return;
        };
        let cols = self.shell_mgr.screen_cols_web(web_id);
        let buffer_id = self
            .state
            .active_buffer()
            .map(|b| b.id.clone())
            .unwrap_or_default();
        self.broadcast_web(crate::web::protocol::WebEvent::ShellScreen {
            buffer_id,
            cols,
            rows,
            cursor_row,
            cursor_col,
            cursor_visible,
        });
    }
}

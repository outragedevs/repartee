use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use chrono::Utc;
use color_eyre::eyre::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Position;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};

use crate::config::{self, AppConfig};
use crate::constants;
use crate::irc::{self, IrcEvent, IrcHandle};
use crate::state::AppState;
use crate::state::buffer::{
    make_buffer_id, ActivityLevel, Buffer, BufferType, Message, MessageType,
};
use crate::state::connection::{Connection, ConnectionStatus};
use crate::theme::{self, ThemeFile};
use crate::ui;
use crate::ui::layout::UiRegions;

pub struct App {
    pub state: AppState,
    pub config: AppConfig,
    pub theme: ThemeFile,
    pub input: ui::input::InputState,
    pub should_quit: bool,
    pub scroll_offset: usize,
    pub ui_regions: Option<UiRegions>,
    /// IRC connection handles keyed by connection ID.
    pub irc_handles: HashMap<String, IrcHandle>,
    /// Shared event sender — each connection's reader task sends here.
    pub irc_tx: mpsc::UnboundedSender<IrcEvent>,
    /// Single receiver for all IRC events.
    irc_rx: mpsc::UnboundedReceiver<IrcEvent>,
    /// Timestamp of last ESC keypress for ESC+key buffer switching.
    last_esc_time: Option<Instant>,
    /// Scroll offset for the buffer list (left sidebar).
    pub buffer_list_scroll: usize,
    /// Total line count in buffer list (set during render, used for scroll clamping).
    pub buffer_list_total: usize,
    /// Scroll offset for the nick list (right sidebar).
    pub nick_list_scroll: usize,
    /// Total line count in nick list (set during render, used for scroll clamping).
    pub nick_list_total: usize,
    /// Last CTCP PING sent time per connection, for lag measurement.
    lag_pings: HashMap<String, Instant>,
    /// Storage subsystem for persistent message logging.
    pub storage: Option<crate::storage::Storage>,
}

impl App {
    pub fn new() -> Result<Self> {
        constants::ensure_config_dir();
        let mut config = config::load_config(&constants::config_path())?;

        // Load .env credentials and apply to server configs
        let env_vars = config::load_env(&constants::env_path())?;
        config::apply_credentials(&mut config.servers, &env_vars);
        let theme_path = constants::theme_dir().join(format!("{}.theme", config.general.theme));
        let theme = theme::load_theme(&theme_path)?;

        let mut state = AppState::new();
        state.flood_protection = config.general.flood_protection;
        state.ignores.clone_from(&config.ignores);
        let (irc_tx, irc_rx) = mpsc::unbounded_channel();

        // Initialize storage if logging is enabled
        let storage = if config.logging.enabled {
            match crate::storage::Storage::init(&config.logging) {
                Ok(s) => {
                    state.log_tx = Some(s.log_tx.clone());
                    state.log_exclude_types.clone_from(&config.logging.exclude_types);
                    Some(s)
                }
                Err(e) => {
                    tracing::error!("failed to initialize storage: {e}");
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            state,
            config,
            theme,
            input: ui::input::InputState::new(),
            should_quit: false,
            scroll_offset: 0,
            ui_regions: None,
            irc_handles: HashMap::new(),
            irc_tx,
            irc_rx,
            last_esc_time: None,
            buffer_list_scroll: 0,
            buffer_list_total: 0,
            nick_list_scroll: 0,
            nick_list_total: 0,
            lag_pings: HashMap::new(),
            storage,
        })
    }

    /// Set up connection state, server buffer, and "Connecting..." message.
    /// Returns the server buffer ID. Shared by autoconnect and /connect command.
    pub fn setup_connection(
        &mut self,
        conn_id: &str,
        server_config: &config::ServerConfig,
    ) -> String {
        // Remove placeholder default Status buffer when first real connection starts
        let default_buf_id = make_buffer_id(Self::DEFAULT_CONN_ID, "Status");
        if self.state.buffers.contains_key(&default_buf_id) {
            self.state.remove_buffer(&default_buf_id);
            self.state.connections.remove(Self::DEFAULT_CONN_ID);
        }

        let auto_reconnect = server_config.auto_reconnect.unwrap_or(true);
        let reconnect_delay = server_config.reconnect_delay.unwrap_or(30);
        let reconnect_max = server_config.reconnect_max_retries.unwrap_or(10);

        self.state.add_connection(Connection {
            id: conn_id.to_string(),
            label: server_config.label.clone(),
            status: ConnectionStatus::Connecting,
            nick: server_config
                .nick
                .as_deref()
                .unwrap_or(&self.config.general.nick)
                .to_string(),
            user_modes: String::new(),
            isupport: HashMap::new(),
            isupport_parsed: crate::irc::isupport::Isupport::new(),
            error: None,
            lag: None,
            reconnect_attempts: 0,
            max_reconnect_attempts: reconnect_max,
            reconnect_delay_secs: reconnect_delay,
            next_reconnect: None,
            should_reconnect: auto_reconnect,
            joined_channels: server_config.channels.clone(),
            origin_config: server_config.clone(),
        });

        let server_buf_id = make_buffer_id(conn_id, &server_config.label);
        self.state.add_buffer(Buffer {
            id: server_buf_id.clone(),
            connection_id: conn_id.to_string(),
            buffer_type: BufferType::Server,
            name: server_config.label.clone(),
            messages: Vec::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
        });
        self.state.set_active_buffer(&server_buf_id);

        let id = self.state.next_message_id();
        self.state.add_message(
            &server_buf_id,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: format!("Connecting to {}...", server_config.label),
                highlight: false,
                event_key: None,
                event_params: None, log_msg_id: None, log_ref_id: None,
            },
        );

        server_buf_id
    }

    /// Connect to a server defined in config by its key (e.g. "libera").
    /// Used for autoconnect at startup.
    async fn connect_server_async(&mut self, server_id: &str) -> Result<()> {
        let server_config = match self.config.servers.get(server_id) {
            Some(cfg) => cfg.clone(),
            None => {
                return Ok(());
            }
        };

        let conn_id = server_id.to_string();
        self.setup_connection(&conn_id, &server_config);

        let general = self.config.general.clone();
        match irc::connect_server(&conn_id, &server_config, &general).await {
            Ok((handle, mut rx)) => {
                let tx = self.irc_tx.clone();
                self.irc_handles.insert(conn_id.clone(), handle);

                // Spawn task to forward events from per-connection receiver to shared channel
                tokio::spawn(async move {
                    while let Some(event) = rx.recv().await {
                        if tx.send(event).is_err() {
                            break;
                        }
                    }
                });
            }
            Err(e) => {
                crate::irc::events::handle_disconnected(
                    &mut self.state,
                    &conn_id,
                    Some(&e.to_string()),
                );
            }
        }

        Ok(())
    }

    pub async fn run(&mut self, terminal: &mut ui::Tui) -> Result<()> {
        // Auto-connect to servers marked with autoconnect
        let autoconnect_ids: Vec<String> = self
            .config
            .servers
            .iter()
            .filter(|(_, cfg)| cfg.autoconnect)
            .map(|(id, _)| id.clone())
            .collect();

        for server_id in &autoconnect_ids {
            let _ = self.connect_server_async(server_id).await;
        }

        // If no servers configured or no autoconnect, show a default status buffer
        if self.state.buffers.is_empty() {
            Self::create_default_status(&mut self.state);
        }

        // Spawn a dedicated blocking task for terminal event reading.
        // Uses poll() with a short timeout so the thread can check the
        // stop flag and exit cleanly when the app quits.
        let (term_tx, mut term_rx) = mpsc::unbounded_channel();
        let reader_stop = Arc::new(AtomicBool::new(false));
        let reader_stop2 = Arc::clone(&reader_stop);
        tokio::task::spawn_blocking(move || {
            while !reader_stop2.load(Ordering::Relaxed) {
                if event::poll(std::time::Duration::from_millis(100)).unwrap_or(false) {
                    match event::read() {
                        Ok(ev) => {
                            if term_tx.send(ev).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        });

        let mut tick = interval(Duration::from_secs(1));

        while !self.should_quit {
            terminal.draw(|frame| ui::layout::draw(frame, self))?;

            tokio::select! {
                ev = term_rx.recv() => match ev {
                    Some(ev) => {
                        self.handle_event(ev);
                        // Drain all queued events before redrawing.
                        while let Ok(ev) = term_rx.try_recv() {
                            self.handle_event(ev);
                        }
                    }
                    None => break,
                },
                irc_ev = self.irc_rx.recv() => {
                    if let Some(event) = irc_ev {
                        self.handle_irc_event(event);
                    }
                },
                _ = tick.tick() => {
                    self.handle_netsplit_tick();
                    self.check_reconnects();
                    self.measure_lag();
                }
            }
        }

        // Stop the terminal reader thread so it doesn't interfere with restore.
        reader_stop.store(true, Ordering::Relaxed);

        // Send QUIT to all connected servers
        for handle in self.irc_handles.values() {
            let _ = handle.sender.send_quit("Leaving");
        }

        // Shut down storage writer (flushes remaining rows)
        if let Some(storage) = self.storage.take() {
            storage.shutdown().await;
        }

        Ok(())
    }

    /// Connection ID for the app-level default Status buffer.
    pub const DEFAULT_CONN_ID: &'static str = "_default";

    fn create_default_status(state: &mut AppState) {
        let buf_id = make_buffer_id(Self::DEFAULT_CONN_ID, "Status");
        state.add_connection(Connection {
            id: Self::DEFAULT_CONN_ID.to_string(),
            label: "Status".to_string(),
            status: ConnectionStatus::Disconnected,
            nick: String::new(),
            user_modes: String::new(),
            isupport: HashMap::new(),
            isupport_parsed: crate::irc::isupport::Isupport::new(),
            error: None,
            lag: None,
            reconnect_attempts: 0,
            max_reconnect_attempts: 0,
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
            },
        });
        state.add_buffer(Buffer {
            id: buf_id.clone(),
            connection_id: Self::DEFAULT_CONN_ID.to_string(),
            buffer_type: BufferType::Server,
            name: "Status".to_string(),
            messages: Vec::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
        });
        state.set_active_buffer(&buf_id);

        let id = state.next_message_id();
        state.add_message(
            &buf_id,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: "Welcome to rustirc! Use /connect <server> to connect.".to_string(),
                highlight: false,
                event_key: None,
                event_params: None, log_msg_id: None, log_ref_id: None,
            },
        );
    }

    /// Recreate the default Status buffer if no real buffers remain.
    pub fn ensure_default_status(&mut self) {
        // Check if any non-default buffers exist
        let has_real_buffers = self
            .state
            .buffers
            .values()
            .any(|b| b.connection_id != Self::DEFAULT_CONN_ID);
        if !has_real_buffers {
            Self::create_default_status(&mut self.state);
        }
    }

    /// Tick the netsplit state and emit batched netsplit/netjoin messages.
    fn handle_netsplit_tick(&mut self) {
        let messages = self.state.netsplit_state.tick();
        for msg in messages {
            for buffer_id in &msg.buffer_ids {
                let id = self.state.next_message_id();
                self.state.add_message(
                    buffer_id,
                    Message {
                        id,
                        timestamp: Utc::now(),
                        message_type: MessageType::Event,
                        nick: None,
                        nick_mode: None,
                        text: msg.text.clone(),
                        highlight: false,
                        event_key: Some("netsplit".to_string()),
                        event_params: None, log_msg_id: None, log_ref_id: None,
                    },
                );
            }
        }
    }

    /// Add an event message to the specified buffer.
    fn add_event_to_buffer(&mut self, buffer_id: &str, text: String) {
        let id = self.state.next_message_id();
        self.state.add_message(
            buffer_id,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text,
                highlight: false,
                event_key: None,
                event_params: None,
                log_msg_id: None,
                log_ref_id: None,
            },
        );
    }

    /// Check connections that need reconnecting and spawn reconnect tasks.
    fn check_reconnects(&mut self) {
        let now = std::time::Instant::now();

        // Collect connections that need reconnecting
        let to_reconnect: Vec<String> = self
            .state
            .connections
            .iter()
            .filter(|(id, conn)| {
                matches!(
                    conn.status,
                    ConnectionStatus::Disconnected | ConnectionStatus::Error
                ) && conn.should_reconnect
                    && conn.next_reconnect.is_some_and(|t| t <= now)
                    && *id != Self::DEFAULT_CONN_ID
                    && !self.irc_handles.contains_key(id.as_str())
            })
            .map(|(id, _)| id.clone())
            .collect();

        for conn_id in to_reconnect {
            let Some(conn) = self.state.connections.get_mut(&conn_id) else {
                continue;
            };

            conn.reconnect_attempts += 1;
            let attempts = conn.reconnect_attempts;
            let max = conn.max_reconnect_attempts;
            conn.next_reconnect = None;

            if attempts > max {
                conn.should_reconnect = false;
                let label = conn.label.clone();
                let buffer_id = make_buffer_id(&conn_id, &label);
                self.add_event_to_buffer(
                    &buffer_id,
                    format!("Reconnect failed after {max} attempts. Use /connect to retry."),
                );
                continue;
            }

            let conn = self.state.connections.get(&conn_id);
            let label = conn.map_or_else(|| conn_id.clone(), |c| c.label.clone());
            let server_config = conn.map(|c| c.origin_config.clone());

            let buffer_id = make_buffer_id(&conn_id, &label);
            self.add_event_to_buffer(
                &buffer_id,
                format!("Reconnecting to {label} (attempt {attempts}/{max})..."),
            );

            if let Some(conn) = self.state.connections.get_mut(&conn_id) {
                conn.status = ConnectionStatus::Connecting;
            }

            self.spawn_reconnect(&conn_id, server_config, &buffer_id, &label);
        }
    }

    /// Spawn a reconnect task or log failure if no config is available.
    fn spawn_reconnect(
        &mut self,
        conn_id: &str,
        server_config: Option<config::ServerConfig>,
        buffer_id: &str,
        label: &str,
    ) {
        if let Some(cfg) = server_config {
            let general = self.config.general.clone();
            let tx = self.irc_tx.clone();
            let id = conn_id.to_string();
            tokio::spawn(async move {
                match crate::irc::connect_server(&id, &cfg, &general).await {
                    Ok((handle, mut rx)) => {
                        let _ = tx.send(IrcEvent::HandleReady(
                            handle.conn_id.clone(),
                            handle.sender,
                        ));
                        while let Some(event) = rx.recv().await {
                            if tx.send(event).is_err() {
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(IrcEvent::Disconnected(id, Some(e.to_string())));
                    }
                }
            });
        } else {
            if let Some(conn) = self.state.connections.get_mut(conn_id) {
                conn.should_reconnect = false;
                conn.status = ConnectionStatus::Disconnected;
            }
            self.add_event_to_buffer(
                buffer_id,
                format!("Cannot reconnect to {label}: server config not found"),
            );
        }
    }

    /// Send IRC PING every 30 seconds per connection to measure lag.
    ///
    /// Uses the current timestamp (ms since UNIX epoch) as the PING token.
    /// When the server responds with PONG containing the same token, we
    /// compute the round-trip time in `handle_irc_event`.
    fn measure_lag(&mut self) {
        let now = Instant::now();
        let conn_ids: Vec<String> = self.irc_handles.keys().cloned().collect();
        for conn_id in conn_ids {
            let is_connected = self
                .state
                .connections
                .get(&conn_id)
                .is_some_and(|c| c.status == ConnectionStatus::Connected);
            if !is_connected {
                continue;
            }

            let should_ping = self
                .lag_pings
                .get(&conn_id)
                .is_none_or(|last| now.duration_since(*last).as_secs() >= 30);

            if should_ping {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .to_string();
                if let Some(handle) = self.irc_handles.get(&conn_id) {
                    let _ = handle.sender.send(::irc::proto::Command::Raw(
                        "PING".to_string(),
                        vec![ts],
                    ));
                }
                self.lag_pings.insert(conn_id, now);
            }
        }
    }

    /// Execute autosendcmd string after successful connection.
    ///
    /// Format: semicolon-separated commands with optional `WAIT <ms>` delays.
    /// Commands without a leading `/` get one prepended automatically.
    /// `$N` / `${N}` are replaced with the current nick.
    ///
    /// WAIT delays are currently skipped (commands execute immediately).
    fn execute_autosendcmd(&mut self, conn_id: &str, cmds: &str) {
        let nick = self
            .state
            .connections
            .get(conn_id)
            .map(|c| c.nick.clone())
            .unwrap_or_default();

        for part in cmds.split(';') {
            let cmd = part.trim();
            if cmd.is_empty() {
                continue;
            }
            // Skip WAIT delays (async delay support can be added later)
            if cmd.to_uppercase().starts_with("WAIT") {
                continue;
            }
            // Replace $N / ${N} with current nick
            let expanded = cmd.replace("$N", &nick).replace("${N}", &nick);
            // Prepend / if not already a command
            let line = if expanded.starts_with('/') {
                expanded
            } else {
                format!("/{expanded}")
            };
            // Parse and execute as if user typed it
            if let Some(parsed) = crate::commands::parser::parse_command(&line) {
                self.execute_command(&parsed);
            }
        }
    }

    fn handle_irc_event(&mut self, event: IrcEvent) {
        match event {
            IrcEvent::HandleReady(conn_id, sender) => {
                self.irc_handles.insert(
                    conn_id.clone(),
                    IrcHandle {
                        conn_id,
                        sender,
                    },
                );
            }
            IrcEvent::Connected(conn_id) => {
                // Collect channels to rejoin before handle_connected resets state
                let rejoin_channels =
                    crate::irc::events::channels_to_rejoin(&self.state, &conn_id);
                crate::irc::events::handle_connected(&mut self.state, &conn_id);
                // Auto-rejoin channels after reconnect
                if !rejoin_channels.is_empty()
                    && let Some(handle) = self.irc_handles.get(&conn_id)
                {
                    for channel in &rejoin_channels {
                        let _ = handle.sender.send_join(channel);
                    }
                }
                // Execute autosendcmd: check config file first, fall back to origin_config
                let autosendcmd = self
                    .config
                    .servers
                    .iter()
                    .find(|(id, cfg)| *id == &conn_id || cfg.label == conn_id)
                    .and_then(|(_, cfg)| cfg.autosendcmd.clone())
                    .or_else(|| {
                        self.state
                            .connections
                            .get(&conn_id)
                            .and_then(|c| c.origin_config.autosendcmd.clone())
                    });
                if let Some(cmds) = autosendcmd {
                    self.execute_autosendcmd(&conn_id, &cmds);
                }
            }
            IrcEvent::Disconnected(conn_id, error) => {
                crate::irc::events::handle_disconnected(
                    &mut self.state,
                    &conn_id,
                    error.as_deref(),
                );
                self.irc_handles.remove(&conn_id);
                self.lag_pings.remove(&conn_id);
            }
            IrcEvent::Message(conn_id, msg) => {
                // Intercept PONG to update lag measurement
                if let ::irc::proto::Command::PONG(_, _) = &msg.command
                    && let Some(sent_at) = self.lag_pings.get(&conn_id)
                {
                    // Lag will never exceed u64::MAX milliseconds
                    let lag_ms = u64::try_from(sent_at.elapsed().as_millis()).unwrap_or(u64::MAX);
                    if let Some(conn) = self.state.connections.get_mut(&conn_id) {
                        conn.lag = Some(lag_ms);
                    }
                }
                // Check for nick-in-use before processing — capture the new nick
                // that events.rs will set so we can send the NICK command.
                let nick_retry = if let ::irc::proto::Command::Response(
                    ::irc::proto::Response::ERR_NICKNAMEINUSE, _
                ) = &msg.command {
                    // events.rs updates conn.nick to attempted + "_"
                    // We need to send the actual NICK command since events.rs can't.
                    true
                } else {
                    false
                };

                crate::irc::events::handle_irc_message(&mut self.state, &conn_id, &msg);

                // Send NICK command for nick-in-use retry
                if nick_retry
                    && let Some(conn) = self.state.connections.get(&conn_id)
                    && let Some(handle) = self.irc_handles.get(&conn_id)
                {
                    let _ = handle.sender.send(::irc::proto::Command::NICK(conn.nick.clone()));
                }
            }
        }
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Paste(text) => self.handle_paste(&text),
            // Resize and other events: redraw happens automatically on next loop iteration
            _ => {}
        }
    }

    /// Maximum time (ms) between ESC and follow-up key to treat as ESC+key combo.
    const ESC_TIMEOUT_MS: u128 = 500;

    /// Check if a recent ESC press should combine with the current key.
    fn consume_esc_prefix(&mut self) -> bool {
        self.last_esc_time
            .take()
            .is_some_and(|t| t.elapsed().as_millis() < Self::ESC_TIMEOUT_MS)
    }

    /// Switch to buffer N (0-9) — shared logic for Alt+N and ESC+N.
    fn switch_to_buffer_num(&mut self, n: usize) {
        if n == 0 {
            // 0 goes to default Status buffer
            let default_buf_id = make_buffer_id(Self::DEFAULT_CONN_ID, "Status");
            if self.state.buffers.contains_key(&default_buf_id) {
                self.state.set_active_buffer(&default_buf_id);
                self.scroll_offset = 0;
                self.reset_sidepanel_scrolls();
            }
        } else {
            // 1..9 map to real buffers (excluding _default)
            let real_ids: Vec<_> = self
                .state
                .sorted_buffer_ids()
                .into_iter()
                .filter(|id| {
                    self.state
                        .buffers
                        .get(id.as_str())
                        .is_none_or(|b| b.connection_id != Self::DEFAULT_CONN_ID)
                })
                .collect();
            let idx = n - 1; // 1 = index 0
            if idx < real_ids.len() {
                self.state.set_active_buffer(&real_ids[idx]);
                self.scroll_offset = 0;
                self.reset_sidepanel_scrolls();
            }
        }
    }

    /// Reset sidepanel scroll offsets (e.g. on buffer switch).
    #[allow(clippy::missing_const_for_fn)] // const &mut self not stable
    fn reset_sidepanel_scrolls(&mut self) {
        self.buffer_list_scroll = 0;
        self.nick_list_scroll = 0;
    }

    fn handle_key(&mut self, key: event::KeyEvent) {
        // Check for ESC+key combos (ESC pressed recently, now a follow-up key)
        let esc_active = if key.code == KeyCode::Esc {
            // Don't consume ESC prefix on another ESC press
            self.last_esc_time.take();
            false
        } else {
            self.consume_esc_prefix()
        };

        // ESC+digit → buffer switch (like Alt+digit)
        // ESC+Left/Right → prev/next buffer (like Alt+Left/Right)
        if esc_active {
            match key.code {
                KeyCode::Char(c) if c.is_ascii_digit() && key.modifiers.is_empty() => {
                    let n = c.to_digit(10).unwrap_or(0) as usize;
                    self.switch_to_buffer_num(n);
                    return;
                }
                KeyCode::Left if key.modifiers.is_empty() => {
                    self.state.prev_buffer();
                    self.scroll_offset = 0;
                    self.reset_sidepanel_scrolls();
                    return;
                }
                KeyCode::Right if key.modifiers.is_empty() => {
                    self.state.next_buffer();
                    self.scroll_offset = 0;
                    self.reset_sidepanel_scrolls();
                    return;
                }
                _ => {
                    // ESC expired or unrecognized follow-up — fall through to normal handling
                }
            }
        }

        match (key.modifiers, key.code) {
            // ESC — record timestamp for potential ESC+key combo
            (_, KeyCode::Esc) => {
                self.last_esc_time = Some(Instant::now());
            }
            (KeyModifiers::CONTROL, KeyCode::Char('q' | 'c')) => self.should_quit = true,
            (KeyModifiers::CONTROL, KeyCode::Char('l')) => {
                // Force redraw (happens automatically on next iteration)
            }
            // Ctrl+U — clear line from cursor to start
            (KeyModifiers::CONTROL, KeyCode::Char('u')) => self.input.clear_to_start(),
            // Ctrl+K — clear line from cursor to end
            (KeyModifiers::CONTROL, KeyCode::Char('k')) => self.input.clear_to_end(),
            // Ctrl+W — delete word before cursor
            (KeyModifiers::CONTROL, KeyCode::Char('w')) => self.input.delete_word_back(),
            // Ctrl+A — move cursor to start (same as Home)
            (KeyModifiers::CONTROL, KeyCode::Char('a')) | (_, KeyCode::Home) => self.input.home(),
            // Ctrl+E — move cursor to end (same as End)
            (KeyModifiers::CONTROL, KeyCode::Char('e')) | (_, KeyCode::End) => {
                self.input.end();
                self.scroll_offset = 0;
            }
            (KeyModifiers::ALT, KeyCode::Char(c)) if c.is_ascii_digit() => {
                let n = c.to_digit(10).unwrap_or(0) as usize;
                self.switch_to_buffer_num(n);
            }
            (mods, KeyCode::Left) if mods.contains(KeyModifiers::ALT) => {
                self.state.prev_buffer();
                self.scroll_offset = 0;
                self.reset_sidepanel_scrolls();
            }
            (mods, KeyCode::Right) if mods.contains(KeyModifiers::ALT) => {
                self.state.next_buffer();
                self.scroll_offset = 0;
                self.reset_sidepanel_scrolls();
            }
            (_, KeyCode::Enter) => {
                let text = self.input.submit();
                if !text.is_empty() {
                    self.handle_submit(text);
                }
            }
            (_, KeyCode::Backspace) => self.input.backspace(),
            (_, KeyCode::Delete) => self.input.delete(),
            (mods, KeyCode::Left) if !mods.contains(KeyModifiers::ALT) => self.input.move_left(),
            (mods, KeyCode::Right) if !mods.contains(KeyModifiers::ALT) => self.input.move_right(),
            (_, KeyCode::Up) => self.input.history_up(),
            (_, KeyCode::Down) => self.input.history_down(),
            (_, KeyCode::PageUp) => {
                self.scroll_offset = self.scroll_offset.saturating_add(10);
            }
            (_, KeyCode::PageDown) => {
                self.scroll_offset = self.scroll_offset.saturating_sub(10);
            }
            (_, KeyCode::Tab) => self.handle_tab(),
            (mods, KeyCode::Char(c))
                if mods.is_empty() || mods == KeyModifiers::SHIFT =>
            {
                self.input.insert_char(c);
            }
            _ => {}
        }
    }

    fn handle_paste(&mut self, text: &str) {
        // Replace newlines with spaces for single-line input (standard IRC behavior).
        // Multiline paste in IRC clients typically collapses to one line or sends
        // multiple lines — we collapse to avoid accidental message floods.
        let cleaned = text.replace('\n', " ").replace('\r', "");
        for ch in cleaned.chars() {
            self.input.insert_char(ch);
        }
    }

    fn handle_mouse(&mut self, mouse: event::MouseEvent) {
        let Some(regions) = self.ui_regions else {
            return;
        };
        let pos = Position::new(mouse.column, mouse.row);

        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if regions.chat_area.is_some_and(|r| r.contains(pos)) {
                    self.scroll_offset = self.scroll_offset.saturating_add(3);
                } else if regions.buffer_list_area.is_some_and(|r| r.contains(pos)) {
                    self.buffer_list_scroll = self.buffer_list_scroll.saturating_sub(1);
                } else if regions.nick_list_area.is_some_and(|r| r.contains(pos)) {
                    self.nick_list_scroll = self.nick_list_scroll.saturating_sub(1);
                }
            }
            MouseEventKind::ScrollDown => {
                if regions.chat_area.is_some_and(|r| r.contains(pos)) {
                    self.scroll_offset = self.scroll_offset.saturating_sub(3);
                } else if let Some(r) = regions.buffer_list_area
                    && r.contains(pos)
                {
                    let visible_h = r.height.saturating_sub(1) as usize; // account for border
                    let max = self.buffer_list_total.saturating_sub(visible_h);
                    if self.buffer_list_scroll < max {
                        self.buffer_list_scroll += 1;
                    }
                } else if let Some(r) = regions.nick_list_area
                    && r.contains(pos)
                {
                    let visible_h = r.height.saturating_sub(1) as usize;
                    let max = self.nick_list_total.saturating_sub(visible_h);
                    if self.nick_list_scroll < max {
                        self.nick_list_scroll += 1;
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(buf_area) = regions.buffer_list_area
                    && buf_area.contains(pos)
                {
                    let y_offset = (mouse.row - buf_area.y) as usize;
                    self.handle_buffer_list_click(y_offset);
                } else if let Some(nick_area) = regions.nick_list_area
                    && nick_area.contains(pos)
                {
                    let y_offset = (mouse.row - nick_area.y) as usize;
                    self.handle_nick_list_click(y_offset);
                }
            }
            _ => {}
        }
    }

    fn handle_buffer_list_click(&mut self, y_offset: usize) {
        use crate::state::buffer::BufferType;

        // Account for scroll offset: the visual row maps to a logical row
        let logical_row = y_offset + self.buffer_list_scroll;
        let sorted_ids = self.state.sorted_buffer_ids();
        // Map logical_row to the correct buffer, accounting for headers.
        // Server buffers are rendered as headers (not numbered items).
        let mut row = 0;
        let mut last_conn_id = String::new();
        for id in &sorted_ids {
            let Some(buf) = self.state.buffers.get(id.as_str()) else {
                continue;
            };
            if buf.connection_id == Self::DEFAULT_CONN_ID {
                continue;
            }
            // Connection header row
            if buf.connection_id != last_conn_id {
                last_conn_id.clone_from(&buf.connection_id);
                if row == logical_row {
                    // Clicked on header — switch to server buffer for this connection
                    if buf.buffer_type == BufferType::Server {
                        self.state.set_active_buffer(id);
                        self.scroll_offset = 0;
                        self.nick_list_scroll = 0;
                    }
                    return;
                }
                row += 1;
            }
            // Server buffers are the header, no separate row
            if buf.buffer_type == BufferType::Server {
                continue;
            }
            if row == logical_row {
                self.state.set_active_buffer(id);
                self.scroll_offset = 0;
                self.nick_list_scroll = 0;
                return;
            }
            row += 1;
        }
    }

    fn handle_nick_list_click(&mut self, y_offset: usize) {
        use crate::state::sorting;

        // Account for scroll offset
        let logical_row = y_offset + self.nick_list_scroll;

        // Row 0 is the "N users" header line — skip it
        if logical_row == 0 {
            return;
        }
        let nick_index = logical_row - 1;

        // Get the sorted nick list from the active buffer
        let (conn_id, nick_name) = {
            let Some(buf) = self.state.active_buffer() else {
                return;
            };
            if buf.buffer_type != BufferType::Channel {
                return;
            }
            let nick_refs: Vec<_> = buf.users.values().collect();
            let sorted = sorting::sort_nicks(&nick_refs, sorting::DEFAULT_PREFIX_ORDER);
            let Some(entry) = sorted.get(nick_index) else {
                return;
            };
            (buf.connection_id.clone(), entry.nick.clone())
        };

        // Create a query buffer for that nick if it doesn't exist, then switch to it
        let query_buf_id = make_buffer_id(&conn_id, &nick_name);
        if !self.state.buffers.contains_key(&query_buf_id) {
            self.state.add_buffer(Buffer {
                id: query_buf_id.clone(),
                connection_id: conn_id,
                buffer_type: BufferType::Query,
                name: nick_name,
                messages: Vec::new(),
                activity: ActivityLevel::None,
                unread_count: 0,
                last_read: Utc::now(),
                topic: None,
                topic_set_by: None,
                users: HashMap::new(),
                modes: None,
                mode_params: None,
                list_modes: HashMap::new(),
            });
        }
        self.state.set_active_buffer(&query_buf_id);
        self.scroll_offset = 0;
        self.nick_list_scroll = 0;
    }

    fn handle_tab(&mut self) {
        let nicks: Vec<String> = self
            .state
            .active_buffer()
            .map_or_else(Vec::new, |buf| buf.users.keys().cloned().collect());
        let commands = crate::commands::registry::get_command_names();
        let setting_paths = crate::commands::settings::get_setting_paths(&self.config);
        self.input.tab_complete(&nicks, &commands, &setting_paths);
    }

    fn handle_submit(&mut self, text: String) {
        if let Some(parsed) = crate::commands::parser::parse_command(&text) {
            self.execute_command(&parsed);
        } else {
            self.handle_plain_message(text);
        }
        self.scroll_offset = 0;
    }

    fn execute_command(&mut self, parsed: &crate::commands::parser::ParsedCommand) {
        let commands = crate::commands::registry::get_commands();
        // Find by name or alias (built-in commands first)
        let found = commands.into_iter().find(|(name, def)| {
            *name == parsed.name || def.aliases.contains(&parsed.name.as_str())
        });
        if let Some((_, def)) = found {
            (def.handler)(self, &parsed.args);
        } else if let Some(template) = self.config.aliases.get(&parsed.name).cloned() {
            // Expand user-defined alias
            let expanded = expand_alias_template(&template, &parsed.args);
            // Re-parse the expanded text (it may itself be a command)
            if let Some(reparsed) = crate::commands::parser::parse_command(&expanded) {
                self.execute_command(&reparsed);
            } else {
                self.handle_plain_message(expanded);
            }
        } else {
            crate::commands::helpers::add_local_event(
                self,
                &format!("Unknown command: /{}. Type /help for a list.", parsed.name),
            );
        }
    }

    fn handle_plain_message(&mut self, text: String) {
        let Some(active_id) = self.state.active_buffer_id.clone() else {
            return;
        };

        let (conn_id, nick, buffer_name) = {
            let Some(buf) = self.state.active_buffer() else {
                return;
            };
            // Only send to channels and queries, not server/status buffers
            if !matches!(buf.buffer_type, BufferType::Channel | BufferType::Query) {
                crate::commands::helpers::add_local_event(
                    self,
                    "Cannot send messages to this buffer",
                );
                return;
            }
            let conn = self.state.connections.get(&buf.connection_id);
            let nick = conn.map(|c| c.nick.clone()).unwrap_or_default();
            (buf.connection_id.clone(), nick, buf.name.clone())
        };

        // Try to send via IRC if connected
        if let Some(handle) = self.irc_handles.get(&conn_id)
            && handle.sender.send_privmsg(&buffer_name, &text).is_err()
        {
            crate::commands::helpers::add_local_event(self, "Failed to send message");
            return;
        }

        // Add to local buffer (echo)
        let id = self.state.next_message_id();
        self.state.add_message(
            &active_id,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Message,
                nick: Some(nick),
                nick_mode: None,
                text,
                highlight: false,
                event_key: None,
                event_params: None, log_msg_id: None, log_ref_id: None,
            },
        );
    }

    /// Get the IRC sender for the active buffer's connection, if connected.
    pub fn active_irc_sender(&self) -> Option<&::irc::client::Sender> {
        let buf = self.state.active_buffer()?;
        let handle = self.irc_handles.get(&buf.connection_id)?;
        Some(&handle.sender)
    }

    /// Get the connection ID of the active buffer.
    pub fn active_conn_id(&self) -> Option<String> {
        self.state
            .active_buffer()
            .map(|buf| buf.connection_id.clone())
    }
}

/// Expand an alias template with positional args.
///
/// Supported variables:
/// - `$0` through `$9` — positional arguments
/// - `$*` — all arguments joined by space
/// - `$-` — all arguments from position 0 onward (same as `$*`)
fn expand_alias_template(template: &str, args: &[String]) -> String {
    let all_args = args.join(" ");
    let mut result = template.to_string();

    // Replace $* and $- with all args
    result = result.replace("$*", &all_args);
    result = result.replace("$-", &all_args);

    // Replace $0-$9 with positional args
    for i in (0..=9).rev() {
        let var = format!("${i}");
        let val = args.get(i).map_or("", String::as_str);
        result = result.replace(&var, val);
    }

    result
}

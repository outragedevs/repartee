use std::collections::HashMap;

use chrono::Utc;
use color_eyre::eyre::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use futures::StreamExt;
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
}

impl App {
    pub fn new() -> Result<Self> {
        let config = config::load_config(&constants::config_path())?;
        let theme_path = constants::theme_dir().join(format!("{}.theme", config.general.theme));
        let theme = theme::load_theme(&theme_path)?;

        let state = AppState::new();
        let (irc_tx, irc_rx) = mpsc::unbounded_channel();

        Ok(App {
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
        })
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

        // Create connection entry and server buffer
        self.state.add_connection(Connection {
            id: conn_id.clone(),
            label: server_config.label.clone(),
            status: ConnectionStatus::Connecting,
            nick: server_config
                .nick
                .as_deref()
                .unwrap_or(&self.config.general.nick)
                .to_string(),
            user_modes: String::new(),
            isupport: HashMap::new(),
            error: None,
            lag: None,
        });

        let server_buf_id = make_buffer_id(&conn_id, &server_config.label);
        self.state.add_buffer(Buffer {
            id: server_buf_id.clone(),
            connection_id: conn_id.clone(),
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
                event_params: None,
            },
        );

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

        let mut events = event::EventStream::new();
        let mut tick = interval(Duration::from_secs(1));

        while !self.should_quit {
            terminal.draw(|frame| ui::layout::draw(frame, self))?;

            tokio::select! {
                ev = events.next() => match ev {
                    Some(Ok(ev)) => self.handle_event(ev),
                    Some(Err(_)) => {}
                    None => break,
                },
                irc_ev = self.irc_rx.recv() => {
                    if let Some(event) = irc_ev {
                        self.handle_irc_event(event);
                    }
                },
                _ = tick.tick() => {}
            }
        }

        // Send QUIT to all connected servers
        for handle in self.irc_handles.values() {
            let _ = handle.sender.send_quit("Leaving");
        }

        Ok(())
    }

    fn create_default_status(state: &mut AppState) {
        let buf_id = "status/status".to_string();
        state.add_connection(Connection {
            id: "status".to_string(),
            label: "Status".to_string(),
            status: ConnectionStatus::Disconnected,
            nick: String::new(),
            user_modes: String::new(),
            isupport: HashMap::new(),
            error: None,
            lag: None,
        });
        state.add_buffer(Buffer {
            id: buf_id.clone(),
            connection_id: "status".to_string(),
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
                event_params: None,
            },
        );
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
                crate::irc::events::handle_connected(&mut self.state, &conn_id);
            }
            IrcEvent::Disconnected(conn_id, error) => {
                crate::irc::events::handle_disconnected(
                    &mut self.state,
                    &conn_id,
                    error.as_deref(),
                );
                self.irc_handles.remove(&conn_id);
            }
            IrcEvent::Message(conn_id, msg) => {
                crate::irc::events::handle_irc_message(&mut self.state, &conn_id, &msg);
            }
        }
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Resize(_, _) => {
                // Terminal redraw happens automatically on next loop iteration
            }
            _ => {}
        }
    }

    fn handle_key(&mut self, key: event::KeyEvent) {
        match (key.modifiers, key.code) {
            (KeyModifiers::CONTROL, KeyCode::Char('q')) => self.should_quit = true,
            (KeyModifiers::CONTROL, KeyCode::Char('c')) => self.should_quit = true,
            (KeyModifiers::CONTROL, KeyCode::Char('l')) => {
                // Force redraw (happens automatically on next iteration)
            }
            (KeyModifiers::ALT, KeyCode::Char(c)) if c.is_ascii_digit() => {
                let n = c.to_digit(10).unwrap_or(0) as usize;
                let ids = self.state.sorted_buffer_ids();
                let idx = if n == 0 { 9 } else { n - 1 };
                if idx < ids.len() {
                    self.state.set_active_buffer(&ids[idx]);
                    self.scroll_offset = 0;
                }
            }
            (mods, KeyCode::Left) if mods.contains(KeyModifiers::ALT) => {
                self.state.prev_buffer();
                self.scroll_offset = 0;
            }
            (mods, KeyCode::Right) if mods.contains(KeyModifiers::ALT) => {
                self.state.next_buffer();
                self.scroll_offset = 0;
            }
            (_, KeyCode::Enter) => {
                let text = self.input.submit();
                if !text.is_empty() {
                    self.handle_submit(text);
                }
            }
            (_, KeyCode::Backspace) => self.input.backspace(),
            (_, KeyCode::Delete) => self.input.delete(),
            (_, KeyCode::Home) => self.input.home(),
            (_, KeyCode::End) => {
                self.input.end();
                self.scroll_offset = 0;
            }
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

    fn handle_mouse(&mut self, mouse: event::MouseEvent) {
        let Some(regions) = self.ui_regions else {
            return;
        };
        let pos = Position::new(mouse.column, mouse.row);

        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if regions.chat_area.is_some_and(|r| r.contains(pos)) {
                    self.scroll_offset = self.scroll_offset.saturating_add(3);
                }
            }
            MouseEventKind::ScrollDown => {
                if regions.chat_area.is_some_and(|r| r.contains(pos)) {
                    self.scroll_offset = self.scroll_offset.saturating_sub(3);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(buf_area) = regions.buffer_list_area
                    && buf_area.contains(pos)
                {
                    let y_offset = (mouse.row - buf_area.y) as usize;
                    self.handle_buffer_list_click(y_offset);
                }
            }
            _ => {}
        }
    }

    fn handle_buffer_list_click(&mut self, y_offset: usize) {
        let sorted_ids = self.state.sorted_buffer_ids();
        // The buffer list renders connection headers and buffer items.
        // We need to map y_offset to the correct buffer, accounting for headers.
        let mut row = 0;
        let mut last_conn_id = String::new();
        for id in &sorted_ids {
            let Some(buf) = self.state.buffers.get(id.as_str()) else {
                continue;
            };
            if buf.connection_id != last_conn_id {
                last_conn_id.clone_from(&buf.connection_id);
                if row == y_offset {
                    // Clicked on a connection header, ignore
                    return;
                }
                row += 1;
            }
            if row == y_offset {
                self.state.set_active_buffer(id);
                self.scroll_offset = 0;
                return;
            }
            row += 1;
        }
    }

    fn handle_tab(&mut self) {
        let nicks: Vec<String> = if let Some(buf) = self.state.active_buffer() {
            buf.users.keys().cloned().collect()
        } else {
            Vec::new()
        };
        let commands = crate::commands::registry::get_command_names();
        self.input.tab_complete(&nicks, &commands);
    }

    fn handle_submit(&mut self, text: String) {
        if let Some(parsed) = crate::commands::parser::parse_command(&text) {
            self.execute_command(parsed);
        } else {
            self.handle_plain_message(text);
        }
        self.scroll_offset = 0;
    }

    fn execute_command(&mut self, parsed: crate::commands::parser::ParsedCommand) {
        let commands = crate::commands::registry::get_commands();
        if let Some((_, def)) = commands.into_iter().find(|(name, _)| *name == parsed.name) {
            (def.handler)(self, &parsed.args);
        } else {
            crate::commands::helpers::add_local_event(
                self,
                &format!("Unknown command: /{}", parsed.name),
            );
        }
    }

    fn handle_plain_message(&mut self, text: String) {
        let Some(active_id) = self.state.active_buffer_id.clone() else {
            return;
        };

        let (conn_id, nick, buffer_name) = {
            let buf = match self.state.active_buffer() {
                Some(b) => b,
                None => return,
            };
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
                event_params: None,
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

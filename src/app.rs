use color_eyre::eyre::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};

use crate::config::{self, AppConfig};
use crate::constants;
use crate::state::AppState;
use crate::state::buffer::{
    make_buffer_id, ActivityLevel, Buffer, BufferType, Message, MessageType, NickEntry,
};
use crate::state::connection::{Connection, ConnectionStatus};
use crate::theme::{self, ThemeFile};
use crate::ui;

pub struct App {
    pub state: AppState,
    pub config: AppConfig,
    pub theme: ThemeFile,
    pub should_quit: bool,
}

impl App {
    pub fn new() -> Result<Self> {
        let config = config::load_config(&constants::config_path())?;
        let theme_path = constants::theme_dir().join(format!("{}.theme", config.general.theme));
        let theme = theme::load_theme(&theme_path)?;

        let mut state = AppState {
            connections: Default::default(),
            buffers: Default::default(),
            active_buffer_id: None,
            previous_buffer_id: None,
            message_counter: 0,
        };

        // Add mock data for visual development
        Self::populate_mock_data(&mut state);

        Ok(App {
            state,
            config,
            theme,
            should_quit: false,
        })
    }

    fn populate_mock_data(state: &mut AppState) {
        use chrono::Utc;
        use std::collections::HashMap;

        let conn_id = "ircnet";
        state.add_connection(Connection {
            id: conn_id.to_string(),
            label: "IRCnet".to_string(),
            status: ConnectionStatus::Connected,
            nick: "rustuser".to_string(),
            user_modes: String::new(),
            isupport: HashMap::new(),
            error: None,
            lag: Some(42),
        });

        // Server buffer
        let server_id = make_buffer_id(conn_id, "IRCnet");
        state.add_buffer(Buffer {
            id: server_id.clone(),
            connection_id: conn_id.to_string(),
            buffer_type: BufferType::Server,
            name: "IRCnet".to_string(),
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

        // Channel buffer with users and messages
        let chan_id = make_buffer_id(conn_id, "#rust");
        state.add_buffer(Buffer {
            id: chan_id.clone(),
            connection_id: conn_id.to_string(),
            buffer_type: BufferType::Channel,
            name: "#rust".to_string(),
            messages: Vec::new(),
            activity: ActivityLevel::None,
            unread_count: 0,
            last_read: Utc::now(),
            topic: Some("Welcome to #rust \u{2014} https://rust-lang.org".to_string()),
            topic_set_by: Some("admin".to_string()),
            users: HashMap::new(),
            modes: Some("+nt".to_string()),
            mode_params: None,
            list_modes: HashMap::new(),
        });

        // Add nicks
        state.add_nick(
            &chan_id,
            NickEntry {
                nick: "rustuser".to_string(),
                prefix: String::new(),
                modes: String::new(),
                away: false,
                account: None,
            },
        );
        state.add_nick(
            &chan_id,
            NickEntry {
                nick: "ferris".to_string(),
                prefix: "@".to_string(),
                modes: "o".to_string(),
                away: false,
                account: Some("ferris".to_string()),
            },
        );
        state.add_nick(
            &chan_id,
            NickEntry {
                nick: "helper".to_string(),
                prefix: "+".to_string(),
                modes: "v".to_string(),
                away: false,
                account: None,
            },
        );
        state.add_nick(
            &chan_id,
            NickEntry {
                nick: "lurker".to_string(),
                prefix: String::new(),
                modes: String::new(),
                away: true,
                account: None,
            },
        );

        // Add messages
        let msgs = vec![
            ("ferris", "Welcome to #rust!", MessageType::Message),
            (
                "helper",
                "Don't forget to check the docs",
                MessageType::Message,
            ),
            (
                "rustuser",
                "Thanks! Learning ratatui now",
                MessageType::Message,
            ),
        ];
        for (nick, text, msg_type) in msgs {
            let id = state.next_message_id();
            state.add_message(
                &chan_id,
                Message {
                    id,
                    timestamp: Utc::now(),
                    message_type: msg_type,
                    nick: Some(nick.to_string()),
                    nick_mode: None,
                    text: text.to_string(),
                    highlight: false,
                    event_key: None,
                    event_params: None,
                },
            );
        }

        // Query buffer
        let query_id = make_buffer_id(conn_id, "ferris");
        state.add_buffer(Buffer {
            id: query_id.clone(),
            connection_id: conn_id.to_string(),
            buffer_type: BufferType::Query,
            name: "ferris".to_string(),
            messages: Vec::new(),
            activity: ActivityLevel::Mention,
            unread_count: 1,
            last_read: Utc::now(),
            topic: None,
            topic_set_by: None,
            users: HashMap::new(),
            modes: None,
            mode_params: None,
            list_modes: HashMap::new(),
        });

        let id = state.next_message_id();
        state.add_message(
            &query_id,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Message,
                nick: Some("ferris".to_string()),
                nick_mode: None,
                text: "Hey, nice to see you using Rust!".to_string(),
                highlight: false,
                event_key: None,
                event_params: None,
            },
        );

        state.set_active_buffer(&chan_id);
    }

    pub fn run(&mut self, terminal: &mut ui::Tui) -> Result<()> {
        while !self.should_quit {
            terminal.draw(|frame| ui::layout::draw(frame, self))?;

            // Simple synchronous event loop for now
            if event::poll(std::time::Duration::from_millis(100))?
                && let Event::Key(key) = event::read()?
            {
                self.handle_key(key);
            }
        }
        Ok(())
    }

    fn handle_key(&mut self, key: event::KeyEvent) {
        match (key.modifiers, key.code) {
            (KeyModifiers::CONTROL, KeyCode::Char('q')) => self.should_quit = true,
            (KeyModifiers::CONTROL, KeyCode::Char('c')) => self.should_quit = true,
            // Alt+number for buffer switching
            (KeyModifiers::ALT, KeyCode::Char(c)) if c.is_ascii_digit() => {
                let n = c.to_digit(10).unwrap_or(0) as usize;
                let ids = self.state.sorted_buffer_ids();
                let idx = if n == 0 { 9 } else { n - 1 }; // Alt+1 = buffer 0, Alt+0 = buffer 9
                if idx < ids.len() {
                    self.state.set_active_buffer(&ids[idx]);
                }
            }
            // Arrow keys for buffer navigation
            (mods, KeyCode::Left) if mods.contains(KeyModifiers::ALT) => {
                self.state.prev_buffer();
            }
            (mods, KeyCode::Right) if mods.contains(KeyModifiers::ALT) => {
                self.state.next_buffer();
            }
            _ => {}
        }
    }
}

use chrono::Utc;
use color_eyre::eyre::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Position;
use futures::StreamExt;
use tokio::time::{interval, Duration};

use crate::config::{self, AppConfig};
use crate::constants;
use crate::state::AppState;
use crate::state::buffer::{
    make_buffer_id, ActivityLevel, Buffer, BufferType, Message, MessageType, NickEntry,
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

        Self::populate_mock_data(&mut state);

        Ok(App {
            state,
            config,
            theme,
            input: ui::input::InputState::new(),
            should_quit: false,
            scroll_offset: 0,
            ui_regions: None,
        })
    }

    fn populate_mock_data(state: &mut AppState) {
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

    pub async fn run(&mut self, terminal: &mut ui::Tui) -> Result<()> {
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
                _ = tick.tick() => {}
            }
        }
        Ok(())
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
        let nick = self
            .state
            .active_buffer()
            .and_then(|buf| {
                self.state
                    .connections
                    .get(&buf.connection_id)
                    .map(|c| c.nick.clone())
            })
            .unwrap_or_default();
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
}

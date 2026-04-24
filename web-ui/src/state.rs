use std::collections::{HashMap, HashSet};

use leptos::prelude::*;

use crate::protocol::*;

const MAX_BUFFER_MESSAGES: usize = 2000;

/// Client-side application state, stored as Leptos signals.
#[derive(Clone, Copy)]
pub struct AppState {
    pub authenticated: RwSignal<bool>,
    pub connected: RwSignal<bool>,
    pub buffers: RwSignal<Vec<BufferMeta>>,
    pub connections: RwSignal<Vec<ConnectionMeta>>,
    pub active_buffer: RwSignal<Option<String>>,
    pub messages: RwSignal<HashMap<String, Vec<WireMessage>>>,
    pub nick_lists: RwSignal<HashMap<String, Vec<WireNick>>>,
    pub mention_count: RwSignal<u32>,
    pub session_hint: RwSignal<bool>,
    pub theme: RwSignal<String>,
    pub error: RwSignal<Option<String>>,
    pub timestamp_format: RwSignal<String>,
    pub line_height: RwSignal<f32>,
    pub nick_column_width: RwSignal<u32>,
    pub nick_max_length: RwSignal<u32>,
    pub nick_colors_enabled: RwSignal<bool>,
    pub nick_colors_in_nicklist: RwSignal<bool>,
    pub nick_color_saturation: RwSignal<f32>,
    pub nick_color_lightness: RwSignal<f32>,
    /// Shell screen content for the active shell buffer.
    pub shell_screen: RwSignal<Option<crate::protocol::ShellScreenData>>,
    /// Bumped on every SyncInit (initial connect or lag recovery) to force
    /// the Layout Effect to re-fetch messages for the active buffer.
    pub sync_version: RwSignal<u32>,
    /// Tracks which buffers have had their DB backlog fetched via FetchMessages.
    /// Prevents the Layout Effect from skipping the fetch when only live
    /// NewMessage events are cached (the root cause of empty-buffer-on-switch).
    pub backlog_loaded: RwSignal<HashSet<String>>,
    /// Whether the chat view is scrolled to (or near) the bottom.
    /// When true, new messages auto-scroll. When false, the user is reading
    /// backlog and the view stays put.
    pub is_at_bottom: RwSignal<bool>,
}

impl AppState {
    pub fn new() -> Self {
        // Load theme from localStorage if available.
        let saved_theme: String = web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .and_then(|s: web_sys::Storage| s.get_item("repartee-theme").ok().flatten())
            .unwrap_or_else(|| "nightfall".to_string());

        Self {
            authenticated: RwSignal::new(false),
            connected: RwSignal::new(false),
            buffers: RwSignal::new(Vec::new()),
            connections: RwSignal::new(Vec::new()),
            active_buffer: RwSignal::new(None),
            messages: RwSignal::new(HashMap::new()),
            nick_lists: RwSignal::new(HashMap::new()),
            mention_count: RwSignal::new(0),
            session_hint: RwSignal::new(false),
            theme: RwSignal::new(saved_theme),
            error: RwSignal::new(None),
            timestamp_format: RwSignal::new("%H:%M".to_string()),
            line_height: RwSignal::new(1.35),
            nick_column_width: RwSignal::new(12),
            nick_max_length: RwSignal::new(9),
            nick_colors_enabled: RwSignal::new(true),
            nick_colors_in_nicklist: RwSignal::new(true),
            nick_color_saturation: RwSignal::new(0.65),
            nick_color_lightness: RwSignal::new(0.65),
            shell_screen: RwSignal::new(None),
            sync_version: RwSignal::new(0),
            backlog_loaded: RwSignal::new(HashSet::new()),
            is_at_bottom: RwSignal::new(true),
        }
    }

    /// Handle a WebEvent from the server, updating signals accordingly.
    pub fn handle_event(&self, event: WebEvent) {
        match event {
            WebEvent::SyncInit {
                buffers,
                connections,
                mention_count,
                active_buffer_id,
                timestamp_format,
            } => {
                // Clear cached messages, nick lists, and backlog-loaded flags —
                // forces re-fetch. Handles both initial connect and lag-recovery resync.
                self.messages.set(HashMap::new());
                self.nick_lists.set(HashMap::new());
                self.backlog_loaded.set(HashSet::new());

                self.buffers.set(buffers);
                self.connections.set(connections);
                self.mention_count.set(mention_count);
                self.authenticated.set(true);
                self.connected.set(true);
                if let Some(fmt) = timestamp_format
                    && !fmt.is_empty()
                {
                    self.timestamp_format.set(fmt);
                }

                self.sort_buffers();

                // Sync to the TUI's active buffer.
                // Clear first so the set always triggers the Layout Effect
                // (even if the buffer ID is the same as before the resync).
                self.active_buffer.set(None);
                if let Some(ref id) = active_buffer_id {
                    self.active_buffer.set(Some(id.clone()));
                } else {
                    // Fallback: select first channel buffer.
                    let bufs = self.buffers.get_untracked();
                    if let Some(first) = bufs.iter().find(|b| b.buffer_type == "channel") {
                        self.active_buffer.set(Some(first.id.clone()));
                    }
                }

                // Bump sync_version to force Layout Effect re-fetch.
                self.sync_version.update(|v| *v += 1);
            }
            WebEvent::NewMessage { buffer_id, message } => {
                self.messages.update(|msgs| {
                    let entry = msgs.entry(buffer_id.clone()).or_default();
                    entry.push(message);
                    cap_messages(entry);
                });
                // Update unread count if not the active buffer.
                let is_active = self.active_buffer.get_untracked().as_deref() == Some(&buffer_id);
                if !is_active {
                    self.buffers.update(|bufs| {
                        if let Some(b) = bufs.iter_mut().find(|b| b.id == buffer_id) {
                            b.unread_count += 1;
                        }
                    });
                }
            }
            WebEvent::TopicChanged {
                buffer_id, topic, ..
            } => {
                self.buffers.update(|bufs| {
                    if let Some(b) = bufs.iter_mut().find(|b| b.id == buffer_id) {
                        b.topic = topic;
                    }
                });
            }
            WebEvent::ActivityChanged {
                buffer_id,
                activity,
                unread_count,
            } => {
                self.buffers.update(|bufs| {
                    if let Some(b) = bufs.iter_mut().find(|b| b.id == buffer_id) {
                        b.activity = activity;
                        b.unread_count = unread_count;
                    }
                });
            }
            WebEvent::BufferCreated { buffer } => {
                let new_id = buffer.id.clone();
                self.buffers.update(|bufs| bufs.push(buffer));
                self.sort_buffers();
                // Auto-switch to newly created buffer (matches terminal behavior).
                self.active_buffer.set(Some(new_id));
            }
            WebEvent::BufferClosed { buffer_id } => {
                // If the closed buffer was active, switch to first available.
                if self.active_buffer.get_untracked().as_deref() == Some(&buffer_id) {
                    let bufs = self.buffers.get_untracked();
                    let fallback = bufs
                        .iter()
                        .find(|b| b.id != buffer_id)
                        .map(|b| b.id.clone());
                    self.active_buffer.set(fallback);
                }
                self.buffers
                    .update(|bufs| bufs.retain(|b| b.id != buffer_id));
                self.messages.update(|msgs| {
                    msgs.remove(&buffer_id);
                });
                self.nick_lists.update(|lists| {
                    lists.remove(&buffer_id);
                });
                self.backlog_loaded.update(|set| {
                    set.remove(&buffer_id);
                });
            }
            WebEvent::ConnectionStatus {
                conn_id,
                connected,
                nick,
                label,
            } => {
                self.connections.update(|conns| {
                    if let Some(c) = conns.iter_mut().find(|c| c.id == conn_id) {
                        c.connected = connected;
                        c.nick = nick;
                    } else {
                        conns.push(ConnectionMeta {
                            id: conn_id,
                            label,
                            nick,
                            connected,
                            user_modes: String::new(),
                            lag: None,
                        });
                    }
                });
            }
            WebEvent::Messages {
                buffer_id,
                messages,
                ..
            } => {
                self.messages.update(|msgs| {
                    let entry = msgs.entry(buffer_id.clone()).or_default();
                    // Insert date separators between messages from different days.
                    let with_separators = insert_date_separators(messages);
                    // Prepend older messages (they come from scroll-back).
                    let mut combined = with_separators;
                    combined.append(entry);
                    cap_messages(&mut combined);
                    *entry = combined;
                });
                // Mark this buffer's DB backlog as loaded so the Layout Effect
                // won't re-fetch on subsequent switches to this buffer.
                self.backlog_loaded.update(|set| {
                    set.insert(buffer_id);
                });
            }
            WebEvent::NickList {
                buffer_id, nicks, ..
            } => {
                self.nick_lists.update(|lists| {
                    let mut sorted = nicks;
                    sort_nicks(&mut sorted);
                    lists.insert(buffer_id, sorted);
                });
            }
            WebEvent::MentionAlert { .. } => {
                self.mention_count.update(|c| *c += 1);
            }
            WebEvent::MentionsList { .. } => {
                self.mention_count.set(0);
            }
            WebEvent::NickEvent {
                buffer_id,
                kind,
                nick,
                new_nick,
                prefix,
                modes,
                away,
                ..
            } => {
                match kind {
                    NickEventKind::Join => {
                        self.nick_lists.update(|lists| {
                            let list = lists.entry(buffer_id.clone()).or_default();
                            if !list.iter().any(|n| n.nick == nick) {
                                list.push(WireNick {
                                    nick: nick.clone(),
                                    prefix: prefix.unwrap_or_default(),
                                    modes: modes.unwrap_or_default(),
                                    away: away.unwrap_or(false),
                                });
                                sort_nicks(list);
                            }
                        });
                        // Update nick_count.
                        self.buffers.update(|bufs| {
                            if let Some(b) = bufs.iter_mut().find(|b| b.id == buffer_id) {
                                b.nick_count += 1;
                            }
                        });
                    }
                    NickEventKind::Part | NickEventKind::Quit => {
                        self.nick_lists.update(|lists| {
                            if let Some(list) = lists.get_mut(&buffer_id) {
                                list.retain(|n| n.nick != nick);
                            }
                        });
                        // Update nick_count.
                        self.buffers.update(|bufs| {
                            if let Some(b) = bufs.iter_mut().find(|b| b.id == buffer_id) {
                                b.nick_count = b.nick_count.saturating_sub(1);
                            }
                        });
                    }
                    NickEventKind::NickChange => {
                        if let Some(ref new) = new_nick {
                            self.nick_lists.update(|lists| {
                                if let Some(list) = lists.get_mut(&buffer_id)
                                    && let Some(entry) = list.iter_mut().find(|n| n.nick == nick)
                                {
                                    entry.nick = new.clone();
                                    sort_nicks(list);
                                }
                            });
                        }
                    }
                    NickEventKind::ModeChange => {
                        self.nick_lists.update(|lists| {
                            if let Some(list) = lists.get_mut(&buffer_id) {
                                if let Some(entry) = list.iter_mut().find(|n| n.nick == nick) {
                                    if let Some(ref p) = prefix {
                                        entry.prefix = p.clone();
                                    }
                                    if let Some(ref m) = modes {
                                        entry.modes = m.clone();
                                    }
                                }
                                sort_nicks(list);
                            }
                        });
                    }
                    NickEventKind::AwayChange => {
                        self.nick_lists.update(|lists| {
                            if let Some(list) = lists.get_mut(&buffer_id)
                                && let Some(entry) = list.iter_mut().find(|n| n.nick == nick)
                                && let Some(a) = away
                            {
                                entry.away = a;
                            }
                        });
                    }
                }
            }
            WebEvent::ActiveBufferChanged { buffer_id } => {
                // Skip if the client already switched to this buffer locally
                // (the click handler sets active_buffer before the server echoes).
                // Without this guard:
                // - The echo re-fires the Layout Effect, causing duplicate FetchMessages
                // - Rapid clicks can be overridden by a stale echo for a previous buffer
                if self.active_buffer.get_untracked().as_deref() != Some(&buffer_id) {
                    self.active_buffer.set(Some(buffer_id));
                }
            }
            WebEvent::SettingsChanged {
                timestamp_format,
                line_height,
                theme,
                nick_column_width,
                nick_max_length,
                nick_colors,
                nick_colors_in_nicklist,
                nick_color_saturation,
                nick_color_lightness,
            } => {
                self.timestamp_format.set(timestamp_format);
                self.line_height.set(line_height);
                self.theme.set(theme);
                if nick_column_width > 0 {
                    self.nick_column_width.set(nick_column_width);
                }
                if nick_max_length > 0 {
                    self.nick_max_length.set(nick_max_length);
                }
                self.nick_colors_enabled.set(nick_colors);
                self.nick_colors_in_nicklist.set(nick_colors_in_nicklist);
                self.nick_color_saturation.set(nick_color_saturation);
                self.nick_color_lightness.set(nick_color_lightness);
            }
            WebEvent::Error { message } => {
                self.error.set(Some(message));
            }
            WebEvent::ShellScreen {
                buffer_id,
                cols,
                rows,
                cursor_row,
                cursor_col,
                cursor_visible,
                ..
            } => {
                // Only update if this shell buffer is currently active.
                if self.active_buffer.get_untracked().as_deref() == Some(buffer_id.as_str()) {
                    self.shell_screen
                        .set(Some(crate::protocol::ShellScreenData {
                            cols,
                            rows,
                            cursor_row,
                            cursor_col,
                            cursor_visible,
                        }));
                }
            }
        }
    }

    /// Sort buffers to match TUI order: mentions first → connection label → buffer type → name.
    ///
    /// Buffer type sort order: mentions(0) < server(1) < channel(2) < query(3) < dcc_chat(4) < special(5).
    fn sort_buffers(&self) {
        let connections = self.connections.get_untracked();
        self.buffers.update(|bufs| {
            bufs.sort_by(|a, b| {
                // Mentions always sorts first, regardless of connection label.
                let a_mentions = a.buffer_type == "mentions";
                let b_mentions = b.buffer_type == "mentions";
                b_mentions
                    .cmp(&a_mentions)
                    .then_with(|| {
                        let label_a = connections
                            .iter()
                            .find(|c| c.id == a.connection_id)
                            .map_or_else(
                                || a.connection_id.to_lowercase(),
                                |c| c.label.to_lowercase(),
                            );
                        let label_b = connections
                            .iter()
                            .find(|c| c.id == b.connection_id)
                            .map_or_else(
                                || b.connection_id.to_lowercase(),
                                |c| c.label.to_lowercase(),
                            );
                        label_a.cmp(&label_b)
                    })
                    .then_with(|| {
                        buf_type_order(&a.buffer_type).cmp(&buf_type_order(&b.buffer_type))
                    })
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
        });
    }
}

fn cap_messages(messages: &mut Vec<WireMessage>) {
    if messages.len() <= MAX_BUFFER_MESSAGES {
        return;
    }
    let drop_count = messages.len() - MAX_BUFFER_MESSAGES;
    messages.drain(..drop_count);
    messages.shrink_to(MAX_BUFFER_MESSAGES);
}

fn buf_type_order(t: &str) -> u8 {
    match t {
        "mentions" => 0,
        "server" => 1,
        "channel" => 2,
        "query" => 3,
        "dcc_chat" => 4,
        "special" => 5,
        "shell" => 6,
        _ => 7,
    }
}

/// Sort nicks by prefix rank (~&@%+ order), then alphabetically.
fn sort_nicks(nicks: &mut [WireNick]) {
    nicks.sort_by(|a, b| {
        prefix_rank(&a.prefix)
            .cmp(&prefix_rank(&b.prefix))
            .then_with(|| a.nick.to_lowercase().cmp(&b.nick.to_lowercase()))
    });
}

fn prefix_rank(prefix: &str) -> u8 {
    const ORDER: &str = "~&@%+";
    prefix
        .chars()
        .next()
        .and_then(|c| ORDER.find(c))
        .map_or(ORDER.len() as u8, |i| i as u8)
}

/// Insert date separator lines between messages from different days.
///
/// Mirrors the TUI's `load_backlog` behavior — adds `─── Day, DD Mon YYYY ───`
/// event lines at each date boundary in the message list.
fn insert_date_separators(messages: Vec<WireMessage>) -> Vec<WireMessage> {
    if messages.is_empty() {
        return messages;
    }

    let mut result = Vec::with_capacity(messages.len() + 5);
    let mut last_date: Option<chrono::NaiveDate> = None;

    for msg in messages {
        let local_date = chrono::DateTime::from_timestamp(msg.timestamp, 0).map(|dt| {
            use chrono::TimeZone;
            chrono::Local
                .from_utc_datetime(&dt.naive_utc())
                .date_naive()
        });

        if let Some(date) = local_date {
            if last_date.is_some_and(|d| d != date) || last_date.is_none() {
                let formatted = date.format("%a, %d %b %Y");
                result.push(WireMessage {
                    id: 0,
                    timestamp: msg.timestamp,
                    msg_type: "event".to_string(),
                    nick: None,
                    nick_mode: None,
                    text: format!("\u{2500}\u{2500}\u{2500} {formatted} \u{2500}\u{2500}\u{2500}"),
                    highlight: false,
                    event_key: Some("date_separator".to_string()),
                });
            }
            last_date = Some(date);
        }

        result.push(msg);
    }

    result
}

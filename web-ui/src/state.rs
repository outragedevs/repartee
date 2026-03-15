use std::collections::HashMap;

use leptos::prelude::*;

use crate::protocol::*;

/// Client-side application state, stored as Leptos signals.
#[derive(Clone)]
pub struct AppState {
    pub connected: RwSignal<bool>,
    pub buffers: RwSignal<Vec<BufferMeta>>,
    pub connections: RwSignal<Vec<ConnectionMeta>>,
    pub active_buffer: RwSignal<Option<String>>,
    pub messages: RwSignal<HashMap<String, Vec<WireMessage>>>,
    pub nick_lists: RwSignal<HashMap<String, Vec<WireNick>>>,
    pub mention_count: RwSignal<u32>,
    pub token: RwSignal<Option<String>>,
    pub theme: RwSignal<String>,
    pub error: RwSignal<Option<String>>,
}

impl AppState {
    pub fn new() -> Self {
        // Load theme from localStorage if available.
        let saved_theme: String = web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .and_then(|s: web_sys::Storage| s.get_item("repartee-theme").ok().flatten())
            .unwrap_or_else(|| "nightfall".to_string());

        Self {
            connected: RwSignal::new(false),
            buffers: RwSignal::new(Vec::new()),
            connections: RwSignal::new(Vec::new()),
            active_buffer: RwSignal::new(None),
            messages: RwSignal::new(HashMap::new()),
            nick_lists: RwSignal::new(HashMap::new()),
            mention_count: RwSignal::new(0),
            token: RwSignal::new(None),
            theme: RwSignal::new(saved_theme),
            error: RwSignal::new(None),
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
            } => {
                self.buffers.set(buffers);
                self.connections.set(connections);
                self.mention_count.set(mention_count);
                self.connected.set(true);

                // Sync to the TUI's active buffer.
                if let Some(ref id) = active_buffer_id {
                    self.active_buffer.set(Some(id.clone()));
                } else if self.active_buffer.get_untracked().is_none() {
                    // Fallback: select first channel buffer.
                    let bufs = self.buffers.get_untracked();
                    if let Some(first) = bufs.iter().find(|b| b.buffer_type == "channel") {
                        self.active_buffer.set(Some(first.id.clone()));
                    }
                }
            }
            WebEvent::NewMessage {
                buffer_id,
                message,
            } => {
                self.messages.update(|msgs| {
                    msgs.entry(buffer_id.clone())
                        .or_default()
                        .push(message);
                });
                // Update unread count if not the active buffer.
                let is_active = self
                    .active_buffer
                    .get_untracked()
                    .as_deref() == Some(&buffer_id);
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
                // Auto-switch to newly created buffer (matches terminal behavior).
                self.active_buffer.set(Some(new_id));
            }
            WebEvent::BufferClosed { buffer_id } => {
                self.buffers.update(|bufs| bufs.retain(|b| b.id != buffer_id));
                self.messages.update(|msgs| { msgs.remove(&buffer_id); });
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
                    let entry = msgs.entry(buffer_id).or_default();
                    // Prepend older messages (they come from scroll-back).
                    let mut combined = messages;
                    combined.extend(entry.drain(..));
                    *entry = combined;
                });
            }
            WebEvent::NickList { buffer_id, nicks, .. } => {
                self.nick_lists.update(|lists| {
                    lists.insert(buffer_id, nicks);
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
                            // Only add if not already present.
                            if !list.iter().any(|n| n.nick == nick) {
                                list.push(WireNick {
                                    nick: nick.clone(),
                                    prefix: prefix.unwrap_or_default(),
                                    modes: modes.unwrap_or_default(),
                                    away: away.unwrap_or(false),
                                });
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
                                if let Some(list) = lists.get_mut(&buffer_id) {
                                    if let Some(entry) = list.iter_mut().find(|n| n.nick == nick) {
                                        entry.nick = new.clone();
                                    }
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
                            }
                        });
                    }
                    NickEventKind::AwayChange => {
                        self.nick_lists.update(|lists| {
                            if let Some(list) = lists.get_mut(&buffer_id) {
                                if let Some(entry) = list.iter_mut().find(|n| n.nick == nick) {
                                    if let Some(a) = away {
                                        entry.away = a;
                                    }
                                }
                            }
                        });
                    }
                }
            }
            WebEvent::ActiveBufferChanged { buffer_id } => {
                self.active_buffer.set(Some(buffer_id));
            }
            WebEvent::Error { message } => {
                self.error.set(Some(message));
            }
        }
    }
}

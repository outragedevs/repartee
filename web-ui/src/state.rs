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
            } => {
                self.buffers.set(buffers);
                self.connections.set(connections);
                self.mention_count.set(mention_count);
                self.connected.set(true);

                // Auto-select first channel buffer if none selected.
                if self.active_buffer.get_untracked().is_none() {
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
                self.buffers.update(|bufs| bufs.push(buffer));
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
            WebEvent::NickEvent { .. } => {
                // Nick events affect nick lists — request refresh.
                // The server broadcasts NickList separately.
            }
            WebEvent::Error { message } => {
                self.error.set(Some(message));
            }
        }
    }
}

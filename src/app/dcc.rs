use std::collections::HashMap;

use chrono::Utc;

use crate::state::buffer::{
    ActivityLevel, Buffer, BufferType, Message, MessageType, make_buffer_id,
};

use super::App;

impl App {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn handle_dcc_event(&mut self, ev: crate::dcc::DccEvent) {
        use crate::dcc::DccEvent;
        match ev {
            DccEvent::IncomingRequest {
                nick,
                conn_id,
                addr,
                port,
                passive_token,
                ident,
                host,
            } => {
                // Notify scripts before any default handling; suppression skips
                // the accept-prompt and auto-accept logic entirely.
                {
                    use crate::scripting::api::events;
                    let mut params = HashMap::new();
                    params.insert("connection_id".to_string(), conn_id.clone());
                    params.insert("nick".to_string(), nick.clone());
                    params.insert("ip".to_string(), addr.to_string());
                    params.insert("port".to_string(), port.to_string());
                    if self.emit_script_event(events::DCC_CHAT_REQUEST, params) {
                        return;
                    }
                }

                // Cross-request auto-allow: if we already have a Listening DCC
                // to this nick (we initiated), tear down our listener and
                // auto-accept their request instead.
                let mut auto = false;
                let our_listening_id = self
                    .dcc
                    .records
                    .iter()
                    .find(|(_, r)| {
                        r.nick.eq_ignore_ascii_case(&nick)
                            && matches!(r.state, crate::dcc::types::DccState::Listening)
                    })
                    .map(|(id, _)| id.clone());
                if let Some(lid) = our_listening_id {
                    self.dcc.close_by_id(&lid);
                    self.dcc.chat_senders.remove(&lid);
                    auto = true;
                }

                // Check hostmask auto-accept
                if !auto {
                    auto = self.dcc.should_auto_accept(&nick, &ident, &host, port);
                }

                if self.dcc.records.len() >= self.dcc.max_connections {
                    crate::commands::helpers::add_local_event(
                        self,
                        &format!("DCC CHAT from {nick} rejected: max connections reached"),
                    );
                    return;
                }

                let id = self.dcc.generate_id(&nick);
                let nick_for_record = nick.clone();
                let record = crate::dcc::types::DccRecord {
                    id: id.clone(),
                    dcc_type: crate::dcc::types::DccType::Chat,
                    nick: nick_for_record,
                    conn_id,
                    addr,
                    port,
                    state: crate::dcc::types::DccState::WaitingUser,
                    passive_token,
                    created: std::time::Instant::now(),
                    started: None,
                    bytes_transferred: 0,
                    mirc_ctcp: true,
                    ident,
                    host,
                };
                self.dcc.records.insert(id, record);

                if auto {
                    crate::commands::handlers_dcc::cmd_dcc(self, &["chat".to_string(), nick]);
                } else {
                    crate::commands::helpers::add_local_event(
                        self,
                        &format!(
                            "DCC CHAT request from {nick} ({addr}:{port}) — \
                             use /dcc chat {nick} to accept"
                        ),
                    );
                }
            }

            DccEvent::ChatConnected { id } => {
                let nick = {
                    let Some(record) = self.dcc.records.get_mut(&id) else {
                        return;
                    };
                    record.state = crate::dcc::types::DccState::Connected;
                    record.started = Some(std::time::Instant::now());
                    record.nick.clone()
                };

                let conn_id = self
                    .dcc
                    .records
                    .get(&id)
                    .map(|r| r.conn_id.clone())
                    .unwrap_or_default();

                let buf_name = format!("={nick}");
                let buffer_id = make_buffer_id(&conn_id, &buf_name);

                if !self.state.buffers.contains_key(&buffer_id) {
                    self.state.add_buffer(Buffer {
                        id: buffer_id.clone(),
                        connection_id: conn_id.clone(),
                        buffer_type: BufferType::DccChat,
                        name: buf_name,
                        messages: std::collections::VecDeque::new(),
                        activity: ActivityLevel::None,
                        unread_count: 0,
                        last_read: chrono::Utc::now(),
                        topic: None,
                        topic_set_by: None,
                        users: std::collections::HashMap::new(),
                        modes: None,
                        mode_params: None,
                        list_modes: std::collections::HashMap::new(),
                        last_speakers: Vec::new(),
                    });
                }

                self.load_backlog(&buffer_id);
                self.state.set_active_buffer(&buffer_id);

                let msg_id = self.state.next_message_id();
                self.state.add_message(
                    &buffer_id,
                    Message {
                        id: msg_id,
                        timestamp: Utc::now(),
                        message_type: MessageType::Event,
                        nick: None,
                        nick_mode: None,
                        text: format!("DCC CHAT connection established with {nick}"),
                        highlight: false,
                        event_key: None,
                        event_params: None,
                        log_msg_id: None,
                        log_ref_id: None,
                        tags: None,
                    },
                );

                {
                    use crate::scripting::api::events;
                    let mut params = HashMap::new();
                    params.insert("connection_id".to_string(), conn_id);
                    params.insert("nick".to_string(), nick);
                    self.emit_script_event(events::DCC_CHAT_CONNECTED, params);
                }
            }

            DccEvent::ChatMessage { id, text } => {
                let (nick, conn_id) = {
                    let Some(record) = self.dcc.records.get_mut(&id) else {
                        return;
                    };
                    record.bytes_transferred += text.len() as u64;
                    (record.nick.clone(), record.conn_id.clone())
                };

                let buf_name = format!("={nick}");
                let buffer_id = make_buffer_id(&conn_id, &buf_name);

                let suppressed = {
                    use crate::scripting::api::events;
                    let mut params = HashMap::new();
                    params.insert("connection_id".to_string(), conn_id);
                    params.insert("nick".to_string(), nick.clone());
                    params.insert("text".to_string(), text.clone());
                    self.emit_script_event(events::DCC_CHAT_MESSAGE, params)
                };

                if !suppressed {
                    let msg_id = self.state.next_message_id();
                    self.state.add_message_with_activity(
                        &buffer_id,
                        Message {
                            id: msg_id,
                            timestamp: Utc::now(),
                            message_type: MessageType::Message,
                            nick: Some(nick),
                            nick_mode: None,
                            text,
                            highlight: false,
                            event_key: None,
                            event_params: None,
                            log_msg_id: None,
                            log_ref_id: None,
                            tags: None,
                        },
                        ActivityLevel::Mention,
                    );
                }
            }

            DccEvent::ChatAction { id, text } => {
                let (nick, conn_id) = {
                    let Some(record) = self.dcc.records.get_mut(&id) else {
                        return;
                    };
                    record.bytes_transferred += text.len() as u64;
                    (record.nick.clone(), record.conn_id.clone())
                };

                let buf_name = format!("={nick}");
                let buffer_id = make_buffer_id(&conn_id, &buf_name);

                let msg_id = self.state.next_message_id();
                self.state.add_message_with_activity(
                    &buffer_id,
                    Message {
                        id: msg_id,
                        timestamp: Utc::now(),
                        message_type: MessageType::Action,
                        nick: Some(nick),
                        nick_mode: None,
                        text,
                        highlight: false,
                        event_key: None,
                        event_params: None,
                        log_msg_id: None,
                        log_ref_id: None,
                        tags: None,
                    },
                    ActivityLevel::Mention,
                );
            }

            DccEvent::ChatClosed { id, reason } => {
                let record = self.dcc.close_by_id(&id);
                self.dcc.chat_senders.remove(&id);
                if let Some(record) = record {
                    let buf_name = format!("={}", record.nick);
                    let buffer_id = make_buffer_id(&record.conn_id, &buf_name);
                    let reason_str = reason
                        .as_deref()
                        .map_or(String::new(), |r| format!(" ({r})"));

                    {
                        use crate::scripting::api::events;
                        let mut params = HashMap::new();
                        params.insert("connection_id".to_string(), record.conn_id.clone());
                        params.insert("nick".to_string(), record.nick.clone());
                        params.insert(
                            "reason".to_string(),
                            reason.as_deref().unwrap_or("").to_string(),
                        );
                        self.emit_script_event(events::DCC_CHAT_CLOSED, params);
                    }

                    let msg_id = self.state.next_message_id();
                    self.state.add_message(
                        &buffer_id,
                        Message {
                            id: msg_id,
                            timestamp: Utc::now(),
                            message_type: MessageType::Event,
                            nick: None,
                            nick_mode: None,
                            text: format!("DCC CHAT with {} closed{reason_str}", record.nick),
                            highlight: false,
                            event_key: None,
                            event_params: None,
                            log_msg_id: None,
                            log_ref_id: None,
                            tags: None,
                        },
                    );
                }
            }

            DccEvent::ChatError { id, error } => {
                self.dcc.close_by_id(&id);
                self.dcc.chat_senders.remove(&id);
                crate::commands::helpers::add_local_event(self, &format!("DCC error: {error}"));
            }
        }
    }
}

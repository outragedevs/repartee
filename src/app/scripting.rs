use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::time::Duration;

use super::App;

impl App {
    /// Build a `ScriptAPI` whose callbacks send `ScriptAction` messages
    /// through the provided channel. The App event loop drains these.
    #[allow(
        clippy::too_many_lines,
        clippy::type_complexity,
        clippy::needless_pass_by_value
    )]
    pub(crate) fn build_script_api(
        tx: mpsc::Sender<crate::scripting::ScriptAction>,
        snapshot: Arc<std::sync::RwLock<crate::scripting::engine::ScriptStateSnapshot>>,
        timer_id_counter: Arc<std::sync::atomic::AtomicU64>,
    ) -> crate::scripting::engine::ScriptAPI {
        use crate::scripting::ScriptAction;

        let t = tx.clone();
        let say: Arc<dyn Fn((String, String, Option<String>)) + Send + Sync> =
            Arc::new(move |(target, text, conn_id)| {
                let _ = t.try_send(ScriptAction::Say {
                    target,
                    text,
                    conn_id,
                });
            });

        let t = tx.clone();
        let action: Arc<dyn Fn((String, String, Option<String>)) + Send + Sync> =
            Arc::new(move |(target, text, conn_id)| {
                let _ = t.try_send(ScriptAction::Action {
                    target,
                    text,
                    conn_id,
                });
            });

        let t = tx.clone();
        let notice: Arc<dyn Fn((String, String, Option<String>)) + Send + Sync> =
            Arc::new(move |(target, text, conn_id)| {
                let _ = t.try_send(ScriptAction::Notice {
                    target,
                    text,
                    conn_id,
                });
            });

        let t = tx.clone();
        let raw: Arc<dyn Fn((String, Option<String>)) + Send + Sync> =
            Arc::new(move |(line, conn_id)| {
                let _ = t.try_send(ScriptAction::Raw { line, conn_id });
            });

        let t = tx.clone();
        let join: Arc<dyn Fn((String, Option<String>, Option<String>)) + Send + Sync> =
            Arc::new(move |(channel, key, conn_id)| {
                let _ = t.try_send(ScriptAction::Join {
                    channel,
                    key,
                    conn_id,
                });
            });

        let t = tx.clone();
        let part: Arc<dyn Fn((String, Option<String>, Option<String>)) + Send + Sync> =
            Arc::new(move |(channel, msg, conn_id)| {
                let _ = t.try_send(ScriptAction::Part {
                    channel,
                    msg,
                    conn_id,
                });
            });

        let t = tx.clone();
        let change_nick: Arc<dyn Fn((String, Option<String>)) + Send + Sync> =
            Arc::new(move |(nick, conn_id)| {
                let _ = t.try_send(ScriptAction::ChangeNick { nick, conn_id });
            });

        let t = tx.clone();
        let whois: Arc<dyn Fn((String, Option<String>)) + Send + Sync> =
            Arc::new(move |(nick, conn_id)| {
                let _ = t.try_send(ScriptAction::Whois { nick, conn_id });
            });

        let t = tx.clone();
        let mode: Arc<dyn Fn((String, String, Option<String>)) + Send + Sync> =
            Arc::new(move |(channel, mode_string, conn_id)| {
                let _ = t.try_send(ScriptAction::Mode {
                    channel,
                    mode_string,
                    conn_id,
                });
            });

        let t = tx.clone();
        let kick: Arc<dyn Fn((String, String, Option<String>, Option<String>)) + Send + Sync> =
            Arc::new(move |(channel, nick, reason, conn_id)| {
                let _ = t.try_send(ScriptAction::Kick {
                    channel,
                    nick,
                    reason,
                    conn_id,
                });
            });

        let t = tx.clone();
        let ctcp: Arc<dyn Fn((String, String, Option<String>, Option<String>)) + Send + Sync> =
            Arc::new(move |(target, ctcp_type, message, conn_id)| {
                let _ = t.try_send(ScriptAction::Ctcp {
                    target,
                    ctcp_type,
                    message,
                    conn_id,
                });
            });

        let t = tx.clone();
        let add_local_event: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |text| {
            let _ = t.try_send(ScriptAction::LocalEvent { text });
        });

        let t = tx.clone();
        let add_buffer_event: Arc<dyn Fn((String, String)) + Send + Sync> =
            Arc::new(move |(buffer_id, text)| {
                let _ = t.try_send(ScriptAction::BufferEvent { buffer_id, text });
            });

        let t = tx.clone();
        let switch_buffer: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |buffer_id| {
            let _ = t.try_send(ScriptAction::SwitchBuffer { buffer_id });
        });

        let t = tx.clone();
        let execute_command: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |line| {
            let _ = t.try_send(ScriptAction::ExecuteCommand { line });
        });

        let t = tx.clone();
        let register_command: Arc<dyn Fn((String, String, String)) + Send + Sync> =
            Arc::new(move |(name, description, usage)| {
                let _ = t.try_send(ScriptAction::RegisterCommand {
                    name,
                    description,
                    usage,
                });
            });

        let t = tx.clone();
        let unregister_command: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |name| {
            let _ = t.try_send(ScriptAction::UnregisterCommand { name });
        });

        let t = tx.clone();
        let log: Arc<dyn Fn((String, String)) + Send + Sync> =
            Arc::new(move |(script, message)| {
                let _ = t.try_send(ScriptAction::Log { script, message });
            });

        // Read-only state queries: read from the shared snapshot.
        let snap = Arc::clone(&snapshot);
        let active_buffer_id: Arc<dyn Fn(()) -> Option<String> + Send + Sync> =
            Arc::new(move |()| snap.read().ok().and_then(|s| s.active_buffer_id.clone()));

        let snap = Arc::clone(&snapshot);
        let our_nick: Arc<dyn Fn(Option<String>) -> Option<String> + Send + Sync> =
            Arc::new(move |conn_id| {
                let s = snap.read().ok()?;
                if let Some(id) = conn_id {
                    s.connections
                        .iter()
                        .find(|c| c.id == id)
                        .map(|c| c.nick.clone())
                } else {
                    let active_buf_id = s.active_buffer_id.as_ref()?;
                    let buf = s.buffers.iter().find(|b| b.id == *active_buf_id)?;
                    s.connections
                        .iter()
                        .find(|c| c.id == buf.connection_id)
                        .map(|c| c.nick.clone())
                }
            });

        let snap = Arc::clone(&snapshot);
        let connection_info: Arc<
            dyn Fn(String) -> Option<crate::scripting::engine::ConnectionInfo> + Send + Sync,
        > = Arc::new(move |id| {
            let s = snap.read().ok()?;
            s.connections.iter().find(|c| c.id == id).cloned()
        });

        let snap = Arc::clone(&snapshot);
        let connections: Arc<
            dyn Fn(()) -> Vec<crate::scripting::engine::ConnectionInfo> + Send + Sync,
        > = Arc::new(move |()| {
            snap.read()
                .map_or_else(|_| Vec::new(), |s| s.connections.clone())
        });

        let snap = Arc::clone(&snapshot);
        let buffer_info: Arc<
            dyn Fn(String) -> Option<crate::scripting::engine::BufferInfo> + Send + Sync,
        > = Arc::new(move |id| {
            let s = snap.read().ok()?;
            s.buffers.iter().find(|b| b.id == id).cloned()
        });

        let snap = Arc::clone(&snapshot);
        let buffers: Arc<dyn Fn(()) -> Vec<crate::scripting::engine::BufferInfo> + Send + Sync> =
            Arc::new(move |()| {
                snap.read()
                    .map_or_else(|_| Vec::new(), |s| s.buffers.clone())
            });

        let snap = Arc::clone(&snapshot);
        let buffer_nicks: Arc<
            dyn Fn(String) -> Vec<crate::scripting::engine::NickInfo> + Send + Sync,
        > = Arc::new(move |buffer_id| {
            snap.read().map_or_else(
                |_| Vec::new(),
                |s| s.buffer_nicks.get(&buffer_id).cloned().unwrap_or_default(),
            )
        });

        // Timers: allocate ID and send ScriptAction to spawn the tokio task.
        let t = tx.clone();
        let counter = Arc::clone(&timer_id_counter);
        let start_timer: Arc<dyn Fn(u64) -> u64 + Send + Sync> = Arc::new(move |interval_ms| {
            let id = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let _ = t.try_send(ScriptAction::StartTimer { id, interval_ms });
            id
        });

        let t = tx.clone();
        let counter = Arc::clone(&timer_id_counter);
        let start_timeout: Arc<dyn Fn(u64) -> u64 + Send + Sync> = Arc::new(move |delay_ms| {
            let id = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let _ = t.try_send(ScriptAction::StartTimeout { id, delay_ms });
            id
        });

        let t = tx.clone();
        let cancel_timer: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(move |id| {
            let _ = t.try_send(ScriptAction::CancelTimer { id });
        });

        // Config: per-script get/set reads from snapshot, set sends ScriptAction.
        let snap = Arc::clone(&snapshot);
        let config_get: Arc<dyn Fn((String, String)) -> Option<String> + Send + Sync> =
            Arc::new(move |(script, key)| {
                snap.read().ok()?.script_config.get(&(script, key)).cloned()
            });
        let config_set: Arc<dyn Fn((String, String, String)) + Send + Sync> =
            Arc::new(move |(script, key, value)| {
                let _ = tx.try_send(ScriptAction::SetScriptConfig { script, key, value });
            });
        let snap = Arc::clone(&snapshot);
        let app_config_get: Arc<dyn Fn(String) -> Option<String> + Send + Sync> =
            Arc::new(move |key_path| {
                let s = snap.read().ok()?;
                let toml_val = s.app_config_toml.as_ref()?;
                let mut current = toml_val;
                for segment in key_path.split('.') {
                    current = current.get(segment)?;
                }
                let result = match current {
                    toml::Value::String(v) => Some(v.clone()),
                    other => Some(other.to_string()),
                };
                drop(s);
                result
            });

        crate::scripting::engine::ScriptAPI {
            say,
            action,
            notice,
            raw,
            join,
            part,
            change_nick,
            whois,
            mode,
            kick,
            ctcp,
            add_local_event,
            add_buffer_event,
            switch_buffer,
            execute_command,
            active_buffer_id,
            our_nick,
            connection_info,
            connections,
            buffer_info,
            buffers,
            buffer_nicks,
            register_command,
            unregister_command,
            start_timer,
            start_timeout,
            cancel_timer,
            config_get,
            config_set,
            app_config_get,
            log,
        }
    }

    /// Push the current `AppState` into the shared script snapshot.
    pub(crate) fn update_script_snapshot(&mut self) {
        if !self.script_snapshot_dirty {
            return;
        }
        self.script_snapshot_dirty = false;
        let config_toml = self.cached_config_toml.get_or_insert_with(|| {
            toml::Value::try_from(&self.config)
                .unwrap_or_else(|_| toml::Value::Table(toml::map::Map::new()))
        });
        if let Ok(mut snap) = self.script_state.write() {
            *snap = self.state.script_snapshot();
            snap.script_config.clone_from(&self.script_config);
            snap.app_config_toml = Some(config_toml.clone());
        }
    }

    /// Resolve the connection ID for a script action.
    fn resolve_conn_id(&self, conn_id: Option<&str>) -> Option<String> {
        conn_id.map_or_else(
            || self.active_conn_id().map(str::to_owned),
            |id| Some(id.to_string()),
        )
    }

    /// Get an IRC sender for a resolved connection ID.
    fn irc_sender_for(&self, conn_id: &str) -> Option<&::irc::client::Sender> {
        self.irc_handles.get(conn_id).map(|h| &h.sender)
    }

    /// Process a single `ScriptAction` from the scripting channel.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn handle_script_action(&mut self, action: crate::scripting::ScriptAction) {
        use crate::scripting::ScriptAction;
        match action {
            ScriptAction::Say {
                target,
                text,
                conn_id,
            } => {
                if let Some(cid) = self.resolve_conn_id(conn_id.as_deref())
                    && let Some(sender) = self.irc_sender_for(&cid)
                {
                    for chunk in crate::irc::split_irc_message(&text, crate::irc::MESSAGE_MAX_BYTES)
                    {
                        let _ = sender.send_privmsg(&target, &chunk);
                    }
                }
            }
            ScriptAction::Action {
                target,
                text,
                conn_id,
            } => {
                if let Some(cid) = self.resolve_conn_id(conn_id.as_deref())
                    && let Some(sender) = self.irc_sender_for(&cid)
                {
                    let _ = sender.send(::irc::proto::Command::Raw(
                        "PRIVMSG".to_string(),
                        vec![target, format!("\x01ACTION {text}\x01")],
                    ));
                }
            }
            ScriptAction::Notice {
                target,
                text,
                conn_id,
            } => {
                if let Some(cid) = self.resolve_conn_id(conn_id.as_deref())
                    && let Some(sender) = self.irc_sender_for(&cid)
                {
                    let _ = sender.send_notice(&target, &text);
                }
            }
            ScriptAction::Raw { line, conn_id } => {
                if let Some(cid) = self.resolve_conn_id(conn_id.as_deref())
                    && let Some(sender) = self.irc_sender_for(&cid)
                {
                    let _ = sender.send(::irc::proto::Command::Raw(line, vec![]));
                }
            }
            ScriptAction::Join {
                channel,
                key,
                conn_id,
            } => {
                if let Some(cid) = self.resolve_conn_id(conn_id.as_deref())
                    && let Some(sender) = self.irc_sender_for(&cid)
                {
                    let _ = sender.send(::irc::proto::Command::JOIN(channel, key, None));
                }
            }
            ScriptAction::Part {
                channel,
                msg,
                conn_id,
            } => {
                if let Some(cid) = self.resolve_conn_id(conn_id.as_deref())
                    && let Some(sender) = self.irc_sender_for(&cid)
                {
                    let _ = sender.send(::irc::proto::Command::PART(channel, msg));
                }
            }
            ScriptAction::ChangeNick { nick, conn_id } => {
                if let Some(cid) = self.resolve_conn_id(conn_id.as_deref())
                    && let Some(sender) = self.irc_sender_for(&cid)
                {
                    let _ = sender.send(::irc::proto::Command::NICK(nick));
                }
            }
            ScriptAction::Whois { nick, conn_id } => {
                if let Some(cid) = self.resolve_conn_id(conn_id.as_deref())
                    && let Some(sender) = self.irc_sender_for(&cid)
                {
                    let _ = sender.send(::irc::proto::Command::WHOIS(None, nick));
                }
            }
            ScriptAction::Mode {
                channel,
                mode_string,
                conn_id,
            } => {
                if let Some(cid) = self.resolve_conn_id(conn_id.as_deref())
                    && let Some(sender) = self.irc_sender_for(&cid)
                {
                    let _ = sender.send(::irc::proto::Command::Raw(
                        "MODE".to_string(),
                        vec![channel, mode_string],
                    ));
                }
            }
            ScriptAction::Kick {
                channel,
                nick,
                reason,
                conn_id,
            } => {
                if let Some(cid) = self.resolve_conn_id(conn_id.as_deref())
                    && let Some(sender) = self.irc_sender_for(&cid)
                {
                    let _ = sender.send(::irc::proto::Command::KICK(channel, nick, reason));
                }
            }
            ScriptAction::Ctcp {
                target,
                ctcp_type,
                message,
                conn_id,
            } => {
                if let Some(cid) = self.resolve_conn_id(conn_id.as_deref())
                    && let Some(sender) = self.irc_sender_for(&cid)
                {
                    let ctcp_text = message.map_or_else(
                        || format!("\x01{ctcp_type}\x01"),
                        |msg| format!("\x01{ctcp_type} {msg}\x01"),
                    );
                    let _ = sender.send_privmsg(&target, &ctcp_text);
                }
            }
            ScriptAction::LocalEvent { text } => {
                crate::commands::helpers::add_local_event(self, &text);
            }
            ScriptAction::BufferEvent { buffer_id, text } => {
                self.add_event_to_buffer(&buffer_id, text);
            }
            ScriptAction::SwitchBuffer { buffer_id } => {
                if self.state.buffers.contains_key(&buffer_id) {
                    self.state.set_active_buffer(&buffer_id);
                    self.scroll_offset = 0;
                }
            }
            ScriptAction::ExecuteCommand { line } => {
                if let Some(parsed) = crate::commands::parser::parse_command(&line) {
                    self.execute_command(&parsed);
                }
            }
            ScriptAction::RegisterCommand {
                name,
                description,
                usage,
            } => {
                self.script_commands.insert(name, (description, usage));
            }
            ScriptAction::UnregisterCommand { name } => {
                self.script_commands.remove(&name);
            }
            ScriptAction::Log { script, message } => {
                tracing::info!(script = %script, "[script] {message}");
            }
            ScriptAction::StartTimer { id, interval_ms } => {
                let tx = self.script_action_tx.clone();
                let handle = tokio::spawn(async move {
                    let mut interval = tokio::time::interval(Duration::from_millis(interval_ms));
                    interval.tick().await; // skip first immediate tick
                    loop {
                        interval.tick().await;
                        if tx
                            .send(crate::scripting::ScriptAction::TimerFired { id })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                });
                self.active_timers.insert(id, handle);
            }
            ScriptAction::StartTimeout { id, delay_ms } => {
                let tx = self.script_action_tx.clone();
                let handle = tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    let _ = tx.send(crate::scripting::ScriptAction::TimerFired { id }).await;
                });
                self.active_timers.insert(id, handle);
            }
            ScriptAction::CancelTimer { id } => {
                if let Some(handle) = self.active_timers.remove(&id) {
                    handle.abort();
                }
            }
            ScriptAction::TimerFired { id } => {
                if let Some(manager) = self.script_manager.as_ref() {
                    manager.fire_timer(id);
                }
                self.active_timers
                    .retain(|_, handle| !handle.is_finished());
            }
            ScriptAction::SetScriptConfig { script, key, value } => {
                self.script_config.insert((script, key), value);
            }
        }
    }

    /// Autoload all scripts from the scripts directory.
    pub fn autoload_scripts(&mut self) {
        let Some(manager) = self.script_manager.as_mut() else {
            return;
        };
        let available = manager.available_scripts();
        if available.is_empty() {
            return;
        }
        let Some(api) = self.script_api.as_ref() else {
            return;
        };
        let mut loaded = 0u32;
        let mut errors = Vec::new();
        for (name, _path, is_loaded) in &available {
            if *is_loaded {
                continue;
            }
            match manager.load(name, api) {
                Ok(meta) => {
                    tracing::info!("autoloaded script: {}", meta.name);
                    loaded += 1;
                }
                Err(e) => {
                    tracing::warn!("failed to autoload script {name}: {e}");
                    errors.push(format!("{name}: {e}"));
                }
            }
        }
        if loaded > 0 || !errors.is_empty() {
            tracing::info!("autoloaded {loaded} script(s), {} error(s)", errors.len());
        }
    }

    /// Emit an IRC event to scripts before default handling.
    pub(crate) fn emit_script_event(
        &self,
        event_name: &str,
        params: std::collections::HashMap<String, String>,
    ) -> bool {
        let Some(manager) = self.script_manager.as_ref() else {
            return false;
        };
        let event = crate::scripting::event_bus::Event {
            name: event_name.to_string(),
            params,
        };
        manager.emit(&event)
    }

    /// Extract event params from an IRC message and emit to scripts.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn emit_irc_to_scripts(&self, conn_id: &str, msg: &::irc::proto::Message) -> bool {
        use crate::scripting::api::events;

        let extract_nick = |prefix: Option<&::irc::proto::Prefix>| -> String {
            match prefix {
                Some(::irc::proto::Prefix::Nickname(nick, _, _)) => nick.clone(),
                Some(::irc::proto::Prefix::ServerName(name)) => name.clone(),
                None => String::new(),
            }
        };
        let extract_ident = |prefix: Option<&::irc::proto::Prefix>| -> String {
            match prefix {
                Some(::irc::proto::Prefix::Nickname(_, user, _)) => user.clone(),
                _ => String::new(),
            }
        };
        let extract_host = |prefix: Option<&::irc::proto::Prefix>| -> String {
            match prefix {
                Some(::irc::proto::Prefix::Nickname(_, _, host)) => host.clone(),
                _ => String::new(),
            }
        };

        let mut params = HashMap::new();
        params.insert("connection_id".to_string(), conn_id.to_string());

        let event_name = match &msg.command {
            ::irc::proto::Command::PRIVMSG(target, text) => {
                params.insert("nick".to_string(), extract_nick(msg.prefix.as_ref()));
                params.insert("ident".to_string(), extract_ident(msg.prefix.as_ref()));
                params.insert("hostname".to_string(), extract_host(msg.prefix.as_ref()));
                params.insert("target".to_string(), target.clone());
                params.insert("channel".to_string(), target.clone());
                params.insert(
                    "is_channel".to_string(),
                    target.starts_with('#').to_string(),
                );
                if let Some(ctcp_body) = text
                    .strip_prefix('\x01')
                    .and_then(|t| t.strip_suffix('\x01'))
                {
                    if let Some(action_text) = ctcp_body.strip_prefix("ACTION ") {
                        params.insert("message".to_string(), action_text.to_string());
                        events::ACTION
                    } else {
                        let (ctcp_type, ctcp_msg) =
                            ctcp_body.split_once(' ').unwrap_or((ctcp_body, ""));
                        params.insert("ctcp_type".to_string(), ctcp_type.to_string());
                        params.insert("message".to_string(), ctcp_msg.to_string());
                        events::CTCP_REQUEST
                    }
                } else {
                    params.insert("message".to_string(), text.clone());
                    events::PRIVMSG
                }
            }
            ::irc::proto::Command::NOTICE(target, text) => {
                params.insert("nick".to_string(), extract_nick(msg.prefix.as_ref()));
                params.insert("target".to_string(), target.clone());
                let from_server =
                    matches!(msg.prefix, Some(::irc::proto::Prefix::ServerName(_)) | None);
                params.insert("from_server".to_string(), from_server.to_string());
                if let Some(ctcp_body) = text
                    .strip_prefix('\x01')
                    .and_then(|t| t.strip_suffix('\x01'))
                {
                    let (ctcp_type, ctcp_msg) =
                        ctcp_body.split_once(' ').unwrap_or((ctcp_body, ""));
                    params.insert("ctcp_type".to_string(), ctcp_type.to_string());
                    params.insert("message".to_string(), ctcp_msg.to_string());
                    events::CTCP_RESPONSE
                } else {
                    params.insert("message".to_string(), text.clone());
                    events::NOTICE
                }
            }
            ::irc::proto::Command::JOIN(channel, _, _) => {
                params.insert("nick".to_string(), extract_nick(msg.prefix.as_ref()));
                params.insert("ident".to_string(), extract_ident(msg.prefix.as_ref()));
                params.insert("hostname".to_string(), extract_host(msg.prefix.as_ref()));
                params.insert("channel".to_string(), channel.clone());
                events::JOIN
            }
            ::irc::proto::Command::PART(channel, reason) => {
                params.insert("nick".to_string(), extract_nick(msg.prefix.as_ref()));
                params.insert("ident".to_string(), extract_ident(msg.prefix.as_ref()));
                params.insert("hostname".to_string(), extract_host(msg.prefix.as_ref()));
                params.insert("channel".to_string(), channel.clone());
                params.insert("message".to_string(), reason.clone().unwrap_or_default());
                events::PART
            }
            ::irc::proto::Command::QUIT(reason) => {
                params.insert("nick".to_string(), extract_nick(msg.prefix.as_ref()));
                params.insert("ident".to_string(), extract_ident(msg.prefix.as_ref()));
                params.insert("hostname".to_string(), extract_host(msg.prefix.as_ref()));
                params.insert("message".to_string(), reason.clone().unwrap_or_default());
                events::QUIT
            }
            ::irc::proto::Command::NICK(new_nick) => {
                params.insert("nick".to_string(), extract_nick(msg.prefix.as_ref()));
                params.insert("new_nick".to_string(), new_nick.clone());
                params.insert("ident".to_string(), extract_ident(msg.prefix.as_ref()));
                params.insert("hostname".to_string(), extract_host(msg.prefix.as_ref()));
                events::NICK
            }
            ::irc::proto::Command::KICK(channel, kicked, reason) => {
                params.insert("nick".to_string(), extract_nick(msg.prefix.as_ref()));
                params.insert("ident".to_string(), extract_ident(msg.prefix.as_ref()));
                params.insert("hostname".to_string(), extract_host(msg.prefix.as_ref()));
                params.insert("channel".to_string(), channel.clone());
                params.insert("kicked".to_string(), kicked.clone());
                params.insert("message".to_string(), reason.clone().unwrap_or_default());
                events::KICK
            }
            ::irc::proto::Command::TOPIC(channel, topic) => {
                params.insert("nick".to_string(), extract_nick(msg.prefix.as_ref()));
                params.insert("channel".to_string(), channel.clone());
                params.insert("topic".to_string(), topic.clone().unwrap_or_default());
                events::TOPIC
            }
            ::irc::proto::Command::INVITE(nick, channel) => {
                params.insert("nick".to_string(), extract_nick(msg.prefix.as_ref()));
                params.insert("channel".to_string(), channel.clone());
                params.insert("invited".to_string(), nick.clone());
                events::INVITE
            }
            ::irc::proto::Command::ChannelMODE(target, modes) => {
                params.insert("nick".to_string(), extract_nick(msg.prefix.as_ref()));
                params.insert("target".to_string(), target.clone());
                let mode_str: Vec<String> =
                    modes.iter().map(std::string::ToString::to_string).collect();
                params.insert("modes".to_string(), mode_str.join(" "));
                events::MODE
            }
            ::irc::proto::Command::UserMODE(target, modes) => {
                params.insert("nick".to_string(), extract_nick(msg.prefix.as_ref()));
                params.insert("target".to_string(), target.clone());
                let mode_str: Vec<String> =
                    modes.iter().map(std::string::ToString::to_string).collect();
                params.insert("modes".to_string(), mode_str.join(" "));
                events::MODE
            }
            ::irc::proto::Command::WALLOPS(text) => {
                params.insert("nick".to_string(), extract_nick(msg.prefix.as_ref()));
                params.insert("message".to_string(), text.clone());
                let from_server =
                    matches!(msg.prefix, Some(::irc::proto::Prefix::ServerName(_)) | None);
                params.insert("from_server".to_string(), from_server.to_string());
                events::WALLOPS
            }
            // For non-scriptable events, don't emit
            _ => return false,
        };

        self.emit_script_event(event_name, params)
    }
}

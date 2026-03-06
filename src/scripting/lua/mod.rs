use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use color_eyre::eyre::{Result, eyre};
use mlua::prelude::*;

use super::engine::{ScriptAPI, ScriptEngine, ScriptMeta};
use super::event_bus::{Event, EventResult, Priority};

// ─── Internal Types ─────────────────────────────────────────

struct LuaHandler {
    event_name: String,
    registry_key: String,
    priority: i32,
    once: bool,
    script_name: String,
    id: u64,
}

struct LuaCommand {
    script_name: String,
    registry_key: String,
}

struct LoadedScript {
    meta: ScriptMeta,
    cleanup_key: Option<String>,
    env_key: String,
}

/// Shared mutable state accessible from Lua closures.
struct HandlerState {
    handlers: Vec<LuaHandler>,
    commands: HashMap<String, LuaCommand>,
    next_id: u64,
}

impl HandlerState {
    fn new() -> Self {
        Self {
            handlers: Vec::new(),
            commands: HashMap::new(),
            next_id: 0,
        }
    }

    const fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

// ─── LuaEngine ──────────────────────────────────────────────

pub struct LuaEngine {
    lua: Lua,
    scripts: HashMap<String, LoadedScript>,
    state: Arc<Mutex<HandlerState>>,
}

impl LuaEngine {
    pub fn new() -> Result<Self> {
        let lua = Lua::new();
        Self::sandbox(&lua)?;
        Ok(Self {
            lua,
            scripts: HashMap::new(),
            state: Arc::new(Mutex::new(HandlerState::new())),
        })
    }

    /// Remove dangerous globals from the Lua VM.
    fn sandbox(lua: &Lua) -> Result<()> {
        let globals = lua.globals();
        for name in &["os", "io", "loadfile", "dofile", "package"] {
            globals.raw_set(*name, LuaNil)?;
        }
        Ok(())
    }

    /// Create an isolated environment table for a script.
    /// Reads fall through to the real globals via __index.
    fn create_script_env(lua: &Lua) -> LuaResult<LuaTable> {
        let env = lua.create_table()?;
        let mt = lua.create_table()?;
        mt.set("__index", lua.globals())?;
        env.set_metatable(Some(mt))?;
        Ok(env)
    }

    /// Read the `meta` table from a script's environment.
    fn read_meta(env: &LuaTable, path: &Path) -> Result<ScriptMeta> {
        let meta_table: LuaTable = env
            .get("meta")
            .map_err(|_| eyre!("script missing `meta` table"))?;
        let name: String = meta_table
            .get("name")
            .map_err(|_| eyre!("meta.name is required"))?;
        let version: Option<String> = meta_table.get("version").ok();
        let description: Option<String> = meta_table.get("description").ok();

        Ok(ScriptMeta {
            name,
            version,
            description,
            path: path.to_path_buf(),
        })
    }

    /// Build the `api` table passed to `setup(api)`.
    fn build_api_table(
        lua: &Lua,
        api: &ScriptAPI,
        script_name: &str,
        state: &Arc<Mutex<HandlerState>>,
    ) -> Result<LuaTable> {
        let api_table = lua.create_table()?;

        // ── Priority constants ──
        api_table.set("PRIORITY_HIGHEST", Priority::Highest as i32)?;
        api_table.set("PRIORITY_HIGH", Priority::High as i32)?;
        api_table.set("PRIORITY_NORMAL", Priority::Normal as i32)?;
        api_table.set("PRIORITY_LOW", Priority::Low as i32)?;
        api_table.set("PRIORITY_LOWEST", Priority::Lowest as i32)?;

        Self::register_event_api(lua, &api_table, script_name, state)?;
        Self::register_irc_api(lua, &api_table, api)?;
        Self::register_ui_api(lua, &api_table, api)?;
        Self::register_store_api(lua, &api_table, api)?;
        Self::register_command_api(lua, &api_table, api, script_name, state)?;
        Self::register_timer_api(lua, &api_table, api)?;
        Self::register_config_api(lua, &api_table, api, script_name)?;

        // ── api.log(message) ──
        let log_fn = Arc::clone(&api.log);
        let sn = script_name.to_string();
        api_table.set(
            "log",
            lua.create_function(move |_, message: String| {
                log_fn((sn.clone(), message));
                Ok(())
            })?,
        )?;

        Ok(api_table)
    }

    // ── Event registration: api.on / api.once / api.off ─────

    fn register_event_api(
        lua: &Lua,
        api_table: &LuaTable,
        script_name: &str,
        state: &Arc<Mutex<HandlerState>>,
    ) -> Result<()> {
        // api.on(event, handler, priority?)
        let st = Arc::clone(state);
        let sn = script_name.to_string();
        api_table.set(
            "on",
            lua.create_function(
                move |lua, (event_name, callback, priority): (String, LuaFunction, Option<i32>)| {
                    let mut inner = st.lock().map_err(|e| mlua::Error::RuntimeError(format!("handler state poisoned: {e}")))?;
                    let id = inner.next_id();
                    let key = format!("__handler_{id}");
                    lua.set_named_registry_value(&key, callback)?;
                    inner.handlers.push(LuaHandler {
                        event_name,
                        registry_key: key,
                        priority: priority.unwrap_or(Priority::Normal as i32),
                        once: false,
                        script_name: sn.clone(),
                        id,
                    });
                    inner.handlers.sort_by(|a, b| b.priority.cmp(&a.priority));
                    drop(inner);
                    Ok(id)
                },
            )?,
        )?;

        // api.once(event, handler, priority?)
        let st = Arc::clone(state);
        let sn = script_name.to_string();
        api_table.set(
            "once",
            lua.create_function(
                move |lua, (event_name, callback, priority): (String, LuaFunction, Option<i32>)| {
                    let mut inner = st.lock().map_err(|e| mlua::Error::RuntimeError(format!("handler state poisoned: {e}")))?;
                    let id = inner.next_id();
                    let key = format!("__handler_{id}");
                    lua.set_named_registry_value(&key, callback)?;
                    inner.handlers.push(LuaHandler {
                        event_name,
                        registry_key: key,
                        priority: priority.unwrap_or(Priority::Normal as i32),
                        once: true,
                        script_name: sn.clone(),
                        id,
                    });
                    inner.handlers.sort_by(|a, b| b.priority.cmp(&a.priority));
                    drop(inner);
                    Ok(id)
                },
            )?,
        )?;

        // api.off(handler_id)
        let st = Arc::clone(state);
        api_table.set(
            "off",
            lua.create_function(move |lua, id: u64| {
                let mut inner = st.lock().map_err(|e| mlua::Error::RuntimeError(format!("handler state poisoned: {e}")))?;
                let removed = inner.handlers.iter().position(|h| h.id == id)
                    .map(|pos| inner.handlers.remove(pos));
                drop(inner);
                if let Some(handler) = removed {
                    lua.unset_named_registry_value(&handler.registry_key)?;
                }
                Ok(())
            })?,
        )?;

        Ok(())
    }

    // ── IRC API: api.irc.* ──────────────────────────────────

    #[allow(clippy::too_many_lines)]
    fn register_irc_api(lua: &Lua, api_table: &LuaTable, api: &ScriptAPI) -> Result<()> {
        let irc = lua.create_table()?;

        let cb = Arc::clone(&api.say);
        irc.set(
            "say",
            lua.create_function(
                move |_, (target, text, conn): (String, String, Option<String>)| {
                    cb((target, text, conn));
                    Ok(())
                },
            )?,
        )?;

        let cb = Arc::clone(&api.action);
        irc.set(
            "action",
            lua.create_function(
                move |_, (target, text, conn): (String, String, Option<String>)| {
                    cb((target, text, conn));
                    Ok(())
                },
            )?,
        )?;

        let cb = Arc::clone(&api.notice);
        irc.set(
            "notice",
            lua.create_function(
                move |_, (target, text, conn): (String, String, Option<String>)| {
                    cb((target, text, conn));
                    Ok(())
                },
            )?,
        )?;

        let cb = Arc::clone(&api.raw);
        irc.set(
            "raw",
            lua.create_function(move |_, (line, conn): (String, Option<String>)| {
                cb((line, conn));
                Ok(())
            })?,
        )?;

        let cb = Arc::clone(&api.join);
        irc.set(
            "join",
            lua.create_function(
                move |_, (channel, key, conn): (String, Option<String>, Option<String>)| {
                    cb((channel, key, conn));
                    Ok(())
                },
            )?,
        )?;

        let cb = Arc::clone(&api.part);
        irc.set(
            "part",
            lua.create_function(
                move |_, (channel, msg, conn): (String, Option<String>, Option<String>)| {
                    cb((channel, msg, conn));
                    Ok(())
                },
            )?,
        )?;

        let cb = Arc::clone(&api.change_nick);
        irc.set(
            "nick",
            lua.create_function(move |_, (new_nick, conn): (String, Option<String>)| {
                cb((new_nick, conn));
                Ok(())
            })?,
        )?;

        let cb = Arc::clone(&api.whois);
        irc.set(
            "whois",
            lua.create_function(move |_, (nick, conn): (String, Option<String>)| {
                cb((nick, conn));
                Ok(())
            })?,
        )?;

        let cb = Arc::clone(&api.mode);
        irc.set(
            "mode",
            lua.create_function(
                move |_, (channel, mode_str, conn): (String, String, Option<String>)| {
                    cb((channel, mode_str, conn));
                    Ok(())
                },
            )?,
        )?;

        let cb = Arc::clone(&api.kick);
        irc.set(
            "kick",
            lua.create_function(
                move |_,
                      (channel, nick, reason, conn): (
                    String,
                    String,
                    Option<String>,
                    Option<String>,
                )| {
                    cb((channel, nick, reason, conn));
                    Ok(())
                },
            )?,
        )?;

        let cb = Arc::clone(&api.ctcp);
        irc.set(
            "ctcp",
            lua.create_function(
                move |_,
                      (target, ctcp_type, msg, conn): (
                    String,
                    String,
                    Option<String>,
                    Option<String>,
                )| {
                    cb((target, ctcp_type, msg, conn));
                    Ok(())
                },
            )?,
        )?;

        api_table.set("irc", irc)?;
        Ok(())
    }

    // ── UI API: api.ui.* ────────────────────────────────────

    fn register_ui_api(lua: &Lua, api_table: &LuaTable, api: &ScriptAPI) -> Result<()> {
        let ui = lua.create_table()?;

        let cb = Arc::clone(&api.add_local_event);
        ui.set(
            "print",
            lua.create_function(move |_, text: String| {
                cb(text);
                Ok(())
            })?,
        )?;

        let cb = Arc::clone(&api.add_buffer_event);
        ui.set(
            "print_to",
            lua.create_function(move |_, (buf_id, text): (String, String)| {
                cb((buf_id, text));
                Ok(())
            })?,
        )?;

        let cb = Arc::clone(&api.switch_buffer);
        ui.set(
            "switch_buffer",
            lua.create_function(move |_, buf_id: String| {
                cb(buf_id);
                Ok(())
            })?,
        )?;

        let cb = Arc::clone(&api.execute_command);
        ui.set(
            "execute",
            lua.create_function(move |_, cmd: String| {
                cb(cmd);
                Ok(())
            })?,
        )?;

        api_table.set("ui", ui)?;
        Ok(())
    }

    // ── Store API: api.store.* ──────────────────────────────

    fn register_store_api(lua: &Lua, api_table: &LuaTable, api: &ScriptAPI) -> Result<()> {
        let store = lua.create_table()?;

        let cb = Arc::clone(&api.active_buffer_id);
        store.set(
            "active_buffer",
            lua.create_function(move |_, ()| Ok(cb(())))?,
        )?;

        let cb = Arc::clone(&api.our_nick);
        store.set(
            "our_nick",
            lua.create_function(move |_, conn_id: Option<String>| Ok(cb(conn_id)))?,
        )?;

        let cb = Arc::clone(&api.connection_info);
        store.set(
            "connection",
            lua.create_function(move |lua, id: String| match cb(id) {
                Some(info) => {
                    let t = lua.create_table()?;
                    t.set("id", info.id)?;
                    t.set("label", info.label)?;
                    t.set("nick", info.nick)?;
                    t.set("connected", info.connected)?;
                    t.set("user_modes", info.user_modes)?;
                    Ok(LuaValue::Table(t))
                }
                None => Ok(LuaNil),
            })?,
        )?;

        let cb = Arc::clone(&api.connections);
        store.set(
            "connections",
            lua.create_function(move |lua, ()| {
                let conns = cb(());
                let t = lua.create_table()?;
                for (i, info) in conns.into_iter().enumerate() {
                    let entry = lua.create_table()?;
                    entry.set("id", info.id)?;
                    entry.set("label", info.label)?;
                    entry.set("nick", info.nick)?;
                    entry.set("connected", info.connected)?;
                    entry.set("user_modes", info.user_modes)?;
                    t.set(i + 1, entry)?;
                }
                Ok(t)
            })?,
        )?;

        let cb = Arc::clone(&api.buffer_info);
        store.set(
            "buffer",
            lua.create_function(move |lua, id: String| match cb(id) {
                Some(info) => {
                    let t = lua.create_table()?;
                    t.set("id", info.id)?;
                    t.set("connection_id", info.connection_id)?;
                    t.set("name", info.name)?;
                    t.set("buffer_type", info.buffer_type)?;
                    t.set("topic", info.topic)?;
                    t.set("unread_count", info.unread_count)?;
                    Ok(LuaValue::Table(t))
                }
                None => Ok(LuaNil),
            })?,
        )?;

        let cb = Arc::clone(&api.buffers);
        store.set(
            "buffers",
            lua.create_function(move |lua, ()| {
                let bufs = cb(());
                let t = lua.create_table()?;
                for (i, info) in bufs.into_iter().enumerate() {
                    let entry = lua.create_table()?;
                    entry.set("id", info.id)?;
                    entry.set("connection_id", info.connection_id)?;
                    entry.set("name", info.name)?;
                    entry.set("buffer_type", info.buffer_type)?;
                    entry.set("topic", info.topic)?;
                    entry.set("unread_count", info.unread_count)?;
                    t.set(i + 1, entry)?;
                }
                Ok(t)
            })?,
        )?;

        let cb = Arc::clone(&api.buffer_nicks);
        store.set(
            "nicks",
            lua.create_function(move |lua, buf_id: String| {
                let nicks = cb(buf_id);
                let t = lua.create_table()?;
                for (i, info) in nicks.into_iter().enumerate() {
                    let entry = lua.create_table()?;
                    entry.set("nick", info.nick)?;
                    entry.set("prefix", info.prefix)?;
                    entry.set("modes", info.modes)?;
                    entry.set("away", info.away)?;
                    t.set(i + 1, entry)?;
                }
                Ok(t)
            })?,
        )?;

        api_table.set("store", store)?;
        Ok(())
    }

    // ── Command API: api.command / api.remove_command ────────

    fn register_command_api(
        lua: &Lua,
        api_table: &LuaTable,
        api: &ScriptAPI,
        script_name: &str,
        state: &Arc<Mutex<HandlerState>>,
    ) -> Result<()> {
        // api.command(name, { handler=fn, description=str, usage=str })
        let st = Arc::clone(state);
        let sn = script_name.to_string();
        let reg_cmd = Arc::clone(&api.register_command);
        api_table.set(
            "command",
            lua.create_function(
                move |lua, (name, opts): (String, LuaTable)| {
                    let handler: LuaFunction = opts.get("handler")?;
                    let description: String =
                        opts.get::<Option<String>>("description")?.unwrap_or_default();
                    let usage: String =
                        opts.get::<Option<String>>("usage")?.unwrap_or_default();

                    let mut inner = st.lock().map_err(|e| mlua::Error::RuntimeError(format!("handler state poisoned: {e}")))?;
                    let id = inner.next_id();
                    let key = format!("__cmd_{id}");
                    lua.set_named_registry_value(&key, handler)?;
                    inner.commands.insert(
                        name.clone(),
                        LuaCommand {
                            script_name: sn.clone(),
                            registry_key: key,
                        },
                    );
                    drop(inner);
                    reg_cmd((name, description, usage));
                    Ok(())
                },
            )?,
        )?;

        // api.remove_command(name)
        let st = Arc::clone(state);
        let unreg_cmd = Arc::clone(&api.unregister_command);
        api_table.set(
            "remove_command",
            lua.create_function(move |lua, name: String| {
                let mut inner = st.lock().map_err(|e| mlua::Error::RuntimeError(format!("handler state poisoned: {e}")))?;
                let removed = inner.commands.remove(&name);
                drop(inner);
                if let Some(cmd) = removed {
                    lua.unset_named_registry_value(&cmd.registry_key)?;
                }
                unreg_cmd(name);
                Ok(())
            })?,
        )?;

        Ok(())
    }

    // ── Timer API: api.timer / api.timeout / api.cancel_timer

    fn register_timer_api(lua: &Lua, api_table: &LuaTable, _api: &ScriptAPI) -> Result<()> {
        api_table.set(
            "timer",
            lua.create_function(|_, (_ms, _callback): (u64, LuaFunction)| -> LuaResult<u64> {
                Err(mlua::Error::RuntimeError("timer() is not yet implemented".to_string()))
            })?,
        )?;

        api_table.set(
            "timeout",
            lua.create_function(|_, (_ms, _callback): (u64, LuaFunction)| -> LuaResult<u64> {
                Err(mlua::Error::RuntimeError("timeout() is not yet implemented".to_string()))
            })?,
        )?;

        api_table.set(
            "cancel_timer",
            lua.create_function(|_, _timer_id: u64| -> LuaResult<()> {
                Err(mlua::Error::RuntimeError("cancel_timer() is not yet implemented".to_string()))
            })?,
        )?;

        Ok(())
    }

    // ── Config API: api.config.* ────────────────────────────

    fn register_config_api(
        lua: &Lua,
        api_table: &LuaTable,
        api: &ScriptAPI,
        script_name: &str,
    ) -> Result<()> {
        let config = lua.create_table()?;

        let cb = Arc::clone(&api.config_get);
        let sn = script_name.to_string();
        config.set(
            "get",
            lua.create_function(move |_, (key, default): (String, Option<String>)| {
                Ok(cb((sn.clone(), key)).or(default))
            })?,
        )?;

        let cb = Arc::clone(&api.config_set);
        let sn = script_name.to_string();
        config.set(
            "set",
            lua.create_function(move |_, (key, value): (String, String)| {
                cb((sn.clone(), key, value));
                Ok(())
            })?,
        )?;

        let cb = Arc::clone(&api.app_config_get);
        config.set(
            "app",
            lua.create_function(move |_, key: String| Ok(cb(key)))?,
        )?;

        api_table.set("config", config)?;
        Ok(())
    }

    /// Convert Event params to a Lua table.
    fn event_to_table(lua: &Lua, event: &Event) -> LuaResult<LuaTable> {
        let t = lua.create_table()?;
        t.set("name", event.name.as_str())?;
        for (k, v) in &event.params {
            t.set(k.as_str(), v.as_str())?;
        }
        Ok(t)
    }
}

// ─── ScriptEngine impl ─────────────────────────────────────

impl ScriptEngine for LuaEngine {
    fn extension(&self) -> &'static str {
        "lua"
    }

    fn load_script(&mut self, path: &Path, api: &ScriptAPI) -> Result<ScriptMeta> {
        let source = std::fs::read_to_string(path)
            .map_err(|e| eyre!("failed to read {}: {e}", path.display()))?;

        // Create isolated environment for this script
        let env = Self::create_script_env(&self.lua)
            .map_err(|e| eyre!("failed to create script env: {e}"))?;

        // Execute the script source within its environment
        self.lua
            .load(&source)
            .set_name(path.to_string_lossy())
            .set_environment(env.clone())
            .exec()
            .map_err(|e| eyre!("Lua error in {}: {e}", path.display()))?;

        // Read meta from the script's environment
        let meta = Self::read_meta(&env, path)?;

        if self.scripts.contains_key(&meta.name) {
            return Err(eyre!("script '{}' is already loaded", meta.name));
        }

        // Build API table
        let api_table = Self::build_api_table(&self.lua, api, &meta.name, &self.state)?;

        // Call setup(api)
        let setup_fn: LuaFunction = env
            .get("setup")
            .map_err(|_| eyre!("script missing `setup` function"))?;

        let cleanup: LuaValue = setup_fn
            .call(api_table)
            .map_err(|e| eyre!("setup() failed: {e}"))?;

        // Store cleanup function if returned
        let cleanup_key = if let LuaValue::Function(f) = cleanup {
            let key = format!("__cleanup_{}", meta.name);
            self.lua
                .set_named_registry_value(&key, f)
                .map_err(|e| eyre!("failed to store cleanup: {e}"))?;
            Some(key)
        } else {
            None
        };

        // Store the script's environment so it stays alive
        let env_key = format!("__env_{}", meta.name);
        self.lua
            .set_named_registry_value(&env_key, env)
            .map_err(|e| eyre!("failed to store env: {e}"))?;

        let name = meta.name.clone();
        self.scripts.insert(
            name,
            LoadedScript {
                meta: meta.clone(),
                cleanup_key,
                env_key,
            },
        );

        Ok(meta)
    }

    fn unload_script(&mut self, name: &str) -> Result<()> {
        let script = self
            .scripts
            .remove(name)
            .ok_or_else(|| eyre!("script '{name}' is not loaded"))?;

        // Call cleanup function
        if let Some(ref key) = script.cleanup_key {
            if let Ok(f) = self.lua.named_registry_value::<LuaFunction>(key)
                && let Err(e) = f.call::<()>(())
            {
                tracing::warn!("cleanup error for script '{name}': {e}");
            }
            let _ = self.lua.unset_named_registry_value(key);
        }

        // Release script environment
        let _ = self.lua.unset_named_registry_value(&script.env_key);

        // Remove event handlers
        let handler_keys: Vec<String> = {
            let mut st = self.state.lock().map_err(|e| eyre!("handler state poisoned: {e}"))?;
            let keys: Vec<String> = st
                .handlers
                .iter()
                .filter(|h| h.script_name == name)
                .map(|h| h.registry_key.clone())
                .collect();
            st.handlers.retain(|h| h.script_name != name);
            keys
        };
        for key in &handler_keys {
            let _ = self.lua.unset_named_registry_value(key);
        }

        // Remove commands
        let cmd_keys: Vec<String> = {
            let mut st = self.state.lock().map_err(|e| eyre!("handler state poisoned: {e}"))?;
            let keys: Vec<String> = st
                .commands
                .iter()
                .filter(|(_, cmd)| cmd.script_name == name)
                .map(|(_, cmd)| cmd.registry_key.clone())
                .collect();
            st.commands.retain(|_, cmd| cmd.script_name != name);
            keys
        };
        for key in &cmd_keys {
            let _ = self.lua.unset_named_registry_value(key);
        }

        Ok(())
    }

    fn emit(&self, event: &Event) -> EventResult {
        // Snapshot matching handlers (release lock before calling Lua)
        let snapshot: Vec<(String, u64, bool)> = {
            let Some(st) = self.state.lock().ok() else {
                tracing::error!("handler state poisoned in emit");
                return EventResult::Continue;
            };
            st.handlers
                .iter()
                .filter(|h| h.event_name == event.name)
                .map(|h| (h.registry_key.clone(), h.id, h.once))
                .collect()
        };

        if snapshot.is_empty() {
            return EventResult::Continue;
        }

        let ev_table = match Self::event_to_table(&self.lua, event) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!("failed to create event table: {e}");
                return EventResult::Continue;
            }
        };

        let mut result = EventResult::Continue;
        let mut once_ids = Vec::new();

        for (key, id, once) in &snapshot {
            if *once {
                once_ids.push(*id);
            }

            let func = match self.lua.named_registry_value::<LuaFunction>(key) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!("handler {id} missing from registry: {e}");
                    continue;
                }
            };

            match func.call::<LuaValue>(ev_table.clone()) {
                Ok(LuaValue::Boolean(true)) => {
                    result = EventResult::Suppress;
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("handler {id} error: {e}");
                }
            }
        }

        // Clean up fired once-handlers
        if !once_ids.is_empty() {
            let keys_to_remove: Vec<String> = {
                let Some(mut st) = self.state.lock().ok() else {
                    tracing::error!("handler state poisoned during once-handler cleanup");
                    return result;
                };
                let keys: Vec<String> = st
                    .handlers
                    .iter()
                    .filter(|h| once_ids.contains(&h.id))
                    .map(|h| h.registry_key.clone())
                    .collect();
                st.handlers.retain(|h| !once_ids.contains(&h.id));
                keys
            };
            for key in keys_to_remove {
                let _ = self.lua.unset_named_registry_value(&key);
            }
        }

        result
    }

    fn handle_command(
        &self,
        name: &str,
        args: &[String],
        connection_id: Option<&str>,
    ) -> Option<EventResult> {
        let st = self.state.lock().ok()?;
        let key = st.commands.get(name)?.registry_key.clone();
        drop(st);

        let func = self.lua.named_registry_value::<LuaFunction>(&key).ok()?;

        let args_table = self.lua.create_table().ok()?;
        for (i, arg) in args.iter().enumerate() {
            args_table.set(i + 1, arg.as_str()).ok()?;
        }

        match func.call::<LuaValue>((args_table, connection_id.map(std::string::ToString::to_string))) {
            Ok(LuaValue::Boolean(true)) => Some(EventResult::Suppress),
            Ok(_) => Some(EventResult::Continue),
            Err(e) => {
                tracing::warn!("command '{name}' error: {e}");
                Some(EventResult::Continue)
            }
        }
    }

    fn loaded_scripts(&self) -> Vec<ScriptMeta> {
        self.scripts.values().map(|s| s.meta.clone()).collect()
    }
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tempfile::NamedTempFile;

    /// Create a no-op `ScriptAPI` for testing. Captures calls via shared state.
    fn mock_api() -> ScriptAPI {
        ScriptAPI {
            say: Arc::new(|_| {}),
            action: Arc::new(|_| {}),
            notice: Arc::new(|_| {}),
            raw: Arc::new(|_| {}),
            join: Arc::new(|_| {}),
            part: Arc::new(|_| {}),
            change_nick: Arc::new(|_| {}),
            whois: Arc::new(|_| {}),
            mode: Arc::new(|_| {}),
            kick: Arc::new(|_| {}),
            ctcp: Arc::new(|_| {}),
            add_local_event: Arc::new(|_| {}),
            add_buffer_event: Arc::new(|_| {}),
            switch_buffer: Arc::new(|_| {}),
            execute_command: Arc::new(|_| {}),
            active_buffer_id: Arc::new(|()| None),
            our_nick: Arc::new(|_| Some("testuser".to_string())),
            connection_info: Arc::new(|_| None),
            connections: Arc::new(|()| vec![]),
            buffer_info: Arc::new(|_| None),
            buffers: Arc::new(|()| vec![]),
            buffer_nicks: Arc::new(|_| vec![]),
            register_command: Arc::new(|_| {}),
            unregister_command: Arc::new(|_| {}),
            start_timer: Arc::new(|_| 0),
            start_timeout: Arc::new(|_| 0),
            cancel_timer: Arc::new(|_| {}),
            config_get: Arc::new(|_| None),
            config_set: Arc::new(|_| {}),
            app_config_get: Arc::new(|_| None),
            log: Arc::new(|_| {}),
        }
    }

    fn write_script(source: &str) -> NamedTempFile {
        let mut f = tempfile::Builder::new()
            .suffix(".lua")
            .tempfile()
            .unwrap();
        f.write_all(source.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn load_and_unload_script() {
        let mut engine = LuaEngine::new().unwrap();
        let api = mock_api();
        let f = write_script(
            r#"
            meta = { name = "test", version = "1.0", description = "Test script" }
            function setup(api)
                return function() end
            end
        "#,
        );

        let meta = engine.load_script(f.path(), &api).unwrap();
        assert_eq!(meta.name, "test");
        assert_eq!(meta.version.as_deref(), Some("1.0"));
        assert_eq!(engine.loaded_scripts().len(), 1);

        engine.unload_script("test").unwrap();
        assert_eq!(engine.loaded_scripts().len(), 0);
    }

    #[test]
    fn duplicate_load_rejected() {
        let mut engine = LuaEngine::new().unwrap();
        let api = mock_api();
        let f = write_script(
            r#"
            meta = { name = "dup" }
            function setup(api) end
        "#,
        );

        engine.load_script(f.path(), &api).unwrap();
        let err = engine.load_script(f.path(), &api);
        assert!(err.is_err());
        assert!(format!("{}", err.unwrap_err()).contains("already loaded"));
    }

    #[test]
    fn missing_meta_rejected() {
        let mut engine = LuaEngine::new().unwrap();
        let api = mock_api();
        let f = write_script(
            r"
            function setup(api) end
        ",
        );

        let err = engine.load_script(f.path(), &api);
        assert!(err.is_err());
    }

    #[test]
    fn missing_setup_rejected() {
        let mut engine = LuaEngine::new().unwrap();
        let api = mock_api();
        let f = write_script(
            r#"
            meta = { name = "nosup" }
        "#,
        );

        let err = engine.load_script(f.path(), &api);
        assert!(err.is_err());
    }

    #[test]
    fn event_handler_fires() {
        let mut engine = LuaEngine::new().unwrap();
        let called = Arc::new(AtomicU64::new(0));
        let called2 = Arc::clone(&called);

        let mut api = mock_api();
        api.log = Arc::new(move |(_name, msg)| {
            if msg == "got_event" {
                called2.fetch_add(1, Ordering::SeqCst);
            }
        });

        let f = write_script(
            r#"
            meta = { name = "evtest" }
            function setup(api)
                api.on("irc.privmsg", function(ev)
                    api.log("got_event")
                end)
            end
        "#,
        );

        engine.load_script(f.path(), &api).unwrap();

        let event = Event {
            name: "irc.privmsg".to_string(),
            params: HashMap::new(),
        };
        engine.emit(&event);
        assert_eq!(called.load(Ordering::SeqCst), 1);

        // Fires again
        engine.emit(&event);
        assert_eq!(called.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn event_params_visible_in_lua() {
        let mut engine = LuaEngine::new().unwrap();
        let captured = Arc::new(Mutex::new(String::new()));
        let captured2 = Arc::clone(&captured);

        let mut api = mock_api();
        api.log = Arc::new(move |(_name, msg)| {
            *captured2.lock().unwrap() = msg;
        });

        let f = write_script(
            r#"
            meta = { name = "params" }
            function setup(api)
                api.on("irc.privmsg", function(ev)
                    api.log(ev.nick .. ":" .. ev.message)
                end)
            end
        "#,
        );

        engine.load_script(f.path(), &api).unwrap();

        let mut params = HashMap::new();
        params.insert("nick".to_string(), "alice".to_string());
        params.insert("message".to_string(), "hello".to_string());
        let event = Event {
            name: "irc.privmsg".to_string(),
            params,
        };
        engine.emit(&event);
        assert_eq!(*captured.lock().unwrap(), "alice:hello");
    }

    #[test]
    fn suppress_event() {
        let mut engine = LuaEngine::new().unwrap();
        let api = mock_api();
        let f = write_script(
            r#"
            meta = { name = "suppress" }
            function setup(api)
                api.on("test", function(ev)
                    return true
                end)
            end
        "#,
        );

        engine.load_script(f.path(), &api).unwrap();
        let event = Event {
            name: "test".to_string(),
            params: HashMap::new(),
        };
        assert_eq!(engine.emit(&event), EventResult::Suppress);
    }

    #[test]
    fn once_handler_fires_once() {
        let mut engine = LuaEngine::new().unwrap();
        let counter = Arc::new(AtomicU64::new(0));
        let counter2 = Arc::clone(&counter);

        let mut api = mock_api();
        api.log = Arc::new(move |_| {
            counter2.fetch_add(1, Ordering::SeqCst);
        });

        let f = write_script(
            r#"
            meta = { name = "oncetest" }
            function setup(api)
                api.once("ping", function(ev)
                    api.log("fired")
                end)
            end
        "#,
        );

        engine.load_script(f.path(), &api).unwrap();

        let event = Event {
            name: "ping".to_string(),
            params: HashMap::new(),
        };
        engine.emit(&event);
        engine.emit(&event);
        engine.emit(&event);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn priority_ordering() {
        let mut engine = LuaEngine::new().unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let order2 = Arc::clone(&order);

        let mut api = mock_api();
        api.log = Arc::new(move |(_name, msg)| {
            order2.lock().unwrap().push(msg);
        });

        let f = write_script(
            r#"
            meta = { name = "prio" }
            function setup(api)
                api.on("test", function(ev)
                    api.log("low")
                end, api.PRIORITY_LOW)
                api.on("test", function(ev)
                    api.log("high")
                end, api.PRIORITY_HIGH)
                api.on("test", function(ev)
                    api.log("normal")
                end, api.PRIORITY_NORMAL)
            end
        "#,
        );

        engine.load_script(f.path(), &api).unwrap();
        let event = Event {
            name: "test".to_string(),
            params: HashMap::new(),
        };
        engine.emit(&event);
        assert_eq!(
            *order.lock().unwrap(),
            vec!["high", "normal", "low"]
        );
    }

    #[test]
    fn unload_removes_handlers() {
        let mut engine = LuaEngine::new().unwrap();
        let api = mock_api();
        let f = write_script(
            r#"
            meta = { name = "rm" }
            function setup(api)
                api.on("test", function(ev)
                    return true
                end)
            end
        "#,
        );

        engine.load_script(f.path(), &api).unwrap();
        let event = Event {
            name: "test".to_string(),
            params: HashMap::new(),
        };
        assert_eq!(engine.emit(&event), EventResult::Suppress);

        engine.unload_script("rm").unwrap();
        assert_eq!(engine.emit(&event), EventResult::Continue);
        assert!(engine.state.lock().unwrap().handlers.is_empty());
    }

    #[test]
    fn cleanup_called_on_unload() {
        let mut engine = LuaEngine::new().unwrap();
        let cleaned = Arc::new(AtomicU64::new(0));
        let cleaned2 = Arc::clone(&cleaned);

        let mut api = mock_api();
        api.log = Arc::new(move |(_name, msg)| {
            if msg == "cleaned_up" {
                cleaned2.fetch_add(1, Ordering::SeqCst);
            }
        });

        let f = write_script(
            r#"
            meta = { name = "clean" }
            function setup(api)
                return function()
                    api.log("cleaned_up")
                end
            end
        "#,
        );

        engine.load_script(f.path(), &api).unwrap();
        engine.unload_script("clean").unwrap();
        assert_eq!(cleaned.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn sandbox_blocks_dangerous_globals() {
        let mut engine = LuaEngine::new().unwrap();
        let api = mock_api();
        let f = write_script(
            r#"
            meta = { name = "sandbox" }
            function setup(api)
                assert(os == nil, "os should be nil")
                assert(io == nil, "io should be nil")
                assert(loadfile == nil, "loadfile should be nil")
                assert(dofile == nil, "dofile should be nil")
                assert(package == nil, "package should be nil")
            end
        "#,
        );

        // If sandbox works, setup succeeds. If not, the asserts fail.
        engine.load_script(f.path(), &api).unwrap();
    }

    #[test]
    fn irc_say_calls_api() {
        let mut engine = LuaEngine::new().unwrap();
        let called_with = Arc::new(Mutex::new(None));
        let called_with2 = Arc::clone(&called_with);

        let mut api = mock_api();
        api.say = Arc::new(move |(target, text, conn)| {
            *called_with2.lock().unwrap() = Some((target, text, conn));
        });

        let f = write_script(
            r##"
            meta = { name = "saytest" }
            function setup(api)
                api.on("test", function(ev)
                    api.irc.say("#chan", "hello world")
                end)
            end
        "##,
        );

        engine.load_script(f.path(), &api).unwrap();
        engine.emit(&Event {
            name: "test".to_string(),
            params: HashMap::new(),
        });

        let result = called_with.lock().unwrap().clone().unwrap();
        assert_eq!(result.0, "#chan".to_string());
        assert_eq!(result.1, "hello world".to_string());
        assert_eq!(result.2, None);
    }

    #[test]
    fn command_registration_and_dispatch() {
        let mut engine = LuaEngine::new().unwrap();
        let registered = Arc::new(Mutex::new(None));
        let registered2 = Arc::clone(&registered);
        let said = Arc::new(Mutex::new(None));
        let said2 = Arc::clone(&said);

        let mut api = mock_api();
        api.register_command = Arc::new(move |(name, desc, usage)| {
            *registered2.lock().unwrap() = Some((name, desc, usage));
        });
        api.say = Arc::new(move |(target, text, _conn)| {
            *said2.lock().unwrap() = Some((target, text));
        });

        let f = write_script(
            r##"
            meta = { name = "cmdtest" }
            function setup(api)
                api.command("greet", {
                    handler = function(args, conn_id)
                        api.irc.say(args[1] or "#default", "Hello!")
                    end,
                    description = "Send a greeting",
                    usage = "/greet [channel]",
                })
            end
        "##,
        );

        engine.load_script(f.path(), &api).unwrap();

        // Verify registration callback
        let reg = registered.lock().unwrap().clone().unwrap();
        assert_eq!(reg.0, "greet");
        assert_eq!(reg.1, "Send a greeting");

        // Dispatch the command
        let test_chan = "#test".to_string();
        let result = engine.handle_command("greet", std::slice::from_ref(&test_chan), None);
        assert!(result.is_some());

        let say_result = said.lock().unwrap().clone().unwrap();
        assert_eq!(say_result.0, test_chan);
        assert_eq!(say_result.1, "Hello!");
    }

    #[test]
    fn unload_removes_commands() {
        let mut engine = LuaEngine::new().unwrap();
        let api = mock_api();
        let f = write_script(
            r#"
            meta = { name = "cmdclean" }
            function setup(api)
                api.command("foo", {
                    handler = function() end,
                })
            end
        "#,
        );

        engine.load_script(f.path(), &api).unwrap();
        assert!(engine.handle_command("foo", &[], None).is_some());

        engine.unload_script("cmdclean").unwrap();
        assert!(engine.handle_command("foo", &[], None).is_none());
    }

    #[test]
    fn script_isolation_between_loads() {
        let mut engine = LuaEngine::new().unwrap();
        let api = mock_api();

        let f1 = write_script(
            r#"
            meta = { name = "script_a" }
            MY_GLOBAL = "from_a"
            function setup(api) end
        "#,
        );

        let f2 = write_script(
            r#"
            meta = { name = "script_b" }
            function setup(api)
                assert(MY_GLOBAL == nil, "should not see script_a's globals")
            end
        "#,
        );

        engine.load_script(f1.path(), &api).unwrap();
        engine.load_script(f2.path(), &api).unwrap();
    }

    #[test]
    fn off_removes_handler() {
        let mut engine = LuaEngine::new().unwrap();
        let counter = Arc::new(AtomicU64::new(0));
        let counter2 = Arc::clone(&counter);

        let mut api = mock_api();
        api.log = Arc::new(move |_| {
            counter2.fetch_add(1, Ordering::SeqCst);
        });

        let f = write_script(
            r#"
            meta = { name = "offtest" }
            function setup(api)
                local id = api.on("test", function(ev)
                    api.log("fired")
                end)
                api.off(id)
            end
        "#,
        );

        engine.load_script(f.path(), &api).unwrap();
        engine.emit(&Event {
            name: "test".to_string(),
            params: HashMap::new(),
        });
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn store_our_nick_works() {
        let mut engine = LuaEngine::new().unwrap();
        let captured = Arc::new(Mutex::new(String::new()));
        let captured2 = Arc::clone(&captured);

        let mut api = mock_api();
        api.our_nick = Arc::new(|_| Some("mynick".to_string()));
        api.log = Arc::new(move |(_name, msg)| {
            *captured2.lock().unwrap() = msg;
        });

        let f = write_script(
            r#"
            meta = { name = "nicktest" }
            function setup(api)
                api.on("test", function(ev)
                    local nick = api.store.our_nick()
                    api.log(nick or "nil")
                end)
            end
        "#,
        );

        engine.load_script(f.path(), &api).unwrap();
        engine.emit(&Event {
            name: "test".to_string(),
            params: HashMap::new(),
        });
        assert_eq!(*captured.lock().unwrap(), "mynick");
    }

    #[test]
    fn handler_error_does_not_crash() {
        let mut engine = LuaEngine::new().unwrap();
        let api = mock_api();
        let f = write_script(
            r#"
            meta = { name = "errtest" }
            function setup(api)
                api.on("test", function(ev)
                    error("intentional error")
                end)
            end
        "#,
        );

        engine.load_script(f.path(), &api).unwrap();
        // Should not panic, returns Continue
        let result = engine.emit(&Event {
            name: "test".to_string(),
            params: HashMap::new(),
        });
        assert_eq!(result, EventResult::Continue);
    }

    #[test]
    fn no_matching_event_returns_continue() {
        let mut engine = LuaEngine::new().unwrap();
        let api = mock_api();
        let f = write_script(
            r#"
            meta = { name = "nomatch" }
            function setup(api)
                api.on("irc.privmsg", function(ev) end)
            end
        "#,
        );

        engine.load_script(f.path(), &api).unwrap();
        let result = engine.emit(&Event {
            name: "irc.join".to_string(),
            params: HashMap::new(),
        });
        assert_eq!(result, EventResult::Continue);
    }
}

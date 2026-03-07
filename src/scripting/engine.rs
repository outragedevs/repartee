use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use color_eyre::eyre::{Result, eyre};

use super::event_bus::{Event, EventResult};

// ─── Script Metadata ─────────────────────────────────────────

/// Metadata about a loaded script.
#[derive(Debug, Clone)]
pub struct ScriptMeta {
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub path: PathBuf,
}

// ─── Data types returned to scripts ──────────────────────────

/// Read-only snapshot of a connection, visible to scripts.
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub id: String,
    pub label: String,
    pub nick: String,
    pub connected: bool,
    pub user_modes: String,
}

/// Read-only snapshot of a buffer, visible to scripts.
#[derive(Debug, Clone)]
pub struct BufferInfo {
    pub id: String,
    pub connection_id: String,
    pub name: String,
    pub buffer_type: String,
    pub topic: Option<String>,
    pub unread_count: u32,
}

/// Read-only snapshot of a nick entry, visible to scripts.
#[derive(Debug, Clone)]
pub struct NickInfo {
    pub nick: String,
    pub prefix: String,
    pub modes: String,
    pub away: bool,
}

/// Lightweight snapshot of app state, updated once per tick (or after
/// state-mutating events). Script callbacks close over an
/// `Arc<RwLock<ScriptStateSnapshot>>` so they can read current state
/// without reaching into `AppState` directly.
#[derive(Debug, Clone, Default)]
pub struct ScriptStateSnapshot {
    pub active_buffer_id: Option<String>,
    pub connections: Vec<ConnectionInfo>,
    pub buffers: Vec<BufferInfo>,
    pub buffer_nicks: HashMap<String, Vec<NickInfo>>,
    /// Per-script config: (script_name, key) → value.
    pub script_config: HashMap<(String, String), String>,
    /// Serialized app config as TOML for `api.config.app_get()` dot-path lookups.
    pub app_config_toml: Option<toml::Value>,
}

// ─── ScriptEngine trait ──────────────────────────────────────

/// Trait that each scripting language backend implements.
///
/// Adding a new language (e.g. Rhai) means implementing this trait
/// and registering the engine in `ScriptManager`.
pub trait ScriptEngine: Send {
    /// File extension this engine handles (e.g. "lua", "rhai").
    fn extension(&self) -> &'static str;

    /// Load a script file and run its setup function.
    fn load_script(&mut self, path: &Path, api: &ScriptAPI) -> Result<ScriptMeta>;

    /// Unload a previously loaded script by name.
    fn unload_script(&mut self, name: &str) -> Result<()>;

    /// Dispatch an event to all loaded scripts. Returns `Suppress` if
    /// any script handler suppressed the event.
    fn emit(&self, event: &Event) -> EventResult;

    /// Handle a script-registered command. Returns `Some(EventResult)` if
    /// this engine owns the command, `None` otherwise.
    fn handle_command(
        &self,
        name: &str,
        args: &[String],
        connection_id: Option<&str>,
    ) -> Option<EventResult>;

    /// Fire a timer callback by ID. The engine looks up the stored Lua
    /// callback and invokes it. No-op if the timer ID is unknown.
    fn fire_timer(&self, timer_id: u64);

    /// List currently loaded scripts.
    fn loaded_scripts(&self) -> Vec<ScriptMeta>;
}

// ─── ScriptAPI ───────────────────────────────────────────────
//
// The full API surface exposed to script engines. Modeled after
// WeeChat, irssi, and kokoirc scripting APIs.
//
// Each engine wraps these callbacks into its language's calling
// convention. This keeps the API definition in one place — new
// engines get the same capabilities automatically.
//
// All callbacks are Arc<dyn Fn> so they can be shared with the
// engine without ownership issues.

/// Type alias for shared, thread-safe callbacks.
type Cb<Args, Ret = ()> = Arc<dyn Fn(Args) -> Ret + Send + Sync>;

pub struct ScriptAPI {
    // ── IRC operations ───────────────────────────────────────
    // All take an optional connection_id; None = active connection.

    /// Send a PRIVMSG. Args: (target, text, `connection_id`?)
    pub say: Cb<(String, String, Option<String>)>,
    /// Send a CTCP ACTION. Args: (target, text, `connection_id`?)
    pub action: Cb<(String, String, Option<String>)>,
    /// Send a NOTICE. Args: (target, text, `connection_id`?)
    pub notice: Cb<(String, String, Option<String>)>,
    /// Send a raw IRC line. Args: (line, `connection_id`?)
    pub raw: Cb<(String, Option<String>)>,
    /// Join a channel. Args: (channel, key?, `connection_id`?)
    pub join: Cb<(String, Option<String>, Option<String>)>,
    /// Part a channel. Args: (channel, message?, `connection_id`?)
    pub part: Cb<(String, Option<String>, Option<String>)>,
    /// Change nick. Args: (`new_nick`, `connection_id`?)
    pub change_nick: Cb<(String, Option<String>)>,
    /// WHOIS query. Args: (nick, `connection_id`?)
    pub whois: Cb<(String, Option<String>)>,
    /// Set channel mode. Args: (channel, `mode_string`, `connection_id`?)
    pub mode: Cb<(String, String, Option<String>)>,
    /// Kick a user. Args: (channel, nick, reason?, `connection_id`?)
    pub kick: Cb<(String, String, Option<String>, Option<String>)>,
    /// Send a CTCP request. Args: (target, `ctcp_type`, message?, `connection_id`?)
    pub ctcp: Cb<(String, String, Option<String>, Option<String>)>,

    // ── UI / output ──────────────────────────────────────────

    /// Display a local event message in the active buffer. Args: text
    pub add_local_event: Cb<String>,
    /// Display a local event in a specific buffer. Args: (`buffer_id`, text)
    pub add_buffer_event: Cb<(String, String)>,
    /// Switch to a buffer. Args: `buffer_id`
    pub switch_buffer: Cb<String>,
    /// Execute a client command (e.g. "/set theme default"). Args: `command_line`
    pub execute_command: Cb<String>,

    // ── State queries (read-only) ────────────────────────────

    /// Get the active buffer ID. Returns None if no buffer active.
    pub active_buffer_id: Cb<(), Option<String>>,
    /// Get our current nick. Args: `connection_id`? Returns None if not connected.
    pub our_nick: Cb<Option<String>, Option<String>>,
    /// Get a connection's info. Args: `connection_id`. Returns None if not found.
    pub connection_info: Cb<String, Option<ConnectionInfo>>,
    /// List all connections.
    pub connections: Cb<(), Vec<ConnectionInfo>>,
    /// Get a buffer's info. Args: `buffer_id`. Returns None if not found.
    pub buffer_info: Cb<String, Option<BufferInfo>>,
    /// List all buffers.
    pub buffers: Cb<(), Vec<BufferInfo>>,
    /// Get nicks in a buffer. Args: `buffer_id`.
    pub buffer_nicks: Cb<String, Vec<NickInfo>>,

    // ── Commands ─────────────────────────────────────────────
    //
    // Scripts register custom commands. The manager stores them
    // keyed by (script_name, command_name) so unload can clean up.

    /// Register a script command. Args: (`command_name`, description, `usage_hint`).
    /// The actual handler is registered inside the engine; this just
    /// tells the command registry about it.
    pub register_command: Cb<(String, String, String)>,
    /// Unregister a script command. Args: `command_name`.
    pub unregister_command: Cb<String>,

    // ── Timers ───────────────────────────────────────────────
    //
    // Timer callbacks live inside the engine. These return a timer ID
    // so scripts can cancel them.

    /// Start a repeating timer. Args: `interval_ms`. Returns `timer_id`.
    pub start_timer: Cb<u64, u64>,
    /// Start a one-shot timeout. Args: `delay_ms`. Returns `timer_id`.
    pub start_timeout: Cb<u64, u64>,
    /// Cancel a timer. Args: `timer_id`.
    pub cancel_timer: Cb<u64>,

    // ── Config ───────────────────────────────────────────────
    //
    // Per-script config stored under [scripts.<name>] in config.toml.
    // Also provides read access to the app config.

    /// Get a per-script config value. Args: (`script_name`, key). Returns None if unset.
    pub config_get: Cb<(String, String), Option<String>>,
    /// Set a per-script config value. Args: (`script_name`, key, value).
    pub config_set: Cb<(String, String, String)>,
    /// Read an app-level config value. Args: `key_path` (dot-separated). Returns None if unset.
    pub app_config_get: Cb<String, Option<String>>,

    // ── Logging ──────────────────────────────────────────────

    /// Script debug log. Args: (`script_name`, message). Only emits if scripts.debug = true.
    pub log: Cb<(String, String)>,
}

// ─── ScriptManager ───────────────────────────────────────────

/// Manages all scripting engines and routes operations to the right one.
pub struct ScriptManager {
    engines: Vec<Box<dyn ScriptEngine>>,
    scripts_dir: PathBuf,
}

impl ScriptManager {
    pub fn new(scripts_dir: PathBuf) -> Self {
        Self {
            engines: Vec::new(),
            scripts_dir,
        }
    }

    /// Register a scripting engine (e.g. Lua, Rhai).
    pub fn register_engine(&mut self, engine: Box<dyn ScriptEngine>) {
        self.engines.push(engine);
    }

    /// The directory where scripts are stored.
    pub fn scripts_dir(&self) -> &Path {
        &self.scripts_dir
    }

    /// Load a script by name or path. The file extension determines which engine handles it.
    pub fn load(&mut self, name_or_path: &str, api: &ScriptAPI) -> Result<ScriptMeta> {
        let path = self.resolve_path(name_or_path);

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();

        let engine = self
            .engines
            .iter_mut()
            .find(|e| e.extension() == ext)
            .ok_or_else(|| eyre!("no engine registered for .{ext} files"))?;

        engine.load_script(&path, api)
    }

    /// Unload a script by name. Tries all engines.
    pub fn unload(&mut self, name: &str) -> Result<()> {
        for engine in &mut self.engines {
            if engine.loaded_scripts().iter().any(|s| s.name == name) {
                return engine.unload_script(name);
            }
        }
        Err(eyre!("script '{name}' is not loaded"))
    }

    /// Reload a script (unload + load).
    pub fn reload(&mut self, name: &str, api: &ScriptAPI) -> Result<ScriptMeta> {
        let path = self
            .loaded_scripts()
            .into_iter()
            .find(|s| s.name == name)
            .map(|s| s.path)
            .ok_or_else(|| eyre!("script '{name}' is not loaded"))?;

        self.unload(name)?;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();

        let engine = self
            .engines
            .iter_mut()
            .find(|e| e.extension() == ext)
            .ok_or_else(|| eyre!("no engine for .{ext}"))?;

        engine.load_script(&path, api)
    }

    /// Emit an event to all engines. Returns true if any handler suppressed it.
    pub fn emit(&self, event: &Event) -> bool {
        for engine in &self.engines {
            if engine.emit(event) == EventResult::Suppress {
                return true;
            }
        }
        false
    }

    /// Try to dispatch a command to script engines. Returns true if handled.
    pub fn handle_command(
        &self,
        name: &str,
        args: &[String],
        connection_id: Option<&str>,
    ) -> bool {
        for engine in &self.engines {
            if engine.handle_command(name, args, connection_id).is_some() {
                return true;
            }
        }
        false
    }

    /// Fire a timer callback. Routes to all engines (only the one that owns
    /// the timer ID will actually invoke it).
    pub fn fire_timer(&self, timer_id: u64) {
        for engine in &self.engines {
            engine.fire_timer(timer_id);
        }
    }

    /// List all loaded scripts across all engines.
    pub fn loaded_scripts(&self) -> Vec<ScriptMeta> {
        self.engines
            .iter()
            .flat_map(|e| e.loaded_scripts())
            .collect()
    }

    /// List available script files in the scripts directory.
    pub fn available_scripts(&self) -> Vec<(String, PathBuf, bool)> {
        let loaded: Vec<String> = self
            .loaded_scripts()
            .iter()
            .map(|s| s.name.clone())
            .collect();
        let mut results = Vec::new();

        let Ok(entries) = std::fs::read_dir(&self.scripts_dir) else {
            return results;
        };

        let known_exts: Vec<String> = self
            .engines
            .iter()
            .map(|e| e.extension().to_string())
            .collect();

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if !known_exts.iter().any(|k| k == ext) {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let is_loaded = loaded.contains(&name);
            results.push((name, path, is_loaded));
        }

        results.sort_by(|a, b| a.0.cmp(&b.0));
        results
    }

    fn resolve_path(&self, name_or_path: &str) -> PathBuf {
        // Expand ~/  to the user's home directory
        if let Some(rest) = name_or_path.strip_prefix("~/")
            && let Some(home) = dirs::home_dir()
        {
            return home.join(rest);
        }
        let p = Path::new(name_or_path);
        if p.is_absolute() || name_or_path.starts_with("./") {
            return p.to_path_buf();
        }
        // Try adding known extensions
        for engine in &self.engines {
            let with_ext = self
                .scripts_dir
                .join(format!("{name_or_path}.{}", engine.extension()));
            if with_ext.exists() {
                return with_ext;
            }
        }
        // Fall back to exact name in scripts dir
        self.scripts_dir.join(name_or_path)
    }
}

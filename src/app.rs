use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use chrono::Utc;
use color_eyre::eyre::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::{Position, Rect};
use tokio::sync::mpsc;
use tokio::time::{Duration, interval};

use crate::config::{self, AppConfig};
use crate::constants;
use crate::irc::{self, IrcEvent, IrcHandle};
use crate::state::AppState;
use crate::state::buffer::{
    ActivityLevel, Buffer, BufferType, Message, MessageType, make_buffer_id,
};
use crate::state::connection::{Connection, ConnectionStatus};
use crate::theme::{self, ThemeFile};
use crate::ui;
use crate::ui::layout::UiRegions;

use ratatui_image::picker::ProtocolType;

/// Detect the outer terminal program name and its image protocol capability.
///
/// This always runs — not just as a fallback — so we know WHO we're talking
/// to for protocol selection, cleanup strategy, and debug logging.
///
/// Returns `(terminal_name, protocol, source)`.
/// Detect the outer terminal via tmux client queries.
///
/// Queries `#{client_termtype}` and `#{client_termname}` to identify the real
/// terminal hosting the tmux session (e.g. iTerm2, Ghostty, Kitty). Falls back
/// to Alacritty heuristic (generic xterm + empty termname).
fn detect_via_tmux() -> Option<(&'static str, Option<ProtocolType>, String)> {
    let termtype = tmux_query_raw("#{client_termtype}");
    let termname = tmux_query_raw("#{client_termname}");
    tracing::debug!(
        client_termtype = ?termtype,
        client_termname = ?termname,
        "tmux outer terminal queries"
    );

    if let Some(ref tt) = termtype
        && let Some((name, proto)) = match_terminal(tt)
    {
        return Some((name, Some(proto), format!("tmux:client_termtype={tt}")));
    }
    if let Some(ref tn) = termname
        && let Some((name, proto)) = match_terminal(tn)
    {
        return Some((name, Some(proto), format!("tmux:client_termname={tn}")));
    }

    // Alacritty: generic termtype like "xterm-256color" + empty termname.
    // No image protocol support — use halfblocks.
    let tt_generic = termtype.as_deref().unwrap_or("").starts_with("xterm");
    let tn_empty = termname.as_deref().unwrap_or("").is_empty();
    if tt_generic && tn_empty {
        return Some((
            "alacritty",
            Some(ProtocolType::Halfblocks),
            "tmux:generic-xterm+empty-termname".into(),
        ));
    }

    None
}

fn detect_outer_terminal(
    in_tmux: bool,
    env_override: Option<&std::collections::HashMap<String, String>>,
) -> (&'static str, Option<ProtocolType>, String) {
    // When a shim is connected, use the shim's env vars (it knows the real terminal).
    // Otherwise fall back to the daemon's own env vars.
    let get_env = |key: &str| -> Option<String> {
        env_override.map_or_else(
            || std::env::var(key).ok().filter(|s| !s.is_empty()),
            |vars| vars.get(key).cloned(),
        )
    };

    // Dump all relevant env vars for debugging.
    tracing::debug!(
        TMUX = ?get_env("TMUX"),
        TERM = ?get_env("TERM"),
        TERM_PROGRAM = ?get_env("TERM_PROGRAM"),
        TERM_PROGRAM_VERSION = ?get_env("TERM_PROGRAM_VERSION"),
        LC_TERMINAL = ?get_env("LC_TERMINAL"),
        LC_TERMINAL_VERSION = ?get_env("LC_TERMINAL_VERSION"),
        ITERM_SESSION_ID = ?get_env("ITERM_SESSION_ID"),
        KITTY_PID = ?get_env("KITTY_PID"),
        GHOSTTY_RESOURCES_DIR = ?get_env("GHOSTTY_RESOURCES_DIR"),
        WT_SESSION = ?get_env("WT_SESSION"),
        COLORTERM = ?get_env("COLORTERM"),
        in_tmux,
        env_from_shim = env_override.is_some(),
        "outer terminal env vars"
    );

    if in_tmux && let Some(result) = detect_via_tmux() {
        return result;
    }

    // ── env var detection (works both direct and in tmux) ──
    let lc_terminal = get_env("LC_TERMINAL").unwrap_or_default();

    // LC_TERMINAL survives tmux and SSH — most reliable after tmux queries.
    if !lc_terminal.is_empty() {
        if lc_terminal.eq_ignore_ascii_case("iterm2")
            || lc_terminal.to_ascii_lowercase().contains("iterm")
        {
            return (
                "iterm2",
                Some(ProtocolType::Iterm2),
                format!("env:LC_TERMINAL={lc_terminal}"),
            );
        }
        if lc_terminal.eq_ignore_ascii_case("ghostty") {
            return (
                "ghostty",
                Some(ProtocolType::Kitty),
                format!("env:LC_TERMINAL={lc_terminal}"),
            );
        }
        if lc_terminal.eq_ignore_ascii_case("subterm") {
            return (
                "subterm",
                Some(ProtocolType::Kitty),
                format!("env:LC_TERMINAL={lc_terminal}"),
            );
        }
    }

    // Terminal-specific env vars.
    if get_env("ITERM_SESSION_ID").is_some() {
        return (
            "iterm2",
            Some(ProtocolType::Iterm2),
            "env:ITERM_SESSION_ID".into(),
        );
    }

    // GHOSTTY_RESOURCES_DIR — validate it's a real path, not just "1" or garbage.
    if let Some(grd) = get_env("GHOSTTY_RESOURCES_DIR")
        && grd.len() > 1
    {
        return (
            "ghostty",
            Some(ProtocolType::Kitty),
            format!("env:GHOSTTY_RESOURCES_DIR={grd}"),
        );
    }

    if get_env("KITTY_PID").is_some() {
        return ("kitty", Some(ProtocolType::Kitty), "env:KITTY_PID".into());
    }
    if get_env("WEZTERM_EXECUTABLE").is_some() {
        return (
            "wezterm",
            Some(ProtocolType::Iterm2),
            "env:WEZTERM_EXECUTABLE".into(),
        );
    }
    if get_env("WT_SESSION").is_some() {
        return (
            "windows-terminal",
            Some(ProtocolType::Sixel),
            "env:WT_SESSION".into(),
        );
    }

    // Non-tmux: TERM_PROGRAM is the actual terminal.
    if !in_tmux {
        let tp = get_env("TERM_PROGRAM").unwrap_or_default();
        if !tp.is_empty()
            && tp != "tmux"
            && let Some((name, proto)) = match_terminal(&tp)
        {
            return (name, Some(proto), format!("env:TERM_PROGRAM={tp}"));
        }
    }

    // ── Generic TERM value — last resort ──
    let term = get_env("TERM").unwrap_or_default();
    if let Some((name, proto)) = match_terminal(&term) {
        return (name, Some(proto), format!("env:TERM={term}"));
    }

    ("unknown", None, "auto:unknown".into())
}

/// Match a terminal identifier string to a terminal name and image protocol.
///
/// Works with `#{client_termtype}` (e.g. "iTerm2 3.6.8"), `#{client_termname}`
/// (e.g. "xterm-ghostty"), `TERM_PROGRAM`, and `TERM` values.
///
/// Returns `(&'static str, ProtocolType)` — terminal names are all known
/// constants, so no heap allocation is needed.
fn match_terminal(name: &str) -> Option<(&'static str, ProtocolType)> {
    // Case-insensitive ASCII substring check — avoids heap-allocating
    // a lowercased copy on every call (terminal names are always ASCII).
    let contains = |needle: &str| -> bool {
        name.as_bytes()
            .windows(needle.len())
            .any(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
    };

    // iTerm2
    if contains("iterm") {
        return Some(("iterm2", ProtocolType::Iterm2));
    }
    // Kitty protocol family
    if contains("ghostty") {
        return Some(("ghostty", ProtocolType::Kitty));
    }
    if contains("kitty") {
        return Some(("kitty", ProtocolType::Kitty));
    }
    if contains("subterm") {
        return Some(("subterm", ProtocolType::Kitty));
    }
    if contains("wezterm") {
        return Some(("wezterm", ProtocolType::Iterm2));
    }
    if contains("rio") {
        return Some(("rio", ProtocolType::Kitty));
    }
    // Sixel-capable terminals
    if contains("foot") {
        return Some(("foot", ProtocolType::Sixel));
    }
    if contains("contour") {
        return Some(("contour", ProtocolType::Sixel));
    }
    if contains("konsole") {
        return Some(("konsole", ProtocolType::Sixel));
    }
    if contains("mintty") {
        return Some(("mintty", ProtocolType::Sixel));
    }
    if contains("mlterm") {
        return Some(("mlterm", ProtocolType::Sixel));
    }

    None
}

/// Resolve the image protocol to use, combining config override, tmux
/// client detection, and IO-based detection.
///
/// Priority order:
///   1. Config override (`protocol = "kitty"`)
///   2. tmux `#{client_termtype}` / `#{client_termname}` — authoritative when
///      in tmux because it identifies the REAL terminal displaying the pane.
///   3. iTerm2 env-based override (non-tmux) — iTerm2 responds to Kitty IO
///      probe but its Kitty implementation is incomplete.
///   4. IO-based detection (`from_query_stdio`) — direct terminal only.
///   5. Env-based outer terminal detection fallback.
///
/// Returns `(Some(protocol), source)` if an override should be applied to the
/// picker, or `(None, source)` if the picker's auto-detected protocol is fine.
fn resolve_image_protocol(
    config_protocol: &str,
    picker: &ratatui_image::picker::Picker,
    outer_terminal: &str,
    outer_proto: Option<ProtocolType>,
    outer_source: String,
    env_is_authoritative: bool,
) -> (Option<ProtocolType>, String) {
    // 1. Config override — user explicitly chose a protocol.
    match config_protocol {
        "kitty" => return (Some(ProtocolType::Kitty), "config:kitty".into()),
        "iterm2" => return (Some(ProtocolType::Iterm2), "config:iterm2".into()),
        "sixel" => return (Some(ProtocolType::Sixel), "config:sixel".into()),
        "halfblocks" => return (Some(ProtocolType::Halfblocks), "config:halfblocks".into()),
        _ => {} // "auto" or anything else — proceed to detection
    }

    // 2. Authoritative outer terminal detection — trust it over IO queries.
    //    This applies when:
    //    - tmux: #{client_termtype} tells us the REAL terminal attached to the pane
    //    - socket: shim sends its env vars, which reflect the actual terminal
    //    In both cases the IO query result is stale/wrong (it was probed against
    //    the original terminal at startup, not the currently attached one).
    if (outer_source.starts_with("tmux:") || env_is_authoritative)
        && let Some(proto) = outer_proto
    {
        return (Some(proto), outer_source);
    }

    // 3. iTerm2 env-based override (non-tmux, non-socket path) — iTerm2 3.5+
    //    responds to the Kitty graphics probe, so ratatui-image detects Kitty.
    //    But iTerm2's Kitty implementation is incomplete; the native iTerm2
    //    (OSC 1337) protocol works correctly.
    if outer_terminal == "iterm2" {
        return (
            Some(ProtocolType::Iterm2),
            format!("iterm2-override:{outer_source}"),
        );
    }

    // 4. IO-based detection — trust it when running directly (not tmux/socket).
    if picker.protocol_type() != ProtocolType::Halfblocks {
        return (None, format!("io-query:{:?}", picker.protocol_type()));
    }

    // 5. Env-based outer terminal detection fallback.
    if let Some(proto) = outer_proto {
        return (Some(proto), outer_source);
    }

    (None, "auto:halfblocks".into())
}

/// Check if we're running inside iTerm2 (directly or via tmux).
///
/// iTerm2 3.5+ responds to the Kitty graphics protocol probe, causing
/// ratatui-image to incorrectly select Kitty over the native iTerm2 protocol.
/// Run a tmux `display-message -p` query and return the trimmed stdout.
pub fn tmux_query_raw(format_str: &str) -> Option<String> {
    let output = std::process::Command::new("tmux")
        .args(["display-message", "-p", format_str])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;

    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "App is the root state container"
)]
pub struct App {
    pub state: AppState,
    pub config: AppConfig,
    pub theme: ThemeFile,
    pub input: ui::input::InputState,
    pub should_quit: bool,
    /// Splash screen: number of logo lines currently visible (progressive reveal).
    pub splash_visible: usize,
    /// Splash screen dismissed — set to true after animation or keypress.
    pub splash_done: bool,
    pub scroll_offset: usize,
    pub ui_regions: Option<UiRegions>,
    /// IRC connection handles keyed by connection ID.
    pub irc_handles: HashMap<String, IrcHandle>,
    /// Forwarder task handles keyed by connection ID — aborted on disconnect/shutdown.
    forwarder_handles: HashMap<String, tokio::task::JoinHandle<()>>,
    /// Shared event sender — each connection's reader task sends here.
    pub irc_tx: mpsc::UnboundedSender<IrcEvent>,
    /// Single receiver for all IRC events.
    irc_rx: mpsc::UnboundedReceiver<IrcEvent>,
    /// Timestamp of last ESC keypress for ESC+key buffer switching.
    last_esc_time: Option<Instant>,
    /// Scroll offset for the buffer list (left sidebar).
    pub buffer_list_scroll: usize,
    /// Total line count in buffer list (set during render, used for scroll clamping).
    pub buffer_list_total: usize,
    /// Scroll offset for the nick list (right sidebar).
    pub nick_list_scroll: usize,
    /// Total line count in nick list (set during render, used for scroll clamping).
    pub nick_list_total: usize,
    /// Last CTCP PING sent time per connection, for lag measurement.
    pub lag_pings: HashMap<String, Instant>,
    /// `IRCv3` batch trackers per connection.
    batch_trackers: HashMap<String, crate::irc::batch::BatchTracker>,
    /// Storage subsystem for persistent message logging.
    pub storage: Option<crate::storage::Storage>,
    /// Custom quit message set by `/quit [msg]`.
    pub quit_message: Option<String>,
    /// Current image preview overlay state.
    pub image_preview: crate::image_preview::PreviewStatus,
    /// Rect to invalidate on next frame after image dismiss (targeted repaint).
    /// Set by `dismiss_image_preview()` for Kitty/iTerm2 protocols to avoid
    /// full terminal clear. Consumed by the renderer on the next draw.
    pub image_clear_rect: Option<Rect>,
    /// Channel receiver for image preview results from background tasks.
    preview_rx: mpsc::UnboundedReceiver<crate::image_preview::ImagePreviewEvent>,
    /// Channel sender cloned into each preview task.
    preview_tx: mpsc::UnboundedSender<crate::image_preview::ImagePreviewEvent>,
    /// Shared HTTP client for image fetching.
    pub http_client: reqwest::Client,
    /// Terminal image protocol picker (for ratatui-image).
    pub picker: ratatui_image::picker::Picker,
    /// Whether we're running inside tmux (for image cleanup DCS wrapping).
    pub in_tmux: bool,
    /// Force a full terminal redraw on the next frame (clears ratatui diff state).
    ///
    /// Set after image preview dismiss to ensure graphics artifacts are fully
    /// overwritten — Kitty graphics persist on a separate layer, and iTerm2+tmux
    /// direct-written images exist outside ratatui's buffer knowledge.
    pub needs_full_redraw: bool,
    /// Detected outer terminal name (e.g. "ghostty", "iterm2", "kitty").
    pub outer_terminal: String,
    /// How the image protocol was resolved (for diagnostics).
    pub image_proto_source: String,
    /// Terminal env vars from the connected shim (overrides daemon's own env for detection).
    pub shim_term_env: Option<std::collections::HashMap<String, String>>,
    /// Per-connection queues of channels awaiting batched WHO + MODE after join.
    channel_query_queues: HashMap<String, VecDeque<String>>,
    /// Per-connection set of channels in the current WHO batch (awaiting `RPL_ENDOFWHO`).
    /// When all channels receive their `RPL_ENDOFWHO`, the next batch is sent.
    channel_query_in_flight: HashMap<String, HashSet<String>>,
    /// When the current in-flight batch was sent (for stale batch timeout).
    channel_query_sent_at: HashMap<String, Instant>,
    /// Queued lines from multiline paste, drained one per 500ms tick.
    paste_queue: VecDeque<String>,
    /// Script manager (owns all scripting engines).
    pub script_manager: Option<crate::scripting::engine::ScriptManager>,
    /// Persistent `ScriptAPI` used when loading/reloading scripts.
    pub script_api: Option<crate::scripting::engine::ScriptAPI>,
    /// Shared snapshot of app state for script callbacks.
    pub script_state: Arc<std::sync::RwLock<crate::scripting::engine::ScriptStateSnapshot>>,
    /// Receiver for actions requested by script callbacks.
    script_action_rx: mpsc::UnboundedReceiver<crate::scripting::ScriptAction>,
    /// Script-registered command names (tracked so we can show them in /help).
    pub script_commands: HashMap<String, (String, String)>,
    /// Per-script config storage: (`script_name`, key) → value.
    pub script_config: HashMap<(String, String), String>,
    /// Handles for active timer tasks, keyed by timer ID. Used to abort on cancel.
    active_timers: HashMap<u64, tokio::task::JoinHandle<()>>,
    /// Sender for script actions — cloned into timer tasks so they can send
    /// `TimerFired` back to the event loop.
    script_action_tx: mpsc::UnboundedSender<crate::scripting::ScriptAction>,
    /// Cached wrap-indent width (columns) for chat continuation lines.
    /// Recomputed when config or theme changes.
    pub wrap_indent: usize,
    /// Cached TOML serialization of `config`, invalidated on config change.
    pub cached_config_toml: Option<toml::Value>,
    // --- Session / detach-reattach ---
    /// The terminal (None when detached with no terminal attached).
    pub terminal: Option<ui::Tui>,
    /// Whether the process is currently detached (no terminal).
    pub detached: bool,
    /// Set by /detach or chord — processed at top of event loop iteration.
    pub should_detach: bool,
    /// Unix socket listener for shim connections (active when detached or always).
    socket_listener: Option<tokio::net::UnixListener>,
    /// Sender for output/control messages to the connected shim.
    socket_output_tx:
        Option<tokio::sync::mpsc::UnboundedSender<crate::session::protocol::MainMessage>>,
    /// Receiver for messages from a connected shim.
    shim_event_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<crate::session::protocol::ShimMessage>>,
    /// True when terminal is socket-backed (vs local stdout).
    pub is_socket_attached: bool,
    /// Stop flag for the terminal reader thread.
    term_reader_stop: Arc<AtomicBool>,
    /// Receiver for terminal events from the blocking reader thread.
    term_rx: Option<tokio::sync::mpsc::UnboundedReceiver<crossterm::event::Event>>,
    /// Handle for the shim output writer task (so we can abort on disconnect).
    shim_output_handle: Option<tokio::task::JoinHandle<()>>,
    /// Handle for the shim input reader task.
    shim_input_handle: Option<tokio::task::JoinHandle<()>>,
    /// Cached terminal dimensions (cols, rows).
    ///
    /// Updated from shim Resize messages or `crossterm::terminal::size()` at startup.
    /// Used instead of `terminal.size()` which calls `backend.size()` (ioctl on
    /// stdout) — broken when stdout is `/dev/null` in daemon/fork mode.
    pub cached_term_cols: u16,
    pub cached_term_rows: u16,
    /// DCC connection manager (owns all DCC records, senders, config).
    pub dcc: crate::dcc::DccManager,
    /// Receiver for events from DCC async tasks.
    dcc_rx: mpsc::UnboundedReceiver<crate::dcc::DccEvent>,
    /// Spell checker (loaded from Hunspell dictionaries).
    pub spellchecker: Option<crate::spellcheck::SpellChecker>,
    /// Receiver for dictionary download events (list/get results).
    dict_rx: mpsc::UnboundedReceiver<crate::spellcheck::DictEvent>,
    /// Sender cloned into dictionary download tasks.
    pub dict_tx: mpsc::UnboundedSender<crate::spellcheck::DictEvent>,
    // --- Web frontend ---
    /// Event broadcaster for connected web clients.
    pub web_broadcaster: std::sync::Arc<crate::web::broadcast::WebBroadcaster>,
    /// Receiver for commands from web clients (processed in the main event loop).
    web_cmd_rx: mpsc::UnboundedReceiver<(crate::web::protocol::WebCommand, String)>,
    /// Sender side — cloned into the web server's `AppHandle`.
    web_cmd_tx: mpsc::UnboundedSender<(crate::web::protocol::WebCommand, String)>,
    /// Handle for the web server task (if running).
    /// Held to keep the spawned server task alive — dropped with App.
    web_server_handle: Option<tokio::task::JoinHandle<()>>,
    /// Shared session store for periodic cleanup.
    web_sessions: Option<std::sync::Arc<tokio::sync::Mutex<crate::web::auth::SessionStore>>>,
    /// Shared rate limiter for periodic cleanup.
    web_rate_limiter: Option<std::sync::Arc<tokio::sync::Mutex<crate::web::auth::RateLimiter>>>,
    /// Shared state snapshot for web handlers (updated every 1s tick).
    web_state_snapshot: Option<std::sync::Arc<std::sync::RwLock<crate::web::server::WebStateSnapshot>>>,
}

impl App {
    #[allow(clippy::too_many_lines)]
    pub fn new() -> Result<Self> {
        constants::ensure_config_dir();
        let mut config = config::load_config(&constants::config_path())?;

        // Load .env credentials and apply to server configs
        let env_vars = config::load_env(&constants::env_path())?;
        config::apply_credentials(&mut config.servers, &env_vars);
        config::apply_web_credentials(&mut config.web, &env_vars);
        let theme_path = constants::theme_dir().join(format!("{}.theme", config.general.theme));
        let theme = theme::load_theme(&theme_path)?;

        let mut state = AppState::new();
        state.flood_protection = config.general.flood_protection;
        state.ignores.clone_from(&config.ignores);
        state.scrollback_limit = config.display.scrollback_lines;
        let (irc_tx, irc_rx) = mpsc::unbounded_channel();

        // Initialize storage if logging is enabled
        let storage = if config.logging.enabled {
            match crate::storage::Storage::init(&config.logging) {
                Ok(s) => {
                    state.log_tx = Some(s.log_tx.clone());
                    state
                        .log_exclude_types
                        .clone_from(&config.logging.exclude_types);
                    Some(s)
                }
                Err(e) => {
                    tracing::error!("failed to initialize storage: {e}");
                    None
                }
            }
        } else {
            None
        };

        let (preview_tx, preview_rx) = mpsc::unbounded_channel();

        // Detect terminal image protocol capabilities.
        // Must be called before entering raw mode (setup_terminal).
        let in_tmux = std::env::var("TMUX").is_ok_and(|s| !s.is_empty());
        let picker_result = ratatui_image::picker::Picker::from_query_stdio();
        tracing::debug!(
            result = ?picker_result.as_ref().map(ratatui_image::picker::Picker::protocol_type),
            capabilities = ?picker_result.as_ref().ok().map(|p| p.capabilities().clone()),
            font_size = ?picker_result.as_ref().ok().map(ratatui_image::picker::Picker::font_size),
            "ratatui-image from_query_stdio result"
        );
        let mut picker =
            picker_result.unwrap_or_else(|_| ratatui_image::picker::Picker::halfblocks());

        // Detect outer terminal — always runs, not just as a fallback.
        // (mirrors kokoirc's detectProtocol: tmux query > env vars > TERM)
        let (outer_terminal, outer_proto, outer_source) = detect_outer_terminal(in_tmux, None);
        tracing::info!(
            outer_terminal = %outer_terminal,
            outer_proto = ?outer_proto,
            outer_source = %outer_source,
            "outer terminal detected"
        );

        // Apply protocol override from config, outer terminal, or IO result.
        let (resolved_proto, source) = resolve_image_protocol(
            &config.image_preview.protocol,
            &picker,
            outer_terminal,
            outer_proto,
            outer_source,
            false, // no shim at startup
        );
        if let Some(proto) = resolved_proto {
            tracing::debug!(
                from = ?picker.protocol_type(),
                to = ?proto,
                "overriding picker protocol"
            );
            picker.set_protocol_type(proto);
        }
        tracing::info!(
            protocol = ?picker.protocol_type(),
            source = %source,
            "image preview protocol selected"
        );

        let http_client = reqwest::Client::new();

        // --- Dictionary download channel ---
        let (dict_tx, dict_rx) = mpsc::unbounded_channel();

        // --- Web frontend channel ---
        let (web_tx, web_rx) = mpsc::unbounded_channel();

        // --- DCC subsystem ---
        let (mut dcc, dcc_rx) = crate::dcc::DccManager::new();
        dcc.timeout_secs = config.dcc.timeout;
        if !config.dcc.own_ip.is_empty() {
            dcc.own_ip = config.dcc.own_ip.parse().ok();
        }
        dcc.port_range = crate::dcc::chat::parse_port_range(&config.dcc.port_range);
        dcc.autoaccept_lowports = config.dcc.autoaccept_lowports;
        dcc.autochat_masks.clone_from(&config.dcc.autochat_masks);
        dcc.max_connections = config.dcc.max_connections;

        // --- Scripting system ---
        let (script_action_tx, script_action_rx) = mpsc::unbounded_channel();
        let script_state = Arc::new(std::sync::RwLock::new(state.script_snapshot()));
        let next_timer_id = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let script_api = Self::build_script_api(
            script_action_tx.clone(),
            Arc::clone(&script_state),
            Arc::clone(&next_timer_id),
        );
        let mut script_manager =
            crate::scripting::engine::ScriptManager::new(constants::scripts_dir());
        match crate::scripting::lua::LuaEngine::new() {
            Ok(lua_engine) => {
                script_manager.register_engine(Box::new(lua_engine));
                tracing::info!("Lua scripting engine registered");
            }
            Err(e) => {
                tracing::error!("failed to initialize Lua engine: {e}");
            }
        }

        let mut app = Self {
            state,
            config,
            theme,
            input: ui::input::InputState::new(),
            should_quit: false,
            splash_visible: 0,
            splash_done: false,
            scroll_offset: 0,
            ui_regions: None,
            irc_handles: HashMap::new(),
            forwarder_handles: HashMap::new(),
            irc_tx,
            irc_rx,
            last_esc_time: None,
            buffer_list_scroll: 0,
            buffer_list_total: 0,
            nick_list_scroll: 0,
            nick_list_total: 0,
            lag_pings: HashMap::new(),
            batch_trackers: HashMap::new(),
            storage,
            quit_message: None,
            image_preview: crate::image_preview::PreviewStatus::default(),
            image_clear_rect: None,
            preview_rx,
            preview_tx,
            http_client,
            picker,
            in_tmux,
            needs_full_redraw: false,
            outer_terminal: outer_terminal.to_string(),
            image_proto_source: source,
            shim_term_env: None,
            channel_query_queues: HashMap::new(),
            channel_query_in_flight: HashMap::new(),
            channel_query_sent_at: HashMap::new(),
            paste_queue: VecDeque::new(),
            script_manager: Some(script_manager),
            script_api: Some(script_api),
            script_state,
            script_action_rx,
            script_commands: HashMap::new(),
            script_config: HashMap::new(),
            active_timers: HashMap::new(),
            script_action_tx,
            wrap_indent: 0,
            cached_config_toml: None,
            terminal: None,
            detached: false,
            should_detach: false,
            socket_listener: None,
            socket_output_tx: None,
            shim_event_rx: None,
            is_socket_attached: false,
            term_reader_stop: Arc::new(AtomicBool::new(false)),
            term_rx: None,
            shim_output_handle: None,
            shim_input_handle: None,
            cached_term_cols: 80,
            cached_term_rows: 24,
            dcc,
            dcc_rx,
            spellchecker: None,
            dict_rx,
            dict_tx,
            web_broadcaster: std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(256)),
            web_cmd_rx: web_rx,
            web_cmd_tx: web_tx,
            web_server_handle: None,
            web_sessions: None,
            web_rate_limiter: None,
            web_state_snapshot: None,
        };
        app.recompute_wrap_indent();

        // Initialize spell checker if enabled.
        if app.config.spellcheck.enabled {
            app.init_spellchecker();
        }

        Ok(app)
    }

    /// Recompute the cached wrap-indent width used by `chat_view`.
    ///
    /// Call this after any change to `config.general.timestamp_format`,
    /// `config.display.nick_column_width`, or `theme.abstracts`.
    pub fn recompute_wrap_indent(&mut self) {
        let ts_sample = chrono::Local::now()
            .format(&self.config.general.timestamp_format)
            .to_string();
        let ts_format = self
            .theme
            .abstracts
            .get("timestamp")
            .cloned()
            .unwrap_or_else(|| "$*".to_string());
        let ts_resolved = crate::theme::resolve_abstractions(&ts_format, &self.theme.abstracts, 0);
        let ts_spans = crate::theme::parse_format_string(&ts_resolved, &[&ts_sample]);
        let ts_visual_width: usize = ts_spans.iter().map(|s| s.text.chars().count()).sum();
        self.wrap_indent = ts_visual_width + 1 + self.config.display.nick_column_width as usize + 1;
    }

    /// Show an image preview for the given URL.
    ///
    /// Re-detects the outer terminal and image protocol every time — the user
    /// may have attached from a different terminal (e.g. `tmux a` from iTerm2
    /// after starting in Ghostty).
    pub fn show_image_preview(&mut self, url: &str) {
        // Don't re-fetch if already loading or showing this URL.
        match &self.image_preview {
            crate::image_preview::PreviewStatus::Loading { url: u }
            | crate::image_preview::PreviewStatus::Ready { url: u, .. }
                if u == url =>
            {
                return;
            }
            _ => {}
        }

        // Re-detect terminal and protocol before every preview.
        self.refresh_image_protocol();

        self.image_preview = crate::image_preview::PreviewStatus::Loading {
            url: url.to_string(),
        };

        let term_size = self.terminal_size();

        crate::image_preview::spawn_preview(
            url,
            &self.config.image_preview,
            &self.picker,
            &self.http_client,
            self.preview_tx.clone(),
            term_size,
        );
    }

    /// Re-detect outer terminal and update the picker's protocol.
    pub fn refresh_image_protocol(&mut self) {
        // When socket-attached, use the shim's env vars for terminal detection
        // (the daemon's own env vars are frozen from fork time).
        let env_override = self.shim_term_env.as_ref();
        let in_tmux = env_override.map_or_else(
            || std::env::var("TMUX").is_ok_and(|s| !s.is_empty()),
            |vars| vars.get("TMUX").is_some_and(|s| !s.is_empty()),
        );
        self.in_tmux = in_tmux;

        let (outer_terminal, outer_proto, outer_source) =
            detect_outer_terminal(in_tmux, env_override);

        let (resolved_proto, source) = resolve_image_protocol(
            &self.config.image_preview.protocol,
            &self.picker,
            outer_terminal,
            outer_proto,
            outer_source,
            env_override.is_some(), // shim env is authoritative
        );

        if let Some(proto) = resolved_proto {
            if proto != self.picker.protocol_type() {
                tracing::info!(
                    from = ?self.picker.protocol_type(),
                    to = ?proto,
                    source = %source,
                    outer = %outer_terminal,
                    "image protocol changed"
                );
            }
            self.picker.set_protocol_type(proto);
        }

        self.outer_terminal = outer_terminal.to_string();
        self.image_proto_source = source;
    }

    /// Dismiss the image preview overlay (e.g. on Escape press).
    ///
    /// Sends protocol-specific cleanup sequences and forces a full terminal
    /// redraw on the next frame. The full redraw is essential because:
    /// - **Kitty**: images live on a separate graphics layer; `set_skip(true)`
    ///   on ratatui cells means the diff algorithm won't detect changes in the
    ///   underlying chat content, so stale graphics persist without a full repaint.
    /// - **iTerm2+tmux**: images are written directly to stdout after ratatui's
    ///   buffer flush, so ratatui has no knowledge of those pixels. A diff-based
    ///   update may not rewrite every cell the image covered.
    pub fn dismiss_image_preview(&mut self) {
        // Capture popup rect before clearing, for targeted repaint.
        let popup_rect = self.image_preview_popup_rect();

        if matches!(
            self.image_preview,
            crate::image_preview::PreviewStatus::Ready { .. }
        ) {
            self.cleanup_image_graphics();
        }
        self.image_preview = crate::image_preview::PreviewStatus::Hidden;

        // Decide cleanup strategy based on graphics protocol.
        // Kitty/iTerm2: escape sequences already deleted the graphics layer,
        // so we only need ratatui to repaint the cells underneath (no full clear).
        // Halfblocks/Sixel: graphics are cell-based, need a full terminal clear.
        match self.picker.protocol_type() {
            ProtocolType::Kitty | ProtocolType::Iterm2 => {
                // Store the popup rect so the renderer can invalidate just
                // that region on the next frame (differential repaint).
                self.image_clear_rect = popup_rect;
            }
            _ => {
                // Sixel / Halfblocks: full redraw required.
                self.needs_full_redraw = true;
            }
        }
    }

    /// Compute the popup Rect for the current image preview.
    fn image_preview_popup_rect(&self) -> Option<Rect> {
        let (w, h) = match &self.image_preview {
            crate::image_preview::PreviewStatus::Ready { width, height, .. } => (*width, *height),
            crate::image_preview::PreviewStatus::Loading { .. } => (40, 5),
            crate::image_preview::PreviewStatus::Error { .. } => (50, 5),
            crate::image_preview::PreviewStatus::Hidden => return None,
        };
        let term_w = self.cached_term_cols;
        let term_h = self.cached_term_rows;
        let pw = w.min(term_w);
        let ph = h.min(term_h);
        let px = (term_w.saturating_sub(pw)) / 2;
        let py = (term_h.saturating_sub(ph)) / 2;
        Some(Rect::new(px, py, pw, ph))
    }

    /// Send protocol-specific escape sequences to clear image graphics.
    ///
    /// **Kitty**: Send `ESC_Ga=d,d=A,q=2 ESC\` to delete all visible image
    /// placements. In tmux, wrapped in DCS passthrough. Matches yazi's cleanup
    /// approach (`a=d,d=A` = delete all visible placements).
    ///
    /// **iTerm2+tmux**: Write spaces over the image area directly to stdout
    /// (same path as the image was written), ensuring the direct-written pixels
    /// are overwritten immediately.
    fn cleanup_image_graphics(&self) {
        use std::io::Write;

        match self.picker.protocol_type() {
            ProtocolType::Kitty => {
                // Delete all visible Kitty image placements.
                // d=A = all visible placements, q=2 = suppress response.
                let seq = if self.in_tmux {
                    "\x1bPtmux;\x1b\x1b_Ga=d,d=A,q=2\x1b\x1b\\\x1b\\"
                } else {
                    "\x1b_Ga=d,d=A,q=2\x1b\\"
                };
                let _ = std::io::stdout().write_all(seq.as_bytes());
                let _ = std::io::stdout().flush();
                // Also clear the area with spaces for tmux direct writes.
                if self.in_tmux {
                    self.clear_direct_image_area();
                }
            }
            ProtocolType::Iterm2 if self.in_tmux => {
                // iTerm2+tmux: image was written directly to stdout.
                // Write spaces over the same area to clear the pixels.
                self.clear_direct_image_area();
            }
            _ => {
                // Sixel / Halfblocks / iTerm2 direct: ratatui's full redraw
                // (triggered by needs_full_redraw) handles cleanup.
            }
        }
    }

    /// Write spaces over the image area directly to stdout for tmux cleanup.
    ///
    /// Mirrors the cursor positioning from `write_tmux_direct_image()` but
    /// writes space characters instead of an image, clearing any direct-written
    /// image pixels.
    fn clear_direct_image_area(&self) {
        use std::io::Write;

        let (popup_width, popup_height) = match &self.image_preview {
            crate::image_preview::PreviewStatus::Ready { width, height, .. } => (*width, *height),
            _ => return,
        };

        let term_size = self.terminal_size();
        let popup_w = popup_width.min(term_size.0);
        let popup_h = popup_height.min(term_size.1);
        let popup_x = (term_size.0.saturating_sub(popup_w)) / 2;
        let popup_y = (term_size.1.saturating_sub(popup_h)) / 2;

        let inner_x = popup_x + 1;
        let inner_y = popup_y + 1;
        let inner_w = popup_w.saturating_sub(2);
        let inner_h = popup_h.saturating_sub(2);

        if inner_w == 0 || inner_h == 0 {
            return;
        }

        let mut out = std::io::stdout().lock();
        // Save cursor, move to inner area, fill with spaces row by row.
        let _ = write!(out, "\x1b7");
        let spaces: String = " ".repeat(usize::from(inner_w));
        for row in 0..inner_h {
            let r = inner_y + row + 1; // 1-based for CUP
            let c = inner_x + 1;
            let _ = write!(out, "\x1b[{r};{c}H{spaces}");
        }
        let _ = write!(out, "\x1b8");
        let _ = out.flush();
    }

    /// Write image directly to stdout for tmux passthrough (all protocols).
    ///
    /// ratatui-image embeds escape sequences as cell symbols, which causes:
    /// - Quality loss (pre-downscales to `cell×font_size` pixels)
    /// - Cleanup issues (`set_skip(true)` breaks ratatui diff)
    ///
    /// Instead, write the image directly to stdout with DCS passthrough,
    /// sending the original PNG at full resolution. The terminal handles
    /// scaling, producing much better quality.
    ///
    /// Called AFTER `terminal.draw()` so ratatui has already flushed the
    /// border/popup. The image is drawn on top at the correct position.
    ///
    /// Supports:
    /// - **Kitty**: PNG format (`f=100`), `c`/`r` params for cell area scaling
    /// - **iTerm2**: OSC 1337 inline image with cell dimensions
    pub fn write_tmux_direct_image(&mut self) {
        // tmux direct-write only applies when we're running directly in tmux
        // with a local terminal. When socket-attached, the shim handles its
        // own terminal — ratatui-image's widget rendering works correctly.
        if !self.in_tmux || self.is_socket_attached {
            return;
        }

        let proto = self.picker.protocol_type();
        if proto == ProtocolType::Halfblocks || proto == ProtocolType::Sixel {
            return;
        }

        // Capture terminal size before mutable borrow of image_preview.
        let term_size = self.terminal_size();

        let (raw_png, popup_width, popup_height) = match &mut self.image_preview {
            crate::image_preview::PreviewStatus::Ready {
                raw_png,
                width,
                height,
                direct_written,
                ..
            } => {
                if *direct_written {
                    return;
                }
                *direct_written = true;
                (&*raw_png, *width, *height)
            }
            _ => return,
        };

        if raw_png.is_empty() {
            return;
        }

        // Calculate popup position (must match image_overlay::centered_rect).
        let popup_w = popup_width.min(term_size.0);
        let popup_h = popup_height.min(term_size.1);
        let popup_x = (term_size.0.saturating_sub(popup_w)) / 2;
        let popup_y = (term_size.1.saturating_sub(popup_h)) / 2;

        // Inner area (1-cell border).
        let inner_x = popup_x + 1;
        let inner_y = popup_y + 1;
        let inner_w = popup_w.saturating_sub(2);
        let inner_h = popup_h.saturating_sub(2);

        if inner_w == 0 || inner_h == 0 {
            return;
        }

        tracing::debug!(
            ?proto,
            inner_w,
            inner_h,
            inner_x,
            inner_y,
            png_len = raw_png.len(),
            "writing tmux direct image"
        );

        match proto {
            ProtocolType::Kitty => {
                write_kitty_tmux_direct(raw_png, inner_x, inner_y, inner_w, inner_h);
            }
            ProtocolType::Iterm2 => {
                write_iterm2_tmux_direct(raw_png, inner_x, inner_y, inner_w, inner_h);
            }
            _ => {}
        }
    }

    /// Set up connection state, server buffer, and "Connecting..." message.
    /// Returns the server buffer ID. Shared by autoconnect and /connect command.
    pub fn setup_connection(
        &mut self,
        conn_id: &str,
        server_config: &config::ServerConfig,
    ) -> String {
        // Remove placeholder default Status buffer when first real connection starts
        let default_buf_id = make_buffer_id(Self::DEFAULT_CONN_ID, "Status");
        if self.state.buffers.contains_key(&default_buf_id) {
            self.state.remove_buffer(&default_buf_id);
            self.state.connections.remove(Self::DEFAULT_CONN_ID);
        }

        let auto_reconnect = server_config.auto_reconnect.unwrap_or(true);
        let reconnect_delay = server_config.reconnect_delay.unwrap_or(30);

        self.state.add_connection(Connection {
            id: conn_id.to_string(),
            label: server_config.label.clone(),
            status: ConnectionStatus::Connecting,
            nick: server_config
                .nick
                .as_deref()
                .unwrap_or(&self.config.general.nick)
                .to_string(),
            user_modes: String::new(),
            isupport: HashMap::new(),
            isupport_parsed: crate::irc::isupport::Isupport::new(),
            error: None,
            lag: None,
            lag_pending: false,
            reconnect_attempts: 0,
            reconnect_delay_secs: reconnect_delay,
            next_reconnect: None,
            should_reconnect: auto_reconnect,
            joined_channels: server_config.channels.clone(),
            origin_config: server_config.clone(),
            local_ip: None,
            enabled_caps: HashSet::new(),
            who_token_counter: 0,
            silent_who_channels: HashSet::new(),
        });

        let server_buf_id = make_buffer_id(conn_id, &server_config.label);
        self.state.add_buffer(Buffer {
            id: server_buf_id.clone(),
            connection_id: conn_id.to_string(),
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
            last_speakers: Vec::new(),
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
                log_msg_id: None,
                log_ref_id: None,
                tags: std::collections::HashMap::new(),
            },
        );

        server_buf_id
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
        self.setup_connection(&conn_id, &server_config);

        let general = self.config.general.clone();
        match irc::connect_server(&conn_id, &server_config, &general).await {
            Ok((handle, mut rx)) => {
                let tx = self.irc_tx.clone();
                // Store local IP on Connection state (for DCC own-IP fallback)
                if let Some(conn) = self.state.connections.get_mut(&conn_id) {
                    conn.local_ip = handle.local_ip;
                }
                self.irc_handles.insert(conn_id.clone(), handle);

                // Spawn task to forward events from per-connection receiver to shared channel
                let fwd_handle = tokio::spawn(async move {
                    while let Some(event) = rx.recv().await {
                        if tx.send(event).is_err() {
                            break;
                        }
                    }
                });
                self.forwarder_handles.insert(conn_id.clone(), fwd_handle);
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

    /// Animated splash screen: progressively reveals the logo, then holds
    /// for 2.5s. Any keypress dismisses immediately.
    async fn run_splash(&mut self) -> Result<()> {
        const LINE_DELAY_MS: u64 = 50;
        const HOLD_MS: u64 = 2500;

        let Some(terminal) = self.terminal.as_mut() else {
            self.splash_done = true;
            return Ok(());
        };
        let total_lines = include_str!("../logo.txt").lines().count();

        let mut line_tick = interval(Duration::from_millis(LINE_DELAY_MS));

        // Phase 1: progressive reveal.
        while self.splash_visible < total_lines {
            terminal.draw(|frame| ui::splash::render(frame, self.splash_visible))?;

            tokio::select! {
                _ = line_tick.tick() => {
                    self.splash_visible += 1;
                }
                ev = tokio::task::spawn_blocking(|| {
                    if event::poll(std::time::Duration::from_millis(1)).unwrap_or(false) {
                        event::read().ok()
                    } else {
                        None
                    }
                }) => {
                    if let Ok(Some(Event::Key(_))) = ev {
                        self.splash_done = true;
                        return Ok(());
                    }
                }
            }
        }

        // Phase 2: hold fully revealed logo.
        // Re-borrow terminal since `self` was borrowed mutably in the loop.
        let terminal = self.terminal.as_mut().unwrap();
        terminal.draw(|frame| ui::splash::render(frame, total_lines))?;
        let hold_start = Instant::now();
        while hold_start.elapsed() < Duration::from_millis(HOLD_MS) {
            let remaining = Duration::from_millis(HOLD_MS).saturating_sub(hold_start.elapsed());
            if remaining.is_zero() {
                break;
            }
            if let Ok(Some(Event::Key(_))) = tokio::task::spawn_blocking(move || {
                if event::poll(remaining.min(Duration::from_millis(100))).unwrap_or(false) {
                    event::read().ok()
                } else {
                    None
                }
            })
            .await
            {
                break;
            }
        }

        self.splash_done = true;
        Ok(())
    }

    /// Spawn the blocking terminal event reader thread for local terminal mode.
    fn start_term_reader(&mut self) {
        let (term_tx, term_rx) = mpsc::unbounded_channel();
        let stop = Arc::clone(&self.term_reader_stop);
        self.term_reader_stop.store(false, Ordering::Relaxed);
        tokio::task::spawn_blocking(move || {
            while !stop.load(Ordering::Relaxed) {
                if event::poll(std::time::Duration::from_millis(100)).unwrap_or(false) {
                    match event::read() {
                        Ok(ev) => {
                            if term_tx.send(ev).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        });
        self.term_rx = Some(term_rx);
    }

    /// Stop the local terminal reader thread.
    fn stop_term_reader(&mut self) {
        self.term_reader_stop.store(true, Ordering::Relaxed);
        self.term_rx = None;
    }

    /// Get terminal size from cached dimensions.
    ///
    /// Uses stored (cols, rows) updated via shim `Resize` messages or at
    /// startup. Does NOT call `terminal.size()` / `backend.size()` because
    /// that does `ioctl(stdout)` which returns garbage when stdout is
    /// `/dev/null` (after detach or `-d` mode).
    pub const fn terminal_size(&self) -> (u16, u16) {
        (self.cached_term_cols, self.cached_term_rows)
    }

    /// Start the Unix socket listener for shim connections.
    fn start_socket_listener(&mut self) -> Result<()> {
        if self.socket_listener.is_some() {
            return Ok(());
        }
        let dir = crate::constants::sessions_dir();
        std::fs::create_dir_all(&dir)?;
        let path = crate::session::socket_path(std::process::id());
        // Remove stale socket from a previous run.
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path)?;
        tracing::info!("session socket listening at {}", path.display());
        self.socket_listener = Some(listener);
        Ok(())
    }

    /// Clean up own socket file.
    pub fn remove_own_socket() {
        let path = crate::session::socket_path(std::process::id());
        let _ = std::fs::remove_file(&path);
    }

    /// Handle a new shim connection from the socket listener.
    #[expect(
        clippy::too_many_lines,
        reason = "flat init sequence, splitting adds indirection"
    )]
    async fn handle_shim_connect(&mut self, stream: tokio::net::UnixStream) -> Result<()> {
        use crate::session::protocol::{self, MainMessage, ShimMessage};
        use crate::session::writer::SocketWriter;

        // If a shim is already connected, disconnect it first.
        if self.is_socket_attached {
            tracing::info!("new shim connecting, disconnecting existing shim");
        }
        self.disconnect_shim();

        let (read_half, write_half) = tokio::io::split(stream);
        let mut read_half = tokio::io::BufReader::new(read_half);

        // Read the initial TerminalEnv message to get dimensions + env vars.
        let term_env =
            match protocol::read_message::<_, protocol::TerminalEnv>(&mut read_half).await {
                Ok(env) => env,
                Err(e) => {
                    tracing::warn!("failed to read initial shim message: {e}");
                    return Ok(());
                }
            };
        let cols = term_env.cols;
        let rows = term_env.rows;

        // Set up output channel: SocketWriter → mpsc → write_half.
        // Both normal output (from terminal.draw()) and control messages
        // (Detached, Quit) flow through this single typed channel.
        let (output_tx, mut output_rx) = mpsc::unbounded_channel::<MainMessage>();
        let output_handle = tokio::spawn(async move {
            let mut write_half = write_half;
            while let Some(msg) = output_rx.recv().await {
                if protocol::write_message(&mut write_half, &msg)
                    .await
                    .is_err()
                {
                    tracing::warn!("shim output write failed, closing output task");
                    break;
                }
            }
            tracing::debug!("shim output task exiting");
        });

        // Create socket-backed terminal.
        let socket_writer = SocketWriter::new(output_tx.clone());
        let terminal = ui::setup_socket_terminal(Box::new(socket_writer), cols, rows)?;

        // Set up input reader: read ShimMessages from socket → mpsc.
        let (shim_tx, shim_rx) = mpsc::unbounded_channel::<ShimMessage>();
        let input_handle = tokio::spawn(async move {
            let mut reader = read_half;
            loop {
                match protocol::read_message::<_, ShimMessage>(&mut reader).await {
                    Ok(msg) => {
                        if shim_tx.send(msg).is_err() {
                            tracing::debug!("shim input channel closed");
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::debug!("shim input read error: {e}");
                        break;
                    }
                }
            }
            tracing::debug!("shim input reader task exiting");
        });

        self.terminal = Some(terminal);
        self.socket_output_tx = Some(output_tx);
        self.shim_event_rx = Some(shim_rx);
        self.shim_output_handle = Some(output_handle);
        self.shim_input_handle = Some(input_handle);
        self.detached = false;
        self.is_socket_attached = true;
        self.needs_full_redraw = true;
        self.cached_term_cols = cols;
        self.cached_term_rows = rows;
        // Reset sidepanel scroll — stale offsets cause click position mismatch
        // because the renderer clamps but click handlers used the raw value.
        self.buffer_list_scroll = 0;
        self.nick_list_scroll = 0;

        // Store shim's terminal env for protocol detection.
        self.shim_term_env = Some(term_env.env_vars);

        // Update picker font_size — reattaching shim may have different cell
        // pixel dimensions than the terminal we started with.
        if let Some(font_size) = term_env.font_size {
            tracing::info!(
                old_font = ?self.picker.font_size(),
                new_font = ?font_size,
                "updating picker font_size from shim terminal"
            );
            #[expect(deprecated, reason = "only API to set font dimensions")]
            let mut new_picker = ratatui_image::picker::Picker::from_fontsize(font_size);
            new_picker.set_protocol_type(self.picker.protocol_type());
            self.picker = new_picker;
        }

        // Re-detect image protocol using the shim's terminal env.
        self.refresh_image_protocol();

        // Add system message to the active buffer.
        let buf_id = self.state.active_buffer_id.clone().unwrap_or_default();
        let id = self.state.next_message_id();
        self.state.add_message(
            &buf_id,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: "Terminal attached".to_string(),
                highlight: false,
                event_key: None,
                event_params: None,
                log_msg_id: None,
                log_ref_id: None,
                tags: std::collections::HashMap::new(),
            },
        );

        tracing::info!(cols, rows, "shim attached");
        Ok(())
    }

    /// Send a control `MainMessage` through the shim output channel.
    fn send_shim_control(&self, msg: crate::session::protocol::MainMessage) {
        if let Some(ref tx) = self.socket_output_tx {
            let _ = tx.send(msg);
        }
    }

    /// Tear down the shim connection (terminal, tasks, channels).
    fn teardown_shim(&mut self) {
        self.terminal = None;
        // Drop the sender — the output task will drain remaining messages
        // (including the Detached control message we just sent) then exit
        // when the channel closes.
        self.socket_output_tx = None;
        self.shim_event_rx = None;
        self.is_socket_attached = false;
        self.shim_term_env = None;
        // Detach the output task handle (sender is already dropped above,
        // so the task exits naturally after draining queued messages).
        self.shim_output_handle.take();
        if let Some(h) = self.shim_input_handle.take() {
            h.abort();
        }
    }

    /// Disconnect the current shim (if any).
    fn disconnect_shim(&mut self) {
        self.send_shim_control(crate::session::protocol::MainMessage::Detached);
        self.teardown_shim();
    }

    /// Perform detach: save state, drop terminal, start socket listener.
    fn perform_detach(&mut self) {
        self.should_detach = false;

        if self.is_socket_attached {
            // Send Detached to shim — it will exit, returning the shell prompt.
            self.send_shim_control(crate::session::protocol::MainMessage::Detached);
            self.teardown_shim();
        }

        self.detached = true;

        tracing::info!(pid = std::process::id(), "detached");
    }

    /// Send Quit to the connected shim before shutdown.
    fn notify_shim_quit(&self) {
        self.send_shim_control(crate::session::protocol::MainMessage::Quit);
    }

    #[allow(clippy::too_many_lines)]
    pub async fn run(&mut self) -> Result<()> {
        // Clean up stale sockets from dead PIDs.
        crate::session::cleanup_stale_sockets();

        // --- Splash screen ---
        // Show progressive logo reveal on the local terminal.
        // Skipped when detached (-d mode) or socket-attached (reattach).
        if self.terminal.is_some() && !self.is_socket_attached {
            self.run_splash().await?;
        }

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

        // Autoload scripts from ~/.repartee/scripts/
        self.autoload_scripts();

        // Spawn terminal event reader thread (only if we have a local terminal).
        if self.terminal.is_some() && !self.is_socket_attached {
            self.start_term_reader();
        }

        // Start socket listener so we can detach later.
        if let Err(e) = self.start_socket_listener() {
            tracing::warn!("failed to start session socket: {e}");
        }

        // Start web server if enabled and password is set.
        if self.config.web.enabled && !self.config.web.password.is_empty() {
            let sessions = std::sync::Arc::new(tokio::sync::Mutex::new(
                crate::web::auth::SessionStore::new(),
            ));
            let limiter = std::sync::Arc::new(tokio::sync::Mutex::new(
                crate::web::auth::RateLimiter::new(),
            ));
            self.web_sessions = Some(std::sync::Arc::clone(&sessions));
            self.web_rate_limiter = Some(std::sync::Arc::clone(&limiter));

            // Build initial state snapshot for web handlers.
            let snapshot = std::sync::Arc::new(std::sync::RwLock::new(
                crate::web::server::WebStateSnapshot {
                    buffers: Vec::new(),
                    connections: Vec::new(),
                    mention_count: 0,
                },
            ));
            self.web_state_snapshot = Some(std::sync::Arc::clone(&snapshot));

            let handle = std::sync::Arc::new(crate::web::server::AppHandle {
                broadcaster: std::sync::Arc::clone(&self.web_broadcaster),
                web_cmd_tx: self.web_cmd_tx.clone(),
                password: self.config.web.password.clone(),
                session_store: sessions,
                rate_limiter: limiter,
                web_state_snapshot: Some(snapshot),
                db: self.storage.as_ref().map(|s| std::sync::Arc::clone(&s.db)),
                db_encrypt: self.storage.as_ref().is_some_and(|s| s.encrypt),
            });
            match crate::web::server::start(&self.config.web, handle).await {
                Ok(h) => {
                    self.web_server_handle = Some(h);
                    tracing::info!(
                        "web frontend at https://{}:{}",
                        self.config.web.bind_address,
                        self.config.web.port
                    );
                }
                Err(e) => {
                    tracing::error!("failed to start web server: {e}");
                }
            }
        } else if self.config.web.enabled && self.config.web.password.is_empty() {
            tracing::warn!("web.enabled=true but web.password is empty — set WEB_PASSWORD in .env");
        }

        // Signal handlers for detached mode.
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;

        let mut tick = interval(Duration::from_secs(1));
        let mut paste_tick = interval(Duration::from_millis(500));

        while !self.should_quit {
            // Handle pending detach request.
            if self.should_detach {
                self.perform_detach();
            }

            // Draw only when we have a terminal.
            // We take() the terminal to avoid borrow conflicts with `self` in the draw closure.
            if let Some(mut terminal) = self.terminal.take() {
                if self.needs_full_redraw {
                    let _ = terminal.clear(); // clear can also fail — ignore
                    self.needs_full_redraw = false;
                }
                match terminal.draw(|frame| ui::layout::draw(frame, self)) {
                    Ok(_) => {
                        self.terminal = Some(terminal);
                    }
                    Err(e) => {
                        // Terminal write failed (likely SIGHUP race — terminal already gone).
                        // Don't put terminal back — it's broken.
                        tracing::warn!("terminal draw failed, triggering detach: {e}");
                        self.should_detach = true;
                    }
                }
            }

            // tmux: write image directly after ratatui frame flush.
            // Only if the draw succeeded (terminal is still valid).
            if self.terminal.is_some() {
                self.write_tmux_direct_image();
            }

            tokio::select! {
                // Local terminal events.
                ev = async {
                    match self.term_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => match ev {
                    Some(ev) => {
                        self.handle_event(ev);
                        if let Some(mut rx) = self.term_rx.take() {
                            while let Ok(ev) = rx.try_recv() {
                                self.handle_event(ev);
                            }
                            self.term_rx = Some(rx);
                        }
                        self.update_script_snapshot();
                    }
                    None => {
                        // Terminal reader died — if we have a terminal, it's dead.
                        self.term_rx = None;
                    }
                },
                // Shim events (socket-attached mode).
                shim_ev = async {
                    match self.shim_event_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => match shim_ev {
                    Some(crate::session::protocol::ShimMessage::TermEvent(ev)) => {
                        self.handle_event(ev);
                        if let Some(mut rx) = self.shim_event_rx.take() {
                            while let Ok(msg) = rx.try_recv() {
                                if let crate::session::protocol::ShimMessage::TermEvent(ev) = msg {
                                    self.handle_event(ev);
                                }
                            }
                            self.shim_event_rx = Some(rx);
                        }
                        self.update_script_snapshot();
                    }
                    Some(crate::session::protocol::ShimMessage::Resize { cols, rows }) => {
                        self.cached_term_cols = cols;
                        self.cached_term_rows = rows;
                        if let Some(ref mut terminal) = self.terminal {
                            let _ = terminal.resize(ratatui::layout::Rect::new(0, 0, cols, rows));
                            self.needs_full_redraw = true;
                        }
                    }
                    Some(crate::session::protocol::ShimMessage::Detach) => {
                        self.should_detach = true;
                    }
                    None => {
                        // Shim disconnected — go back to detached mode.
                        tracing::info!("shim disconnected, returning to detached mode");
                        self.terminal = None;
                        self.socket_output_tx = None;
                        self.shim_event_rx = None;
                        self.is_socket_attached = false;
                        self.shim_term_env = None;
                        if let Some(h) = self.shim_output_handle.take() { h.abort(); }
                        if let Some(h) = self.shim_input_handle.take() { h.abort(); }
                        self.detached = true;
                    }
                },
                // Socket listener: accept new shim connections.
                stream = async {
                    match self.socket_listener.as_ref() {
                        Some(listener) => {
                            match listener.accept().await {
                                Ok((stream, _)) => Some(stream),
                                Err(e) => {
                                    tracing::warn!("socket accept error: {e}");
                                    None
                                }
                            }
                        }
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(stream) = stream
                        && let Err(e) = self.handle_shim_connect(stream).await
                    {
                        tracing::warn!("failed to handle shim connection: {e}");
                    }
                },
                irc_ev = self.irc_rx.recv() => {
                    if let Some(event) = irc_ev {
                        self.handle_irc_event(event);
                        self.update_script_snapshot();
                    }
                },
                preview_ev = self.preview_rx.recv() => {
                    if let Some(ev) = preview_ev {
                        self.handle_preview_event(ev);
                    }
                },
                dcc_ev = self.dcc_rx.recv() => {
                    if let Some(ev) = dcc_ev {
                        self.handle_dcc_event(ev);
                    }
                },
                dict_ev = self.dict_rx.recv() => {
                    if let Some(ev) = dict_ev {
                        self.handle_dict_event(ev);
                    }
                },
                web_cmd = self.web_cmd_rx.recv() => {
                    if let Some((cmd, _session_id)) = web_cmd {
                        self.handle_web_command(cmd);
                    }
                },
                _ = tick.tick() => {
                    self.handle_netsplit_tick();
                    self.purge_expired_batches();
                    self.check_reconnects();
                    self.measure_lag();
                    self.update_script_snapshot();
                    self.check_stale_who_batches();
                    // Purge expired web sessions and rate limiter entries.
                    if let Some(ref sessions) = self.web_sessions
                        && let Ok(mut s) = sessions.try_lock() { s.purge_expired(); }
                    if let Some(ref limiter) = self.web_rate_limiter
                        && let Ok(mut l) = limiter.try_lock() { l.purge_expired(); }
                    // Update web state snapshot.
                    if let Some(ref snapshot) = self.web_state_snapshot
                        && let Ok(mut snap) = snapshot.write()
                    {
                        let init = crate::web::snapshot::build_sync_init(&self.state, 0);
                        if let crate::web::protocol::WebEvent::SyncInit { buffers, connections, mention_count } = init {
                            snap.buffers = buffers;
                            snap.connections = connections;
                            snap.mention_count = mention_count;
                        }
                    }
                    let expired = self.dcc.purge_expired();
                    for (_id, nick) in expired {
                        crate::commands::helpers::add_local_event(
                            self,
                            &format!("DCC CHAT request from {nick} timed out"),
                        );
                    }
                },
                _ = paste_tick.tick() => {
                    self.drain_paste_queue();
                },
                action = self.script_action_rx.recv() => {
                    if let Some(action) = action {
                        self.handle_script_action(action);
                        while let Ok(action) = self.script_action_rx.try_recv() {
                            self.handle_script_action(action);
                        }
                        self.update_script_snapshot();
                    }
                },
                _ = sigterm.recv() => {
                    self.should_quit = true;
                },
                _ = sigint.recv() => {
                    // In detached mode, SIGINT should quit. With terminal, ignore
                    // (Ctrl+C is handled by the terminal reader).
                    if self.detached {
                        self.should_quit = true;
                    }
                },
                _ = sighup.recv() => {
                    // Terminal closed externally — auto-detach instead of dying.
                    if !self.detached {
                        tracing::info!("SIGHUP received, auto-detaching");
                        self.should_detach = true;
                    }
                },
            }
        }

        // Abort all outstanding script timer tasks so they don't send on a dropped channel.
        for (_, handle) in self.active_timers.drain() {
            handle.abort();
        }

        // Abort forwarder tasks so they don't spin on a dropped receiver.
        for (_, handle) in self.forwarder_handles.drain() {
            handle.abort();
        }

        // Notify shim of quit and stop terminal reader.
        self.notify_shim_quit();
        self.stop_term_reader();

        // Send QUIT to all connected servers (once — cmd_quit defers to here)
        let default_quit = crate::constants::default_quit_message();
        let quit_msg = self.quit_message.as_deref().unwrap_or(&default_quit);
        for handle in self.irc_handles.values() {
            let _ = handle.sender.send_quit(quit_msg);
        }

        // Shut down storage writer (flushes remaining rows)
        if let Some(storage) = self.storage.take() {
            storage.shutdown().await;
        }

        // Clean up socket file.
        Self::remove_own_socket();

        Ok(())
    }

    /// Connection ID for the app-level default Status buffer.
    pub const DEFAULT_CONN_ID: &'static str = "_default";

    fn create_default_status(state: &mut AppState) {
        let buf_id = make_buffer_id(Self::DEFAULT_CONN_ID, "Status");
        state.add_connection(Connection {
            id: Self::DEFAULT_CONN_ID.to_string(),
            label: "Status".to_string(),
            status: ConnectionStatus::Disconnected,
            nick: String::new(),
            user_modes: String::new(),
            isupport: HashMap::new(),
            isupport_parsed: crate::irc::isupport::Isupport::new(),
            error: None,
            lag: None,
            lag_pending: false,
            reconnect_attempts: 0,
            reconnect_delay_secs: 0,
            next_reconnect: None,
            should_reconnect: false,
            joined_channels: Vec::new(),
            origin_config: config::ServerConfig {
                label: String::new(),
                address: String::new(),
                port: 0,
                tls: false,
                tls_verify: true,
                autoconnect: false,
                channels: vec![],
                nick: None,
                username: None,
                realname: None,
                password: None,
                sasl_user: None,
                sasl_pass: None,
                bind_ip: None,
                encoding: None,
                auto_reconnect: Some(false),
                reconnect_delay: None,
                reconnect_max_retries: None,
                autosendcmd: None,
                sasl_mechanism: None,
                client_cert_path: None,
            },
            local_ip: None,
            enabled_caps: HashSet::new(),
            who_token_counter: 0,
            silent_who_channels: HashSet::new(),
        });
        state.add_buffer(Buffer {
            id: buf_id.clone(),
            connection_id: Self::DEFAULT_CONN_ID.to_string(),
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
            last_speakers: Vec::new(),
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
                text: format!(
                    "Welcome to {}! Use /connect <server> to connect.",
                    crate::constants::APP_NAME
                ),
                highlight: false,
                event_key: None,
                event_params: None,
                log_msg_id: None,
                log_ref_id: None,
                tags: std::collections::HashMap::new(),
            },
        );
    }

    /// Recreate the default Status buffer if no real buffers remain.
    pub fn ensure_default_status(&mut self) {
        // Check if any non-default buffers exist
        let has_real_buffers = self
            .state
            .buffers
            .values()
            .any(|b| b.connection_id != Self::DEFAULT_CONN_ID);
        if !has_real_buffers {
            Self::create_default_status(&mut self.state);
        }
    }

    /// Handle a completed image preview event from a background task.
    fn handle_preview_event(&mut self, event: crate::image_preview::ImagePreviewEvent) {
        use crate::image_preview::{ImagePreviewEvent, PreviewStatus};
        self.image_preview = match event {
            ImagePreviewEvent::Ready {
                url,
                title,
                image,
                raw_png,
                width,
                height,
            } => PreviewStatus::Ready {
                url,
                title,
                image,
                raw_png,
                width,
                height,
                direct_written: false,
            },
            ImagePreviewEvent::Error { url, message } => PreviewStatus::Error { url, message },
        };
    }

    // ── Backlog loading ────────────────────────────────────────────────────

    /// Load recent chat history from the log database into a newly created buffer.
    ///
    /// Mimics `WeeChat`'s backlog feature: when a channel/query buffer is opened,
    /// the last N messages from persistent storage are prepended so the user
    /// sees recent context immediately. A separator line marks the end of
    /// the backlog.
    ///
    /// Backlog messages are tagged `no_highlight` to avoid spurious notifications
    /// and are not re-logged (they already exist in the database).
    fn load_backlog(&mut self, buffer_id: &str) {
        let limit = self.config.display.backlog_lines;
        if limit == 0 {
            return;
        }

        let Some(storage) = self.storage.as_ref() else {
            return;
        };

        let Some(buf) = self.state.buffers.get(buffer_id) else {
            return;
        };

        // Only chat buffers get backlog — skip server/special
        if !matches!(
            buf.buffer_type,
            BufferType::Channel | BufferType::Query | BufferType::DccChat
        ) {
            return;
        }

        // The DB stores connection.label as the network name (not conn_id),
        // matching how maybe_log() writes rows.
        let network = self
            .state
            .connections
            .get(&buf.connection_id)
            .map_or_else(|| buf.connection_id.clone(), |c| c.label.clone());
        let buf_name = buf.name.clone();

        // Scope the mutex lock tightly — WAL mode means reads don't block
        // the writer task, but we still want to release ASAP.
        let messages = {
            let Ok(db) = storage.db.lock() else {
                return;
            };
            crate::storage::query::get_messages(
                &db,
                &network,
                &buf_name,
                None,
                limit,
                storage.encrypt,
                None,
            )
        };

        let Ok(messages) = messages else {
            return;
        };

        if messages.is_empty() {
            return;
        }

        let count = messages.len();

        // Convert StoredMessage → Message and prepend to the buffer
        for stored in &messages {
            let msg_type = match stored.msg_type.as_str() {
                "action" => MessageType::Action,
                "notice" => MessageType::Notice,
                "event" => MessageType::Event,
                _ => MessageType::Message,
            };

            let id = self.state.next_message_id();
            let ts = chrono::DateTime::from_timestamp(stored.timestamp, 0).unwrap_or_else(Utc::now);

            // Insert directly into messages vec — no activity, no logging, no highlight
            if let Some(buf) = self.state.buffers.get_mut(buffer_id) {
                buf.messages.push(Message {
                    id,
                    timestamp: ts,
                    message_type: msg_type,
                    nick: stored.nick.clone(),
                    nick_mode: None,
                    text: stored.text.clone(),
                    highlight: false,
                    event_key: None,
                    event_params: None,
                    log_msg_id: None,
                    log_ref_id: None,
                    tags: std::collections::HashMap::new(),
                });
            }
        }

        // Add separator after backlog
        let sep_id = self.state.next_message_id();
        if let Some(buf) = self.state.buffers.get_mut(buffer_id) {
            buf.messages.push(Message {
                id: sep_id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text: format!("─── End of backlog ({count} lines) ───"),
                highlight: false,
                event_key: Some("backlog_end".to_string()),
                event_params: None,
                log_msg_id: None,
                log_ref_id: None,
                tags: std::collections::HashMap::new(),
            });
        }
    }

    // ── Dictionary download event handling ─────────────────────────────────

    fn handle_dict_event(&mut self, ev: crate::spellcheck::DictEvent) {
        use crate::commands::types::{C_CMD, C_DIM, C_ERR, C_OK, C_RST, divider};
        use crate::spellcheck::DictEvent;
        let ev_fn = crate::commands::helpers::add_local_event;
        match ev {
            DictEvent::ListResult { entries } => {
                ev_fn(self, &divider("Available Dictionaries"));
                for entry in &entries {
                    let status = if entry.installed {
                        format!(" {C_OK}[installed]{C_RST}")
                    } else {
                        String::new()
                    };
                    ev_fn(
                        self,
                        &format!(
                            "  {C_CMD}{:<8}{C_RST} {}{status}",
                            entry.code, entry.name
                        ),
                    );
                }
                ev_fn(
                    self,
                    &format!("  {C_DIM}Use /spellcheck get <lang> to download{C_RST}"),
                );
            }
            DictEvent::Downloaded { lang } => {
                ev_fn(
                    self,
                    &format!("{C_OK}Dictionary {lang} downloaded successfully{C_RST}"),
                );
                self.reload_spellchecker();
                let loaded = self
                    .spellchecker
                    .as_ref()
                    .map_or(0, crate::spellcheck::SpellChecker::dict_count);
                ev_fn(
                    self,
                    &format!("{C_OK}Spell checker reloaded ({loaded} dictionaries){C_RST}"),
                );
            }
            DictEvent::Error { message } => {
                ev_fn(self, &format!("{C_ERR}{message}{C_RST}"));
            }
        }
    }

    // ── Web command handling ──────────────────────────────────────────────────

    /// Broadcast a `WebEvent` to all connected web clients.
    fn broadcast_web(&self, event: crate::web::protocol::WebEvent) {
        let _ = self.web_broadcaster.send(event);
    }

    /// Dispatch a command received from a web client.
    fn handle_web_command(&mut self, cmd: crate::web::protocol::WebCommand) {
        use crate::web::protocol::WebCommand;
        use crate::web::snapshot;

        match cmd {
            WebCommand::SendMessage { buffer_id, text } => {
                self.web_send_message(&buffer_id, &text);
            }
            WebCommand::SwitchBuffer { .. } => {
                // Session-local on client side — no server state change.
            }
            WebCommand::MarkRead { buffer_id, up_to } => {
                self.web_mark_read(&buffer_id, up_to);
            }
            WebCommand::FetchMessages {
                buffer_id,
                limit,
                before,
            } => {
                self.web_fetch_messages(&buffer_id, limit, before);
            }
            WebCommand::FetchNickList { buffer_id } => {
                if let Some(event) = snapshot::build_nick_list(&self.state, &buffer_id) {
                    self.broadcast_web(event);
                }
            }
            WebCommand::FetchMentions => {
                self.web_fetch_mentions();
            }
            WebCommand::RunCommand { buffer_id, text } => {
                self.web_run_command(&buffer_id, &text);
            }
        }
    }

    /// Send a message from a web client to IRC.
    fn web_send_message(&mut self, buffer_id: &str, text: &str) {
        // Reuse the same path as terminal input — handles commands,
        // PRIVMSG, echo, logging, and script events.
        self.web_run_command(buffer_id, text);
    }

    /// Mark a buffer as read from a web client.
    fn web_mark_read(&mut self, buffer_id: &str, _up_to: i64) {
        if let Some(buf) = self.state.buffers.get_mut(buffer_id) {
            buf.unread_count = 0;
            buf.activity = crate::state::buffer::ActivityLevel::None;
        }
        self.broadcast_web(crate::web::protocol::WebEvent::ActivityChanged {
            buffer_id: buffer_id.to_string(),
            activity: 0,
            unread_count: 0,
        });
    }

    /// Fetch messages from `SQLite` for a web client.
    fn web_fetch_messages(&self, buffer_id: &str, limit: u32, before: Option<i64>) {
        let Some(ref storage) = self.storage else {
            return;
        };
        let Ok(db) = storage.db.lock() else {
            return;
        };
        let (network, buffer) = crate::web::snapshot::split_buffer_id(buffer_id);
        let messages = crate::storage::query::get_messages(
            &db,
            network,
            buffer,
            before,
            limit as usize,
            storage.encrypt,
            None, // TODO: pass crypto key for encrypted DBs
        );
        if let Ok(msgs) = messages {
            let has_more = msgs.len() == limit as usize;
            let wire: Vec<_> = msgs
                .iter()
                .map(crate::web::snapshot::stored_to_wire)
                .collect();
            self.broadcast_web(crate::web::protocol::WebEvent::Messages {
                buffer_id: buffer_id.to_string(),
                messages: wire,
                has_more,
            });
        }
    }

    /// Fetch unread mentions for a web client.
    fn web_fetch_mentions(&self) {
        let Some(ref storage) = self.storage else {
            return;
        };
        let Ok(db) = storage.db.lock() else {
            return;
        };
        if let Ok(mentions) = crate::storage::query::get_unread_mentions(&db) {
            let wire: Vec<_> = mentions
                .iter()
                .map(|m| crate::web::protocol::WireMention {
                    id: m.id,
                    timestamp: m.timestamp,
                    buffer_id: format!("{}/{}", m.network, m.buffer),
                    channel: m.channel.clone(),
                    nick: m.nick.clone(),
                    text: m.text.clone(),
                })
                .collect();
            self.broadcast_web(crate::web::protocol::WebEvent::MentionsList { mentions: wire });
        }
    }

    /// Execute a command from a web client in the context of a buffer.
    fn web_run_command(&mut self, buffer_id: &str, text: &str) {
        // Temporarily set active buffer to the web client's buffer context.
        let saved = self.state.active_buffer_id.clone();
        self.state.active_buffer_id = Some(buffer_id.to_string());

        // Reuse the same input path as the terminal.
        self.handle_submit(text);

        // Restore active buffer.
        self.state.active_buffer_id = saved;
    }

    // ── DCC event handling ───────────────────────────────────────────────────

    #[allow(clippy::too_many_lines)]
    fn handle_dcc_event(&mut self, ev: crate::dcc::DccEvent) {
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
                        messages: Vec::new(),
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
                        tags: std::collections::HashMap::new(),
                    },
                );

                // Inform scripts after the buffer is ready so they can print
                // to it immediately if desired.
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

                // Scripts can suppress message display (e.g., for filtering or
                // routing DCC chat lines to a custom buffer).
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
                            tags: std::collections::HashMap::new(),
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
                        tags: std::collections::HashMap::new(),
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

                    // Notify scripts before the "closed" message so they can
                    // react while the nick/conn context is still meaningful.
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
                            tags: std::collections::HashMap::new(),
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

    /// Tick the netsplit state and emit batched netsplit/netjoin messages.
    fn handle_netsplit_tick(&mut self) {
        let messages = self.state.netsplit_state.tick();
        for msg in messages {
            for buffer_id in &msg.buffer_ids {
                let id = self.state.next_message_id();
                self.state.add_message(
                    buffer_id,
                    Message {
                        id,
                        timestamp: Utc::now(),
                        message_type: MessageType::Event,
                        nick: None,
                        nick_mode: None,
                        text: msg.text.clone(),
                        highlight: false,
                        event_key: Some("netsplit".to_string()),
                        event_params: None,
                        log_msg_id: None,
                        log_ref_id: None,
                        tags: std::collections::HashMap::new(),
                    },
                );
            }
        }
    }

    /// Discard any batches that have been open too long (e.g. dropped `-BATCH`).
    fn purge_expired_batches(&mut self) {
        for tracker in self.batch_trackers.values_mut() {
            tracker.purge_expired();
        }
    }

    /// Add an event message to the specified buffer.
    fn add_event_to_buffer(&mut self, buffer_id: &str, text: String) {
        let id = self.state.next_message_id();
        self.state.add_local_message(
            buffer_id,
            Message {
                id,
                timestamp: Utc::now(),
                message_type: MessageType::Event,
                nick: None,
                nick_mode: None,
                text,
                highlight: false,
                event_key: None,
                event_params: None,
                log_msg_id: None,
                log_ref_id: None,
                tags: std::collections::HashMap::new(),
            },
        );
    }

    /// Check connections that need reconnecting and spawn reconnect tasks.
    fn check_reconnects(&mut self) {
        let now = std::time::Instant::now();

        // Collect connections that need reconnecting
        let to_reconnect: Vec<String> = self
            .state
            .connections
            .iter()
            .filter(|(id, conn)| {
                matches!(
                    conn.status,
                    ConnectionStatus::Disconnected | ConnectionStatus::Error
                ) && conn.should_reconnect
                    && conn.next_reconnect.is_some_and(|t| t <= now)
                    && *id != Self::DEFAULT_CONN_ID
                    && !self.irc_handles.contains_key(id.as_str())
            })
            .map(|(id, _)| id.clone())
            .collect();

        for conn_id in to_reconnect {
            let Some(conn) = self.state.connections.get_mut(&conn_id) else {
                continue;
            };

            conn.reconnect_attempts += 1;
            let attempts = conn.reconnect_attempts;
            conn.next_reconnect = None;

            let conn = self.state.connections.get(&conn_id);
            let label = conn.map_or_else(|| conn_id.clone(), |c| c.label.clone());
            let server_config = conn.map(|c| c.origin_config.clone());

            let buffer_id = make_buffer_id(&conn_id, &label);
            self.add_event_to_buffer(
                &buffer_id,
                format!("Reconnecting to {label} (attempt {attempts})..."),
            );

            if let Some(conn) = self.state.connections.get_mut(&conn_id) {
                conn.status = ConnectionStatus::Connecting;
            }

            self.spawn_reconnect(&conn_id, server_config, &buffer_id, &label);
        }
    }

    /// Spawn a reconnect task or log failure if no config is available.
    fn spawn_reconnect(
        &mut self,
        conn_id: &str,
        server_config: Option<config::ServerConfig>,
        buffer_id: &str,
        label: &str,
    ) {
        if let Some(cfg) = server_config {
            let general = self.config.general.clone();
            let tx = self.irc_tx.clone();
            let id = conn_id.to_string();
            tokio::spawn(async move {
                match crate::irc::connect_server(&id, &cfg, &general).await {
                    Ok((handle, mut rx)) => {
                        let _ = tx.send(IrcEvent::HandleReady(
                            handle.conn_id.clone(),
                            handle.sender,
                            handle.local_ip,
                        ));
                        while let Some(event) = rx.recv().await {
                            if tx.send(event).is_err() {
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(IrcEvent::Disconnected(id, Some(e.to_string())));
                    }
                }
            });
        } else {
            if let Some(conn) = self.state.connections.get_mut(conn_id) {
                conn.should_reconnect = false;
                conn.status = ConnectionStatus::Disconnected;
            }
            self.add_event_to_buffer(
                buffer_id,
                format!("Cannot reconnect to {label}: server config not found"),
            );
        }
    }

    /// Queue a channel for batched auto-WHO + auto-MODE after joining.
    ///
    /// If no batch is currently in-flight for this connection, starts one
    /// immediately. Otherwise the channel is queued for the next batch.
    fn queue_channel_query(&mut self, conn_id: &str, channel: String) {
        tracing::trace!(conn_id, %channel, "queue_channel_query");
        self.channel_query_queues
            .entry(conn_id.to_string())
            .or_default()
            .push_back(channel);

        if !self.channel_query_in_flight.contains_key(conn_id) {
            self.send_channel_query_batch(conn_id);
        }
    }

    /// Send the next batch of WHO + MODE queries for a connection.
    ///
    /// Builds comma-separated channel lists within the 512-byte IRC line
    /// limit, sends one batched WHO (WHOX if supported) and one batched
    /// MODE query. Waits for `RPL_ENDOFWHO` before sending the next batch.
    fn send_channel_query_batch(&mut self, conn_id: &str) {
        /// Max channels per WHO command. `IRCnet` ircd 2.12 silently drops
        /// targets beyond ~11 in comma-separated WHO. Use 5 for safety.
        const MAX_WHO_TARGETS: usize = 5;

        let queue = match self.channel_query_queues.get_mut(conn_id) {
            Some(q) if !q.is_empty() => q,
            _ => {
                self.channel_query_in_flight.remove(conn_id);
                self.channel_query_sent_at.remove(conn_id);
                return;
            }
        };

        let has_whox = self
            .state
            .connections
            .get(conn_id)
            .is_some_and(|c| c.isupport_parsed.has_whox());

        // WHO overhead: "WHO " (4) + " %tcuihnfar,NNN" (~16 for WHOX) + "\r\n" (2)
        let who_overhead = if has_whox { 22 } else { 6 };
        let who_budget = 512 - who_overhead;

        // MODE overhead: "MODE " (5) + "\r\n" (2)
        let mode_budget = 512 - 7;

        // Use the smaller budget so both commands fit their channels.
        let budget = who_budget.min(mode_budget);

        let mut batch = Vec::new();
        let mut len = 0;

        while let Some(ch) = queue.front() {
            if batch.len() >= MAX_WHO_TARGETS {
                break;
            }
            let add = if batch.is_empty() {
                ch.len()
            } else {
                1 + ch.len() // comma + channel name
            };
            if len + add > budget && !batch.is_empty() {
                break;
            }
            len += add;
            batch.push(queue.pop_front().expect("front() was Some"));
        }

        if batch.is_empty() {
            self.channel_query_in_flight.remove(conn_id);
            return;
        }

        let Some(handle) = self.irc_handles.get(conn_id) else {
            self.channel_query_in_flight.remove(conn_id);
            return;
        };

        // Track in-flight channels for RPL_ENDOFWHO completion.
        let batch_set: HashSet<String> = batch.iter().cloned().collect();
        self.channel_query_in_flight
            .insert(conn_id.to_string(), batch_set);
        self.channel_query_sent_at
            .insert(conn_id.to_string(), Instant::now());

        // Mark all batch channels as silent (no display for auto-WHO replies).
        if let Some(conn) = self.state.connections.get_mut(conn_id) {
            for ch in &batch {
                conn.silent_who_channels.insert(ch.clone());
            }
        }

        // Send batched WHO (single command, comma-separated channels).
        let chanlist = batch.join(",");
        tracing::trace!(conn_id, %chanlist, has_whox, "send_channel_query_batch: sending WHO+MODE");
        if has_whox {
            let token = crate::irc::events::next_who_token(&mut self.state, conn_id);
            let fields = format!("{},{token}", crate::constants::WHOX_FIELDS);
            tracing::trace!(conn_id, %chanlist, %fields, "WHOX command");
            let _ = handle.sender.send(::irc::proto::Command::Raw(
                "WHO".to_string(),
                vec![chanlist.clone(), fields],
            ));
        } else {
            tracing::trace!(conn_id, %chanlist, "standard WHO (no WHOX)");
            let _ = handle
                .sender
                .send(::irc::proto::Command::WHO(Some(chanlist.clone()), None));
        }

        // Send batched MODE (single command, comma-separated channels).
        let _ = handle.sender.send(::irc::proto::Command::Raw(
            "MODE".to_string(),
            vec![chanlist],
        ));
    }

    /// Handle `RPL_ENDOFWHO` for batch tracking.
    ///
    /// Removes completed channels from the in-flight set. Handles both
    /// individual per-channel responses and a single response with the
    /// comma-separated batch target. Starts the next batch when complete.
    fn handle_who_batch_complete(&mut self, conn_id: &str, target: &str) {
        tracing::trace!(conn_id, %target, "handle_who_batch_complete");
        let Some(in_flight) = self.channel_query_in_flight.get_mut(conn_id) else {
            tracing::trace!(conn_id, "no in-flight batch for this connection");
            return;
        };

        // Try removing as individual channel.
        in_flight.remove(target);

        // Also split comma-separated (server may send one EOW for the batch).
        if target.contains(',') {
            for ch in target.split(',') {
                in_flight.remove(ch);
            }
        }

        tracing::trace!(
            conn_id,
            remaining = in_flight.len(),
            "in-flight after removal"
        );

        if in_flight.is_empty() {
            let remaining_queued = self
                .channel_query_queues
                .get(conn_id)
                .map_or(0, VecDeque::len);
            tracing::trace!(conn_id, remaining_queued, "batch complete, sending next");
            let conn_id = conn_id.to_string();
            self.channel_query_in_flight.remove(&conn_id);
            self.send_channel_query_batch(&conn_id);
        }
    }

    /// Detect stale WHO batches where the server silently dropped some targets.
    /// If a batch has been in-flight for >30s, clear the `silent_who_channels` for
    /// the stuck channels, discard the in-flight set, and send the next batch.
    fn check_stale_who_batches(&mut self) {
        let stale_conns: Vec<String> = self
            .channel_query_sent_at
            .iter()
            .filter(|(_, sent_at)| sent_at.elapsed() > Duration::from_secs(30))
            .map(|(conn_id, _)| conn_id.clone())
            .collect();

        for conn_id in stale_conns {
            if let Some(stale) = self.channel_query_in_flight.remove(&conn_id) {
                tracing::warn!(
                    %conn_id,
                    stale_channels = ?stale,
                    "WHO batch timed out — server likely dropped targets, moving on"
                );
                // Clean up silent_who_channels for the stuck channels.
                if let Some(conn) = self.state.connections.get_mut(&conn_id) {
                    for ch in &stale {
                        conn.silent_who_channels.remove(ch.as_str());
                    }
                }
            }
            self.channel_query_sent_at.remove(&conn_id);
            self.send_channel_query_batch(&conn_id);
        }
    }

    /// Send IRC PING every 30s per connection to measure lag.
    ///
    /// Uses the current timestamp (ms since UNIX epoch) as the PING token.
    /// When the server responds with PONG containing the same token, we
    /// compute the round-trip time in `handle_irc_event`.
    fn measure_lag(&mut self) {
        let now = Instant::now();
        let conn_ids: Vec<String> = self.irc_handles.keys().cloned().collect();
        for conn_id in conn_ids {
            let is_connected = self
                .state
                .connections
                .get(&conn_id)
                .is_some_and(|c| c.status == ConnectionStatus::Connected);
            if !is_connected {
                continue;
            }

            // Check for lag timeout (no PONG for 5 minutes)
            if let Some(sent_at) = self.lag_pings.get(&conn_id) {
                let pending = self
                    .state
                    .connections
                    .get(&conn_id)
                    .is_some_and(|c| c.lag_pending);
                if pending && sent_at.elapsed().as_secs() >= 300 {
                    let buf_id = self.state.connections.get(&conn_id).map_or_else(
                        || conn_id.clone(),
                        |c| crate::state::buffer::make_buffer_id(&conn_id, &c.label),
                    );
                    let msg_id = self.state.next_message_id();
                    self.state.add_message(
                        &buf_id,
                        crate::state::buffer::Message {
                            id: msg_id,
                            timestamp: chrono::Utc::now(),
                            message_type: crate::state::buffer::MessageType::Event,
                            nick: None,
                            nick_mode: None,
                            text: format!(
                                "Connection to {conn_id} timed out (no PONG for 5 minutes)"
                            ),
                            highlight: false,
                            tags: std::collections::HashMap::new(),
                            log_msg_id: None,
                            log_ref_id: None,
                            event_key: None,
                            event_params: Some(Vec::new()),
                        },
                    );
                    if let Some(handle) = self.irc_handles.get(&conn_id) {
                        let _ = handle.sender.send(::irc::proto::Command::QUIT(Some(
                            "Ping timeout".to_string(),
                        )));
                    }
                    continue;
                }
            }

            let should_ping = self
                .lag_pings
                .get(&conn_id)
                .is_none_or(|last| now.duration_since(*last).as_secs() >= 30);

            if should_ping {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .to_string();
                if let Some(handle) = self.irc_handles.get(&conn_id) {
                    let _ = handle
                        .sender
                        .send(::irc::proto::Command::Raw("PING".to_string(), vec![ts]));
                }
                self.lag_pings.insert(conn_id.clone(), now);
                if let Some(conn) = self.state.connections.get_mut(&conn_id) {
                    conn.lag_pending = true;
                }
            }
        }
    }

    /// Execute autosendcmd string after successful connection.
    ///
    /// Format: semicolon-separated commands with optional `WAIT <ms>` delays.
    /// Commands without a leading `/` get one prepended automatically.
    /// `$N` / `${N}` are replaced with the current nick.
    ///
    /// WAIT delays are currently skipped (commands execute immediately).
    fn execute_autosendcmd(&mut self, conn_id: &str, cmds: &str) {
        let nick = self
            .state
            .connections
            .get(conn_id)
            .map(|c| c.nick.clone())
            .unwrap_or_default();

        for part in cmds.split(';') {
            let cmd = part.trim();
            if cmd.is_empty() {
                continue;
            }
            // Skip WAIT delays (async delay support can be added later)
            if cmd.to_uppercase().starts_with("WAIT") {
                continue;
            }
            // Replace $N / ${N} with current nick
            let expanded = cmd.replace("$N", &nick).replace("${N}", &nick);
            // Prepend / if not already a command
            let line = if expanded.starts_with('/') {
                expanded
            } else {
                format!("/{expanded}")
            };
            // Parse and execute as if user typed it
            if let Some(parsed) = crate::commands::parser::parse_command(&line) {
                self.execute_command(&parsed);
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn handle_irc_event(&mut self, event: IrcEvent) {
        match event {
            IrcEvent::HandleReady(conn_id, sender, local_ip) => {
                // Store local IP on Connection state (for DCC own-IP fallback)
                if let Some(conn) = self.state.connections.get_mut(&conn_id) {
                    conn.local_ip = local_ip;
                }
                self.irc_handles.insert(
                    conn_id.clone(),
                    IrcHandle {
                        conn_id,
                        sender,
                        local_ip,
                    },
                );
            }
            IrcEvent::NegotiationInfo(conn_id, diag) => {
                // Display CAP/SASL diagnostics in status buffer — fires immediately
                // so they're visible even if connection fails before RPL_WELCOME.
                let buf_id = self.state.connections.get(&conn_id).map_or_else(
                    || conn_id.clone(),
                    |c| crate::state::buffer::make_buffer_id(&conn_id, &c.label),
                );
                for msg in &diag {
                    crate::irc::events::emit(&mut self.state, &buf_id, &format!("%Z56b6c2{msg}%N"));
                }
            }
            IrcEvent::Connected(conn_id, enabled_caps) => {
                // Store negotiated caps on connection
                if let Some(conn) = self.state.connections.get_mut(&conn_id) {
                    conn.enabled_caps = enabled_caps;
                }
                // Collect channels to rejoin before handle_connected resets state
                let rejoin_channels = crate::irc::events::channels_to_rejoin(&self.state, &conn_id);
                crate::irc::events::handle_connected(&mut self.state, &conn_id);

                // Notify scripts
                {
                    use crate::scripting::api::events;
                    let nick = self
                        .state
                        .connections
                        .get(&conn_id)
                        .map_or_else(String::new, |c| c.nick.clone());
                    let mut params = HashMap::new();
                    params.insert("connection_id".to_string(), conn_id.clone());
                    params.insert("nick".to_string(), nick);
                    self.emit_script_event(events::CONNECTED, params);
                }

                // Config channels (used for eager buffer creation + rejoin filtering)
                let config_channels: Vec<String> = self
                    .config
                    .servers
                    .iter()
                    .find(|(id, cfg)| *id == &conn_id || cfg.label == conn_id)
                    .map(|(_, cfg)| cfg.channels.clone())
                    .unwrap_or_default();

                // Merge config + rejoin for buffer creation
                let mut all_channels = config_channels.clone();
                for ch in &rejoin_channels {
                    if !all_channels.iter().any(|c| c.eq_ignore_ascii_case(ch)) {
                        all_channels.push(ch.clone());
                    }
                }

                // Execute autosendcmd BEFORE autojoin (e.g. NickServ identify)
                let autosendcmd = self
                    .config
                    .servers
                    .iter()
                    .find(|(id, cfg)| *id == &conn_id || cfg.label == conn_id)
                    .and_then(|(_, cfg)| cfg.autosendcmd.clone())
                    .or_else(|| {
                        self.state
                            .connections
                            .get(&conn_id)
                            .and_then(|c| c.origin_config.autosendcmd.clone())
                    });
                if let Some(cmds) = autosendcmd {
                    self.execute_autosendcmd(&conn_id, &cmds);
                }

                // Eager buffer creation (erssi pattern): create all channel
                // buffers upfront so the buffer list is stable from the start.
                // Buffers are destroyed on join failure (474, 471, etc.).
                for entry in &all_channels {
                    let chan_name = entry.split(' ').next().unwrap_or(entry);
                    let buf_id = make_buffer_id(&conn_id, chan_name);
                    if !self.state.buffers.contains_key(&buf_id) {
                        self.state.add_buffer(Buffer {
                            id: buf_id,
                            connection_id: conn_id.clone(),
                            buffer_type: BufferType::Channel,
                            name: chan_name.to_string(),
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
                            last_speakers: Vec::new(),
                        });
                    }
                }

                // Load backlog for eagerly created channel buffers
                for entry in &all_channels {
                    let chan_name = entry.split(' ').next().unwrap_or(entry);
                    let buf_id = make_buffer_id(&conn_id, chan_name);
                    self.load_backlog(&buf_id);
                }

                // Channel joining is handled by the irc crate on ENDOFMOTD:
                // it batches channels into comma-separated JOINs with keys-first
                // ordering and 512-byte splitting. Channels are passed via Config.
                // Rejoin channels (from reconnect) need manual joining since
                // they aren't in the library's config.
                if !rejoin_channels.is_empty()
                    && let Some(handle) = self.irc_handles.get(&conn_id)
                {
                    let extra: Vec<&str> = rejoin_channels
                        .iter()
                        .filter(|ch| {
                            !config_channels.iter().any(|c| {
                                c.split_once(' ')
                                    .map_or(c.as_str(), |(n, _)| n)
                                    .eq_ignore_ascii_case(ch)
                            })
                        })
                        .map(String::as_str)
                        .collect();
                    if !extra.is_empty() {
                        let chanlist = extra.join(",");
                        let _ = handle
                            .sender
                            .send(::irc::proto::Command::JOIN(chanlist, None, None));
                    }
                }
            }
            IrcEvent::Disconnected(conn_id, error) => {
                // DCC connections are peer-to-peer and independent of the IRC
                // server.  Do NOT close DCC records on IRC disconnect.
                crate::irc::events::handle_disconnected(
                    &mut self.state,
                    &conn_id,
                    error.as_deref(),
                );
                // Notify scripts
                {
                    use crate::scripting::api::events;
                    let mut params = HashMap::new();
                    params.insert("connection_id".to_string(), conn_id.clone());
                    self.emit_script_event(events::DISCONNECTED, params);
                }
                self.irc_handles.remove(&conn_id);
                if let Some(fwd) = self.forwarder_handles.remove(&conn_id) {
                    fwd.abort();
                }
                self.lag_pings.remove(&conn_id);
                self.batch_trackers.remove(&conn_id);
                self.channel_query_queues.remove(&conn_id);
                self.channel_query_in_flight.remove(&conn_id);
                self.channel_query_sent_at.remove(&conn_id);
            }
            IrcEvent::Message(conn_id, msg) => {
                // Intercept PONG to update lag measurement
                if let ::irc::proto::Command::PONG(_, _) = &msg.command
                    && let Some(sent_at) = self.lag_pings.get(&conn_id)
                {
                    // Lag will never exceed u64::MAX milliseconds
                    let lag_ms = u64::try_from(sent_at.elapsed().as_millis()).unwrap_or(u64::MAX);
                    if let Some(conn) = self.state.connections.get_mut(&conn_id) {
                        conn.lag = Some(lag_ms);
                        conn.lag_pending = false;
                    }
                }
                // Handle CAP subcommands for cap-notify (runtime capability changes)
                if let ::irc::proto::Command::CAP(_, ref subcmd, ref field3, ref field4) =
                    msg.command
                {
                    use ::irc::proto::command::CapSubCommand;
                    match subcmd {
                        CapSubCommand::NEW => {
                            let to_request = crate::irc::events::handle_cap_new(
                                &mut self.state,
                                &conn_id,
                                field3.as_deref(),
                                field4.as_deref(),
                            );
                            if !to_request.is_empty()
                                && let Some(handle) = self.irc_handles.get(&conn_id)
                            {
                                let req_str = to_request.join(" ");
                                tracing::info!("sending CAP REQ for new caps: {req_str}");
                                let _ = handle.sender.send(::irc::proto::Command::CAP(
                                    None,
                                    CapSubCommand::REQ,
                                    None,
                                    Some(req_str),
                                ));
                            }
                        }
                        CapSubCommand::DEL => {
                            crate::irc::events::handle_cap_del(
                                &mut self.state,
                                &conn_id,
                                field3.as_deref(),
                                field4.as_deref(),
                            );
                        }
                        CapSubCommand::ACK => {
                            crate::irc::events::handle_cap_ack(
                                &mut self.state,
                                &conn_id,
                                field3.as_deref(),
                                field4.as_deref(),
                            );
                        }
                        CapSubCommand::NAK => {
                            crate::irc::events::handle_cap_nak(
                                &mut self.state,
                                &conn_id,
                                field3.as_deref(),
                                field4.as_deref(),
                            );
                        }
                        _ => {}
                    }
                }

                // --- IRCv3 batch interception ---
                // Handle BATCH commands (start/end) and collect @batch-tagged messages.
                if let ::irc::proto::Command::BATCH(ref ref_tag, ref sub, ref params) = msg.command
                {
                    let tracker = self.batch_trackers.entry(conn_id.clone()).or_default();
                    if let Some(tag) = ref_tag.strip_prefix('+') {
                        // Start batch
                        let batch_type = sub
                            .as_ref()
                            .map_or_else(String::new, |s| s.to_str().to_string());
                        let batch_params = params.clone().unwrap_or_default();
                        tracker.start_batch(tag, &batch_type, batch_params);
                        tracing::debug!("batch started: tag={tag} type={batch_type}");
                    } else if let Some(tag) = ref_tag.strip_prefix('-') {
                        // End batch
                        if let Some(batch) = tracker.end_batch(tag) {
                            tracing::debug!(
                                "batch ended: tag={tag} type={} msgs={}",
                                batch.batch_type,
                                batch.messages.len()
                            );
                            crate::irc::batch::process_completed_batch(
                                &mut self.state,
                                &conn_id,
                                &batch,
                            );
                        }
                    }
                    // BATCH commands themselves are not dispatched further
                } else if self
                    .batch_trackers
                    .entry(conn_id.clone())
                    .or_default()
                    .is_batched(&msg)
                {
                    // Message belongs to an open batch — collect it, don't process now
                    if let Some(tracker) = self.batch_trackers.get_mut(&conn_id) {
                        tracker.add_message(*msg);
                    }
                } else {
                    // Normal message processing

                    // Extract channel from RPL_ENDOFNAMES (for auto-WHO/MODE batch).
                    let endofnames_channel = if let ::irc::proto::Command::Response(
                        ::irc::proto::Response::RPL_ENDOFNAMES,
                        ref args,
                    ) = msg.command
                    {
                        args.get(1).cloned()
                    } else {
                        None
                    };

                    // Extract target from RPL_ENDOFWHO (for batch completion).
                    let endofwho_target = if let ::irc::proto::Command::Response(
                        ::irc::proto::Response::RPL_ENDOFWHO,
                        ref args,
                    ) = msg.command
                    {
                        args.get(1).cloned()
                    } else {
                        None
                    };

                    // Update conn.nick from RPL_WELCOME — args[0] is our confirmed nick
                    // after any ERR_NICKNAMEINUSE retries by the irc crate.
                    if let ::irc::proto::Command::Response(
                        ::irc::proto::Response::RPL_WELCOME,
                        ref args,
                    ) = msg.command
                        && let Some(confirmed_nick) = args.first()
                        && let Some(conn) = self.state.connections.get_mut(&conn_id)
                    {
                        conn.nick.clone_from(confirmed_nick);
                    }

                    // Emit to scripts before default handling. If suppressed, skip.
                    if self.emit_irc_to_scripts(&conn_id, &msg) {
                        // Script suppressed — still process channel queries.
                        if let Some(channel) = endofnames_channel {
                            self.queue_channel_query(&conn_id, channel);
                        }
                        if let Some(ref target) = endofwho_target {
                            self.handle_who_batch_complete(&conn_id, target);
                        }
                        return;
                    }

                    // Intercept DCC CTCP before normal IRC handling.
                    // DCC messages arrive as CTCP inside PRIVMSG; events.rs ignores
                    // non-ACTION CTCPs, so we must consume them here to avoid them
                    // appearing as garbled text in the chat view.
                    if let ::irc::proto::Command::PRIVMSG(_, ref text) = msg.command
                        && text.starts_with('\x01')
                        && text.ends_with('\x01')
                        && text.len() > 2
                    {
                        let inner = &text[1..text.len() - 1];
                        if let Some(dcc_msg) = crate::dcc::protocol::parse_dcc_ctcp(inner) {
                            let (nick, ident, host) =
                                crate::irc::formatting::extract_nick_userhost(msg.prefix.as_ref());

                            // A passive DCC response from the peer looks like:
                            //   DCC CHAT CHAT <peer_ip> <peer_port> <our_token>
                            // where port > 0 and passive_token matches what we sent.
                            // We find our pending record by token and connect to the peer.
                            if let Some(token) = dcc_msg.passive_token
                                && dcc_msg.port > 0
                            {
                                let matching_id = self
                                    .dcc
                                    .records
                                    .iter()
                                    .find(|(_, r)| r.passive_token == Some(token))
                                    .map(|(id, _)| id.clone());

                                if let Some(id) = matching_id {
                                    // Update the record to point at the peer's real address.
                                    if let Some(rec) = self.dcc.records.get_mut(&id) {
                                        rec.addr = dcc_msg.addr;
                                        rec.port = dcc_msg.port;
                                        rec.state = crate::dcc::types::DccState::Connecting;
                                    }

                                    let (line_tx, line_rx) = tokio::sync::mpsc::unbounded_channel();
                                    self.dcc.chat_senders.insert(id.clone(), line_tx);

                                    let task_id = id.clone();
                                    let event_tx = self.dcc.dcc_tx.clone();
                                    let timeout_dur =
                                        std::time::Duration::from_secs(self.dcc.timeout_secs);
                                    let peer_addr =
                                        std::net::SocketAddr::new(dcc_msg.addr, dcc_msg.port);

                                    tracing::debug!(
                                        "passive DCC response from {nick}: \
                                         connecting to {peer_addr} (token={token})"
                                    );

                                    tokio::spawn(async move {
                                        crate::dcc::chat::connect_for_chat(
                                            task_id,
                                            peer_addr,
                                            timeout_dur,
                                            event_tx,
                                            line_rx,
                                        )
                                        .await;
                                    });

                                    // Don't fall through to normal IRC handling.
                                    if let Some(channel) = endofnames_channel {
                                        self.queue_channel_query(&conn_id, channel);
                                    }
                                    if let Some(ref target) = endofwho_target {
                                        self.handle_who_batch_complete(&conn_id, target);
                                    }
                                    return;
                                }
                            }

                            // Otherwise this is a fresh incoming DCC CHAT offer.
                            self.handle_dcc_event(crate::dcc::DccEvent::IncomingRequest {
                                nick,
                                conn_id: conn_id.clone(),
                                addr: dcc_msg.addr,
                                port: dcc_msg.port,
                                passive_token: dcc_msg.passive_token,
                                ident,
                                host,
                            });

                            // Don't pass to normal IRC handler — the CTCP is consumed.
                            if let Some(channel) = endofnames_channel {
                                self.queue_channel_query(&conn_id, channel);
                            }
                            if let Some(ref target) = endofwho_target {
                                self.handle_who_batch_complete(&conn_id, target);
                            }
                            return;
                        }
                    }

                    // Snapshot buffer count so we can detect newly created buffers
                    // and feed them with chat history from the log database.
                    let buffers_before = self.state.buffers.len();

                    crate::irc::events::handle_irc_message(&mut self.state, &conn_id, &msg);

                    // Load backlog for any buffers created by handle_irc_message
                    // (e.g. query buffer on first PRIVMSG from a new nick)
                    if self.state.buffers.len() > buffers_before {
                        let new_ids: Vec<String> = self
                            .state
                            .buffers
                            .keys()
                            .skip(buffers_before)
                            .cloned()
                            .collect();
                        for buf_id in &new_ids {
                            self.load_backlog(buf_id);
                        }
                    }

                    // ── DCC: track nick renames ──────────────────────────────────
                    // When a user renames on IRC their DCC record and buffer must
                    // follow, since buffers are named after the peer's nick (=Nick).
                    if let ::irc::proto::Command::NICK(ref new_nick) = msg.command
                        && let Some(::irc::proto::Prefix::Nickname(ref old_nick, _, _)) = msg.prefix
                    {
                        let renames = self.dcc.update_nick(old_nick, new_nick);
                        for (_old_id, _new_id, old_buf_suffix, new_buf_suffix) in renames {
                            let old_buf_id =
                                crate::state::buffer::make_buffer_id(&conn_id, &old_buf_suffix);
                            let new_buf_id =
                                crate::state::buffer::make_buffer_id(&conn_id, &new_buf_suffix);

                            if let Some(mut buf) = self.state.buffers.shift_remove(&old_buf_id) {
                                buf.id.clone_from(&new_buf_id);
                                buf.name = format!("={new_nick}");
                                self.state.buffers.insert(new_buf_id.clone(), buf);

                                // Keep active selection consistent.
                                if self.state.active_buffer_id.as_deref() == Some(&old_buf_id) {
                                    self.state.active_buffer_id = Some(new_buf_id);
                                }
                            }
                        }
                    }

                    // ── DCC: ERR_NOSUCHNICK cleanup ──────────────────────────────
                    // If the IRC server reports that a nick does not exist,
                    // cancel any pending DCC request to that nick so it doesn't
                    // sit in the queue until timeout.
                    if let ::irc::proto::Command::Response(
                        ::irc::proto::Response::ERR_NOSUCHNICK,
                        ref args,
                    ) = msg.command
                        && let Some(target_nick) = args.get(1)
                        && let Some(record) = self.dcc.close_by_nick(target_nick)
                    {
                        crate::commands::helpers::add_local_event(
                            self,
                            &format!("DCC CHAT to {} cancelled: no such nick", record.nick),
                        );
                    }

                    // Queue channel for batched WHO + MODE after join.
                    if let Some(channel) = endofnames_channel {
                        self.queue_channel_query(&conn_id, channel);
                    }

                    // Check if a WHO batch completed.
                    if let Some(ref target) = endofwho_target {
                        self.handle_who_batch_complete(&conn_id, target);
                    }
                }
            }
        }
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Paste(text) => self.handle_paste(&text),
            Event::Resize(cols, rows) => {
                self.cached_term_cols = cols;
                self.cached_term_rows = rows;
            }
            _ => {}
        }
    }

    /// Maximum time (ms) between ESC and follow-up key to treat as ESC+key combo.
    const ESC_TIMEOUT_MS: u128 = 500;

    /// Check if a recent ESC press should combine with the current key.
    fn consume_esc_prefix(&mut self) -> bool {
        self.last_esc_time
            .take()
            .is_some_and(|t| t.elapsed().as_millis() < Self::ESC_TIMEOUT_MS)
    }

    /// Switch to buffer N (0-9) — shared logic for Alt+N and ESC+N.
    fn switch_to_buffer_num(&mut self, n: usize) {
        if n == 0 {
            // 0 goes to default Status buffer
            let default_buf_id = make_buffer_id(Self::DEFAULT_CONN_ID, "Status");
            if self.state.buffers.contains_key(&default_buf_id) {
                self.state.set_active_buffer(&default_buf_id);
                self.scroll_offset = 0;
                self.reset_sidepanel_scrolls();
            }
        } else {
            // 1..9 map to real buffers (excluding _default)
            let real_ids: Vec<_> = self
                .state
                .sorted_buffer_ids()
                .into_iter()
                .filter(|id| {
                    self.state
                        .buffers
                        .get(id.as_str())
                        .is_none_or(|b| b.connection_id != Self::DEFAULT_CONN_ID)
                })
                .collect();
            let idx = n - 1; // 1 = index 0
            if idx < real_ids.len() {
                self.state.set_active_buffer(&real_ids[idx]);
                self.scroll_offset = 0;
                self.reset_sidepanel_scrolls();
            }
        }
    }

    /// Reset sidepanel scroll offsets (e.g. on buffer switch).
    #[allow(clippy::missing_const_for_fn)] // const &mut self not stable
    fn reset_sidepanel_scrolls(&mut self) {
        self.buffer_list_scroll = 0;
        self.nick_list_scroll = 0;
    }

    #[allow(clippy::too_many_lines)]
    fn handle_key(&mut self, key: event::KeyEvent) {
        // Check for ESC+key combos (ESC pressed recently, now a follow-up key)
        let esc_active = if key.code == KeyCode::Esc {
            // Don't consume ESC prefix on another ESC press
            self.last_esc_time.take();
            false
        } else {
            self.consume_esc_prefix()
        };

        // ESC+digit → buffer switch (like Alt+digit)
        // ESC+Left/Right → prev/next buffer (like Alt+Left/Right)
        if esc_active {
            match key.code {
                KeyCode::Char(c) if c.is_ascii_digit() && key.modifiers.is_empty() => {
                    let n = c.to_digit(10).unwrap_or(0) as usize;
                    self.switch_to_buffer_num(n);
                    return;
                }
                KeyCode::Left if key.modifiers.is_empty() => {
                    self.state.prev_buffer();
                    self.scroll_offset = 0;
                    self.reset_sidepanel_scrolls();
                    return;
                }
                KeyCode::Right if key.modifiers.is_empty() => {
                    self.state.next_buffer();
                    self.scroll_offset = 0;
                    self.reset_sidepanel_scrolls();
                    return;
                }
                _ => {
                    // ESC expired or unrecognized follow-up — fall through to normal handling
                }
            }
        }

        match (key.modifiers, key.code) {
            // ESC — dismiss spell suggestions, image preview, or record for ESC+key combo
            (_, KeyCode::Esc) => {
                if self.input.spell_state.is_some() {
                    self.input.dismiss_spell();
                } else if matches!(
                    self.image_preview,
                    crate::image_preview::PreviewStatus::Hidden
                ) {
                    self.last_esc_time = Some(Instant::now());
                } else {
                    self.dismiss_image_preview();
                }
            }
            (KeyModifiers::CONTROL, KeyCode::Char('q' | 'c')) => self.should_quit = true,
            (KeyModifiers::CONTROL, KeyCode::Char('l')) => {
                // Force redraw (happens automatically on next iteration)
            }
            // Ctrl+U — clear line from cursor to start
            (KeyModifiers::CONTROL, KeyCode::Char('u')) => self.input.clear_to_start(),
            // Ctrl+K — clear line from cursor to end
            (KeyModifiers::CONTROL, KeyCode::Char('k')) => self.input.clear_to_end(),
            // Ctrl+W — delete word before cursor
            (KeyModifiers::CONTROL, KeyCode::Char('w')) => self.input.delete_word_back(),
            // Ctrl+A — move cursor to start (same as Home)
            (KeyModifiers::CONTROL, KeyCode::Char('a')) | (_, KeyCode::Home) => self.input.home(),
            // Ctrl+B — move cursor left (same as Left)
            (KeyModifiers::CONTROL, KeyCode::Char('b')) => self.input.move_left(),
            // Ctrl+E — move cursor to end (same as End)
            (KeyModifiers::CONTROL, KeyCode::Char('e')) | (_, KeyCode::End) => {
                self.input.end();
                self.scroll_offset = 0;
            }
            (KeyModifiers::ALT, KeyCode::Char(c)) if c.is_ascii_digit() => {
                let n = c.to_digit(10).unwrap_or(0) as usize;
                self.switch_to_buffer_num(n);
            }
            (mods, KeyCode::Left) if mods.contains(KeyModifiers::ALT) => {
                self.state.prev_buffer();
                self.scroll_offset = 0;
                self.reset_sidepanel_scrolls();
            }
            (mods, KeyCode::Right) if mods.contains(KeyModifiers::ALT) => {
                self.state.next_buffer();
                self.scroll_offset = 0;
                self.reset_sidepanel_scrolls();
            }
            // Enter key, or newline chars arriving individually when bracketed
            // paste isn't supported — submit the current input line.
            (_, KeyCode::Enter | KeyCode::Char('\n' | '\r')) => {
                // Accept any active spell correction before submitting.
                self.input.spell_state = None;
                let text = self.input.submit();
                if !text.is_empty() {
                    self.handle_submit(&text);
                }
            }
            (_, KeyCode::Backspace) => {
                self.input.dismiss_spell();
                self.input.backspace();
            }
            (_, KeyCode::Delete) => self.input.delete(),
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
            (_, KeyCode::Tab) => {
                // Spell suggestion cycling takes priority over tab completion.
                if self.input.spell_state.is_some() {
                    self.input.cycle_spell_suggestion();
                } else {
                    self.handle_tab();
                }
            }
            (mods, KeyCode::Char(c)) if mods.is_empty() || mods == KeyModifiers::SHIFT => {
                if self.input.spell_state.is_some() {
                    // Spell correction is active — handle accept keys specially.
                    if c == ' ' {
                        // Space: accept current suggestion.
                        // If the trigger was a space, it's already in the input — done.
                        // If the trigger was punctuation (e.g., "word."), add a space
                        // after it so the user doesn't have to press Space twice.
                        let needs_space = self.input.spell_state.as_ref().is_some_and(|s| {
                            self.input.value[s.word_end..]
                                .chars()
                                .next()
                                .is_none_or(|ch| ch != ' ')
                        });
                        self.input.spell_state = None;
                        if needs_space {
                            self.input.insert_char(' ');
                        }
                    } else if matches!(c, '.' | ',' | '!' | '?' | ';' | ':') {
                        // Punctuation: accept and replace trailing separator with it.
                        // "corrected_word " → "corrected_word."
                        self.input.accept_spell_with_punctuation(c);
                    } else {
                        // Any other char: accept current suggestion and continue typing.
                        self.input.spell_state = None;
                        self.input.insert_char(c);
                    }
                } else {
                    self.input.insert_char(c);
                    // After typing a word separator, check spelling of the completed word.
                    if c == ' ' || (c.is_ascii_punctuation() && c != '/') {
                        self.check_spelling_after_separator();
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_paste(&mut self, text: &str) {
        let lines: Vec<&str> = text.split('\n').collect();
        let non_empty: Vec<&str> = lines
            .iter()
            .map(|l| l.trim_end_matches('\r'))
            .filter(|l| !l.is_empty())
            .collect();

        if non_empty.len() <= 1 {
            // Single line (or empty): insert into input buffer at cursor.
            let single = non_empty.first().copied().unwrap_or("");
            for ch in single.chars() {
                self.input.insert_char(ch);
            }
            return;
        }

        // Multiline paste: prepend any existing input to the first line,
        // send it immediately, queue the rest with 500ms spacing.
        // Matches kokoirc and erssi behavior.
        self.paste_queue.clear();

        let current_input = self.input.submit();
        let first = if current_input.is_empty() {
            non_empty[0].to_string()
        } else {
            format!("{current_input}{}", non_empty[0])
        };

        // Send first line immediately
        self.handle_submit(&first);

        // Queue remaining lines
        for line in &non_empty[1..] {
            self.paste_queue.push_back((*line).to_string());
        }
    }

    /// Send one queued paste line. Called every 500ms by the paste timer.
    fn drain_paste_queue(&mut self) {
        if let Some(line) = self.paste_queue.pop_front() {
            self.handle_submit(&line);
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
                } else if regions.buffer_list_area.is_some_and(|r| r.contains(pos)) {
                    self.buffer_list_scroll = self.buffer_list_scroll.saturating_sub(1);
                } else if regions.nick_list_area.is_some_and(|r| r.contains(pos)) {
                    self.nick_list_scroll = self.nick_list_scroll.saturating_sub(1);
                }
            }
            MouseEventKind::ScrollDown => {
                if regions.chat_area.is_some_and(|r| r.contains(pos)) {
                    self.scroll_offset = self.scroll_offset.saturating_sub(3);
                } else if let Some(r) = regions.buffer_list_area
                    && r.contains(pos)
                {
                    let visible_h = r.height as usize;
                    let max = self.buffer_list_total.saturating_sub(visible_h);
                    if self.buffer_list_scroll < max {
                        self.buffer_list_scroll += 1;
                    }
                } else if let Some(r) = regions.nick_list_area
                    && r.contains(pos)
                {
                    let visible_h = r.height as usize;
                    let max = self.nick_list_total.saturating_sub(visible_h);
                    if self.nick_list_scroll < max {
                        self.nick_list_scroll += 1;
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // Dismiss image preview on any click (same as ESC).
                if !matches!(
                    self.image_preview,
                    crate::image_preview::PreviewStatus::Hidden
                ) {
                    self.dismiss_image_preview();
                    return;
                }
                if let Some(buf_area) = regions.buffer_list_area
                    && buf_area.contains(pos)
                {
                    let y_offset = mouse.row.saturating_sub(buf_area.y) as usize;
                    self.handle_buffer_list_click(y_offset);
                } else if let Some(nick_area) = regions.nick_list_area
                    && nick_area.contains(pos)
                {
                    let y_offset = mouse.row.saturating_sub(nick_area.y) as usize;
                    self.handle_nick_list_click(y_offset);
                } else if let Some(chat_area) = regions.chat_area
                    && chat_area.contains(pos)
                {
                    let y_offset = mouse.row.saturating_sub(chat_area.y) as usize;
                    self.handle_chat_click(y_offset);
                }
            }
            _ => {}
        }
    }

    fn handle_buffer_list_click(&mut self, y_offset: usize) {
        use crate::state::buffer::BufferType;

        // Clamp scroll the same way the renderer does — prevents click offset
        // when buffer_list_scroll exceeds max_scroll (e.g. after reattach or
        // channels parted while scrolled).
        let visible_h = self
            .ui_regions
            .and_then(|r| r.buffer_list_area)
            .map_or(0, |r| r.height as usize);
        let max_scroll = self.buffer_list_total.saturating_sub(visible_h);
        let clamped_scroll = self.buffer_list_scroll.min(max_scroll);
        self.buffer_list_scroll = clamped_scroll;
        let logical_row = y_offset + clamped_scroll;
        let sorted_ids = self.state.sorted_buffer_ids();
        // Map logical_row to the correct buffer, accounting for headers.
        // Server buffers are rendered as headers (not numbered items).
        let mut row = 0;
        let mut last_conn_id = String::new();
        for id in &sorted_ids {
            let Some(buf) = self.state.buffers.get(id.as_str()) else {
                continue;
            };
            if buf.connection_id == Self::DEFAULT_CONN_ID {
                continue;
            }
            // Connection header row
            if buf.connection_id != last_conn_id {
                last_conn_id.clone_from(&buf.connection_id);
                if row == logical_row {
                    // Clicked on header — switch to server buffer for this connection
                    if buf.buffer_type == BufferType::Server {
                        self.state.set_active_buffer(id);
                        self.scroll_offset = 0;
                        self.nick_list_scroll = 0;
                    }
                    return;
                }
                row += 1;
            }
            // Server buffers are the header, no separate row
            if buf.buffer_type == BufferType::Server {
                continue;
            }
            if row == logical_row {
                self.state.set_active_buffer(id);
                self.scroll_offset = 0;
                self.nick_list_scroll = 0;
                return;
            }
            row += 1;
        }
    }

    fn handle_nick_list_click(&mut self, y_offset: usize) {
        use crate::state::sorting;

        // Clamp scroll the same way the renderer does.
        let visible_h = self
            .ui_regions
            .and_then(|r| r.nick_list_area)
            .map_or(0, |r| r.height as usize);
        let max_scroll = self.nick_list_total.saturating_sub(visible_h);
        let clamped_scroll = self.nick_list_scroll.min(max_scroll);
        self.nick_list_scroll = clamped_scroll;
        let logical_row = y_offset + clamped_scroll;

        // Row 0 is the "N users" header line — skip it
        if logical_row == 0 {
            return;
        }
        let nick_index = logical_row - 1;

        // Get the sorted nick list from the active buffer
        let (conn_id, nick_name) = {
            let Some(buf) = self.state.active_buffer() else {
                return;
            };
            if buf.buffer_type != BufferType::Channel {
                return;
            }
            let nick_refs: Vec<_> = buf.users.values().collect();
            let sorted = sorting::sort_nicks(&nick_refs, sorting::DEFAULT_PREFIX_ORDER);
            let Some(entry) = sorted.get(nick_index) else {
                return;
            };
            (buf.connection_id.clone(), entry.nick.clone())
        };

        // Create a query buffer for that nick if it doesn't exist, then switch to it
        let query_buf_id = make_buffer_id(&conn_id, &nick_name);
        if !self.state.buffers.contains_key(&query_buf_id) {
            self.state.add_buffer(Buffer {
                id: query_buf_id.clone(),
                connection_id: conn_id,
                buffer_type: BufferType::Query,
                name: nick_name,
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
                last_speakers: Vec::new(),
            });
        }
        self.state.set_active_buffer(&query_buf_id);
        self.scroll_offset = 0;
        self.nick_list_scroll = 0;
    }

    fn handle_chat_click(&mut self, y_offset: usize) {
        if !self.config.image_preview.enabled {
            return;
        }

        let Some(buf) = self.state.active_buffer() else {
            return;
        };

        // Map the clicked row to the corresponding message, same logic as chat_view render.
        let total = buf.messages.len();
        let chat_height = self
            .ui_regions
            .and_then(|r| r.chat_area)
            .map_or(0, |a| a.height as usize);
        let max_scroll = total.saturating_sub(chat_height);
        let scroll = self.scroll_offset.min(max_scroll);
        let skip = total.saturating_sub(chat_height + scroll);
        let msg_index = skip + y_offset;

        let Some(msg) = buf.messages.get(msg_index) else {
            return;
        };

        // Extract URLs from message text and preview the first classifiable one.
        let urls = crate::image_preview::detect::extract_urls(&msg.text);
        if let Some(classification) = urls.first() {
            self.show_image_preview(&classification.url);
        }
    }

    /// Initialize the spell checker from config.
    fn init_spellchecker(&mut self) {
        let dict_dir = crate::spellcheck::SpellChecker::resolve_dict_dir(
            &self.config.spellcheck.dictionary_dir,
        );
        let checker =
            crate::spellcheck::SpellChecker::load(&self.config.spellcheck.languages, &dict_dir);
        if checker.is_active() {
            tracing::info!(dicts = checker.dict_count(), "spell checker initialized");
            self.spellchecker = Some(checker);
        } else {
            tracing::info!("spell checker: no dictionaries loaded");
            self.spellchecker = None;
        }
    }

    /// Reload the spell checker (called from `/set spellcheck.*`).
    pub fn reload_spellchecker(&mut self) {
        if self.config.spellcheck.enabled {
            self.init_spellchecker();
        } else {
            self.spellchecker = None;
        }
    }

    /// Check the last completed word for spelling and set up correction state.
    fn check_spelling_after_separator(&mut self) {
        // Skip if spell checking is disabled or no checker loaded.
        let Some(ref checker) = self.spellchecker else {
            return;
        };
        // Skip commands.
        if self.input.is_command() {
            return;
        }
        // Extract the last completed word (may include trailing punctuation).
        let Some((raw_start, _raw_end, raw_word)) = self.input.last_completed_word() else {
            return;
        };

        // Strip leading/trailing punctuation (WeeChat-style).
        // "do?" → "do", "hello!" → "hello", "'test'" → "test"
        let (stripped, strip_offset, strip_end) =
            crate::spellcheck::strip_word_punctuation(&raw_word);
        if stripped.is_empty() {
            return;
        }

        // Actual byte positions in the input buffer for the stripped word.
        let word_start = raw_start + strip_offset;
        let word_end = raw_start + strip_end;

        // Collect nicks from the active buffer to skip.
        let nicks: std::collections::HashSet<String> = self
            .state
            .active_buffer()
            .map_or_else(std::collections::HashSet::new, |buf| {
                buf.users.values().map(|e| e.nick.clone()).collect()
            });

        // Check the stripped word.
        if checker.check(stripped, &nicks) {
            return;
        }
        // Misspelled — get suggestions ranked by dictionary priority.
        let suggestions = checker.suggest(stripped);
        if suggestions.is_empty() {
            return;
        }
        self.input.spell_state = Some(crate::ui::input::SpellCorrection {
            word_start,
            word_end,
            original: stripped.to_string(),
            suggestions,
            index: 0,
        });

        // Immediately apply the first suggestion so it's visible in the input
        // and ready to accept with Space. Tab cycles to the next one.
        self.input.apply_spell_suggestion(0);
    }

    fn handle_tab(&mut self) {
        let (nicks, last_speakers): (Vec<String>, Vec<String>) =
            self.state.active_buffer().map_or_else(
                || (Vec::new(), Vec::new()),
                |buf| {
                    let nicks = buf.users.values().map(|e| e.nick.clone()).collect();
                    let speakers = buf.last_speakers.clone();
                    (nicks, speakers)
                },
            );
        let commands = crate::commands::registry::get_command_names();
        let setting_paths = crate::commands::settings::get_setting_paths(&self.config);
        self.input
            .tab_complete(&nicks, &last_speakers, commands, &setting_paths);
    }

    fn handle_submit(&mut self, text: &str) {
        if let Some(parsed) = crate::commands::parser::parse_command(text) {
            self.execute_command(&parsed);
        } else {
            self.handle_plain_message(text);
        }
        self.scroll_offset = 0;
    }

    fn execute_command(&mut self, parsed: &crate::commands::parser::ParsedCommand) {
        // Emit to scripts — they can suppress commands
        {
            use crate::scripting::api::events;
            let mut params = HashMap::new();
            params.insert("command".to_string(), parsed.name.clone());
            params.insert("args".to_string(), parsed.args.join(" "));
            if let Some(conn_id) = self.active_conn_id() {
                params.insert("connection_id".to_string(), conn_id.to_owned());
            }
            if self.emit_script_event(events::COMMAND_INPUT, params) {
                return;
            }
        }
        let commands = crate::commands::registry::get_commands();
        // Find by name or alias (built-in commands first)
        let found = commands.iter().find(|(name, def)| {
            *name == parsed.name || def.aliases.contains(&parsed.name.as_str())
        });
        if let Some((_, def)) = found {
            (def.handler)(self, &parsed.args);
        } else if let Some(template) = self.config.aliases.get(&parsed.name).cloned() {
            // Expand user-defined alias
            let expanded = expand_alias_template(&template, &parsed.args);
            // Re-parse the expanded text (it may itself be a command)
            if let Some(reparsed) = crate::commands::parser::parse_command(&expanded) {
                self.execute_command(&reparsed);
            } else {
                self.handle_plain_message(&expanded);
            }
        } else if self.script_manager.as_ref().is_some_and(|m| {
            let conn_id = self.state.active_buffer().map(|b| b.connection_id.as_str());
            m.handle_command(&parsed.name, &parsed.args, conn_id)
                .is_some()
        }) {
            // Script handled the command
        } else {
            crate::commands::helpers::add_local_event(
                self,
                &format!("Unknown command: /{}. Type /help for a list.", parsed.name),
            );
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "flat dispatch for DCC/channel/query message routing"
    )]
    fn handle_plain_message(&mut self, text: &str) {
        let Some(active_id) = self.state.active_buffer_id.clone() else {
            return;
        };

        let (conn_id, nick, buffer_name, buf_type) = {
            let Some(buf) = self.state.active_buffer() else {
                return;
            };
            // Only send to channels and queries, not server/status buffers
            if !matches!(
                buf.buffer_type,
                BufferType::Channel | BufferType::Query | BufferType::DccChat
            ) {
                crate::commands::helpers::add_local_event(
                    self,
                    "Cannot send messages to this buffer",
                );
                return;
            }
            let conn = self.state.connections.get(&buf.connection_id);
            let nick = conn.map(|c| c.nick.clone()).unwrap_or_default();
            (
                buf.connection_id.clone(),
                nick,
                buf.name.clone(),
                buf.buffer_type.clone(),
            )
        };

        // DCC CHAT routing: send via DCC channel, not IRC.
        if buf_type == BufferType::DccChat {
            let dcc_nick = buffer_name.strip_prefix('=').unwrap_or(&buffer_name);
            if let Some(record) = self.dcc.find_connected(dcc_nick) {
                let record_id = record.id.clone();
                if let Err(e) = self.dcc.send_chat_line(&record_id, text) {
                    crate::commands::helpers::add_local_event(
                        self,
                        &format!("DCC send error: {e}"),
                    );
                    return;
                }
                // Display locally
                let our_nick = self
                    .state
                    .connections
                    .values()
                    .next()
                    .map(|c| c.nick.clone())
                    .unwrap_or_default();
                let msg_id = self.state.next_message_id();
                self.state.add_message(
                    &active_id,
                    Message {
                        id: msg_id,
                        timestamp: Utc::now(),
                        message_type: MessageType::Message,
                        nick: Some(our_nick),
                        nick_mode: None,
                        text: text.to_string(),
                        highlight: false,
                        event_key: None,
                        event_params: None,
                        log_msg_id: None,
                        log_ref_id: None,
                        tags: std::collections::HashMap::new(),
                    },
                );
            } else {
                crate::commands::helpers::add_local_event(
                    self,
                    "No active DCC CHAT session for this buffer",
                );
            }
            return;
        }

        // Split long messages at word boundaries to stay within IRC byte limits.
        let chunks = crate::irc::split_irc_message(text, crate::irc::MESSAGE_MAX_BYTES);

        // When echo-message is enabled, the server will echo our message back
        // with authoritative server-time — skip local display and wait for echo.
        let echo_message_enabled = self
            .state
            .connections
            .get(&conn_id)
            .is_some_and(|c| c.enabled_caps.contains("echo-message"));

        let own_mode = self.state.nick_prefix(&active_id, &nick);

        for chunk in chunks {
            // Try to send via IRC if connected
            if let Some(handle) = self.irc_handles.get(&conn_id)
                && handle.sender.send_privmsg(&buffer_name, &chunk).is_err()
            {
                crate::commands::helpers::add_local_event(self, "Failed to send message");
                return;
            }

            if !echo_message_enabled {
                let id = self.state.next_message_id();
                self.state.add_message(
                    &active_id,
                    Message {
                        id,
                        timestamp: Utc::now(),
                        message_type: MessageType::Message,
                        nick: Some(nick.clone()),
                        nick_mode: own_mode.map(|c| c.to_string()),
                        text: chunk,
                        highlight: false,
                        event_key: None,
                        event_params: None,
                        log_msg_id: None,
                        log_ref_id: None,
                        tags: std::collections::HashMap::new(),
                    },
                );
            }
        }
    }

    /// Get the IRC sender for the active buffer's connection, if connected.
    pub fn active_irc_sender(&self) -> Option<&::irc::client::Sender> {
        let buf = self.state.active_buffer()?;
        let handle = self.irc_handles.get(&buf.connection_id)?;
        Some(&handle.sender)
    }

    /// Get the connection ID of the active buffer.
    pub fn active_conn_id(&self) -> Option<&str> {
        self.state
            .active_buffer()
            .map(|buf| buf.connection_id.as_str())
    }

    /// Build a `ScriptAPI` whose callbacks send `ScriptAction` messages
    /// through the provided channel. The App event loop drains these.
    #[allow(
        clippy::too_many_lines,
        clippy::type_complexity,
        clippy::needless_pass_by_value
    )]
    fn build_script_api(
        tx: mpsc::UnboundedSender<crate::scripting::ScriptAction>,
        snapshot: Arc<std::sync::RwLock<crate::scripting::engine::ScriptStateSnapshot>>,
        timer_id_counter: Arc<std::sync::atomic::AtomicU64>,
    ) -> crate::scripting::engine::ScriptAPI {
        use crate::scripting::ScriptAction;

        let t = tx.clone();
        let say: Arc<dyn Fn((String, String, Option<String>)) + Send + Sync> =
            Arc::new(move |(target, text, conn_id)| {
                let _ = t.send(ScriptAction::Say {
                    target,
                    text,
                    conn_id,
                });
            });

        let t = tx.clone();
        let action: Arc<dyn Fn((String, String, Option<String>)) + Send + Sync> =
            Arc::new(move |(target, text, conn_id)| {
                let _ = t.send(ScriptAction::Action {
                    target,
                    text,
                    conn_id,
                });
            });

        let t = tx.clone();
        let notice: Arc<dyn Fn((String, String, Option<String>)) + Send + Sync> =
            Arc::new(move |(target, text, conn_id)| {
                let _ = t.send(ScriptAction::Notice {
                    target,
                    text,
                    conn_id,
                });
            });

        let t = tx.clone();
        let raw: Arc<dyn Fn((String, Option<String>)) + Send + Sync> =
            Arc::new(move |(line, conn_id)| {
                let _ = t.send(ScriptAction::Raw { line, conn_id });
            });

        let t = tx.clone();
        let join: Arc<dyn Fn((String, Option<String>, Option<String>)) + Send + Sync> =
            Arc::new(move |(channel, key, conn_id)| {
                let _ = t.send(ScriptAction::Join {
                    channel,
                    key,
                    conn_id,
                });
            });

        let t = tx.clone();
        let part: Arc<dyn Fn((String, Option<String>, Option<String>)) + Send + Sync> =
            Arc::new(move |(channel, msg, conn_id)| {
                let _ = t.send(ScriptAction::Part {
                    channel,
                    msg,
                    conn_id,
                });
            });

        let t = tx.clone();
        let change_nick: Arc<dyn Fn((String, Option<String>)) + Send + Sync> =
            Arc::new(move |(nick, conn_id)| {
                let _ = t.send(ScriptAction::ChangeNick { nick, conn_id });
            });

        let t = tx.clone();
        let whois: Arc<dyn Fn((String, Option<String>)) + Send + Sync> =
            Arc::new(move |(nick, conn_id)| {
                let _ = t.send(ScriptAction::Whois { nick, conn_id });
            });

        let t = tx.clone();
        let mode: Arc<dyn Fn((String, String, Option<String>)) + Send + Sync> =
            Arc::new(move |(channel, mode_string, conn_id)| {
                let _ = t.send(ScriptAction::Mode {
                    channel,
                    mode_string,
                    conn_id,
                });
            });

        let t = tx.clone();
        let kick: Arc<dyn Fn((String, String, Option<String>, Option<String>)) + Send + Sync> =
            Arc::new(move |(channel, nick, reason, conn_id)| {
                let _ = t.send(ScriptAction::Kick {
                    channel,
                    nick,
                    reason,
                    conn_id,
                });
            });

        let t = tx.clone();
        let ctcp: Arc<dyn Fn((String, String, Option<String>, Option<String>)) + Send + Sync> =
            Arc::new(move |(target, ctcp_type, message, conn_id)| {
                let _ = t.send(ScriptAction::Ctcp {
                    target,
                    ctcp_type,
                    message,
                    conn_id,
                });
            });

        let t = tx.clone();
        let add_local_event: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |text| {
            let _ = t.send(ScriptAction::LocalEvent { text });
        });

        let t = tx.clone();
        let add_buffer_event: Arc<dyn Fn((String, String)) + Send + Sync> =
            Arc::new(move |(buffer_id, text)| {
                let _ = t.send(ScriptAction::BufferEvent { buffer_id, text });
            });

        let t = tx.clone();
        let switch_buffer: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |buffer_id| {
            let _ = t.send(ScriptAction::SwitchBuffer { buffer_id });
        });

        let t = tx.clone();
        let execute_command: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |line| {
            let _ = t.send(ScriptAction::ExecuteCommand { line });
        });

        let t = tx.clone();
        let register_command: Arc<dyn Fn((String, String, String)) + Send + Sync> =
            Arc::new(move |(name, description, usage)| {
                let _ = t.send(ScriptAction::RegisterCommand {
                    name,
                    description,
                    usage,
                });
            });

        let t = tx.clone();
        let unregister_command: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |name| {
            let _ = t.send(ScriptAction::UnregisterCommand { name });
        });

        let t = tx.clone();
        let log: Arc<dyn Fn((String, String)) + Send + Sync> =
            Arc::new(move |(script, message)| {
                let _ = t.send(ScriptAction::Log { script, message });
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
                    // No conn_id — use active buffer's connection
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
            let _ = t.send(ScriptAction::StartTimer { id, interval_ms });
            id
        });

        let t = tx.clone();
        let counter = Arc::clone(&timer_id_counter);
        let start_timeout: Arc<dyn Fn(u64) -> u64 + Send + Sync> = Arc::new(move |delay_ms| {
            let id = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let _ = t.send(ScriptAction::StartTimeout { id, delay_ms });
            id
        });

        let t = tx.clone();
        let cancel_timer: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(move |id| {
            let _ = t.send(ScriptAction::CancelTimer { id });
        });

        // Config: per-script get/set reads from snapshot, set sends ScriptAction.
        let snap = Arc::clone(&snapshot);
        let config_get: Arc<dyn Fn((String, String)) -> Option<String> + Send + Sync> =
            Arc::new(move |(script, key)| {
                snap.read().ok()?.script_config.get(&(script, key)).cloned()
            });
        let config_set: Arc<dyn Fn((String, String, String)) + Send + Sync> =
            Arc::new(move |(script, key, value)| {
                let _ = tx.send(ScriptAction::SetScriptConfig { script, key, value });
            });
        let snap = Arc::clone(&snapshot);
        let app_config_get: Arc<dyn Fn(String) -> Option<String> + Send + Sync> =
            Arc::new(move |key_path| {
                let s = snap.read().ok()?;
                let toml_val = s.app_config_toml.as_ref()?;
                // Navigate dot-separated path: "general.theme" → toml["general"]["theme"]
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
    fn update_script_snapshot(&mut self) {
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
    /// If `conn_id` is None, uses the active buffer's connection.
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
    fn handle_script_action(&mut self, action: crate::scripting::ScriptAction) {
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
                    let _ = tx.send(crate::scripting::ScriptAction::TimerFired { id });
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
        // Take api out temporarily
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
    /// Returns true if any script suppressed the event.
    fn emit_script_event(
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
    /// Returns true if any script suppressed the event.
    #[allow(clippy::too_many_lines)]
    fn emit_irc_to_scripts(&self, conn_id: &str, msg: &::irc::proto::Message) -> bool {
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
                // Check for CTCP
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
                // CTCP response comes as NOTICE with \x01...\x01
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

/// Expand an alias template with positional args.
///
/// Supported variables:
/// - `$0` through `$9` — positional arguments
/// - `$*` — all arguments joined by space
/// - `$-` — all arguments from position 0 onward (same as `$*`)
fn expand_alias_template(template: &str, args: &[String]) -> String {
    let all_args = args.join(" ");
    let mut result = template.to_string();

    // Replace $* and $- with all args
    result = result.replace("$*", &all_args);
    result = result.replace("$-", &all_args);

    // Replace $0-$9 with positional args
    for i in (0..=9).rev() {
        let var = format!("${i}");
        let val = args.get(i).map_or("", String::as_str);
        result = result.replace(&var, val);
    }

    result
}

// ---------------------------------------------------------------------------
// Direct-write image functions (free functions, called from App methods)
// ---------------------------------------------------------------------------

/// Write Kitty graphics image directly to stdout via tmux DCS passthrough.
///
/// Sends the original PNG (`f=100`) at full resolution with `c`/`r` params
/// telling the terminal to scale the image to fit the cell area. This
/// produces much better quality than ratatui-image's pre-downscaled RGBA.
///
/// Image data is chunked into 4096-byte base64 pieces, each individually
/// wrapped in DCS passthrough (tmux has a ~1MB limit per passthrough block).
fn write_kitty_tmux_direct(raw_png: &[u8], inner_x: u16, inner_y: u16, inner_w: u16, inner_h: u16) {
    use std::io::Write;

    const CHARS_PER_CHUNK: usize = 4096;
    const CHUNK_SIZE: usize = (CHARS_PER_CHUNK / 4) * 3;

    let mut out = std::io::stdout().lock();

    // Save cursor + position at inner area (1-based for CUP).
    let row = inner_y + 1;
    let col = inner_x + 1;
    let _ = write!(out, "\x1b7\x1b[{row};{col}H");
    let _ = out.flush();

    let chunks: Vec<&[u8]> = raw_png.chunks(CHUNK_SIZE).collect();
    let chunk_count = chunks.len();

    for (i, chunk) in chunks.iter().enumerate() {
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, chunk);
        let more = u8::from(i + 1 < chunk_count);

        // DCS passthrough: \x1bPtmux; <escaped-kitty-cmd> \x1b\\
        // Inside DCS, ESC is doubled: \x1b → \x1b\x1b
        if i == 0 {
            // First chunk: transmit with display params.
            // f=100 = PNG format (terminal decodes at native quality)
            // a=T   = transmit and display
            // c/r   = cell area (terminal scales image to fit)
            // q=2   = suppress response
            let _ = write!(
                out,
                "\x1bPtmux;\x1b\x1b_Gq=2,a=T,f=100,t=d,c={inner_w},r={inner_h},m={more};{b64}\x1b\x1b\\\x1b\\"
            );
        } else {
            // Continuation chunks: just data + more flag.
            let _ = write!(out, "\x1bPtmux;\x1b\x1b_Gm={more};{b64}\x1b\x1b\\\x1b\\");
        }
        let _ = out.flush();
    }

    // Restore cursor.
    let _ = write!(out, "\x1b8");
    let _ = out.flush();
}

/// Write iTerm2 image directly to stdout via tmux DCS passthrough.
///
/// Sends the original PNG via OSC 1337 at full resolution with cell-based
/// dimensions. The terminal handles scaling.
fn write_iterm2_tmux_direct(
    raw_png: &[u8],
    inner_x: u16,
    inner_y: u16,
    inner_w: u16,
    inner_h: u16,
) {
    use std::io::Write;

    // Mouse tracking modes — must be disabled during DCS image write to
    // prevent interference with tmux passthrough (matches kokoirc).
    const MOUSE_DISABLE: &[u8] = b"\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l";
    const MOUSE_ENABLE: &[u8] = b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h";

    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw_png);

    // Build the iTerm2 OSC 1337 sequence.
    let osc = format!(
        "\x1b]1337;File=inline=1;width={inner_w};height={inner_h};preserveAspectRatio=0:{b64}\x07"
    );

    // Wrap in tmux DCS passthrough: double all ESC bytes in the payload.
    let escaped = osc.replace('\x1b', "\x1b\x1b");
    let dcs = format!("\x1bPtmux;{escaped}\x1b\\");

    // Terminal rows/cols are 1-based for CUP.
    let row = inner_y + 1;
    let col = inner_x + 1;

    let mut out = std::io::stdout().lock();

    // Step 1: Disable mouse tracking.
    let _ = out.write_all(MOUSE_DISABLE);
    let _ = out.flush();

    // Step 2: Save cursor + position.
    let _ = write!(out, "\x1b7\x1b[{row};{col}H");
    let _ = out.flush();

    // Step 3: Write DCS-wrapped image data.
    let _ = out.write_all(dcs.as_bytes());
    let _ = out.flush();

    // Step 4: Restore cursor.
    let _ = out.write_all(b"\x1b8");
    let _ = out.flush();

    // Step 5: Re-enable mouse tracking.
    let _ = out.write_all(MOUSE_ENABLE);
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── match_terminal tests ──

    #[test]
    fn match_terminal_iterm2_termtype() {
        let (name, proto) = match_terminal("iTerm2 3.6.8").unwrap();
        assert_eq!(name, "iterm2");
        assert_eq!(proto, ProtocolType::Iterm2);
    }

    #[test]
    fn match_terminal_ghostty() {
        let (name, proto) = match_terminal("ghostty 1.3.0").unwrap();
        assert_eq!(name, "ghostty");
        assert_eq!(proto, ProtocolType::Kitty);
        let (name, proto) = match_terminal("xterm-ghostty").unwrap();
        assert_eq!(name, "ghostty");
        assert_eq!(proto, ProtocolType::Kitty);
    }

    #[test]
    fn match_terminal_kitty() {
        let (name, proto) = match_terminal("xterm-kitty").unwrap();
        assert_eq!(name, "kitty");
        assert_eq!(proto, ProtocolType::Kitty);
    }

    #[test]
    fn match_terminal_subterm() {
        let (name, proto) = match_terminal("Subterm 1.0").unwrap();
        assert_eq!(name, "subterm");
        assert_eq!(proto, ProtocolType::Kitty);
    }

    #[test]
    fn match_terminal_wezterm() {
        let (name, proto) = match_terminal("WezTerm 20240203").unwrap();
        assert_eq!(name, "wezterm");
        assert_eq!(proto, ProtocolType::Iterm2);
    }

    #[test]
    fn match_terminal_foot() {
        let (name, proto) = match_terminal("foot").unwrap();
        assert_eq!(name, "foot");
        assert_eq!(proto, ProtocolType::Sixel);
    }

    #[test]
    fn match_terminal_konsole() {
        let (name, proto) = match_terminal("konsole").unwrap();
        assert_eq!(name, "konsole");
        assert_eq!(proto, ProtocolType::Sixel);
    }

    #[test]
    fn match_terminal_unknown() {
        assert!(match_terminal("some-random-terminal").is_none());
    }

    // ── resolve_image_protocol tests ──

    #[test]
    fn resolve_config_override_kitty() {
        let picker = ratatui_image::picker::Picker::halfblocks();
        let (proto, source) =
            resolve_image_protocol("kitty", &picker, "unknown", None, String::new(), false);
        assert_eq!(proto, Some(ProtocolType::Kitty));
        assert_eq!(source, "config:kitty");
    }

    #[test]
    fn resolve_config_override_iterm2() {
        let picker = ratatui_image::picker::Picker::halfblocks();
        let (proto, source) =
            resolve_image_protocol("iterm2", &picker, "unknown", None, String::new(), false);
        assert_eq!(proto, Some(ProtocolType::Iterm2));
        assert_eq!(source, "config:iterm2");
    }

    #[test]
    fn resolve_tmux_overrides_io_detection() {
        // tmux client_termtype says Kitty — should override IO even if IO
        // also detected Kitty (they agree here, but tmux is authoritative).
        let mut picker = ratatui_image::picker::Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        let (proto, source) = resolve_image_protocol(
            "auto",
            &picker,
            "ghostty",
            Some(ProtocolType::Kitty),
            "tmux:client_termtype=ghostty 1.3.0".into(),
            false,
        );
        assert_eq!(proto, Some(ProtocolType::Kitty));
        assert!(source.starts_with("tmux:"));
    }

    #[test]
    fn resolve_tmux_iterm2_overrides_kitty_io() {
        // tmux says iTerm2, IO detected Kitty (iTerm2 responds to Kitty probe).
        // tmux is authoritative — use iTerm2 protocol.
        let mut picker = ratatui_image::picker::Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        let (proto, source) = resolve_image_protocol(
            "auto",
            &picker,
            "iterm2",
            Some(ProtocolType::Iterm2),
            "tmux:client_termtype=iTerm2 3.6.8".into(),
            false,
        );
        assert_eq!(proto, Some(ProtocolType::Iterm2));
        assert!(source.starts_with("tmux:"));
    }

    #[test]
    fn resolve_direct_trusts_io_detection() {
        // Not in tmux, not socket — outer terminal from env, IO detection is fine.
        let mut picker = ratatui_image::picker::Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        let (proto, source) = resolve_image_protocol(
            "auto",
            &picker,
            "ghostty",
            Some(ProtocolType::Kitty),
            "env:LC_TERMINAL=Ghostty".into(),
            false,
        );
        assert_eq!(proto, None); // trust IO
        assert!(source.starts_with("io-query:"));
    }

    #[test]
    fn resolve_env_iterm2_override_over_kitty_io() {
        // Not in tmux, env says iTerm2, IO detected Kitty.
        // iTerm2 override still works via env path.
        let mut picker = ratatui_image::picker::Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        let (proto, _source) = resolve_image_protocol(
            "auto",
            &picker,
            "iterm2",
            Some(ProtocolType::Iterm2),
            "env:ITERM_SESSION_ID".into(),
            false,
        );
        assert_eq!(proto, Some(ProtocolType::Iterm2));
    }

    #[test]
    fn resolve_shim_env_overrides_io_detection() {
        // Socket-attached shim says Kitty (subterm) — should override stale IO.
        let mut picker = ratatui_image::picker::Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Iterm2); // stale IO result
        let (proto, source) = resolve_image_protocol(
            "auto",
            &picker,
            "subterm",
            Some(ProtocolType::Kitty),
            "env:LC_TERMINAL=subterm".into(),
            true,
        );
        assert_eq!(proto, Some(ProtocolType::Kitty));
        assert_eq!(source, "env:LC_TERMINAL=subterm");
    }
}

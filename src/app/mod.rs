mod backlog;
mod dcc;
mod image;
mod input;
mod irc;
mod maintenance;
mod mentions;
mod scripting;
mod session;
mod shell;
mod web;
mod who;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use chrono::Utc;
use color_eyre::eyre::Result;
use crossterm::event::{self, Event};
use ratatui::layout::Rect;
use tokio::sync::mpsc;
use tokio::time::{Duration, interval};

use crate::config::{self, AppConfig};
use crate::constants;
use crate::irc::{IrcEvent, IrcHandle};
use crate::state::AppState;
use crate::state::buffer::{
    ActivityLevel, Buffer, BufferType, Message, MessageType, make_buffer_id,
};
use crate::state::connection::{Connection, ConnectionStatus};
use crate::theme::{self, ThemeFile};
use crate::ui;
use crate::ui::layout::UiRegions;

use ratatui_image::picker::ProtocolType;

/// Maximum number of lines queued from a multiline paste.
const MAX_PASTE_LINES: usize = 1000;

/// Maximum alias recursion depth (prevents infinite loops from circular aliases).
const MAX_ALIAS_DEPTH: u8 = 10;

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
    let get_env = |key: &str| -> Option<String> {
        env_override.map_or_else(
            || std::env::var(key).ok().filter(|s| !s.is_empty()),
            |vars| vars.get(key).cloned(),
        )
    };

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

    let lc_terminal = get_env("LC_TERMINAL").unwrap_or_default();

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

    if get_env("ITERM_SESSION_ID").is_some() {
        return (
            "iterm2",
            Some(ProtocolType::Iterm2),
            "env:ITERM_SESSION_ID".into(),
        );
    }

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

    if !in_tmux {
        let tp = get_env("TERM_PROGRAM").unwrap_or_default();
        if !tp.is_empty()
            && tp != "tmux"
            && let Some((name, proto)) = match_terminal(&tp)
        {
            return (name, Some(proto), format!("env:TERM_PROGRAM={tp}"));
        }
    }

    let term = get_env("TERM").unwrap_or_default();
    if let Some((name, proto)) = match_terminal(&term) {
        return (name, Some(proto), format!("env:TERM={term}"));
    }

    ("unknown", None, "auto:unknown".into())
}

/// Match a terminal identifier string to a terminal name and image protocol.
fn match_terminal(name: &str) -> Option<(&'static str, ProtocolType)> {
    let contains = |needle: &str| -> bool {
        name.as_bytes()
            .windows(needle.len())
            .any(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
    };

    if contains("iterm") {
        return Some(("iterm2", ProtocolType::Iterm2));
    }
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

/// Resolve the image protocol to use.
fn resolve_image_protocol(
    config_protocol: &str,
    picker: &ratatui_image::picker::Picker,
    outer_terminal: &str,
    outer_proto: Option<ProtocolType>,
    outer_source: String,
    env_is_authoritative: bool,
) -> (Option<ProtocolType>, String) {
    match config_protocol {
        "kitty" => return (Some(ProtocolType::Kitty), "config:kitty".into()),
        "iterm2" => return (Some(ProtocolType::Iterm2), "config:iterm2".into()),
        "sixel" => return (Some(ProtocolType::Sixel), "config:sixel".into()),
        "halfblocks" => return (Some(ProtocolType::Halfblocks), "config:halfblocks".into()),
        _ => {}
    }

    if (outer_source.starts_with("tmux:") || env_is_authoritative)
        && let Some(proto) = outer_proto
    {
        return (Some(proto), outer_source);
    }

    if outer_terminal == "iterm2" {
        return (
            Some(ProtocolType::Iterm2),
            format!("iterm2-override:{outer_source}"),
        );
    }

    if picker.protocol_type() != ProtocolType::Halfblocks {
        return (None, format!("io-query:{:?}", picker.protocol_type()));
    }

    if let Some(proto) = outer_proto {
        return (Some(proto), outer_source);
    }

    (None, "auto:halfblocks".into())
}

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

/// Expand an alias template with positional args and context variables.
fn expand_alias_template(
    template: &str,
    args: &[String],
    channel: &str,
    nick: &str,
    server: &str,
) -> String {
    let mut body = template.to_string();

    if !body.contains('$') {
        body.push_str(" $*");
    }

    body = body.replace("${C}", channel);
    body = body.replace("$C", channel);
    body = body.replace("${N}", nick);
    body = body.replace("$N", nick);
    body = body.replace("${S}", server);
    body = body.replace("$S", server);
    body = body.replace("${T}", channel);
    body = body.replace("$T", channel);

    for i in (0..=9).rev() {
        let range_var = format!("${i}-");
        if body.contains(&range_var) {
            let val = if i < args.len() {
                args[i..].join(" ")
            } else {
                String::new()
            };
            body = body.replace(&range_var, &val);
        }
    }

    let all_args = args.join(" ");
    body = body.replace("$*", &all_args);
    body = body.replace("$-", &all_args);

    for i in (0..=9).rev() {
        let var = format!("${i}");
        let val = args.get(i).map_or("", String::as_str);
        body = body.replace(&var, val);
    }

    body.trim().to_string()
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
    pub(crate) script_snapshot_dirty: bool,
    pub splash_visible: usize,
    pub splash_done: bool,
    pub scroll_offset: usize,
    pub ui_regions: Option<UiRegions>,
    pub irc_handles: HashMap<String, IrcHandle>,
    pub(crate) forwarder_handles: HashMap<String, tokio::task::JoinHandle<()>>,
    pub irc_tx: mpsc::Sender<IrcEvent>,
    pub(crate) irc_rx: mpsc::Receiver<IrcEvent>,
    pub(crate) last_esc_time: Option<Instant>,
    pub buffer_list_scroll: usize,
    pub buffer_list_total: usize,
    pub nick_list_scroll: usize,
    pub nick_list_total: usize,
    pub lag_pings: HashMap<String, Instant>,
    pub(crate) batch_trackers: HashMap<String, crate::irc::batch::BatchTracker>,
    pub storage: Option<crate::storage::Storage>,
    pub(crate) last_event_purge: Instant,
    pub(crate) last_mention_purge: Instant,
    pub quit_message: Option<String>,
    pub image_preview: crate::image_preview::PreviewStatus,
    pub image_clear_rect: Option<Rect>,
    pub(crate) preview_rx: mpsc::Receiver<crate::image_preview::ImagePreviewEvent>,
    pub(crate) preview_tx: mpsc::Sender<crate::image_preview::ImagePreviewEvent>,
    pub http_client: reqwest::Client,
    pub picker: ratatui_image::picker::Picker,
    pub in_tmux: bool,
    pub needs_full_redraw: bool,
    pub outer_terminal: String,
    pub color_support: crate::nick_color::ColorSupport,
    pub image_proto_source: String,
    pub shim_term_env: Option<std::collections::HashMap<String, String>>,
    pub(crate) channel_query_queues: HashMap<String, VecDeque<String>>,
    pub(crate) channel_query_in_flight: HashMap<String, HashSet<String>>,
    pub(crate) channel_query_sent_at: HashMap<String, Instant>,
    pub(crate) paste_queue: VecDeque<String>,
    pub script_manager: Option<crate::scripting::engine::ScriptManager>,
    pub script_api: Option<crate::scripting::engine::ScriptAPI>,
    pub script_state: Arc<std::sync::RwLock<crate::scripting::engine::ScriptStateSnapshot>>,
    pub(crate) script_action_rx: mpsc::Receiver<crate::scripting::ScriptAction>,
    pub script_commands: HashMap<String, (String, String)>,
    pub script_config: HashMap<(String, String), String>,
    pub(crate) active_timers: HashMap<u64, tokio::task::JoinHandle<()>>,
    pub(crate) script_action_tx: mpsc::Sender<crate::scripting::ScriptAction>,
    pub wrap_indent: usize,
    pub cached_config_toml: Option<toml::Value>,
    pub terminal: Option<ui::Tui>,
    pub detached: bool,
    pub should_detach: bool,
    pub(crate) socket_listener: Option<tokio::net::UnixListener>,
    pub(crate) socket_output_tx:
        Option<tokio::sync::mpsc::Sender<crate::session::protocol::MainMessage>>,
    pub(crate) shim_event_rx:
        Option<tokio::sync::mpsc::Receiver<crate::session::protocol::ShimMessage>>,
    pub is_socket_attached: bool,
    pub(crate) term_reader_stop: Arc<AtomicBool>,
    pub(crate) term_rx: Option<tokio::sync::mpsc::Receiver<crossterm::event::Event>>,
    pub(crate) shim_output_handle: Option<tokio::task::JoinHandle<()>>,
    pub(crate) shim_input_handle: Option<tokio::task::JoinHandle<()>>,
    pub cached_term_cols: u16,
    pub cached_term_rows: u16,
    pub dcc: crate::dcc::DccManager,
    pub(crate) dcc_rx: mpsc::Receiver<crate::dcc::DccEvent>,
    pub shell_mgr: crate::shell::ShellManager,
    pub(crate) shell_rx: mpsc::Receiver<crate::shell::ShellEvent>,
    pub shell_input_active: bool,
    pub(crate) last_shell_web_broadcast: Instant,
    pub(crate) shell_broadcast_pending: Option<String>,
    pub spellchecker: Option<crate::spellcheck::SpellChecker>,
    pub(crate) dict_rx: mpsc::Receiver<crate::spellcheck::DictEvent>,
    pub dict_tx: mpsc::Sender<crate::spellcheck::DictEvent>,
    pub web_broadcaster: std::sync::Arc<crate::web::broadcast::WebBroadcaster>,
    pub(crate) web_cmd_rx: mpsc::Receiver<(crate::web::protocol::WebCommand, String)>,
    pub(crate) web_cmd_tx: mpsc::Sender<(crate::web::protocol::WebCommand, String)>,
    pub(crate) web_server_handle: Option<tokio::task::JoinHandle<()>>,
    pub(crate) web_sessions:
        Option<std::sync::Arc<tokio::sync::Mutex<crate::web::auth::SessionStore>>>,
    pub(crate) web_rate_limiter:
        Option<std::sync::Arc<tokio::sync::Mutex<crate::web::auth::RateLimiter>>>,
    pub(crate) web_state_snapshot:
        Option<std::sync::Arc<std::sync::RwLock<crate::web::server::WebStateSnapshot>>>,
    pub web_restart_pending: bool,
    /// Tracks the current local date for emitting "day changed" markers.
    pub(crate) last_day: chrono::NaiveDate,
}

impl App {
    /// Connection ID for the app-level default Status buffer.
    pub const DEFAULT_CONN_ID: &'static str = "_default";

    #[allow(clippy::too_many_lines)]
    pub fn new() -> Result<Self> {
        constants::ensure_config_dir();
        let mut config = config::load_config(&constants::config_path())?;

        let env_vars = config::load_env(&constants::env_path())?;
        config::apply_credentials(&mut config.servers, &env_vars);
        config::apply_web_credentials(&mut config.web, &env_vars);
        let theme_path = constants::theme_dir().join(format!("{}.theme", config.general.theme));
        let theme = theme::load_theme(&theme_path)?;

        let mut state = AppState::new();
        state.flood_protection = config.general.flood_protection;
        state.ignores.clone_from(&config.ignores);
        state.scrollback_limit = config.display.scrollback_lines;
        state.nick_color_sat = config.display.nick_color_saturation;
        state.nick_color_lit = config.display.nick_color_lightness;
        let (irc_tx, irc_rx) = mpsc::channel(4096);

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

        // RPE2E manager — needs storage to be up. The keyring shares the
        // same SQLite connection owned by Storage.
        if config.e2e.enabled
            && let Some(storage_ref) = storage.as_ref()
        {
            let keyring = crate::e2e::keyring::Keyring::new(storage_ref.db.clone());
            match crate::e2e::E2eManager::load_or_init_with_config(keyring, &config.e2e) {
                Ok(mgr) => {
                    let fp = mgr.fingerprint();
                    tracing::info!(
                        "e2e: manager initialized, fingerprint={}",
                        crate::e2e::crypto::fingerprint::fingerprint_hex(&fp)
                    );
                    state.e2e_manager = Some(std::sync::Arc::new(mgr));
                }
                Err(e) => tracing::error!("e2e: manager init failed: {e}"),
            }
        }

        let (preview_tx, preview_rx) = mpsc::channel(64);

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

        let (outer_terminal, outer_proto, outer_source) = detect_outer_terminal(in_tmux, None);
        tracing::info!(
            outer_terminal = %outer_terminal,
            outer_proto = ?outer_proto,
            outer_source = %outer_source,
            "outer terminal detected"
        );

        let (resolved_proto, source) = resolve_image_protocol(
            &config.image_preview.protocol,
            &picker,
            outer_terminal,
            outer_proto,
            outer_source,
            false,
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

        let (dict_tx, dict_rx) = mpsc::channel(64);
        let (web_tx, web_rx) = mpsc::channel(256);

        let (mut dcc, dcc_rx) = crate::dcc::DccManager::new();
        dcc.timeout_secs = config.dcc.timeout;
        if !config.dcc.own_ip.is_empty() {
            dcc.own_ip = config.dcc.own_ip.parse().ok();
        }
        dcc.port_range = crate::dcc::chat::parse_port_range(&config.dcc.port_range);
        dcc.autoaccept_lowports = config.dcc.autoaccept_lowports;
        dcc.autochat_masks.clone_from(&config.dcc.autochat_masks);
        dcc.max_connections = config.dcc.max_connections;

        let (shell_mgr, shell_rx) = crate::shell::ShellManager::new();

        let (script_action_tx, script_action_rx) = mpsc::channel(1024);
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

        let color_support = crate::nick_color::detect_color_support(outer_terminal);
        tracing::info!(%outer_terminal, ?color_support, "terminal color support detected");

        let mut app = Self {
            state,
            config,
            theme,
            input: ui::input::InputState::new(),
            should_quit: false,
            script_snapshot_dirty: true,
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
            last_event_purge: Instant::now(),
            last_mention_purge: Instant::now(),
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
            color_support,
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
            shell_mgr,
            shell_rx,
            shell_input_active: false,
            last_shell_web_broadcast: Instant::now(),
            shell_broadcast_pending: None,
            spellchecker: None,
            dict_rx,
            dict_tx,
            web_broadcaster: std::sync::Arc::new(crate::web::broadcast::WebBroadcaster::new(2048)),
            web_cmd_rx: web_rx,
            web_cmd_tx: web_tx,
            web_server_handle: None,
            web_sessions: None,
            web_rate_limiter: None,
            web_state_snapshot: None,
            web_restart_pending: false,
            last_day: chrono::Local::now().date_naive(),
        };
        app.recompute_wrap_indent();

        if app.config.spellcheck.enabled {
            app.init_spellchecker();
        }

        Ok(app)
    }

    /// Recompute the cached wrap-indent width used by `chat_view`.
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

    /// Animated splash screen.
    async fn run_splash(&mut self) -> Result<()> {
        const LINE_DELAY_MS: u64 = 50;
        const HOLD_MS: u64 = 2500;

        let Some(terminal) = self.terminal.as_mut() else {
            self.splash_done = true;
            return Ok(());
        };
        let total_lines = include_str!("../../logo.txt").lines().count();

        let mut line_tick = interval(Duration::from_millis(LINE_DELAY_MS));

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
        let (term_tx, term_rx) = mpsc::channel(4096);
        let stop = Arc::clone(&self.term_reader_stop);
        self.term_reader_stop.store(false, Ordering::Relaxed);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if event::poll(std::time::Duration::from_millis(100)).unwrap_or(false) {
                    match event::read() {
                        Ok(ev) => {
                            if term_tx.blocking_send(ev).is_err() {
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
    pub const fn terminal_size(&self) -> (u16, u16) {
        (self.cached_term_cols, self.cached_term_rows)
    }

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
            messages: VecDeque::new(),
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
            peer_handle: None,
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
                tags: None,
            },
        );
    }

    /// Recreate the default Status buffer if no real buffers remain.
    pub fn ensure_default_status(&mut self) {
        let has_real_buffers = self
            .state
            .buffers
            .values()
            .any(|b| b.connection_id != Self::DEFAULT_CONN_ID);
        if !has_real_buffers {
            Self::create_default_status(&mut self.state);
        }
    }

    #[allow(clippy::too_many_lines)]
    pub async fn run(&mut self) -> Result<()> {
        crate::session::cleanup_stale_sockets();

        if self.terminal.is_some() && !self.is_socket_attached {
            self.run_splash().await?;
        }

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

        if self.state.buffers.is_empty() {
            Self::create_default_status(&mut self.state);
        }

        self.autoload_scripts();

        if self.config.display.mentions_buffer {
            self.create_mentions_buffer();
        }

        if self.terminal.is_some() && !self.is_socket_attached {
            self.start_term_reader();
        }

        if let Err(e) = self.start_socket_listener() {
            tracing::warn!("failed to start session socket: {e}");
        }

        self.start_web_server().await;

        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;

        let mut tick = interval(Duration::from_secs(1));
        let mut paste_tick = interval(Duration::from_millis(500));
        let shell_broadcast_sleep = tokio::time::sleep(std::time::Duration::from_secs(86400));
        tokio::pin!(shell_broadcast_sleep);

        while !self.should_quit {
            if self.should_detach {
                self.perform_detach();
            }

            if self.web_restart_pending {
                self.web_restart_pending = false;
                self.stop_web_server();
                self.start_web_server().await;
            }

            if let Some(mut terminal) = self.terminal.take() {
                if self.needs_full_redraw {
                    let _ = terminal.clear();
                    self.needs_full_redraw = false;
                }
                match terminal.draw(|frame| ui::layout::draw(frame, self)) {
                    Ok(_) => {
                        self.terminal = Some(terminal);
                    }
                    Err(e) => {
                        tracing::warn!("terminal draw failed, triggering detach: {e}");
                        self.should_detach = true;
                    }
                }
            }

            if self.terminal.is_some() {
                self.write_tmux_direct_image();
            }

            tokio::select! {
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
                        self.drain_pending_web_events();
                    }
                    None => {
                        self.term_rx = None;
                    }
                },
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
                        self.drain_pending_web_events();
                    }
                    Some(crate::session::protocol::ShimMessage::Resize { cols, rows }) => {
                        self.cached_term_cols = cols;
                        self.cached_term_rows = rows;
                        if let Some(ref mut terminal) = self.terminal {
                            let _ = terminal.resize(ratatui::layout::Rect::new(0, 0, cols, rows));
                            self.needs_full_redraw = true;
                        }
                        self.resize_all_shells();
                    }
                    Some(crate::session::protocol::ShimMessage::Detach) => {
                        self.should_detach = true;
                    }
                    None => {
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
                        self.script_snapshot_dirty = true;
                        self.update_script_snapshot();
                        self.drain_pending_web_events();
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
                        self.drain_pending_web_events();
                    }
                },
                shell_ev = self.shell_rx.recv() => {
                    if let Some(ev) = shell_ev {
                        self.handle_shell_event(ev);
                        if self.shell_broadcast_pending.is_some() {
                            shell_broadcast_sleep.as_mut().reset(
                                tokio::time::Instant::now() + std::time::Duration::from_millis(150)
                            );
                        }
                    }
                },
                dict_ev = self.dict_rx.recv() => {
                    if let Some(ev) = dict_ev {
                        self.handle_dict_event(ev);
                    }
                },
                web_cmd = self.web_cmd_rx.recv() => {
                    if let Some((cmd, session_id)) = web_cmd {
                        tracing::debug!(?cmd, %session_id, "web command received");
                        self.handle_web_command(cmd, &session_id);
                        self.drain_pending_web_events();
                    }
                },
                () = &mut shell_broadcast_sleep, if self.shell_broadcast_pending.is_some() => {
                    if let Some(shell_id) = self.shell_broadcast_pending.take() {
                        if self.shell_mgr.is_web_session(&shell_id) {
                            self.force_broadcast_web_shell_screen(&shell_id);
                        } else {
                            self.force_broadcast_shell_screen(&shell_id);
                        }
                    }
                    shell_broadcast_sleep.as_mut().reset(tokio::time::Instant::now() + std::time::Duration::from_secs(86400));
                },
                _ = tick.tick() => {
                    self.handle_netsplit_tick();
                    self.purge_expired_batches();
                    self.check_reconnects();
                    self.measure_lag();
                    self.check_day_changed();
                    if self.script_manager.is_some() && !self.script_commands.is_empty() {
                        self.update_script_snapshot();
                    }
                    self.check_stale_who_batches();
                    if let Some(ref sessions) = self.web_sessions
                        && let Ok(mut s) = sessions.try_lock() { s.purge_expired(); }
                    if let Some(ref limiter) = self.web_rate_limiter
                        && let Ok(mut l) = limiter.try_lock() { l.purge_expired(); }
                    if let Some(ref snapshot) = self.web_state_snapshot
                        && let Ok(mut snap) = snapshot.write()
                    {
                        let mention_count = self.storage.as_ref().and_then(|s| {
                            s.db.try_lock().ok().and_then(|db| {
                                crate::storage::query::get_unread_mention_count(&db).ok()
                            })
                        }).unwrap_or(0);
                        let init = crate::web::snapshot::build_sync_init(&self.state, mention_count, &self.config.web.timestamp_format);
                        if let crate::web::protocol::WebEvent::SyncInit { buffers, connections, mention_count, active_buffer_id, timestamp_format, .. } = init {
                            snap.buffers = buffers;
                            snap.connections = connections;
                            snap.mention_count = mention_count;
                            snap.active_buffer_id = active_buffer_id;
                            snap.timestamp_format = timestamp_format;
                        }
                    }
                    let expired = self.dcc.purge_expired();
                    for (_id, nick) in expired {
                        crate::commands::helpers::add_local_event(
                            self,
                            &format!("DCC CHAT request from {nick} timed out"),
                        );
                    }
                    self.maybe_purge_old_events();
                    self.maybe_purge_old_mentions();
                    self.drain_pending_web_events();
                },
                _ = paste_tick.tick() => {
                    self.drain_paste_queue();
                    self.drain_pending_web_events();
                },
                action = self.script_action_rx.recv() => {
                    if let Some(action) = action {
                        self.handle_script_action(action);
                        while let Ok(action) = self.script_action_rx.try_recv() {
                            self.handle_script_action(action);
                        }
                        self.script_snapshot_dirty = true;
                        self.update_script_snapshot();
                        self.drain_pending_web_events();
                    }
                },
                _ = sigterm.recv() => {
                    self.should_quit = true;
                },
                _ = sigint.recv() => {
                    if self.detached {
                        self.should_quit = true;
                    }
                },
                _ = sighup.recv() => {
                    if !self.detached {
                        tracing::info!("SIGHUP received, auto-detaching");
                        self.should_detach = true;
                    }
                },
            }
        }

        for (_, handle) in self.active_timers.drain() {
            handle.abort();
        }

        for (_, handle) in self.forwarder_handles.drain() {
            handle.abort();
        }

        self.notify_shim_quit();
        self.stop_term_reader();

        self.shell_mgr.kill_all();

        let default_quit = crate::constants::default_quit_message();
        let quit_msg = self.quit_message.as_deref().unwrap_or(&default_quit);
        for handle in self.irc_handles.values() {
            let _ = handle.sender.send_quit(quit_msg);
        }
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        for handle in self.irc_handles.values_mut() {
            if let Some(oh) = handle.outgoing_handle.take() {
                oh.abort();
            }
        }

        if let Some(storage) = self.storage.take() {
            storage.shutdown().await;
        }

        Self::remove_own_socket();

        Ok(())
    }
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
        assert_eq!(proto, None);
        assert!(source.starts_with("io-query:"));
    }

    #[test]
    fn resolve_env_iterm2_override_over_kitty_io() {
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
        let mut picker = ratatui_image::picker::Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Iterm2);
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

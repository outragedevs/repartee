use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use chrono::Utc;
use color_eyre::eyre::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Position;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};

use crate::config::{self, AppConfig};
use crate::constants;
use crate::irc::{self, IrcEvent, IrcHandle};
use crate::state::AppState;
use crate::state::buffer::{
    make_buffer_id, ActivityLevel, Buffer, BufferType, Message, MessageType,
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
fn detect_outer_terminal(in_tmux: bool) -> (&'static str, Option<ProtocolType>, String) {
    // Dump all relevant env vars for debugging.
    tracing::debug!(
        TMUX = ?std::env::var("TMUX").ok(),
        TERM = ?std::env::var("TERM").ok(),
        TERM_PROGRAM = ?std::env::var("TERM_PROGRAM").ok(),
        TERM_PROGRAM_VERSION = ?std::env::var("TERM_PROGRAM_VERSION").ok(),
        LC_TERMINAL = ?std::env::var("LC_TERMINAL").ok(),
        LC_TERMINAL_VERSION = ?std::env::var("LC_TERMINAL_VERSION").ok(),
        ITERM_SESSION_ID = ?std::env::var("ITERM_SESSION_ID").ok(),
        KITTY_PID = ?std::env::var("KITTY_PID").ok(),
        GHOSTTY_RESOURCES_DIR = ?std::env::var("GHOSTTY_RESOURCES_DIR").ok(),
        WT_SESSION = ?std::env::var("WT_SESSION").ok(),
        COLORTERM = ?std::env::var("COLORTERM").ok(),
        in_tmux,
        "outer terminal env vars"
    );

    // ── tmux: query the REAL outer terminal ──
    if in_tmux {
        // #{client_termtype} returns the actual terminal identity
        // (e.g. "iTerm2 3.6.8", "ghostty 1.3.0", "subterm 1.0")
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
            return (name, Some(proto), format!("tmux:client_termtype={tt}"));
        }
        if let Some(ref tn) = termname
            && let Some((name, proto)) = match_terminal(tn)
        {
            return (name, Some(proto), format!("tmux:client_termname={tn}"));
        }

        // Alacritty: generic termtype like "xterm-256color" + empty termname.
        // No image protocol support — use halfblocks.
        let tt_generic = termtype.as_deref().unwrap_or("").starts_with("xterm");
        let tn_empty = termname.as_deref().unwrap_or("").is_empty();
        if tt_generic && tn_empty {
            return ("alacritty", Some(ProtocolType::Halfblocks), "tmux:generic-xterm+empty-termname".into());
        }
    }

    // ── env var detection (works both direct and in tmux) ──
    let lc_terminal = std::env::var("LC_TERMINAL").unwrap_or_default();

    // LC_TERMINAL survives tmux and SSH — most reliable after tmux queries.
    if !lc_terminal.is_empty() {
        if lc_terminal.eq_ignore_ascii_case("iterm2") || lc_terminal.to_ascii_lowercase().contains("iterm") {
            return ("iterm2", Some(ProtocolType::Iterm2), format!("env:LC_TERMINAL={lc_terminal}"));
        }
        if lc_terminal.eq_ignore_ascii_case("ghostty") {
            return ("ghostty", Some(ProtocolType::Kitty), format!("env:LC_TERMINAL={lc_terminal}"));
        }
        if lc_terminal.eq_ignore_ascii_case("subterm") {
            return ("subterm", Some(ProtocolType::Kitty), format!("env:LC_TERMINAL={lc_terminal}"));
        }
    }

    // Terminal-specific env vars.
    if std::env::var("ITERM_SESSION_ID").is_ok_and(|s| !s.is_empty()) {
        return ("iterm2", Some(ProtocolType::Iterm2), "env:ITERM_SESSION_ID".into());
    }

    // GHOSTTY_RESOURCES_DIR — validate it's a real path, not just "1" or garbage.
    if let Ok(grd) = std::env::var("GHOSTTY_RESOURCES_DIR")
        && !grd.is_empty()
        && grd.len() > 1
    {
        return ("ghostty", Some(ProtocolType::Kitty), format!("env:GHOSTTY_RESOURCES_DIR={grd}"));
    }

    if std::env::var("KITTY_PID").is_ok_and(|s| !s.is_empty()) {
        return ("kitty", Some(ProtocolType::Kitty), "env:KITTY_PID".into());
    }
    if std::env::var("WEZTERM_EXECUTABLE").is_ok_and(|s| !s.is_empty()) {
        return ("wezterm", Some(ProtocolType::Iterm2), "env:WEZTERM_EXECUTABLE".into());
    }
    if std::env::var("WT_SESSION").is_ok_and(|s| !s.is_empty()) {
        return ("windows-terminal", Some(ProtocolType::Sixel), "env:WT_SESSION".into());
    }

    // Non-tmux: TERM_PROGRAM is the actual terminal.
    if !in_tmux {
        let tp = std::env::var("TERM_PROGRAM").unwrap_or_default();
        if !tp.is_empty()
            && tp != "tmux"
            && let Some((name, proto)) = match_terminal(&tp)
        {
            return (name, Some(proto), format!("env:TERM_PROGRAM={tp}"));
        }
    }

    // ── Generic TERM value — last resort ──
    let term = std::env::var("TERM").unwrap_or_default();
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
) -> (Option<ProtocolType>, String) {
    // 1. Config override — user explicitly chose a protocol.
    match config_protocol {
        "kitty" => return (Some(ProtocolType::Kitty), "config:kitty".into()),
        "iterm2" => return (Some(ProtocolType::Iterm2), "config:iterm2".into()),
        "sixel" => return (Some(ProtocolType::Sixel), "config:sixel".into()),
        "halfblocks" => return (Some(ProtocolType::Halfblocks), "config:halfblocks".into()),
        _ => {} // "auto" or anything else — proceed to detection
    }

    // 2. tmux client detection is authoritative — #{client_termtype} tells us
    //    the REAL terminal program attached to the pane.  IO queries go through
    //    DCS passthrough and might hit the wrong client when multiple are
    //    attached; env vars can be stale.  Always trust tmux over IO.
    if outer_source.starts_with("tmux:")
        && let Some(proto) = outer_proto
    {
        return (Some(proto), outer_source);
    }

    // 3. iTerm2 env-based override (non-tmux path) — iTerm2 3.5+ responds to
    //    the Kitty graphics probe, so ratatui-image detects Kitty. But iTerm2's
    //    Kitty implementation is incomplete; the native iTerm2 (OSC 1337)
    //    protocol works correctly.
    if outer_terminal == "iterm2" {
        return (Some(ProtocolType::Iterm2), format!("iterm2-override:{outer_source}"));
    }

    // 4. IO-based detection — trust it when running directly (not tmux).
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
    /// Detected outer terminal name (e.g. "ghostty", "iterm2", "kitty").
    pub outer_terminal: String,
    /// How the image protocol was resolved (for diagnostics).
    pub image_proto_source: String,
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
}

impl App {
    #[allow(clippy::too_many_lines)]
    pub fn new() -> Result<Self> {
        constants::ensure_config_dir();
        let mut config = config::load_config(&constants::config_path())?;

        // Load .env credentials and apply to server configs
        let env_vars = config::load_env(&constants::env_path())?;
        config::apply_credentials(&mut config.servers, &env_vars);
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
                    state.log_exclude_types.clone_from(&config.logging.exclude_types);
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
        let (outer_terminal, outer_proto, outer_source) = detect_outer_terminal(in_tmux);
        tracing::info!(
            outer_terminal = %outer_terminal,
            outer_proto = ?outer_proto,
            outer_source = %outer_source,
            "outer terminal detected"
        );

        // Apply protocol override from config, outer terminal, or IO result.
        let (resolved_proto, source) =
            resolve_image_protocol(
                &config.image_preview.protocol,
                &picker,
                outer_terminal,
                outer_proto,
                outer_source,
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

        // --- Scripting system ---
        let (script_action_tx, script_action_rx) = mpsc::unbounded_channel();
        let script_state = Arc::new(std::sync::RwLock::new(
            state.script_snapshot(),
        ));
        let next_timer_id = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let script_api = Self::build_script_api(
            script_action_tx.clone(),
            Arc::clone(&script_state),
            Arc::clone(&next_timer_id),
        );
        let mut script_manager = crate::scripting::engine::ScriptManager::new(constants::scripts_dir());
        match crate::scripting::lua::LuaEngine::new() {
            Ok(lua_engine) => {
                script_manager.register_engine(Box::new(lua_engine));
                tracing::info!("Lua scripting engine registered");
            }
            Err(e) => {
                tracing::error!("failed to initialize Lua engine: {e}");
            }
        }

        Ok(Self {
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
            preview_rx,
            preview_tx,
            http_client,
            picker,
            in_tmux,
            outer_terminal: outer_terminal.to_string(),
            image_proto_source: source,
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
        })
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
            | crate::image_preview::PreviewStatus::Ready { url: u, .. } if u == url => return,
            _ => {}
        }

        // Re-detect terminal and protocol before every preview.
        self.refresh_image_protocol();

        self.image_preview = crate::image_preview::PreviewStatus::Loading {
            url: url.to_string(),
        };

        let term_size = crossterm::terminal::size().unwrap_or((80, 24));

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
        let in_tmux = std::env::var("TMUX").is_ok_and(|s| !s.is_empty());
        self.in_tmux = in_tmux;

        let (outer_terminal, outer_proto, outer_source) = detect_outer_terminal(in_tmux);

        let (resolved_proto, source) = resolve_image_protocol(
            &self.config.image_preview.protocol,
            &self.picker,
            outer_terminal,
            outer_proto,
            outer_source,
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
    pub fn dismiss_image_preview(&mut self) {
        // Send protocol-specific cleanup before clearing the state.
        // Kitty graphics persist on a separate layer — terminals like Subterm
        // don't auto-remove them when the unicode placeholders disappear,
        // leaving pixel "garbage". Send an explicit delete command.
        if matches!(self.image_preview, crate::image_preview::PreviewStatus::Ready { .. }) {
            self.cleanup_image_graphics();
        }
        self.image_preview = crate::image_preview::PreviewStatus::Hidden;
    }

    /// Send protocol-specific escape sequences to clear image graphics.
    ///
    /// For Kitty protocol: `ESC_Ga=d,q=2 ESC\` deletes all graphics.
    /// In tmux: wrapped in DCS passthrough so it reaches the outer terminal.
    fn cleanup_image_graphics(&self) {
        use std::io::Write;

        if self.picker.protocol_type() == ProtocolType::Kitty {
            let seq = if self.in_tmux {
                // DCS passthrough: ESC P tmux; <escaped-seq> ESC backslash
                "\x1bPtmux;\x1b\x1b_Ga=d,q=2\x1b\x1b\\\x1b\\"
            } else {
                "\x1b_Ga=d,q=2\x1b\\"
            };
            let _ = std::io::stdout().write_all(seq.as_bytes());
            let _ = std::io::stdout().flush();
        }
        // iTerm2 / Sixel / Halfblocks: image is in the cell buffer,
        // ratatui's Clear widget + redraw handles cleanup.
    }

    /// Write iTerm2 image directly to stdout for tmux passthrough.
    ///
    /// ratatui-image embeds the entire DCS-wrapped sequence as a cell symbol,
    /// but that approach doesn't work reliably through tmux. Instead, write
    /// the OSC 1337 sequence directly to stdout (wrapped in DCS passthrough),
    /// matching kokoirc's proven approach.
    ///
    /// Called AFTER `terminal.draw()` so ratatui has already flushed the
    /// border/popup. The image is drawn on top at the correct position.
    ///
    /// Matches kokoirc's write strategy exactly:
    ///   1. Disable mouse tracking (prevents interference during DCS write)
    ///   2. Save cursor + position at inner area
    ///   3. Write DCS-wrapped OSC 1337 with immediate flush
    ///   4. Restore cursor
    ///   5. Re-enable mouse tracking
    pub fn write_iterm2_tmux_direct(&self) {
        use std::io::Write;

        // Mouse tracking modes — must be disabled during DCS image write to
        // prevent interference with tmux passthrough (matches kokoirc).
        const MOUSE_DISABLE: &[u8] =
            b"\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l";
        const MOUSE_ENABLE: &[u8] =
            b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h";

        // Only for iTerm2+tmux with a Ready preview.
        if !self.in_tmux || self.picker.protocol_type() != ProtocolType::Iterm2 {
            return;
        }

        let (raw_png, popup_width, popup_height) = match &self.image_preview {
            crate::image_preview::PreviewStatus::Ready {
                raw_png,
                width,
                height,
                ..
            } => (raw_png, *width, *height),
            _ => return,
        };

        if raw_png.is_empty() {
            return;
        }

        // Calculate popup position (must match image_overlay::centered_rect).
        let term_size = crossterm::terminal::size().unwrap_or((80, 24));
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
            inner_w,
            inner_h,
            inner_x,
            inner_y,
            png_len = raw_png.len(),
            "writing iTerm2+tmux direct image"
        );

        // Encode: base64 the PNG data.
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw_png);

        // Build the iTerm2 OSC 1337 sequence (kokoirc-style: cell dims, preserveAspectRatio=0).
        let osc = format!(
            "\x1b]1337;File=inline=1;width={inner_w};height={inner_h};preserveAspectRatio=0:{b64}\x07"
        );

        // Wrap in tmux DCS passthrough: double all ESC bytes in the payload.
        let escaped = osc.replace('\x1b', "\x1b\x1b");
        let dcs = format!("\x1bPtmux;{escaped}\x1b\\");

        // ── Write sequence (matches kokoirc render.ts exactly) ──
        //
        // Use per-step flush for immediate delivery to tmux,
        // matching kokoirc's writeSync(1, ...) pattern.
        // Terminal rows/cols are 1-based for CUP.
        let row = inner_y + 1;
        let col = inner_x + 1;
        let cursor_save = format!("\x1b7\x1b[{row};{col}H");

        let mut out = std::io::stdout().lock();

        // Step 1: Disable mouse tracking.
        let _ = out.write_all(MOUSE_DISABLE);
        let _ = out.flush();

        // Step 2: Save cursor + position.
        let _ = out.write_all(cursor_save.as_bytes());
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
        let reconnect_max = server_config.reconnect_max_retries.unwrap_or(10);

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
            max_reconnect_attempts: reconnect_max,
            reconnect_delay_secs: reconnect_delay,
            next_reconnect: None,
            should_reconnect: auto_reconnect,
            joined_channels: server_config.channels.clone(),
            origin_config: server_config.clone(),
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
                event_params: None, log_msg_id: None, log_ref_id: None,
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
                self.irc_handles.insert(conn_id.clone(), handle);

                // Spawn task to forward events from per-connection receiver to shared channel
                tokio::spawn(async move {
                    while let Some(event) = rx.recv().await {
                        if tx.send(event).is_err() {
                            break;
                        }
                    }
                });
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
    async fn run_splash(&mut self, terminal: &mut ui::Tui) -> Result<()> {
        const LINE_DELAY_MS: u64 = 50;
        const HOLD_MS: u64 = 2500;
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
        terminal.draw(|frame| ui::splash::render(frame, total_lines))?;
        let hold_start = Instant::now();
        while hold_start.elapsed() < Duration::from_millis(HOLD_MS) {
            let remaining = Duration::from_millis(HOLD_MS)
                .saturating_sub(hold_start.elapsed());
            if remaining.is_zero() {
                break;
            }
            if let Ok(Some(Event::Key(_))) = tokio::task::spawn_blocking(move || {
                if event::poll(remaining.min(Duration::from_millis(100))).unwrap_or(false) {
                    event::read().ok()
                } else {
                    None
                }
            }).await {
                break;
            }
        }

        self.splash_done = true;
        Ok(())
    }

    pub async fn run(&mut self, terminal: &mut ui::Tui) -> Result<()> {
        // --- Splash screen ---
        self.run_splash(terminal).await?;

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

        // Spawn a dedicated blocking task for terminal event reading.
        // Uses poll() with a short timeout so the thread can check the
        // stop flag and exit cleanly when the app quits.
        let (term_tx, mut term_rx) = mpsc::unbounded_channel();
        let reader_stop = Arc::new(AtomicBool::new(false));
        let reader_stop2 = Arc::clone(&reader_stop);
        tokio::task::spawn_blocking(move || {
            while !reader_stop2.load(Ordering::Relaxed) {
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

        let mut tick = interval(Duration::from_secs(1));
        let mut paste_tick = interval(Duration::from_millis(500));

        while !self.should_quit {
            terminal.draw(|frame| ui::layout::draw(frame, self))?;

            // iTerm2+tmux: write image directly to stdout after ratatui
            // has flushed the frame (border/popup already drawn).
            self.write_iterm2_tmux_direct();

            tokio::select! {
                ev = term_rx.recv() => match ev {
                    Some(ev) => {
                        self.handle_event(ev);
                        // Drain all queued events before redrawing.
                        while let Ok(ev) = term_rx.try_recv() {
                            self.handle_event(ev);
                        }
                        self.update_script_snapshot();
                    }
                    None => break,
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
                _ = tick.tick() => {
                    self.handle_netsplit_tick();
                    self.purge_expired_batches();
                    self.check_reconnects();
                    self.measure_lag();
                    self.update_script_snapshot();
                    self.check_stale_who_batches();
                },
                _ = paste_tick.tick() => {
                    self.drain_paste_queue();
                },
                action = self.script_action_rx.recv() => {
                    if let Some(action) = action {
                        self.handle_script_action(action);
                        // Drain any queued actions
                        while let Ok(action) = self.script_action_rx.try_recv() {
                            self.handle_script_action(action);
                        }
                        self.update_script_snapshot();
                    }
                }
            }
        }

        // Stop the terminal reader thread so it doesn't interfere with restore.
        reader_stop.store(true, Ordering::Relaxed);

        // Send QUIT to all connected servers (once — cmd_quit defers to here)
        let quit_msg = self.quit_message.as_deref().unwrap_or("Leaving");
        for handle in self.irc_handles.values() {
            let _ = handle.sender.send_quit(quit_msg);
        }

        // Shut down storage writer (flushes remaining rows)
        if let Some(storage) = self.storage.take() {
            storage.shutdown().await;
        }

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
            max_reconnect_attempts: 0,
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
                text: "Welcome to repartee! Use /connect <server> to connect.".to_string(),
                highlight: false,
                event_key: None,
                event_params: None, log_msg_id: None, log_ref_id: None,
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
            },
            ImagePreviewEvent::Error { url, message } => PreviewStatus::Error { url, message },
        };
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
                        event_params: None, log_msg_id: None, log_ref_id: None,
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
        self.state.add_message(
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
            let max = conn.max_reconnect_attempts;
            conn.next_reconnect = None;

            if attempts > max {
                conn.should_reconnect = false;
                let label = conn.label.clone();
                let buffer_id = make_buffer_id(&conn_id, &label);
                self.add_event_to_buffer(
                    &buffer_id,
                    format!("Reconnect failed after {max} attempts. Use /connect to retry."),
                );
                continue;
            }

            let conn = self.state.connections.get(&conn_id);
            let label = conn.map_or_else(|| conn_id.clone(), |c| c.label.clone());
            let server_config = conn.map(|c| c.origin_config.clone());

            let buffer_id = make_buffer_id(&conn_id, &label);
            self.add_event_to_buffer(
                &buffer_id,
                format!("Reconnecting to {label} (attempt {attempts}/{max})..."),
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
        /// Max channels per WHO command. IRCnet ircd 2.12 silently drops
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
            let _ = handle.sender.send(::irc::proto::Command::WHO(
                Some(chanlist.clone()),
                None,
            ));
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

        tracing::trace!(conn_id, remaining = in_flight.len(), "in-flight after removal");

        if in_flight.is_empty() {
            let remaining_queued = self.channel_query_queues.get(conn_id).map_or(0, VecDeque::len);
            tracing::trace!(conn_id, remaining_queued, "batch complete, sending next");
            let conn_id = conn_id.to_string();
            self.channel_query_in_flight.remove(&conn_id);
            self.send_channel_query_batch(&conn_id);
        }
    }

    /// Detect stale WHO batches where the server silently dropped some targets.
    /// If a batch has been in-flight for >30s, clear the silent_who_channels for
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
                    let buf_id = self.state.connections.get(&conn_id)
                        .map_or_else(|| conn_id.clone(), |c| crate::state::buffer::make_buffer_id(&conn_id, &c.label));
                    let msg_id = self.state.next_message_id();
                    self.state.add_message(&buf_id, crate::state::buffer::Message {
                        id: msg_id,
                        timestamp: chrono::Utc::now(),
                        message_type: crate::state::buffer::MessageType::Event,
                        nick: None,
                        nick_mode: None,
                        text: format!("Connection to {conn_id} timed out (no PONG for 5 minutes)"),
                        highlight: false,
                        tags: std::collections::HashMap::new(),
                        log_msg_id: None,
                        log_ref_id: None,
                        event_key: None,
                        event_params: Some(Vec::new()),
                    });
                    if let Some(handle) = self.irc_handles.get(&conn_id) {
                        let _ = handle.sender.send(::irc::proto::Command::QUIT(
                            Some("Ping timeout".to_string()),
                        ));
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
                    let _ = handle.sender.send(::irc::proto::Command::Raw(
                        "PING".to_string(),
                        vec![ts],
                    ));
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
            IrcEvent::HandleReady(conn_id, sender) => {
                self.irc_handles.insert(
                    conn_id.clone(),
                    IrcHandle {
                        conn_id,
                        sender,
                    },
                );
            }
            IrcEvent::NegotiationInfo(conn_id, diag) => {
                // Display CAP/SASL diagnostics in status buffer — fires immediately
                // so they're visible even if connection fails before RPL_WELCOME.
                let buf_id = self.state.connections.get(&conn_id)
                    .map_or_else(|| conn_id.clone(), |c| crate::state::buffer::make_buffer_id(&conn_id, &c.label));
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
                let rejoin_channels =
                    crate::irc::events::channels_to_rejoin(&self.state, &conn_id);
                crate::irc::events::handle_connected(&mut self.state, &conn_id);

                // Notify scripts
                {
                    use crate::scripting::api::events;
                    let nick = self.state.connections.get(&conn_id)
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
                        });
                    }
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
                        let _ = handle.sender.send(::irc::proto::Command::JOIN(
                            chanlist,
                            None,
                            None,
                        ));
                    }
                }
            }
            IrcEvent::Disconnected(conn_id, error) => {
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
                if let ::irc::proto::Command::CAP(_, ref subcmd, ref field3, ref field4) = msg.command {
                    use ::irc::proto::command::CapSubCommand;
                    match subcmd {
                        CapSubCommand::NEW => {
                            let to_request = crate::irc::events::handle_cap_new(
                                &mut self.state, &conn_id,
                                field3.as_deref(), field4.as_deref(),
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
                                &mut self.state, &conn_id,
                                field3.as_deref(), field4.as_deref(),
                            );
                        }
                        CapSubCommand::ACK => {
                            crate::irc::events::handle_cap_ack(
                                &mut self.state, &conn_id,
                                field3.as_deref(), field4.as_deref(),
                            );
                        }
                        CapSubCommand::NAK => {
                            crate::irc::events::handle_cap_nak(
                                &mut self.state, &conn_id,
                                field3.as_deref(), field4.as_deref(),
                            );
                        }
                        _ => {}
                    }
                }

                // --- IRCv3 batch interception ---
                // Handle BATCH commands (start/end) and collect @batch-tagged messages.
                if let ::irc::proto::Command::BATCH(ref ref_tag, ref sub, ref params) = msg.command {
                    let tracker = self.batch_trackers
                        .entry(conn_id.clone())
                        .or_default();
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
                } else if self.batch_trackers
                    .entry(conn_id.clone())
                    .or_default()
                    .is_batched(&msg)
                {
                    // Message belongs to an open batch — collect it, don't process now
                    self.batch_trackers
                        .get_mut(&conn_id)
                        .expect("just inserted")
                        .add_message(*msg);
                } else {
                    // Normal message processing

                    // Extract channel from RPL_ENDOFNAMES (for auto-WHO/MODE batch).
                    let endofnames_channel = if let ::irc::proto::Command::Response(
                        ::irc::proto::Response::RPL_ENDOFNAMES, ref args
                    ) = msg.command {
                        args.get(1).cloned()
                    } else {
                        None
                    };

                    // Extract target from RPL_ENDOFWHO (for batch completion).
                    let endofwho_target = if let ::irc::proto::Command::Response(
                        ::irc::proto::Response::RPL_ENDOFWHO, ref args
                    ) = msg.command {
                        args.get(1).cloned()
                    } else {
                        None
                    };

                    // Update conn.nick from RPL_WELCOME — args[0] is our confirmed nick
                    // after any ERR_NICKNAMEINUSE retries by the irc crate.
                    if let ::irc::proto::Command::Response(
                        ::irc::proto::Response::RPL_WELCOME, ref args
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

                    crate::irc::events::handle_irc_message(&mut self.state, &conn_id, &msg);

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
            // Resize and other events: redraw happens automatically on next loop iteration
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
            // ESC — dismiss image preview if active, otherwise record for ESC+key combo
            (_, KeyCode::Esc) => {
                if matches!(self.image_preview, crate::image_preview::PreviewStatus::Hidden) {
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
                let text = self.input.submit();
                if !text.is_empty() {
                    self.handle_submit(&text);
                }
            }
            (_, KeyCode::Backspace) => self.input.backspace(),
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
            (_, KeyCode::Tab) => self.handle_tab(),
            (mods, KeyCode::Char(c))
                if mods.is_empty() || mods == KeyModifiers::SHIFT =>
            {
                self.input.insert_char(c);
            }
            _ => {}
        }
    }

    fn handle_paste(&mut self, text: &str) {
        let lines: Vec<&str> = text.split('\n').collect();
        let non_empty: Vec<&str> = lines.iter().map(|l| l.trim_end_matches('\r')).filter(|l| !l.is_empty()).collect();

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
                    let visible_h = r.height.saturating_sub(1) as usize; // account for border
                    let max = self.buffer_list_total.saturating_sub(visible_h);
                    if self.buffer_list_scroll < max {
                        self.buffer_list_scroll += 1;
                    }
                } else if let Some(r) = regions.nick_list_area
                    && r.contains(pos)
                {
                    let visible_h = r.height.saturating_sub(1) as usize;
                    let max = self.nick_list_total.saturating_sub(visible_h);
                    if self.nick_list_scroll < max {
                        self.nick_list_scroll += 1;
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // Dismiss image preview on any click (same as ESC).
                if !matches!(self.image_preview, crate::image_preview::PreviewStatus::Hidden) {
                    self.dismiss_image_preview();
                    return;
                }
                if let Some(buf_area) = regions.buffer_list_area
                    && buf_area.contains(pos)
                {
                    let y_offset = (mouse.row - buf_area.y) as usize;
                    self.handle_buffer_list_click(y_offset);
                } else if let Some(nick_area) = regions.nick_list_area
                    && nick_area.contains(pos)
                {
                    let y_offset = (mouse.row - nick_area.y) as usize;
                    self.handle_nick_list_click(y_offset);
                } else if let Some(chat_area) = regions.chat_area
                    && chat_area.contains(pos)
                {
                    let y_offset = (mouse.row - chat_area.y) as usize;
                    self.handle_chat_click(y_offset);
                }
            }
            _ => {}
        }
    }

    fn handle_buffer_list_click(&mut self, y_offset: usize) {
        use crate::state::buffer::BufferType;

        // Account for scroll offset: the visual row maps to a logical row
        let logical_row = y_offset + self.buffer_list_scroll;
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

        // Account for scroll offset
        let logical_row = y_offset + self.nick_list_scroll;

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

    fn handle_tab(&mut self) {
        let nicks: Vec<String> = self
            .state
            .active_buffer()
            .map_or_else(Vec::new, |buf| buf.users.values().map(|e| e.nick.clone()).collect());
        let commands = crate::commands::registry::get_command_names();
        let setting_paths = crate::commands::settings::get_setting_paths(&self.config);
        self.input.tab_complete(&nicks, &commands, &setting_paths);
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
        }) {
            // Script handled the command
        } else {
            crate::commands::helpers::add_local_event(
                self,
                &format!("Unknown command: /{}. Type /help for a list.", parsed.name),
            );
        }
    }

    fn handle_plain_message(&mut self, text: &str) {
        let Some(active_id) = self.state.active_buffer_id.clone() else {
            return;
        };

        let (conn_id, nick, buffer_name) = {
            let Some(buf) = self.state.active_buffer() else {
                return;
            };
            // Only send to channels and queries, not server/status buffers
            if !matches!(buf.buffer_type, BufferType::Channel | BufferType::Query) {
                crate::commands::helpers::add_local_event(
                    self,
                    "Cannot send messages to this buffer",
                );
                return;
            }
            let conn = self.state.connections.get(&buf.connection_id);
            let nick = conn.map(|c| c.nick.clone()).unwrap_or_default();
            (buf.connection_id.clone(), nick, buf.name.clone())
        };

        // Split long messages at word boundaries to stay within IRC byte limits.
        let chunks = crate::irc::split_irc_message(text, crate::irc::MESSAGE_MAX_BYTES);

        // When echo-message is enabled, the server will echo our message back
        // with authoritative server-time — skip local display and wait for echo.
        let echo_message_enabled = self
            .state
            .connections
            .get(&conn_id)
            .is_some_and(|c| c.enabled_caps.contains("echo-message"));

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
                        nick_mode: None,
                        text: chunk,
                        highlight: false,
                        event_key: None,
                        event_params: None, log_msg_id: None, log_ref_id: None,
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
    #[allow(clippy::too_many_lines, clippy::type_complexity, clippy::needless_pass_by_value)]
    fn build_script_api(
        tx: mpsc::UnboundedSender<crate::scripting::ScriptAction>,
        snapshot: Arc<std::sync::RwLock<crate::scripting::engine::ScriptStateSnapshot>>,
        timer_id_counter: Arc<std::sync::atomic::AtomicU64>,
    ) -> crate::scripting::engine::ScriptAPI {
        use crate::scripting::ScriptAction;

        let t = tx.clone();
        let say: Arc<dyn Fn((String, String, Option<String>)) + Send + Sync> =
            Arc::new(move |(target, text, conn_id)| {
                let _ = t.send(ScriptAction::Say { target, text, conn_id });
            });

        let t = tx.clone();
        let action: Arc<dyn Fn((String, String, Option<String>)) + Send + Sync> =
            Arc::new(move |(target, text, conn_id)| {
                let _ = t.send(ScriptAction::Action { target, text, conn_id });
            });

        let t = tx.clone();
        let notice: Arc<dyn Fn((String, String, Option<String>)) + Send + Sync> =
            Arc::new(move |(target, text, conn_id)| {
                let _ = t.send(ScriptAction::Notice { target, text, conn_id });
            });

        let t = tx.clone();
        let raw: Arc<dyn Fn((String, Option<String>)) + Send + Sync> =
            Arc::new(move |(line, conn_id)| {
                let _ = t.send(ScriptAction::Raw { line, conn_id });
            });

        let t = tx.clone();
        let join: Arc<dyn Fn((String, Option<String>, Option<String>)) + Send + Sync> =
            Arc::new(move |(channel, key, conn_id)| {
                let _ = t.send(ScriptAction::Join { channel, key, conn_id });
            });

        let t = tx.clone();
        let part: Arc<dyn Fn((String, Option<String>, Option<String>)) + Send + Sync> =
            Arc::new(move |(channel, msg, conn_id)| {
                let _ = t.send(ScriptAction::Part { channel, msg, conn_id });
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
                let _ = t.send(ScriptAction::Mode { channel, mode_string, conn_id });
            });

        let t = tx.clone();
        let kick: Arc<dyn Fn((String, String, Option<String>, Option<String>)) + Send + Sync> =
            Arc::new(move |(channel, nick, reason, conn_id)| {
                let _ = t.send(ScriptAction::Kick { channel, nick, reason, conn_id });
            });

        let t = tx.clone();
        let ctcp: Arc<dyn Fn((String, String, Option<String>, Option<String>)) + Send + Sync> =
            Arc::new(move |(target, ctcp_type, message, conn_id)| {
                let _ = t.send(ScriptAction::Ctcp { target, ctcp_type, message, conn_id });
            });

        let t = tx.clone();
        let add_local_event: Arc<dyn Fn(String) + Send + Sync> =
            Arc::new(move |text| {
                let _ = t.send(ScriptAction::LocalEvent { text });
            });

        let t = tx.clone();
        let add_buffer_event: Arc<dyn Fn((String, String)) + Send + Sync> =
            Arc::new(move |(buffer_id, text)| {
                let _ = t.send(ScriptAction::BufferEvent { buffer_id, text });
            });

        let t = tx.clone();
        let switch_buffer: Arc<dyn Fn(String) + Send + Sync> =
            Arc::new(move |buffer_id| {
                let _ = t.send(ScriptAction::SwitchBuffer { buffer_id });
            });

        let t = tx.clone();
        let execute_command: Arc<dyn Fn(String) + Send + Sync> =
            Arc::new(move |line| {
                let _ = t.send(ScriptAction::ExecuteCommand { line });
            });

        let t = tx.clone();
        let register_command: Arc<dyn Fn((String, String, String)) + Send + Sync> =
            Arc::new(move |(name, description, usage)| {
                let _ = t.send(ScriptAction::RegisterCommand { name, description, usage });
            });

        let t = tx.clone();
        let unregister_command: Arc<dyn Fn(String) + Send + Sync> =
            Arc::new(move |name| {
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
            Arc::new(move |()| {
                snap.read().ok().and_then(|s| s.active_buffer_id.clone())
            });

        let snap = Arc::clone(&snapshot);
        let our_nick: Arc<dyn Fn(Option<String>) -> Option<String> + Send + Sync> =
            Arc::new(move |conn_id| {
                let s = snap.read().ok()?;
                if let Some(id) = conn_id {
                    s.connections.iter().find(|c| c.id == id).map(|c| c.nick.clone())
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
            snap.read().map_or_else(|_| Vec::new(), |s| s.connections.clone())
        });

        let snap = Arc::clone(&snapshot);
        let buffer_info: Arc<
            dyn Fn(String) -> Option<crate::scripting::engine::BufferInfo> + Send + Sync,
        > = Arc::new(move |id| {
            let s = snap.read().ok()?;
            s.buffers.iter().find(|b| b.id == id).cloned()
        });

        let snap = Arc::clone(&snapshot);
        let buffers: Arc<
            dyn Fn(()) -> Vec<crate::scripting::engine::BufferInfo> + Send + Sync,
        > = Arc::new(move |()| {
            snap.read().map_or_else(|_| Vec::new(), |s| s.buffers.clone())
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
        let start_timer: Arc<dyn Fn(u64) -> u64 + Send + Sync> =
            Arc::new(move |interval_ms| {
                let id = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let _ = t.send(ScriptAction::StartTimer { id, interval_ms });
                id
            });

        let t = tx.clone();
        let counter = Arc::clone(&timer_id_counter);
        let start_timeout: Arc<dyn Fn(u64) -> u64 + Send + Sync> =
            Arc::new(move |delay_ms| {
                let id = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let _ = t.send(ScriptAction::StartTimeout { id, delay_ms });
                id
            });

        let t = tx.clone();
        let cancel_timer: Arc<dyn Fn(u64) + Send + Sync> =
            Arc::new(move |id| {
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
    fn update_script_snapshot(&self) {
        if let Ok(mut snap) = self.script_state.write() {
            *snap = self.state.script_snapshot();
            snap.script_config.clone_from(&self.script_config);
            // Serialize app config to TOML value for dot-path lookups
            snap.app_config_toml = toml::Value::try_from(&self.config).ok();
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
            ScriptAction::Say { target, text, conn_id } => {
                if let Some(cid) = self.resolve_conn_id(conn_id.as_deref())
                    && let Some(sender) = self.irc_sender_for(&cid)
                {
                    for chunk in crate::irc::split_irc_message(&text, crate::irc::MESSAGE_MAX_BYTES) {
                        let _ = sender.send_privmsg(&target, &chunk);
                    }
                }
            }
            ScriptAction::Action { target, text, conn_id } => {
                if let Some(cid) = self.resolve_conn_id(conn_id.as_deref())
                    && let Some(sender) = self.irc_sender_for(&cid)
                {
                    let _ = sender.send(::irc::proto::Command::Raw(
                        "PRIVMSG".to_string(),
                        vec![target, format!("\x01ACTION {text}\x01")],
                    ));
                }
            }
            ScriptAction::Notice { target, text, conn_id } => {
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
                    let _ = sender.send(::irc::proto::Command::Raw(
                        line,
                        vec![],
                    ));
                }
            }
            ScriptAction::Join { channel, key, conn_id } => {
                if let Some(cid) = self.resolve_conn_id(conn_id.as_deref())
                    && let Some(sender) = self.irc_sender_for(&cid)
                {
                    let _ = sender.send(::irc::proto::Command::JOIN(
                        channel,
                        key,
                        None,
                    ));
                }
            }
            ScriptAction::Part { channel, msg, conn_id } => {
                if let Some(cid) = self.resolve_conn_id(conn_id.as_deref())
                    && let Some(sender) = self.irc_sender_for(&cid)
                {
                    let _ = sender.send(::irc::proto::Command::PART(
                        channel,
                        msg,
                    ));
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
                    let _ = sender.send(::irc::proto::Command::WHOIS(
                        None,
                        nick,
                    ));
                }
            }
            ScriptAction::Mode { channel, mode_string, conn_id } => {
                if let Some(cid) = self.resolve_conn_id(conn_id.as_deref())
                    && let Some(sender) = self.irc_sender_for(&cid)
                {
                    let _ = sender.send(::irc::proto::Command::Raw(
                        "MODE".to_string(),
                        vec![channel, mode_string],
                    ));
                }
            }
            ScriptAction::Kick { channel, nick, reason, conn_id } => {
                if let Some(cid) = self.resolve_conn_id(conn_id.as_deref())
                    && let Some(sender) = self.irc_sender_for(&cid)
                {
                    let _ = sender.send(::irc::proto::Command::KICK(
                        channel,
                        nick,
                        reason,
                    ));
                }
            }
            ScriptAction::Ctcp { target, ctcp_type, message, conn_id } => {
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
            ScriptAction::RegisterCommand { name, description, usage } => {
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
                        if tx.send(crate::scripting::ScriptAction::TimerFired { id }).is_err() {
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
        let api = self.script_api.as_ref().expect("script_api must be set");
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
    fn emit_irc_to_scripts(
        &self,
        conn_id: &str,
        msg: &::irc::proto::Message,
    ) -> bool {
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
                params.insert("is_channel".to_string(), target.starts_with('#').to_string());
                // Check for CTCP
                if let Some(ctcp_body) = text.strip_prefix('\x01')
                    .and_then(|t| t.strip_suffix('\x01'))
                {
                    if let Some(action_text) = ctcp_body.strip_prefix("ACTION ") {
                        params.insert("message".to_string(), action_text.to_string());
                        events::ACTION
                    } else {
                        let (ctcp_type, ctcp_msg) = ctcp_body
                            .split_once(' ')
                            .unwrap_or((ctcp_body, ""));
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
                let from_server = matches!(msg.prefix, Some(::irc::proto::Prefix::ServerName(_)) | None);
                params.insert("from_server".to_string(), from_server.to_string());
                // CTCP response comes as NOTICE with \x01...\x01
                if let Some(ctcp_body) = text.strip_prefix('\x01')
                    .and_then(|t| t.strip_suffix('\x01'))
                {
                    let (ctcp_type, ctcp_msg) = ctcp_body
                        .split_once(' ')
                        .unwrap_or((ctcp_body, ""));
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
                let mode_str: Vec<String> = modes.iter().map(std::string::ToString::to_string).collect();
                params.insert("modes".to_string(), mode_str.join(" "));
                events::MODE
            }
            ::irc::proto::Command::UserMODE(target, modes) => {
                params.insert("nick".to_string(), extract_nick(msg.prefix.as_ref()));
                params.insert("target".to_string(), target.clone());
                let mode_str: Vec<String> = modes.iter().map(std::string::ToString::to_string).collect();
                params.insert("modes".to_string(), mode_str.join(" "));
                events::MODE
            }
            ::irc::proto::Command::WALLOPS(text) => {
                params.insert("nick".to_string(), extract_nick(msg.prefix.as_ref()));
                params.insert("message".to_string(), text.clone());
                let from_server = matches!(msg.prefix, Some(::irc::proto::Prefix::ServerName(_)) | None);
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
        let (proto, source) = resolve_image_protocol("kitty", &picker, "unknown", None, String::new());
        assert_eq!(proto, Some(ProtocolType::Kitty));
        assert_eq!(source, "config:kitty");
    }

    #[test]
    fn resolve_config_override_iterm2() {
        let picker = ratatui_image::picker::Picker::halfblocks();
        let (proto, source) = resolve_image_protocol("iterm2", &picker, "unknown", None, String::new());
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
            "auto", &picker, "ghostty", Some(ProtocolType::Kitty),
            "tmux:client_termtype=ghostty 1.3.0".into(),
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
            "auto", &picker, "iterm2", Some(ProtocolType::Iterm2),
            "tmux:client_termtype=iTerm2 3.6.8".into(),
        );
        assert_eq!(proto, Some(ProtocolType::Iterm2));
        assert!(source.starts_with("tmux:"));
    }

    #[test]
    fn resolve_direct_trusts_io_detection() {
        // Not in tmux — outer terminal from env, IO detection is fine.
        let mut picker = ratatui_image::picker::Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        let (proto, source) = resolve_image_protocol(
            "auto", &picker, "ghostty", Some(ProtocolType::Kitty),
            "env:LC_TERMINAL=Ghostty".into(),
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
            "auto", &picker, "iterm2", Some(ProtocolType::Iterm2),
            "env:ITERM_SESSION_ID".into(),
        );
        assert_eq!(proto, Some(ProtocolType::Iterm2));
    }
}

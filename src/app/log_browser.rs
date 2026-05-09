//! Log-browser-only methods on `App`. Only invoked when
//! `log_browser_mode == true`. Keeps the chat-mode call sites in
//! `app/mod.rs` free of log-mode branches.

use std::collections::{HashMap, HashSet, VecDeque};

use chrono::Utc;
use color_eyre::eyre::{Result, eyre};

use crate::config;
use crate::state::buffer::{ActivityLevel, Buffer, BufferType, make_buffer_id};
use crate::state::connection::{Connection, ConnectionStatus};

use super::App;

impl App {
    /// Connection ID prefix used for log-mode pseudo-networks. Distinct
    /// from any real network identifier (which the user picks in their
    /// config TOML) so live and log buffers never collide on
    /// `make_buffer_id`.
    pub const LOG_CONN_PREFIX: &'static str = "_log_";

    /// Build an `App` instance configured for the read-only log browser.
    /// No IRC, no scripts, no web server, no socket listener — just a
    /// SQLite connection backing a sidebar built from the message log.
    pub fn new_log_browser() -> Result<Self> {
        let mut app = Self::new()?;
        app.log_browser_mode = true;

        let log_db = crate::storage::load_log_db(&app.config.logging)
            .map_err(|e| eyre!("{e}"))?;
        app.log_db = Some(log_db);

        // Wipe state populated by the chat-mode `App::new` (default
        // Status buffer, any state derived from `[servers]`) so
        // `build_log_catalog` sees a clean sidebar.
        app.state.connections.clear();
        app.state.buffers.clear();
        app.state.active_buffer_id = None;
        app.state.previous_buffer_id = None;

        app.build_log_catalog()?;
        Ok(app)
    }

    /// Populate `state.connections` and `state.buffers` from the distinct
    /// `(network, buffer)` pairs in the log database. Each network
    /// becomes a synthetic `Connection`, each buffer a `BufferType::Log`
    /// placeholder with empty `messages` (filled lazily when first
    /// activated).
    pub fn build_log_catalog(&mut self) -> Result<()> {
        let log_db = self
            .log_db
            .as_ref()
            .ok_or_else(|| eyre!("log catalog requires log_db"))?;
        let networks = {
            let db = log_db.db.lock().expect("log db poisoned");
            crate::storage::query::list_networks(&db)
                .map_err(|e| eyre!("list_networks: {e}"))?
        };

        // Look up friendly labels from the user's chat config when present
        // ("libera" -> "Libera Chat" if they configured a server with
        // that id). Falls back to the network id verbatim.
        let label_for = |net: &str| -> String {
            self.config
                .servers
                .get(net)
                .map_or_else(|| net.to_string(), |c| c.label.clone())
        };

        let mut first_buffer_id: Option<String> = None;

        for net in &networks {
            let conn_id = format!("{}{net}", Self::LOG_CONN_PREFIX);
            self.state.add_connection(Connection {
                id: conn_id.clone(),
                label: label_for(net),
                status: ConnectionStatus::Connected,
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
                silent_banlist_channels: HashSet::new(),
            });

            let buffers = {
                let db = log_db.db.lock().expect("log db poisoned");
                crate::storage::query::list_buffers_for_network(&db, net)
                    .map_err(|e| eyre!("list_buffers_for_network: {e}"))?
            };
            for buf in buffers {
                let buf_id = make_buffer_id(&conn_id, &buf);
                if first_buffer_id.is_none() {
                    first_buffer_id = Some(buf_id.clone());
                }
                self.state.add_buffer(Buffer {
                    id: buf_id,
                    connection_id: conn_id.clone(),
                    buffer_type: BufferType::Log,
                    name: buf.clone(),
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
                    log_total_lines: None,
                    log_oldest_ts: None,
                    log_newest_ts: None,
                    history_exhausted: false,
                });
            }
        }

        if let Some(id) = first_buffer_id {
            self.state.set_active_buffer(&id);
        }
        Ok(())
    }
}

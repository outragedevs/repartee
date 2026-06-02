#[allow(dead_code)]
pub mod crypto;
#[allow(dead_code)]
pub mod db;
#[allow(dead_code)]
pub mod query;
pub mod types;
pub mod writer;

pub use types::LogRow;
#[allow(unused_imports)]
pub use types::{ReadMarker, StorageStats, StoredMessage};

use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use tokio::sync::mpsc;

use crate::config::LoggingConfig;
use crate::constants;

/// High-level handle to the storage subsystem.
///
/// Owns the database connection and the background writer task.
/// Created once at startup and shut down when the app exits.
#[allow(dead_code)]
pub struct Storage {
    pub db: Arc<Mutex<Connection>>,
    pub log_tx: mpsc::Sender<LogRow>,
    writer: writer::LogWriterHandle,
    pub encrypt: bool,
}

impl Storage {
    /// Initialize storage from the logging config section.
    ///
    /// Opens (or creates) the `SQLite` database under `~/.repartee/logs/`,
    /// optionally sets up encryption, and spawns the background writer.
    pub fn init(config: &LoggingConfig) -> Result<Self, String> {
        let db_dir = constants::log_dir();
        std::fs::create_dir_all(&db_dir).map_err(|e| format!("failed to create log dir: {e}"))?;

        let db_path = db_dir.join("messages.db");
        let conn = db::open_database_at(
            db_path.to_str().ok_or("invalid log dir path")?,
            config.encrypt,
        )
        .map_err(|e| format!("failed to open log database: {e}"))?;

        let crypto_key = if config.encrypt {
            let hex_key = crypto::load_or_create_key()?;
            Some(crypto::import_key(&hex_key)?)
        } else {
            None
        };

        let has_fts = !config.encrypt;

        // Purge old messages on startup if retention is configured
        if config.retention_days > 0 {
            let removed = db::purge_old_messages(&conn, config.retention_days, has_fts);
            if removed > 0 {
                tracing::info!(
                    "purged {removed} messages older than {} days",
                    config.retention_days
                );
            }
        }

        // Purge old event messages (join/part/quit/nick/kick/mode) on startup
        if config.event_retention_hours > 0 {
            let removed = db::purge_old_events(&conn, config.event_retention_hours, has_fts);
            if removed > 0 {
                tracing::info!(
                    "purged {removed} event messages older than {}h",
                    config.event_retention_hours
                );
            }
        }

        let db = Arc::new(Mutex::new(conn));
        let (writer, log_tx) = writer::LogWriterHandle::spawn(Arc::clone(&db), crypto_key);

        Ok(Self {
            db,
            log_tx,
            writer,
            encrypt: config.encrypt,
        })
    }

    /// Drain remaining rows and stop the background writer.
    pub async fn shutdown(self) {
        self.writer.shutdown().await;
    }
}

/// Read-only handle bundle used by the `repartee l` log browser. Same
/// crypto-key derivation as `Storage::init`, but no writer task — we
/// only ever read from this handle.
#[allow(dead_code)]
pub struct LogDb {
    pub db: Arc<Mutex<Connection>>,
    pub crypto_key: Option<aes_gcm::Key<aes_gcm::Aes256Gcm>>,
    /// `false` when `[storage] encrypt = true` (FTS triggers can't index
    /// ciphertext), `true` for plain logs. Drives the `/search` fallback
    /// from FTS to `LIKE`.
    pub has_fts: bool,
}

/// Open the message database read-only and (when `[storage] encrypt =
/// true`) load the same AES-256-GCM key the daemon uses. Returns a
/// human-readable error so `repartee l` can print it on the user's TTY
/// directly.
pub fn load_log_db(config: &LoggingConfig) -> Result<LogDb, String> {
    let db_dir = constants::log_dir();
    let db_path = db_dir.join("messages.db");
    if !db_path.exists() {
        return Err(format!(
            "no message log at {} — start `repartee` and chat first",
            db_path.display()
        ));
    }
    let path_str = db_path.to_str().ok_or("invalid log dir path")?;
    let conn =
        db::open_readonly_at(path_str).map_err(|e| format!("failed to open log database: {e}"))?;

    // Important: `load_existing_key`, NOT `load_or_create_key`. If the
    // user's LOG_KEY went missing for any reason (deleted .env, copied
    // DB onto a fresh machine), generating a fresh key would produce a
    // bad cipher and a silent empty UI. We surface a clear error
    // instead.
    let crypto_key = if config.encrypt {
        let hex_key = crypto::load_existing_key()?;
        Some(crypto::import_key(&hex_key)?)
    } else {
        None
    };

    Ok(LogDb {
        db: Arc::new(Mutex::new(conn)),
        crypto_key,
        has_fts: !config.encrypt,
    })
}

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

use aes_gcm::{Aes256Gcm, Key};
use rusqlite::Connection;
use tokio::sync::mpsc;

use crate::config::LoggingConfig;
use crate::constants;

/// High-level handle to the storage subsystem.
///
/// Owns the database connection, the background writer task, and the
/// encryption key (if configured). Created once at startup and shut
/// down when the app exits.
#[allow(dead_code)]
pub struct Storage {
    pub db: Arc<Mutex<Connection>>,
    pub log_tx: mpsc::UnboundedSender<LogRow>,
    writer: writer::LogWriterHandle,
    #[allow(dead_code)]
    pub encrypt: bool,
    #[allow(dead_code)]
    pub crypto_key: Option<Key<Aes256Gcm>>,
}

impl Storage {
    /// Initialize storage from the logging config section.
    ///
    /// Opens (or creates) the `SQLite` database under `~/.rustirc/logs/`,
    /// optionally sets up encryption, and spawns the background writer.
    pub fn init(config: &LoggingConfig) -> Result<Self, String> {
        let db_dir = constants::log_dir();
        std::fs::create_dir_all(&db_dir)
            .map_err(|e| format!("failed to create log dir: {e}"))?;

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
                tracing::info!("purged {removed} messages older than {} days", config.retention_days);
            }
        }

        let db = Arc::new(Mutex::new(conn));
        let (writer, log_tx) =
            writer::LogWriterHandle::spawn(Arc::clone(&db), config.encrypt, crypto_key);

        Ok(Self {
            db,
            log_tx,
            writer,
            encrypt: config.encrypt,
            crypto_key,
        })
    }

    /// Drain remaining rows and stop the background writer.
    pub async fn shutdown(self) {
        self.writer.shutdown().await;
    }
}

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aes_gcm::{Aes256Gcm, Key};
use rusqlite::{Connection, params};
use tokio::sync::mpsc;

use super::crypto;
use super::types::LogRow;

const BATCH_SIZE: usize = 50;
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// Handle to the background writer task.
pub struct LogWriterHandle {
    shutdown_tx: mpsc::Sender<()>,
    join: tokio::task::JoinHandle<()>,
}

impl LogWriterHandle {
    /// Spawn the background writer loop.
    ///
    /// Returns the handle and an unbounded sender for submitting log rows.
    pub fn spawn(
        db: Arc<Mutex<Connection>>,
        encrypt: bool,
        crypto_key: Option<Key<Aes256Gcm>>,
    ) -> (Self, mpsc::UnboundedSender<LogRow>) {
        let (row_tx, row_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);
        let has_fts = !encrypt;

        let join = tokio::spawn(writer_loop(db, row_rx, shutdown_rx, has_fts, crypto_key));

        let handle = Self { shutdown_tx, join };
        (handle, row_tx)
    }

    /// Signal the writer to drain remaining rows and stop.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(()).await;
        let _ = self.join.await;
    }
}

async fn writer_loop(
    db: Arc<Mutex<Connection>>,
    mut row_rx: mpsc::UnboundedReceiver<LogRow>,
    mut shutdown_rx: mpsc::Receiver<()>,
    has_fts: bool,
    crypto_key: Option<Key<Aes256Gcm>>,
) {
    let mut queue: Vec<LogRow> = Vec::new();
    let mut tick = tokio::time::interval(FLUSH_INTERVAL);
    // The first tick fires immediately — consume it so we start with a clean slate.
    tick.tick().await;

    loop {
        tokio::select! {
            Some(row) = row_rx.recv() => {
                queue.push(row);
                if queue.len() >= BATCH_SIZE {
                    queue = flush_blocking(&db, queue, has_fts, crypto_key).await;
                }
            }
            _ = tick.tick() => {
                if !queue.is_empty() {
                    queue = flush_blocking(&db, queue, has_fts, crypto_key).await;
                }
            }
            _ = shutdown_rx.recv() => {
                while let Ok(row) = row_rx.try_recv() {
                    queue.push(row);
                }
                if !queue.is_empty() {
                    flush_blocking(&db, queue, has_fts, crypto_key).await;
                }
                return;
            }
        }
    }
}

async fn flush_blocking(
    db: &Arc<Mutex<Connection>>,
    queue: Vec<LogRow>,
    _has_fts: bool,
    crypto_key: Option<Key<Aes256Gcm>>,
) -> Vec<LogRow> {
    let db = Arc::clone(db);
    match tokio::task::spawn_blocking(move || flush(&db, queue, crypto_key.as_ref())).await {
        Ok(remaining) => remaining,
        Err(e) => {
            tracing::error!("flush task panicked: {e}");
            Vec::new()
        }
    }
}

fn flush(
    db: &Arc<Mutex<Connection>>,
    queue: Vec<LogRow>,
    crypto_key: Option<&Key<Aes256Gcm>>,
) -> Vec<LogRow> {
    let Ok(conn) = db.lock() else {
        tracing::error!("failed to lock database for flush");
        return queue;
    };

    if let Err(e) = conn.execute_batch("BEGIN") {
        tracing::error!("failed to begin transaction: {e}");
        return queue;
    }

    for row in &queue {
        let msg_type_str = format!("{:?}", row.msg_type).to_lowercase();
        let highlight_int = i32::from(row.highlight);

        let (stored_text, iv): (String, Option<Vec<u8>>) = match crypto_key {
            Some(key) => match crypto::encrypt(&row.text, key) {
                Ok(enc) => (enc.ciphertext, Some(enc.iv)),
                Err(e) => {
                    tracing::error!("encryption failed for msg_id={}: {e}", row.msg_id);
                    let _ = conn.execute_batch("ROLLBACK");
                    return queue;
                }
            },
            None => (row.text.clone(), None),
        };

        if let Err(e) = conn.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight, iv, ref_id, tags)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                row.msg_id,
                row.network,
                row.buffer,
                row.timestamp,
                msg_type_str,
                row.nick,
                stored_text,
                highlight_int,
                iv,
                row.ref_id,
                row.tags,
            ],
        ) {
            tracing::error!("failed to insert msg_id={}: {e}", row.msg_id);
            let _ = conn.execute_batch("ROLLBACK");
            return queue;
        }
    }

    if let Err(e) = conn.execute_batch("COMMIT") {
        tracing::error!("failed to commit transaction: {e}");
        let _ = conn.execute_batch("ROLLBACK");
        return queue;
    }

    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::buffer::MessageType;
    use crate::storage::crypto::{generate_key_hex, import_key};
    use crate::storage::db::open_database;

    fn make_row(text: &str) -> LogRow {
        LogRow {
            msg_id: uuid::Uuid::new_v4().to_string(),
            network: "testnet".into(),
            buffer: "#test".into(),
            timestamp: chrono::Utc::now().timestamp(),
            msg_type: MessageType::Message,
            nick: Some("alice".into()),
            text: text.into(),
            highlight: false,
            ref_id: None,
            tags: None,
        }
    }

    fn msg_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap()
    }

    #[tokio::test]
    async fn writer_flushes_on_shutdown() {
        let db = Arc::new(Mutex::new(open_database(false).unwrap()));
        let (handle, tx) = LogWriterHandle::spawn(Arc::clone(&db), false, None);

        for _ in 0..5 {
            tx.send(make_row("hello")).unwrap();
        }

        handle.shutdown().await;

        let conn = db.lock().unwrap();
        assert_eq!(msg_count(&conn), 5);
    }

    #[tokio::test]
    async fn writer_flushes_at_batch_size() {
        let db = Arc::new(Mutex::new(open_database(false).unwrap()));
        let (handle, tx) = LogWriterHandle::spawn(Arc::clone(&db), false, None);

        for _ in 0..BATCH_SIZE {
            tx.send(make_row("batch")).unwrap();
        }

        // Give the writer loop time to process the batch.
        tokio::time::sleep(Duration::from_millis(50)).await;

        {
            let conn = db.lock().unwrap();
            #[expect(clippy::cast_possible_wrap, reason = "test constant is small")]
            {
                assert_eq!(msg_count(&conn), BATCH_SIZE as i64);
            }
        }

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn writer_populates_fts() {
        let db = Arc::new(Mutex::new(open_database(false).unwrap()));
        let (handle, tx) = LogWriterHandle::spawn(Arc::clone(&db), false, None);

        let unique = "xyzzyplughmagicword";
        tx.send(make_row(unique)).unwrap();

        handle.shutdown().await;

        let fts_query = format!("\"{unique}\"");
        let fts_count: i64 = db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH ?1",
                params![fts_query],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts_count, 1);
    }

    #[tokio::test]
    async fn writer_encrypts_when_configured() {
        let key_hex = generate_key_hex();
        let key = import_key(&key_hex).unwrap();

        let db = Arc::new(Mutex::new(open_database(true).unwrap()));
        let (handle, tx) = LogWriterHandle::spawn(Arc::clone(&db), true, Some(key));

        let plaintext = "super secret message";
        tx.send(make_row(plaintext)).unwrap();

        handle.shutdown().await;

        let (stored_text, iv): (String, Option<Vec<u8>>) = db
            .lock()
            .unwrap()
            .query_row("SELECT text, iv FROM messages LIMIT 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();

        // The stored text should NOT be the plaintext (it's base64 ciphertext).
        assert_ne!(stored_text, plaintext);
        // IV should be present as a 12-byte blob.
        let iv = iv.expect("iv should be present for encrypted row");
        assert_eq!(iv.len(), 12);
    }
}

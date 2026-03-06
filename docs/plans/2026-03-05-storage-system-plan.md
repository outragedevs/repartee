# Storage System Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Persistent SQLite message logging with batched async writes, FTS5 search, read markers, and optional AES-256-GCM encryption — 1:1 port of kokoirc's storage system.

**Architecture:** A `src/storage/` module with 5 files (types, db, crypto, writer, query) plus mod.rs. The writer runs as a spawned tokio task receiving `LogRow` via mpsc channel. All messages flow through `log_message()` which enqueues to the writer. The writer batches inserts (50 rows or 1s timer) in a single SQLite transaction with WAL mode. Encryption is optional per config.

**Tech Stack:** rusqlite 0.38.0 (bundled-full), aes-gcm 0.10.3, rand 0.10.0, base64 0.22.1, uuid 1.21.0

---

### Task 1: Add Dependencies to Cargo.toml

**Files:**
- Modify: `Cargo.toml`

**Step 1: Add new crate dependencies**

Add these lines to `[dependencies]` in `Cargo.toml`:

```toml
rusqlite = { version = "0.38", features = ["bundled-full"] }
aes-gcm = "0.10"
rand = "0.10"
base64 = "0.22"
uuid = { version = "1.21", features = ["v4"] }
```

**Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles with no errors (new deps downloaded)

**Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "feat(storage): add rusqlite, aes-gcm, rand, base64, uuid deps"
```

---

### Task 2: Storage Types

**Files:**
- Create: `src/storage/types.rs`
- Create: `src/storage/mod.rs`
- Modify: `src/main.rs` (add `mod storage`)

**Step 1: Create `src/storage/types.rs`**

```rust
use crate::state::buffer::MessageType;

/// A row to be written to the log database.
#[derive(Debug, Clone)]
pub struct LogRow {
    pub msg_id: String,
    pub network: String,
    pub buffer: String,
    pub timestamp: i64, // Unix ms
    pub msg_type: MessageType,
    pub nick: Option<String>,
    pub text: String,
    pub highlight: bool,
}

/// A message read back from the database.
#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub id: i64,
    pub msg_id: String,
    pub network: String,
    pub buffer: String,
    pub timestamp: i64,
    pub msg_type: String,
    pub nick: Option<String>,
    pub text: String,
    pub highlight: bool,
}

/// Per-client read position for a buffer.
#[derive(Debug, Clone)]
pub struct ReadMarker {
    pub network: String,
    pub buffer: String,
    pub client: String,
    pub last_read: i64, // Unix ms
}

/// Stats about the log database.
#[derive(Debug, Clone)]
pub struct StorageStats {
    pub message_count: u64,
    pub db_size_bytes: u64,
}
```

**Step 2: Create `src/storage/mod.rs`**

```rust
pub mod types;

pub use types::{LogRow, StoredMessage, ReadMarker, StorageStats};
```

**Step 3: Add `mod storage` to `src/main.rs`**

Add `mod storage;` after `mod scripting;`.

**Step 4: Verify it compiles**

Run: `cargo check`
Expected: compiles

**Step 5: Commit**

```bash
git add src/storage/ src/main.rs
git commit -m "feat(storage): add storage module with types"
```

---

### Task 3: Database Setup (db.rs)

**Files:**
- Create: `src/storage/db.rs`
- Modify: `src/storage/mod.rs`

**Step 1: Write tests for db.rs**

Add to the bottom of `src/storage/db.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_creates_tables() {
        let db = open_database(false).unwrap();
        // Verify messages table exists
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn open_creates_read_markers_table() {
        let db = open_database(false).unwrap();
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM read_markers", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn open_creates_fts_when_not_encrypted() {
        let db = open_database(false).unwrap();
        // FTS5 table should exist
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='messages_fts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn open_skips_fts_when_encrypted() {
        let db = open_database(true).unwrap();
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='messages_fts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn purge_removes_old_messages() {
        let db = open_database(false).unwrap();
        // Insert a message with old timestamp (1 day ago minus extra)
        let old_ts = chrono::Utc::now().timestamp_millis() - 2 * 86_400_000;
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params!["id1", "net", "#chan", old_ts, "message", "nick", "old msg", 0],
        ).unwrap();

        // Insert a recent message
        let now_ts = chrono::Utc::now().timestamp_millis();
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params!["id2", "net", "#chan", now_ts, "message", "nick", "new msg", 0],
        ).unwrap();

        let purged = purge_old_messages(&db, 1, false);
        assert_eq!(purged, 1);

        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib storage::db`
Expected: FAIL (module doesn't exist yet)

**Step 3: Implement db.rs**

```rust
use rusqlite::{Connection, params};

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS messages (
        id        INTEGER PRIMARY KEY AUTOINCREMENT,
        msg_id    TEXT,
        network   TEXT NOT NULL,
        buffer    TEXT NOT NULL,
        timestamp INTEGER NOT NULL,
        type      TEXT NOT NULL,
        nick      TEXT,
        text      TEXT NOT NULL,
        highlight INTEGER DEFAULT 0,
        iv        BLOB
    );
    CREATE INDEX IF NOT EXISTS idx_messages_lookup ON messages(network, buffer, timestamp);
    CREATE INDEX IF NOT EXISTS idx_messages_time ON messages(timestamp);
    CREATE INDEX IF NOT EXISTS idx_messages_msg_id ON messages(msg_id);

    CREATE TABLE IF NOT EXISTS read_markers (
        network    TEXT NOT NULL,
        buffer     TEXT NOT NULL,
        client     TEXT NOT NULL,
        last_read  INTEGER NOT NULL,
        PRIMARY KEY (network, buffer, client)
    );
";

const FTS_SCHEMA: &str = "
    CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
        nick, text, content=messages, content_rowid=id
    );
";

/// Open (or create) the log database.
/// Pass `encrypt = true` to skip FTS5 index creation.
/// When `path` is None, uses an in-memory database (for tests).
pub fn open_database(encrypt: bool) -> rusqlite::Result<Connection> {
    open_database_at(":memory:", encrypt)
}

/// Open (or create) the log database at a specific path.
pub fn open_database_at(path: &str, encrypt: bool) -> rusqlite::Result<Connection> {
    let db = Connection::open(path)?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
    db.execute_batch(SCHEMA)?;

    if !encrypt {
        db.execute_batch(FTS_SCHEMA)?;
    }

    Ok(db)
}

/// Purge messages older than `retention_days`. Returns number of rows deleted.
pub fn purge_old_messages(db: &Connection, retention_days: u32, has_fts: bool) -> usize {
    if retention_days == 0 {
        return 0;
    }
    let cutoff = chrono::Utc::now().timestamp_millis() - (retention_days as i64) * 86_400_000;

    if has_fts {
        let _ = db.execute(
            "DELETE FROM messages_fts WHERE rowid IN (SELECT id FROM messages WHERE timestamp < ?1)",
            params![cutoff],
        );
    }

    db.execute("DELETE FROM messages WHERE timestamp < ?1", params![cutoff])
        .unwrap_or(0) as usize
}
```

**Step 4: Update `src/storage/mod.rs`**

```rust
pub mod db;
pub mod types;

pub use types::{LogRow, StoredMessage, ReadMarker, StorageStats};
```

**Step 5: Run tests**

Run: `cargo test --lib storage::db`
Expected: all 5 tests pass

**Step 6: Commit**

```bash
git add src/storage/
git commit -m "feat(storage): SQLite db setup with schema, FTS5, and purge"
```

---

### Task 4: Crypto Module (crypto.rs)

**Files:**
- Create: `src/storage/crypto.rs`
- Modify: `src/storage/mod.rs`

**Step 1: Write tests**

Add to bottom of `src/storage/crypto.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_key_is_32_bytes_hex() {
        let hex = generate_key_hex();
        assert_eq!(hex.len(), 64); // 32 bytes = 64 hex chars
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key_hex = generate_key_hex();
        let key = import_key(&key_hex).unwrap();
        let plaintext = "Hello, this is a secret IRC message!";

        let encrypted = encrypt(plaintext, &key).unwrap();
        assert_ne!(encrypted.ciphertext, plaintext); // should be different
        assert_eq!(encrypted.iv.len(), 12);

        let decrypted = decrypt(&encrypted.ciphertext, &encrypted.iv, &key).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn different_ivs_produce_different_ciphertext() {
        let key_hex = generate_key_hex();
        let key = import_key(&key_hex).unwrap();
        let plaintext = "same message";

        let e1 = encrypt(plaintext, &key).unwrap();
        let e2 = encrypt(plaintext, &key).unwrap();
        assert_ne!(e1.ciphertext, e2.ciphertext);
        assert_ne!(e1.iv, e2.iv);
    }

    #[test]
    fn wrong_key_fails_decrypt() {
        let key1 = import_key(&generate_key_hex()).unwrap();
        let key2 = import_key(&generate_key_hex()).unwrap();

        let encrypted = encrypt("secret", &key1).unwrap();
        let result = decrypt(&encrypted.ciphertext, &encrypted.iv, &key2);
        assert!(result.is_err());
    }

    #[test]
    fn load_or_create_key_roundtrip() {
        let dir = std::env::temp_dir().join(format!("repartee_crypto_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let env_path = dir.join(".env");

        // First call: creates key
        let key1_hex = load_or_create_key_at(&env_path).unwrap();
        assert_eq!(key1_hex.len(), 64);

        // Second call: loads same key
        let key2_hex = load_or_create_key_at(&env_path).unwrap();
        assert_eq!(key1_hex, key2_hex);

        // Verify .env file contains the key
        let content = std::fs::read_to_string(&env_path).unwrap();
        assert!(content.contains(&key1_hex));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib storage::crypto`
Expected: FAIL

**Step 3: Implement crypto.rs**

```rust
use aes_gcm::{
    Aes256Gcm, Key, Nonce,
    aead::{Aead, KeyInit, OsRng},
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use rand::RngCore;

use crate::constants::APP_NAME;

const IV_LENGTH: usize = 12;
const KEY_ENV_NAME: &str = "_LOG_KEY"; // prefixed with APP_NAME at runtime

pub struct EncryptedData {
    pub ciphertext: String, // base64
    pub iv: Vec<u8>,        // 12 bytes
}

/// Generate a random 256-bit key as a hex string.
pub fn generate_key_hex() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Import a hex key string into an AES-256-GCM key.
pub fn import_key(hex_key: &str) -> Result<Key<Aes256Gcm>, String> {
    let bytes = hex::decode(hex_key).map_err(|e| format!("Invalid hex key: {e}"))?;
    if bytes.len() != 32 {
        return Err(format!("Key must be 32 bytes, got {}", bytes.len()));
    }
    Ok(*Key::<Aes256Gcm>::from_slice(&bytes))
}

/// Encrypt plaintext. Returns base64 ciphertext + 12-byte IV.
pub fn encrypt(plaintext: &str, key: &Key<Aes256Gcm>) -> Result<EncryptedData, String> {
    let cipher = Aes256Gcm::new(key);
    let mut iv_bytes = [0u8; IV_LENGTH];
    OsRng.fill_bytes(&mut iv_bytes);
    let nonce = Nonce::from_slice(&iv_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| format!("Encryption failed: {e}"))?;

    Ok(EncryptedData {
        ciphertext: BASE64.encode(&ciphertext),
        iv: iv_bytes.to_vec(),
    })
}

/// Decrypt base64 ciphertext with the given IV.
pub fn decrypt(ciphertext_b64: &str, iv: &[u8], key: &Key<Aes256Gcm>) -> Result<String, String> {
    let cipher = Aes256Gcm::new(key);
    let nonce = Nonce::from_slice(iv);
    let ciphertext = BASE64
        .decode(ciphertext_b64)
        .map_err(|e| format!("Invalid base64: {e}"))?;

    let plaintext = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|e| format!("Decryption failed: {e}"))?;

    String::from_utf8(plaintext).map_err(|e| format!("Invalid UTF-8: {e}"))
}

/// Load encryption key from .env file, or generate and save one if missing.
/// Returns the hex key string.
pub fn load_or_create_key() -> Result<String, String> {
    let env_path = crate::constants::env_path();
    load_or_create_key_at(&env_path)
}

/// Load or create key at a specific path (for testing).
pub fn load_or_create_key_at(env_path: &std::path::Path) -> Result<String, String> {
    let key_name = format!("{}{}", APP_NAME.to_uppercase(), KEY_ENV_NAME);

    let content = std::fs::read_to_string(env_path).unwrap_or_default();

    // Try to find existing key
    for line in content.lines() {
        if let Some(val) = line.strip_prefix(&format!("{key_name}=")) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
        }
    }

    // Generate new key and append to .env
    let hex_key = generate_key_hex();
    let new_line = format!("{key_name}={hex_key}\n");
    let mut new_content = content;
    if !new_content.is_empty() && !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push_str(&new_line);

    std::fs::write(env_path, &new_content)
        .map_err(|e| format!("Failed to write .env: {e}"))?;

    // chmod 0o600 on unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(env_path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(hex_key)
}
```

**Note:** We need to add the `hex` crate. Add to Cargo.toml:

```toml
hex = "0.4"
```

**Step 4: Update mod.rs**

Add `pub mod crypto;` to `src/storage/mod.rs`.

**Step 5: Run tests**

Run: `cargo test --lib storage::crypto`
Expected: all 5 tests pass

**Step 6: Commit**

```bash
git add src/storage/ Cargo.toml Cargo.lock
git commit -m "feat(storage): AES-256-GCM crypto with key management"
```

---

### Task 5: Batched Writer (writer.rs)

**Files:**
- Create: `src/storage/writer.rs`
- Modify: `src/storage/mod.rs`

**Step 1: Write tests**

Add to bottom of `src/storage/writer.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::buffer::MessageType;
    use crate::storage::db::open_database;

    fn make_log_row(text: &str) -> LogRow {
        LogRow {
            msg_id: uuid::Uuid::new_v4().to_string(),
            network: "testnet".to_string(),
            buffer: "#test".to_string(),
            timestamp: chrono::Utc::now().timestamp_millis(),
            msg_type: MessageType::Message,
            nick: Some("tester".to_string()),
            text: text.to_string(),
            highlight: false,
        }
    }

    #[tokio::test]
    async fn writer_flushes_on_shutdown() {
        let db = open_database(false).unwrap();
        let db = std::sync::Arc::new(std::sync::Mutex::new(db));
        let (handle, tx) = LogWriterHandle::spawn(db.clone(), false, None);

        for i in 0..5 {
            tx.send(make_log_row(&format!("msg {i}"))).unwrap();
        }

        handle.shutdown().await;

        let db = db.lock().unwrap();
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 5);
    }

    #[tokio::test]
    async fn writer_flushes_at_batch_size() {
        let db = open_database(false).unwrap();
        let db = std::sync::Arc::new(std::sync::Mutex::new(db));
        let (handle, tx) = LogWriterHandle::spawn(db.clone(), false, None);

        // Send BATCH_SIZE messages to trigger immediate flush
        for i in 0..BATCH_SIZE {
            tx.send(make_log_row(&format!("msg {i}"))).unwrap();
        }

        // Small delay for the flush to complete
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        {
            let db = db.lock().unwrap();
            let count: i64 = db
                .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, BATCH_SIZE as i64);
        }

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn writer_populates_fts() {
        let db = open_database(false).unwrap();
        let db = std::sync::Arc::new(std::sync::Mutex::new(db));
        let (handle, tx) = LogWriterHandle::spawn(db.clone(), false, None);

        tx.send(make_log_row("searchable content here")).unwrap();
        handle.shutdown().await;

        let db = db.lock().unwrap();
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH 'searchable'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn writer_encrypts_when_configured() {
        let key_hex = crate::storage::crypto::generate_key_hex();
        let key = crate::storage::crypto::import_key(&key_hex).unwrap();

        let db = open_database(true).unwrap(); // encrypted = no FTS
        let db = std::sync::Arc::new(std::sync::Mutex::new(db));
        let (handle, tx) = LogWriterHandle::spawn(db.clone(), true, Some(key));

        tx.send(make_log_row("secret message")).unwrap();
        handle.shutdown().await;

        let db = db.lock().unwrap();
        let row: (String, Option<Vec<u8>>) = db
            .query_row(
                "SELECT text, iv FROM messages LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        // Text should be base64 ciphertext, not plaintext
        assert_ne!(row.0, "secret message");
        // IV should be present
        assert!(row.1.is_some());
        assert_eq!(row.1.unwrap().len(), 12);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib storage::writer`
Expected: FAIL

**Step 3: Implement writer.rs**

```rust
use std::sync::{Arc, Mutex};

use aes_gcm::Key;
use aes_gcm::Aes256Gcm;
use rusqlite::params;
use tokio::sync::mpsc;
use tokio::time::{Duration, interval};

use crate::storage::crypto;
use crate::storage::types::LogRow;

const BATCH_SIZE: usize = 50;
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// Handle to the background writer task.
pub struct LogWriterHandle {
    shutdown_tx: mpsc::Sender<()>,
    join: tokio::task::JoinHandle<()>,
}

impl LogWriterHandle {
    /// Spawn the writer task. Returns (handle, sender_for_log_rows).
    pub fn spawn(
        db: Arc<Mutex<rusqlite::Connection>>,
        encrypt: bool,
        crypto_key: Option<Key<Aes256Gcm>>,
    ) -> (Self, mpsc::UnboundedSender<LogRow>) {
        let (row_tx, row_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);

        let join = tokio::spawn(writer_loop(db, row_rx, shutdown_rx, encrypt, crypto_key));

        (Self { shutdown_tx, join }, row_tx)
    }

    /// Flush remaining messages and stop the writer.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(()).await;
        let _ = self.join.await;
    }
}

async fn writer_loop(
    db: Arc<Mutex<rusqlite::Connection>>,
    mut row_rx: mpsc::UnboundedReceiver<LogRow>,
    mut shutdown_rx: mpsc::Receiver<()>,
    encrypt: bool,
    crypto_key: Option<Key<Aes256Gcm>>,
) {
    let mut queue: Vec<LogRow> = Vec::with_capacity(BATCH_SIZE);
    let mut tick = interval(FLUSH_INTERVAL);
    let has_fts = !encrypt;

    loop {
        tokio::select! {
            row = row_rx.recv() => {
                match row {
                    Some(row) => {
                        queue.push(row);
                        if queue.len() >= BATCH_SIZE {
                            flush(&db, &mut queue, has_fts, crypto_key.as_ref());
                        }
                    }
                    None => {
                        // Channel closed — flush and exit
                        flush(&db, &mut queue, has_fts, crypto_key.as_ref());
                        return;
                    }
                }
            }
            _ = tick.tick() => {
                if !queue.is_empty() {
                    flush(&db, &mut queue, has_fts, crypto_key.as_ref());
                }
            }
            _ = shutdown_rx.recv() => {
                // Drain remaining messages
                while let Ok(row) = row_rx.try_recv() {
                    queue.push(row);
                }
                flush(&db, &mut queue, has_fts, crypto_key.as_ref());
                return;
            }
        }
    }
}

fn flush(
    db: &Arc<Mutex<rusqlite::Connection>>,
    queue: &mut Vec<LogRow>,
    has_fts: bool,
    crypto_key: Option<&Key<Aes256Gcm>>,
) {
    if queue.is_empty() {
        return;
    }

    let batch: Vec<LogRow> = queue.drain(..).collect();
    let db = db.lock().unwrap();

    let tx = db.unchecked_transaction().unwrap();

    for row in &batch {
        let type_str = format!("{:?}", row.msg_type).to_lowercase();
        let highlight_int: i32 = if row.highlight { 1 } else { 0 };

        let (text, iv): (String, Option<Vec<u8>>) = if let Some(key) = crypto_key {
            match crypto::encrypt(&row.text, key) {
                Ok(enc) => (enc.ciphertext, Some(enc.iv)),
                Err(e) => {
                    tracing::error!("Encryption failed, storing plaintext: {e}");
                    (row.text.clone(), None)
                }
            }
        } else {
            (row.text.clone(), None)
        };

        let result = db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight, iv) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![row.msg_id, row.network, row.buffer, row.timestamp, type_str, row.nick, text, highlight_int, iv],
        );

        if let Ok(_) = result {
            if has_fts {
                let rowid = db.last_insert_rowid();
                let _ = db.execute(
                    "INSERT INTO messages_fts (rowid, nick, text) VALUES (?1, ?2, ?3)",
                    params![rowid, row.nick.as_deref().unwrap_or(""), row.text],
                );
            }
        }
    }

    tx.commit().unwrap();
}
```

**Step 4: Update mod.rs**

Add `pub mod writer;` to `src/storage/mod.rs`.

**Step 5: Run tests**

Run: `cargo test --lib storage::writer`
Expected: all 4 tests pass

**Step 6: Commit**

```bash
git add src/storage/
git commit -m "feat(storage): batched async writer with encryption support"
```

---

### Task 6: Query Module (query.rs)

**Files:**
- Create: `src/storage/query.rs`
- Modify: `src/storage/mod.rs`

**Step 1: Write tests**

Add to bottom of `src/storage/query.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db::open_database;

    fn insert_test_messages(db: &rusqlite::Connection, count: usize) {
        for i in 0..count {
            let ts = 1_000_000 + (i as i64) * 1000;
            db.execute(
                "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![format!("id{i}"), "testnet", "#test", ts, "message", "user", format!("msg {i}"), 0],
            ).unwrap();
            // FTS sync
            let rowid = db.last_insert_rowid();
            let _ = db.execute(
                "INSERT INTO messages_fts (rowid, nick, text) VALUES (?1, ?2, ?3)",
                params![rowid, "user", format!("msg {i}")],
            );
        }
    }

    #[test]
    fn get_messages_returns_chronological() {
        let db = open_database(false).unwrap();
        insert_test_messages(&db, 5);

        let msgs = get_messages(&db, "testnet", "#test", None, 100, false, None).unwrap();
        assert_eq!(msgs.len(), 5);
        assert!(msgs[0].timestamp < msgs[4].timestamp);
    }

    #[test]
    fn get_messages_cursor_pagination() {
        let db = open_database(false).unwrap();
        insert_test_messages(&db, 10);

        let page1 = get_messages(&db, "testnet", "#test", None, 5, false, None).unwrap();
        assert_eq!(page1.len(), 5);

        let before = page1[0].timestamp; // oldest in page1
        let page2 = get_messages(&db, "testnet", "#test", Some(before), 5, false, None).unwrap();
        assert_eq!(page2.len(), 5);
        assert!(page2.last().unwrap().timestamp < before);
    }

    #[test]
    fn search_messages_fts() {
        let db = open_database(false).unwrap();
        insert_test_messages(&db, 5);

        // Insert a unique message
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params!["unique", "testnet", "#test", 9_999_999, "message", "finder", "unique_keyword_xyz", 0],
        ).unwrap();
        let rowid = db.last_insert_rowid();
        let _ = db.execute(
            "INSERT INTO messages_fts (rowid, nick, text) VALUES (?1, ?2, ?3)",
            params![rowid, "finder", "unique_keyword_xyz"],
        );

        let results = search_messages(&db, "unique_keyword_xyz", None, None, 50).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].nick, Some("finder".to_string()));
    }

    #[test]
    fn get_buffers_lists_distinct() {
        let db = open_database(false).unwrap();
        insert_test_messages(&db, 3);
        // Add another buffer
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params!["x", "testnet", "#other", 1, "message", "u", "t", 0],
        ).unwrap();

        let buffers = get_buffers(&db, "testnet").unwrap();
        assert_eq!(buffers.len(), 2);
        assert!(buffers.contains(&"#other".to_string()));
        assert!(buffers.contains(&"#test".to_string()));
    }

    #[test]
    fn read_marker_crud() {
        let db = open_database(false).unwrap();

        update_read_marker(&db, "net", "#ch", "tui", 1000).unwrap();
        let marker = get_read_marker(&db, "net", "#ch", "tui").unwrap();
        assert_eq!(marker, Some(1000));

        // Update
        update_read_marker(&db, "net", "#ch", "tui", 2000).unwrap();
        let marker = get_read_marker(&db, "net", "#ch", "tui").unwrap();
        assert_eq!(marker, Some(2000));

        // Different client
        let marker = get_read_marker(&db, "net", "#ch", "web").unwrap();
        assert_eq!(marker, None);
    }

    #[test]
    fn unread_count() {
        let db = open_database(false).unwrap();
        insert_test_messages(&db, 10);

        // Mark as read at message 5's timestamp
        let ts5 = 1_000_000 + 5 * 1000;
        update_read_marker(&db, "testnet", "#test", "tui", ts5).unwrap();

        let unread = get_unread_count(&db, "testnet", "#test", "tui").unwrap();
        assert_eq!(unread, 4); // messages 6,7,8,9
    }

    #[test]
    fn get_stats_works() {
        let db = open_database(false).unwrap();
        insert_test_messages(&db, 3);

        let count = get_message_count(&db).unwrap();
        assert_eq!(count, 3);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib storage::query`
Expected: FAIL

**Step 3: Implement query.rs**

```rust
use aes_gcm::{Aes256Gcm, Key};
use rusqlite::{params, Connection};

use crate::storage::crypto;
use crate::storage::types::StoredMessage;

/// Get messages for a buffer, paginated by timestamp (cursor-based).
/// Returns messages in chronological order.
pub fn get_messages(
    db: &Connection,
    network: &str,
    buffer: &str,
    before: Option<i64>,
    limit: usize,
    encrypt: bool,
    crypto_key: Option<&Key<Aes256Gcm>>,
) -> rusqlite::Result<Vec<StoredMessage>> {
    let mut rows = if let Some(before_ts) = before {
        let mut stmt = db.prepare(
            "SELECT id, msg_id, network, buffer, timestamp, type, nick, text, highlight, iv \
             FROM messages WHERE network = ?1 AND buffer = ?2 AND timestamp < ?3 \
             ORDER BY timestamp DESC LIMIT ?4",
        )?;
        let rows = stmt.query_map(params![network, buffer, before_ts, limit as i64], |row| {
            map_row(row, encrypt, crypto_key)
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let mut stmt = db.prepare(
            "SELECT id, msg_id, network, buffer, timestamp, type, nick, text, highlight, iv \
             FROM messages WHERE network = ?1 AND buffer = ?2 \
             ORDER BY timestamp DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![network, buffer, limit as i64], |row| {
            map_row(row, encrypt, crypto_key)
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    rows.reverse(); // chronological order
    Ok(rows)
}

/// Full-text search (plain mode only — returns empty if encrypted).
pub fn search_messages(
    db: &Connection,
    query: &str,
    network: Option<&str>,
    buffer: Option<&str>,
    limit: usize,
) -> rusqlite::Result<Vec<StoredMessage>> {
    // Wrap query in double quotes for literal phrase match
    let safe_query = format!("\"{}\"", query.replace('"', "\"\""));
    let mut sql = String::from(
        "SELECT m.id, m.msg_id, m.network, m.buffer, m.timestamp, m.type, m.nick, m.text, m.highlight, m.iv \
         FROM messages m JOIN messages_fts fts ON m.id = fts.rowid \
         WHERE messages_fts MATCH ?1",
    );
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(safe_query)];

    if let Some(n) = network {
        sql.push_str(" AND m.network = ?");
        sql = sql.replace(
            &format!("?{}", param_values.len() + 1),
            &format!("?{}", param_values.len() + 1),
        );
        param_values.push(Box::new(n.to_string()));
    }
    if let Some(b) = buffer {
        param_values.push(Box::new(b.to_string()));
    }
    param_values.push(Box::new(limit as i64));

    // Build with positional params
    let mut sql = String::from(
        "SELECT m.id, m.msg_id, m.network, m.buffer, m.timestamp, m.type, m.nick, m.text, m.highlight, m.iv \
         FROM messages m JOIN messages_fts fts ON m.id = fts.rowid \
         WHERE messages_fts MATCH ?1",
    );
    let safe_query = format!("\"{}\"", query.replace('"', "\"\""));

    let mut params_vec: Vec<String> = vec![safe_query];

    if let Some(n) = network {
        sql.push_str(&format!(" AND m.network = ?{}", params_vec.len() + 1));
        params_vec.push(n.to_string());
    }
    if let Some(b) = buffer {
        sql.push_str(&format!(" AND m.buffer = ?{}", params_vec.len() + 1));
        params_vec.push(b.to_string());
    }

    sql.push_str(&format!(" ORDER BY m.timestamp DESC LIMIT ?{}", params_vec.len() + 1));
    params_vec.push(limit.to_string());

    let mut stmt = db.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        params_vec.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();

    let mut rows: Vec<StoredMessage> = stmt
        .query_map(param_refs.as_slice(), |row| map_row(row, false, None))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    rows.reverse();
    Ok(rows)
}

/// List all buffers that have logged messages for a network.
pub fn get_buffers(db: &Connection, network: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = db.prepare(
        "SELECT DISTINCT buffer FROM messages WHERE network = ?1 ORDER BY buffer",
    )?;
    let rows = stmt.query_map(params![network], |row| row.get(0))?;
    rows.collect()
}

/// Get total message count.
pub fn get_message_count(db: &Connection) -> rusqlite::Result<u64> {
    db.query_row("SELECT COUNT(*) FROM messages", [], |row| {
        row.get::<_, i64>(0).map(|n| n as u64)
    })
}

// === Read Markers ===

/// Upsert a read marker.
pub fn update_read_marker(
    db: &Connection,
    network: &str,
    buffer: &str,
    client: &str,
    timestamp: i64,
) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO read_markers (network, buffer, client, last_read) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (network, buffer, client) DO UPDATE SET last_read = excluded.last_read",
        params![network, buffer, client, timestamp],
    )?;
    Ok(())
}

/// Get the read marker for a specific client.
pub fn get_read_marker(
    db: &Connection,
    network: &str,
    buffer: &str,
    client: &str,
) -> rusqlite::Result<Option<i64>> {
    let mut stmt = db.prepare(
        "SELECT last_read FROM read_markers WHERE network = ?1 AND buffer = ?2 AND client = ?3",
    )?;
    let mut rows = stmt.query_map(params![network, buffer, client], |row| row.get(0))?;
    Ok(rows.next().transpose()?)
}

/// Get all read markers for a buffer.
pub fn get_read_markers(
    db: &Connection,
    network: &str,
    buffer: &str,
) -> rusqlite::Result<Vec<crate::storage::types::ReadMarker>> {
    let mut stmt = db.prepare(
        "SELECT network, buffer, client, last_read FROM read_markers WHERE network = ?1 AND buffer = ?2",
    )?;
    let rows = stmt.query_map(params![network, buffer], |row| {
        Ok(crate::storage::types::ReadMarker {
            network: row.get(0)?,
            buffer: row.get(1)?,
            client: row.get(2)?,
            last_read: row.get(3)?,
        })
    })?;
    rows.collect()
}

/// Count unread messages for a client.
pub fn get_unread_count(
    db: &Connection,
    network: &str,
    buffer: &str,
    client: &str,
) -> rusqlite::Result<u64> {
    let marker = get_read_marker(db, network, buffer, client)?;
    match marker {
        Some(ts) => db.query_row(
            "SELECT COUNT(*) FROM messages WHERE network = ?1 AND buffer = ?2 AND timestamp > ?3",
            params![network, buffer, ts],
            |row| row.get::<_, i64>(0).map(|n| n as u64),
        ),
        None => db.query_row(
            "SELECT COUNT(*) FROM messages WHERE network = ?1 AND buffer = ?2",
            params![network, buffer],
            |row| row.get::<_, i64>(0).map(|n| n as u64),
        ),
    }
}

fn map_row(
    row: &rusqlite::Row,
    encrypt: bool,
    crypto_key: Option<&Key<Aes256Gcm>>,
) -> rusqlite::Result<StoredMessage> {
    let text: String = row.get(7)?;
    let iv: Option<Vec<u8>> = row.get(9)?;

    let decrypted_text = if encrypt {
        if let (Some(key), Some(iv_bytes)) = (crypto_key, iv.as_ref()) {
            crypto::decrypt(&text, iv_bytes, key).unwrap_or(text)
        } else {
            text
        }
    } else {
        text
    };

    Ok(StoredMessage {
        id: row.get(0)?,
        msg_id: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        network: row.get(2)?,
        buffer: row.get(3)?,
        timestamp: row.get(4)?,
        msg_type: row.get(5)?,
        nick: row.get(6)?,
        text: decrypted_text,
        highlight: row.get::<_, i32>(8)? == 1,
    })
}
```

**Step 4: Update mod.rs**

Add `pub mod query;` to `src/storage/mod.rs`.

**Step 5: Run tests**

Run: `cargo test --lib storage::query`
Expected: all 7 tests pass

**Step 6: Commit**

```bash
git add src/storage/
git commit -m "feat(storage): query module with pagination, FTS5, read markers"
```

---

### Task 7: Public API and Integration (mod.rs)

**Files:**
- Modify: `src/storage/mod.rs`
- Modify: `src/app.rs`
- Modify: `src/state/events.rs` or integrate in `src/app.rs`

**Step 1: Finalize mod.rs public API**

Replace `src/storage/mod.rs` with:

```rust
pub mod crypto;
pub mod db;
pub mod query;
pub mod types;
pub mod writer;

pub use types::{LogRow, ReadMarker, StorageStats, StoredMessage};
pub use writer::LogWriterHandle;

use std::sync::{Arc, Mutex};

use aes_gcm::{Aes256Gcm, Key};
use tokio::sync::mpsc;

use crate::config::LoggingConfig;
use crate::state::buffer::MessageType;

/// Shared handle to the storage system.
pub struct Storage {
    pub db: Arc<Mutex<rusqlite::Connection>>,
    writer_handle: Option<LogWriterHandle>,
    log_tx: Option<mpsc::UnboundedSender<LogRow>>,
    encrypt: bool,
    crypto_key: Option<Key<Aes256Gcm>>,
    exclude_types: Vec<String>,
}

impl Storage {
    /// Initialize the storage system. Returns None if logging is disabled.
    pub fn init(config: &LoggingConfig) -> Result<Option<Self>, String> {
        if !config.enabled {
            return Ok(None);
        }

        let db_path = crate::constants::home_dir().join("logs.db");
        let db_path_str = db_path.to_string_lossy().to_string();

        let db = db::open_database_at(&db_path_str, config.encrypt)
            .map_err(|e| format!("Failed to open log database: {e}"))?;

        // Purge old messages
        if config.retention_days > 0 {
            let purged = db::purge_old_messages(&db, config.retention_days, !config.encrypt);
            if purged > 0 {
                tracing::info!("Purged {purged} old log messages");
            }
        }

        let crypto_key = if config.encrypt {
            let hex = crypto::load_or_create_key()
                .map_err(|e| format!("Failed to load encryption key: {e}"))?;
            Some(crypto::import_key(&hex).map_err(|e| format!("Invalid key: {e}"))?)
        } else {
            None
        };

        let db = Arc::new(Mutex::new(db));
        let (writer_handle, log_tx) =
            LogWriterHandle::spawn(db.clone(), config.encrypt, crypto_key);

        Ok(Some(Storage {
            db,
            writer_handle: Some(writer_handle),
            log_tx: Some(log_tx),
            encrypt: config.encrypt,
            crypto_key,
            exclude_types: config.exclude_types.clone(),
        }))
    }

    /// Log a message. Call after every add_message().
    pub fn log_message(
        &self,
        network: &str,
        buffer: &str,
        msg_id: &str,
        msg_type: &MessageType,
        text: &str,
        nick: Option<&str>,
        highlight: bool,
        timestamp: chrono::DateTime<chrono::Utc>,
    ) {
        let type_str = format!("{:?}", msg_type).to_lowercase();
        if self.exclude_types.contains(&type_str) {
            return;
        }

        if let Some(tx) = &self.log_tx {
            let row = LogRow {
                msg_id: msg_id.to_string(),
                network: network.to_string(),
                buffer: buffer.to_string(),
                timestamp: timestamp.timestamp_millis(),
                msg_type: msg_type.clone(),
                nick: nick.map(|s| s.to_string()),
                text: text.to_string(),
                highlight,
            };
            let _ = tx.send(row);
        }
    }

    /// Flush and shutdown the writer.
    pub async fn shutdown(mut self) {
        // Drop sender first so writer can drain
        self.log_tx.take();
        if let Some(handle) = self.writer_handle.take() {
            handle.shutdown().await;
        }
    }
}
```

**Step 2: Add `storage` field to App and wire init/shutdown**

In `src/app.rs`, add to the `App` struct:

```rust
/// Message logging storage (None if disabled).
pub storage: Option<crate::storage::Storage>,
```

In `App::new()`, after config loading, add:

```rust
let storage = match crate::storage::Storage::init(&config.logging) {
    Ok(s) => s,
    Err(e) => {
        tracing::error!("Storage init failed: {e}");
        None
    }
};
```

And include `storage` in the `App` struct initialization.

**Step 3: Wire log_message into add_message flow**

In `src/app.rs`, find where messages are added to buffers and add logging calls after each `add_message` / `add_message_with_activity`. The cleanest approach: add a helper method on App:

```rust
/// Log a message to persistent storage (if enabled).
fn log_to_storage(&self, buffer_id: &str, msg: &crate::state::buffer::Message) {
    let Some(storage) = &self.storage else { return };
    // Extract network (connection_id) from buffer
    let network = buffer_id.split('/').next().unwrap_or(buffer_id);
    let buffer_name = buffer_id.split('/').nth(1).unwrap_or(buffer_id);
    storage.log_message(
        network,
        buffer_name,
        &msg.id.to_string(),
        &msg.message_type,
        &msg.text,
        msg.nick.as_deref(),
        msg.highlight,
        msg.timestamp,
    );
}
```

Call `self.log_to_storage(buffer_id, &msg)` after every `self.state.add_message(...)` and `self.state.add_message_with_activity(...)` in `handle_irc_event()` and command handlers.

**Step 4: Wire shutdown**

In `src/app.rs`, after the main loop ends (after `while !self.should_quit`), add:

```rust
// Shutdown storage
if let Some(storage) = self.storage.take() {
    storage.shutdown().await;
}
```

**Step 5: Run all tests**

Run: `cargo test`
Expected: all tests pass (existing + new storage tests)

**Step 6: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: 0 errors

**Step 7: Commit**

```bash
git add src/storage/ src/app.rs src/main.rs
git commit -m "feat(storage): wire storage into app lifecycle and message flow"
```

---

### Task 8: Command Documentation

**Files:**
- Create: `docs/commands/log.md`

**Step 1: Create help doc for /log command (future)**

```markdown
---
category: Info
description: View logging status and stats
---

# /log

## Syntax

    /log [stats|search <query>]

## Description

View message logging status and statistics, or search logged messages.

With no arguments, shows whether logging is enabled and basic stats (message count, database size).

## Subcommands

### stats

Show detailed logging statistics including message count, database size, and configuration.

    /log stats

### search

Search through logged messages using full-text search.

    /log search <query>

## Examples

    /log
    /log stats
    /log search hello world

## See Also

/set logging.enabled, /set logging.encrypt
```

**Step 2: Commit**

```bash
git add docs/commands/
git commit -m "docs: add /log command help"
```

---

### Task 9: Build and Verify

**Step 1: Run full test suite**

Run: `cargo test`
Expected: all tests pass (existing 285 + ~25 new storage tests)

**Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings -W clippy::perf`
Expected: 0 errors

**Step 3: Build release binary**

Run: `cargo build --release`
Expected: compiles cleanly

**Step 4: Check binary size**

Run: `ls -lh target/release/repartee`
Expected: reasonable size (likely ~5-6MB with bundled SQLite)

**Step 5: Commit any remaining fixes**

```bash
git add -A
git commit -m "feat(storage): storage system complete — SQLite logging with FTS5 and encryption"
```

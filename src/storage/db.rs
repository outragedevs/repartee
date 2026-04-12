use rusqlite::{Connection, params};

const CREATE_MESSAGES: &str = "
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
    iv        BLOB,
    ref_id    TEXT,
    tags      TEXT
)";

const CREATE_MESSAGES_IDX: &str = "
CREATE INDEX IF NOT EXISTS idx_messages_network_buffer
ON messages (network, buffer, timestamp)";

const CREATE_MESSAGES_MSG_ID_IDX: &str = "
CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_msg_id
ON messages (msg_id) WHERE msg_id IS NOT NULL";

/// Partial index for fast event pruning — only indexes rows with type='event'.
const CREATE_MESSAGES_EVENT_IDX: &str = "
CREATE INDEX IF NOT EXISTS idx_messages_event_timestamp
ON messages (timestamp) WHERE type = 'event'";

const CREATE_READ_MARKERS: &str = "
CREATE TABLE IF NOT EXISTS read_markers (
    network   TEXT NOT NULL,
    buffer    TEXT NOT NULL,
    client    TEXT NOT NULL,
    last_read INTEGER NOT NULL,
    PRIMARY KEY (network, buffer, client)
)";

const CREATE_FTS: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts
USING fts5(nick, text, content=messages, content_rowid=id)";

const CREATE_FTS_TRIGGERS: &str = "
CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, nick, text)
    VALUES (new.id, new.nick, new.text);
END;
CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, nick, text)
    VALUES ('delete', old.id, old.nick, old.text);
END;
CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, nick, text)
    VALUES ('delete', old.id, old.nick, old.text);
    INSERT INTO messages_fts(rowid, nick, text)
    VALUES (new.id, new.nick, new.text);
END";

fn apply_pragmas(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;",
    )
}

/// `buffer` is the lowercased buffer name (matches messages table).
/// `channel` is the display name (e.g. `#Rust` with original casing).
const CREATE_MENTIONS: &str = "
CREATE TABLE IF NOT EXISTS mentions (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp INTEGER NOT NULL,
    network   TEXT NOT NULL,
    buffer    TEXT NOT NULL,
    channel   TEXT NOT NULL,
    nick      TEXT NOT NULL,
    text      TEXT NOT NULL,
    read_at   INTEGER
)";

const CREATE_MENTIONS_IDX: &str = "
CREATE INDEX IF NOT EXISTS idx_mentions_unread
ON mentions (read_at) WHERE read_at IS NULL";

const CREATE_MENTIONS_TIMESTAMP_IDX: &str = "
CREATE INDEX IF NOT EXISTS idx_mentions_timestamp
ON mentions (timestamp)";

// ---------- RPE2E tables ----------

const CREATE_E2E_IDENTITY: &str = "
CREATE TABLE IF NOT EXISTS e2e_identity (
    id            INTEGER PRIMARY KEY CHECK (id = 1),
    pubkey        BLOB NOT NULL,
    privkey       BLOB NOT NULL,
    fingerprint   BLOB NOT NULL,
    created_at    INTEGER NOT NULL
)";

const CREATE_E2E_PEERS: &str = "
CREATE TABLE IF NOT EXISTS e2e_peers (
    fingerprint   BLOB PRIMARY KEY,
    pubkey        BLOB NOT NULL,
    last_handle   TEXT,
    last_nick     TEXT,
    first_seen    INTEGER NOT NULL,
    last_seen     INTEGER NOT NULL,
    global_status TEXT NOT NULL DEFAULT 'pending'
)";

const CREATE_E2E_OUTGOING: &str = "
CREATE TABLE IF NOT EXISTS e2e_outgoing_sessions (
    channel           TEXT PRIMARY KEY,
    sk                BLOB NOT NULL,
    created_at        INTEGER NOT NULL,
    pending_rotation  INTEGER NOT NULL DEFAULT 0
)";

const CREATE_E2E_INCOMING: &str = "
CREATE TABLE IF NOT EXISTS e2e_incoming_sessions (
    handle       TEXT NOT NULL,
    channel      TEXT NOT NULL,
    fingerprint  BLOB NOT NULL,
    sk           BLOB NOT NULL,
    status       TEXT NOT NULL DEFAULT 'pending',
    created_at   INTEGER NOT NULL,
    PRIMARY KEY (handle, channel)
)";

const CREATE_E2E_CHANNEL_CONFIG: &str = "
CREATE TABLE IF NOT EXISTS e2e_channel_config (
    channel  TEXT PRIMARY KEY,
    enabled  INTEGER NOT NULL DEFAULT 0,
    mode     TEXT NOT NULL DEFAULT 'normal'
)";

const CREATE_E2E_AUTOTRUST: &str = "
CREATE TABLE IF NOT EXISTS e2e_autotrust (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    scope           TEXT NOT NULL,
    handle_pattern  TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    UNIQUE(scope, handle_pattern)
)";

/// Recipients of our outgoing session key, per channel. Populated when we
/// serve a KEYRSP (auto-accept or explicit /e2e accept), consumed by the
/// lazy-rotate distribution loop to know which peers must receive a REKEY
/// when we regenerate the outgoing session for a channel. This is NOT the
/// same as `e2e_incoming_sessions` — that table tracks session keys used
/// to DECRYPT peer-sent messages; this one tracks the peer's handle +
/// fingerprint so we can re-push our outgoing key after a /e2e revoke.
const CREATE_E2E_OUTGOING_RECIPIENTS: &str = "
CREATE TABLE IF NOT EXISTS e2e_outgoing_recipients (
    channel        TEXT NOT NULL,
    handle         TEXT NOT NULL,
    fingerprint    BLOB NOT NULL,
    first_sent_at  INTEGER NOT NULL,
    PRIMARY KEY (channel, handle)
)";

fn create_schema(db: &Connection, encrypt: bool) -> rusqlite::Result<()> {
    db.execute_batch(CREATE_MESSAGES)?;
    db.execute_batch(CREATE_MESSAGES_IDX)?;
    db.execute_batch(CREATE_MESSAGES_MSG_ID_IDX)?;
    db.execute_batch(CREATE_MESSAGES_EVENT_IDX)?;
    db.execute_batch(CREATE_READ_MARKERS)?;
    db.execute_batch(CREATE_MENTIONS)?;
    db.execute_batch(CREATE_MENTIONS_IDX)?;
    db.execute_batch(CREATE_MENTIONS_TIMESTAMP_IDX)?;
    // RPE2E tables — always created, independent of the message-log encrypt flag.
    db.execute_batch(CREATE_E2E_IDENTITY)?;
    db.execute_batch(CREATE_E2E_PEERS)?;
    db.execute_batch(CREATE_E2E_OUTGOING)?;
    db.execute_batch(CREATE_E2E_INCOMING)?;
    db.execute_batch(CREATE_E2E_CHANNEL_CONFIG)?;
    db.execute_batch(CREATE_E2E_AUTOTRUST)?;
    db.execute_batch(CREATE_E2E_OUTGOING_RECIPIENTS)?;
    if !encrypt {
        db.execute_batch(CREATE_FTS)?;
        db.execute_batch(CREATE_FTS_TRIGGERS)?;
    }
    migrate_schema(db);
    Ok(())
}

/// Add columns that may be missing from older database files.
///
/// `ALTER TABLE ADD COLUMN` returns a "duplicate column name" error if the
/// column already exists — that is expected and silenced.  Any *other* error
/// (permissions, corruption, wrong table) is logged as a warning so it does
/// not go unnoticed.
fn migrate_schema(db: &Connection) {
    for col in ["ref_id TEXT", "tags TEXT", "event_key TEXT"] {
        let sql = format!("ALTER TABLE messages ADD COLUMN {col}");
        if let Err(e) = db.execute_batch(&sql) {
            if !e.to_string().contains("duplicate column name") {
                tracing::warn!("migration warning for '{col}': {e}");
            }
        } else {
            tracing::info!("migrated messages table: added {col}");
        }
    }
}

#[cfg(test)]
pub fn open_database(encrypt: bool) -> rusqlite::Result<Connection> {
    let db = Connection::open_in_memory()?;
    apply_pragmas(&db)?;
    create_schema(&db, encrypt)?;
    Ok(db)
}

pub fn open_database_at(path: &str, encrypt: bool) -> rusqlite::Result<Connection> {
    let db = Connection::open(path)?;
    apply_pragmas(&db)?;
    create_schema(&db, encrypt)?;
    Ok(db)
}

pub fn purge_old_messages(db: &Connection, retention_days: u32, has_fts: bool) -> usize {
    let cutoff = chrono::Utc::now().timestamp() - i64::from(retention_days) * 86400;

    if has_fts
        && let Err(e) = db.execute(
            "INSERT INTO messages_fts(messages_fts, rowid, nick, text)
             SELECT 'delete', id, nick, text
             FROM messages WHERE timestamp < ?1",
            params![cutoff],
        )
    {
        tracing::warn!("Failed to delete FTS entries during purge: {e}");
    }

    match db.execute("DELETE FROM messages WHERE timestamp < ?1", params![cutoff]) {
        Ok(count) => count,
        Err(e) => {
            tracing::warn!("Failed to purge old messages: {e}");
            0
        }
    }
}

/// Delete event-type messages (join/part/quit/nick/kick/mode) older than `hours`.
///
/// Uses the partial index `idx_messages_event_timestamp` for fast scans.
/// FTS entries are cleaned both manually (for large batch efficiency) and
/// via the AFTER DELETE trigger as a safety net.
pub fn purge_old_events(db: &Connection, hours: u32, has_fts: bool) -> usize {
    let cutoff = chrono::Utc::now().timestamp() - i64::from(hours) * 3600;

    // For FTS-enabled databases, manually remove FTS entries first (triggers handle it,
    // but explicit delete-before is safer for large batch deletes).
    if has_fts
        && let Err(e) = db.execute(
            "INSERT INTO messages_fts(messages_fts, rowid, nick, text)
             SELECT 'delete', id, nick, text
             FROM messages WHERE type = 'event' AND timestamp < ?1",
            params![cutoff],
        )
    {
        tracing::warn!("Failed to delete FTS entries during event purge: {e}");
    }

    match db.execute(
        "DELETE FROM messages WHERE type = 'event' AND timestamp < ?1",
        params![cutoff],
    ) {
        Ok(count) => count,
        Err(e) => {
            tracing::warn!("Failed to purge old events: {e}");
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_exists(db: &Connection, name: &str) -> bool {
        db.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type IN ('table','view') AND name = ?1",
            params![name],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
            > 0
    }

    #[test]
    fn open_creates_tables() {
        let db = open_database(false).unwrap();
        assert!(table_exists(&db, "messages"));
        assert!(table_exists(&db, "mentions"));
    }

    #[test]
    fn open_creates_read_markers_table() {
        let db = open_database(false).unwrap();
        assert!(table_exists(&db, "read_markers"));
    }

    #[test]
    fn open_creates_e2e_tables() {
        let db = open_database(false).unwrap();
        for t in [
            "e2e_identity",
            "e2e_peers",
            "e2e_outgoing_sessions",
            "e2e_incoming_sessions",
            "e2e_channel_config",
            "e2e_autotrust",
        ] {
            assert!(table_exists(&db, t), "missing table {t}");
        }
    }

    #[test]
    fn open_creates_e2e_tables_when_encrypted() {
        let db = open_database(true).unwrap();
        assert!(table_exists(&db, "e2e_identity"));
        assert!(table_exists(&db, "e2e_peers"));
    }

    #[test]
    fn open_creates_fts_when_not_encrypted() {
        let db = open_database(false).unwrap();
        assert!(table_exists(&db, "messages_fts"));
    }

    #[test]
    fn open_skips_fts_when_encrypted() {
        let db = open_database(true).unwrap();
        assert!(!table_exists(&db, "messages_fts"));
    }

    #[test]
    fn purge_removes_old_messages() {
        let db = open_database(false).unwrap();
        let now = chrono::Utc::now().timestamp();
        let old = now - 100 * 86400; // 100 days ago

        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                "old1",
                "net",
                "#chan",
                old,
                "Message",
                "alice",
                "old message",
                0
            ],
        )
        .unwrap();

        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                "new1",
                "net",
                "#chan",
                now,
                "Message",
                "bob",
                "new message",
                0
            ],
        )
        .unwrap();

        let removed = purge_old_messages(&db, 30, true);
        assert_eq!(removed, 1);

        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let remaining: String = db
            .query_row("SELECT msg_id FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining, "new1");
    }

    #[test]
    fn purge_events_removes_only_old_events() {
        let db = open_database(false).unwrap();
        let now = chrono::Utc::now().timestamp();
        let old = now - 100 * 3600; // 100 hours ago

        // Old event (should be purged)
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                "ev_old",
                "net",
                "#chan",
                old,
                "event",
                "alice",
                "alice joined",
                0
            ],
        )
        .unwrap();

        // Old chat message (should NOT be purged)
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                "msg_old", "net", "#chan", old, "message", "alice", "hello", 0
            ],
        )
        .unwrap();

        // Recent event (should NOT be purged)
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                "ev_new",
                "net",
                "#chan",
                now,
                "event",
                "bob",
                "bob joined",
                0
            ],
        )
        .unwrap();

        // Recent chat message (should NOT be purged)
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params!["msg_new", "net", "#chan", now, "message", "bob", "hi", 0],
        )
        .unwrap();

        let removed = purge_old_events(&db, 72, true);
        assert_eq!(removed, 1, "only the old event should be purged");

        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 3, "3 messages should remain");

        // Verify old chat message survived
        let old_msg: String = db
            .query_row(
                "SELECT msg_id FROM messages WHERE msg_id = 'msg_old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_msg, "msg_old");
    }

    #[test]
    fn purge_events_leaves_chat_messages_untouched() {
        let db = open_database(false).unwrap();
        let old = chrono::Utc::now().timestamp() - 200 * 3600; // 200 hours ago

        // Insert only chat messages (type != "event")
        for (id, typ) in [("m1", "message"), ("m2", "action"), ("m3", "notice")] {
            db.execute(
                "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![id, "net", "#chan", old, typ, "x", "hello", 0],
            )
            .unwrap();
        }

        let removed = purge_old_events(&db, 72, true);
        assert_eq!(
            removed, 0,
            "chat messages should never be purged by event pruner"
        );

        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 3);
    }
}

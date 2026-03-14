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

fn create_schema(db: &Connection, encrypt: bool) -> rusqlite::Result<()> {
    db.execute_batch(CREATE_MESSAGES)?;
    db.execute_batch(CREATE_MESSAGES_IDX)?;
    db.execute_batch(CREATE_MESSAGES_MSG_ID_IDX)?;
    db.execute_batch(CREATE_READ_MARKERS)?;
    db.execute_batch(CREATE_MENTIONS)?;
    db.execute_batch(CREATE_MENTIONS_IDX)?;
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
    for col in ["ref_id TEXT", "tags TEXT"] {
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
}

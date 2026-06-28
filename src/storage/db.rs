use rusqlite::{Connection, params};

const CREATE_MESSAGES: &str = "
CREATE TABLE IF NOT EXISTS messages (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    msg_id    TEXT,
    network   TEXT NOT NULL,
    buffer    TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    ts_ms     INTEGER,
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

// Unique on (network, msg_id), NOT msg_id alone: an IRCv3 @msgid is only unique
// per network/server, so the same @msgid from two configured networks are
// distinct messages. A global unique index would make INSERT OR IGNORE silently
// drop the second one. Live↔CHATHISTORY dedup still works because both copies of
// a message carry the same (network, @msgid).
const CREATE_MESSAGES_MSG_ID_IDX: &str = "
CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_network_msg_id
ON messages (network, msg_id) WHERE msg_id IS NOT NULL";

/// Partial index for fast event pruning — only indexes rows with type='event'.
const CREATE_MESSAGES_EVENT_IDX: &str = "
CREATE INDEX IF NOT EXISTS idx_messages_event_timestamp
ON messages (timestamp) WHERE type = 'event'";

// Expression index matching the subsecond scroll-back ordering key
// `COALESCE(ts_ms, timestamp * 1000)` (see `ts_ms_expr`). Without it the
// scroll-back, web FetchMessages and log-browser hot paths order by an
// expression no existing index covers, so SQLite materializes and sorts every
// row in the buffer (USE TEMP B-TREE FOR ORDER BY) before applying LIMIT —
// O(N log N) on large logs. With it SQLite walks the index in reverse and stops
// at LIMIT. The trailing `id` matches the `m.id DESC` tiebreaker so the keyset
// cursor is fully index-satisfied. Created only after the `ts_ms` column is
// guaranteed present (see `migrate_schema`), since the expression references it.
const CREATE_MESSAGES_SUBSECOND_IDX: &str = "
CREATE INDEX IF NOT EXISTS idx_messages_subsecond
ON messages (network, buffer, COALESCE(ts_ms, timestamp * 1000), id)";

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

// (network, nick) -> last-seen DM handle, used to resolve a query peer's
// `ident@host` before they speak this session. Keyed by (network, nick) rather
// than the fingerprint so the SAME E2E identity seen on two networks gets one
// cache row per network — `e2e_peers` keeps only one (global-newest) handle per
// fingerprint and cannot be network-scoped.
const CREATE_E2E_DM_HANDLE_CACHE: &str = "
CREATE TABLE IF NOT EXISTS e2e_dm_handle_cache (
    network TEXT NOT NULL,
    nick    TEXT NOT NULL COLLATE NOCASE,
    handle  TEXT NOT NULL,
    PRIMARY KEY (network, nick)
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
    db.execute_batch(CREATE_E2E_DM_HANDLE_CACHE)?;
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
    // `ts_ms` holds the full-millisecond message time (the IRCv3 `@time`), added
    // so scroll-back pagination can order same-second rows by real time instead
    // of insertion id (a backfilled older row gets a larger autoincrement id and
    // would otherwise be skipped by a `(timestamp, id)` keyset cursor). It is
    // left NULL on existing rows: read paths `COALESCE(ts_ms, timestamp * 1000)`,
    // so un-migrated rows keep exactly their old second-resolution ordering — no
    // backfill UPDATE is run, keeping the migration cheap on large logs.
    for col in ["ref_id TEXT", "tags TEXT", "event_key TEXT", "ts_ms INTEGER"] {
        let sql = format!("ALTER TABLE messages ADD COLUMN {col}");
        if let Err(e) = db.execute_batch(&sql) {
            if !e.to_string().contains("duplicate column name") {
                tracing::warn!("migration warning for '{col}': {e}");
            }
        } else {
            tracing::info!("migrated messages table: added {col}");
        }
    }

    // Build the subsecond-ordering index now that `ts_ms` is guaranteed to exist
    // (the expression references it). Cheap on existing logs — it indexes a
    // computed key, not a backfill UPDATE. Idempotent via IF NOT EXISTS.
    if let Err(e) = db.execute_batch(CREATE_MESSAGES_SUBSECOND_IDX) {
        tracing::warn!("migration warning creating idx_messages_subsecond: {e}");
    }
    // The legacy global-unique `idx_messages_msg_id` only exists on databases
    // created by `main` (before the composite key). Its presence is our one-shot
    // signal that this database predates @msgid-keyed logging and needs the
    // backfill below — once dropped, neither runs again.
    let upgraded_from_legacy: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_messages_msg_id'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    // Replace the old global-unique msg_id index with the composite
    // (network, msg_id) one (created by CREATE_MESSAGES_MSG_ID_IDX above). The
    // composite is a weaker constraint, so dropping the global never conflicts
    // with existing data. No-op once the old index is gone. Dropped BEFORE the
    // backfill so re-keying two same-@msgid rows on different networks isn't
    // rejected by the stricter global constraint.
    if let Err(e) = db.execute_batch("DROP INDEX IF EXISTS idx_messages_msg_id") {
        tracing::warn!("migration warning dropping idx_messages_msg_id: {e}");
    }

    if upgraded_from_legacy > 0 {
        backfill_msgid_keys(db);
    }
}

/// One-time backfill for databases upgraded from `main`: live conversational
/// rows there were stored with a random-UUID `msg_id` even when their server
/// `@msgid` was already present in the `tags` JSON. CHATHISTORY dedup keys on
/// `(network, msg_id)` = the server `@msgid`, so without this a replay of
/// already-logged history inserts duplicate rows after upgrade.
///
/// Re-key only conversational rows (`message`/`notice`/`action`) whose stored
/// `msg_id` differs from `tags.msgid`. Reference/event rows are excluded: fan-out
/// reference rows deliberately carry fresh UUIDs (siblings share one `@msgid`),
/// and conversational rows are never a fan-out ref target — so this never breaks
/// the `ref_id -> msg_id` linkage. `UPDATE OR IGNORE` skips the rare row whose
/// target key is already taken by a pre-existing duplicate (a message `main`
/// happened to log twice), leaving it untouched rather than aborting.
fn backfill_msgid_keys(db: &Connection) {
    let sql = "UPDATE OR IGNORE messages
                  SET msg_id = json_extract(tags, '$.msgid')
                WHERE type IN ('message', 'notice', 'action')
                  AND ref_id IS NULL
                  AND tags IS NOT NULL
                  AND json_extract(tags, '$.msgid') IS NOT NULL
                  AND json_extract(tags, '$.msgid') <> ''
                  AND msg_id <> json_extract(tags, '$.msgid')";
    match db.execute(sql, []) {
        Ok(n) => tracing::info!("migrated messages table: re-keyed {n} row(s) to their @msgid"),
        Err(e) => tracing::warn!("migration warning backfilling @msgid keys: {e}"),
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

/// Open the message database read-only.
///
/// Used by the `repartee l` log browser. The `mode=ro` URI flag rejects
/// all writes at the `SQLite` layer, so the daemon (running concurrently
/// against the same WAL) is never blocked. No pragmas are applied —
/// `journal_mode=WAL` is a write operation and the file already carries
/// the WAL setting from when the daemon created it. No schema creation:
/// the database must already exist (we error out otherwise).
///
/// Path is percent-encoded for the URI form before being concatenated
/// — a home directory like `/Users/Foo Bar/.repartee/...` would
/// otherwise produce a malformed `file:` URI.
pub fn open_readonly_at(path: &str) -> rusqlite::Result<Connection> {
    let uri = format!("file:{}?mode=ro", encode_sqlite_uri_path(path));
    Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
}

/// Percent-encode characters that have special meaning inside a
/// `SQLite` `file:` URI. Conservative encoder — only escapes the specific
/// characters that break URI parsing (`?`, `#`, `%`, whitespace) and
/// the byte range outside printable ASCII. `/` and `:` are preserved
/// so `/tmp/x.db` stays readable. Per `SQLite` docs any character may
/// be percent-encoded; we deliberately keep the encoded set narrow to
/// keep error messages readable when the path appears in diagnostics.
fn encode_sqlite_uri_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.as_bytes() {
        match byte {
            b'?' | b'#' | b'%' | b' ' | b'\t' | b'\n' | b'\r' => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{byte:02X}");
            }
            b if b.is_ascii() && !b.is_ascii_control() => out.push(*b as char),
            b => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
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

    fn column_exists(db: &Connection, table: &str, column: &str) -> bool {
        db.prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .filter_map(Result::ok)
            .any(|name| name == column)
    }

    fn index_exists(db: &Connection, name: &str) -> bool {
        db.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
            params![name],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
            > 0
    }

    #[test]
    fn migration_adds_ts_ms_to_pre_existing_database() {
        // Simulate an old log DB whose `messages` table predates the `ts_ms`
        // column, with a row already stored. create_schema must add the column
        // (CREATE TABLE IF NOT EXISTS is a no-op, so the ALTER in migrate_schema
        // is what backfills the schema) without disturbing the existing row.
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE messages (
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
                 tags      TEXT,
                 event_key TEXT
             )",
        )
        .unwrap();
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight) \
             VALUES ('old-1', 'libera', '#rust', 100, 'message', 'ada', 'legacy line', 0)",
            [],
        )
        .unwrap();
        assert!(!column_exists(&db, "messages", "ts_ms"), "precondition: no ts_ms");

        create_schema(&db, false).unwrap();

        assert!(column_exists(&db, "messages", "ts_ms"), "migration added ts_ms");
        // The subsecond-ordering index is built once ts_ms exists, so upgraded
        // logs get the same index-backed scroll-back as fresh ones (no temp
        // B-tree sort over the whole buffer). It references ts_ms, so it cannot
        // exist before the column was added above.
        assert!(
            index_exists(&db, "idx_messages_subsecond"),
            "migration created idx_messages_subsecond"
        );
        // The pre-existing row keeps NULL ts_ms; the read path COALESCEs it to
        // timestamp*1000, so it still paginates with whole-second ordering.
        let rows = crate::storage::query::get_messages_paginated_subsecond(
            &db, "libera", "#rust", None, 10, false, None,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "legacy line");
        assert_eq!(rows[0].ts_ms, 100_000);
    }

    /// Build an in-memory DB shaped like a non-encrypted one created by `main`:
    /// the legacy global-unique `idx_messages_msg_id` index, no `ts_ms` column,
    /// and the external-content FTS (with its triggers) that `main` populated on
    /// every insert. The FTS is included so inserted rows get indexed — exactly
    /// like a real upgraded database — so the backfill's `messages_au` trigger
    /// finds them instead of tripping an FTS "malformed" error on a phantom row.
    fn legacy_main_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        apply_pragmas(&db).unwrap();
        db.execute_batch(
            "CREATE TABLE messages (
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
                 tags      TEXT,
                 event_key TEXT
             );
             CREATE UNIQUE INDEX idx_messages_msg_id ON messages (msg_id) WHERE msg_id IS NOT NULL;",
        )
        .unwrap();
        db.execute_batch(CREATE_FTS).unwrap();
        db.execute_batch(CREATE_FTS_TRIGGERS).unwrap();
        db
    }

    #[test]
    fn migration_backfills_msgid_for_chathistory_dedup() {
        // A `main` live row: random-UUID msg_id, but the real server @msgid sits
        // in the tags JSON. After migration its msg_id must become that @msgid so
        // a later CHATHISTORY replay (keyed on (network, @msgid)) dedups instead
        // of inserting a duplicate.
        let db = legacy_main_db();
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight, tags) \
             VALUES ('11111111-2222-3333-4444-555555555555', 'libera', '#rust', 100, 'message', 'ada', 'hi', 0, \
                     '{\"time\":\"2024-01-01T00:00:00.000Z\",\"msgid\":\"server-abc\"}')",
            [],
        )
        .unwrap();

        create_schema(&db, false).unwrap();

        let keyed: String = db
            .query_row("SELECT msg_id FROM messages WHERE text = 'hi'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(keyed, "server-abc", "re-keyed to the @msgid from tags");

        // Simulate the CHATHISTORY replay of the same message: INSERT OR IGNORE
        // under the new (network, msg_id) unique key must be a no-op.
        db.execute(
            "INSERT OR IGNORE INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight) \
             VALUES ('server-abc', 'libera', '#rust', 100, 'message', 'ada', 'hi', 0)",
            [],
        )
        .unwrap();
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "replay deduped against the backfilled @msgid");
    }

    #[test]
    fn migration_backfill_leaves_reference_and_keyless_rows_alone() {
        // Fan-out reference rows (ref_id set) keep their fresh UUID so siblings
        // sharing one @msgid don't collide; a row whose tags lack a msgid keeps
        // its UUID. Only plain conversational rows are re-keyed.
        let db = legacy_main_db();
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight, ref_id, tags) \
             VALUES ('ref-uuid', 'libera', '#rust', 100, 'event', 'ada', '', 0, 'primary-1', \
                     '{\"msgid\":\"shared-q\"}')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight, tags) \
             VALUES ('plain-uuid', 'libera', '#go', 100, 'message', 'ada', 'no msgid here', 0, '{\"time\":\"x\"}')",
            [],
        )
        .unwrap();

        create_schema(&db, false).unwrap();

        let ref_key: String = db
            .query_row("SELECT msg_id FROM messages WHERE ref_id = 'primary-1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ref_key, "ref-uuid", "reference row keeps its UUID");
        let plain_key: String = db
            .query_row("SELECT msg_id FROM messages WHERE buffer = '#go'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(plain_key, "plain-uuid", "row without tags.msgid keeps its UUID");
    }

    #[test]
    fn migration_backfill_skipped_for_fresh_database() {
        // A branch-native DB has no legacy index, so the backfill never runs —
        // and a fresh open is idempotent (the UUID stays put).
        let db = open_database(false).unwrap();
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight, tags) \
             VALUES ('keep-uuid', 'libera', '#rust', 100, 'message', 'ada', 'hi', 0, '{\"msgid\":\"server-xyz\"}')",
            [],
        )
        .unwrap();
        // Re-run schema creation (simulates next startup); no legacy index exists.
        create_schema(&db, false).unwrap();
        let key: String = db
            .query_row("SELECT msg_id FROM messages WHERE text = 'hi'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(key, "keep-uuid", "no backfill without the legacy index");
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

    #[test]
    fn encode_sqlite_uri_path_handles_special_chars() {
        // Plain path: untouched.
        assert_eq!(encode_sqlite_uri_path("/tmp/x.db"), "/tmp/x.db");
        // Spaces: encoded so `?mode=ro` parses correctly.
        assert_eq!(
            encode_sqlite_uri_path("/Users/Foo Bar/.repartee/messages.db"),
            "/Users/Foo%20Bar/.repartee/messages.db"
        );
        // Question mark, hash, percent: all encoded.
        assert_eq!(encode_sqlite_uri_path("/a?b#c%d"), "/a%3Fb%23c%25d");
    }

    #[test]
    fn open_readonly_handles_path_with_spaces() {
        let dir = std::env::temp_dir().join("repartee logbrowser ro space");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("messages.db");
        let path_str = path.to_str().unwrap();

        // Seed via the writable opener.
        let rw = open_database_at(path_str, false).unwrap();
        rw.execute(
            "INSERT INTO messages (timestamp, network, buffer, type, nick, text) \
             VALUES (?1, 'libera', '#test', 'message', 'ada', 'hello')",
            params![100],
        )
        .unwrap();
        drop(rw);

        let ro = open_readonly_at(path_str).unwrap();
        let count: i64 = ro
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_readonly_rejects_writes() {
        // Need a real file (URI mode=ro doesn't apply to in-memory).
        let dir = std::env::temp_dir().join("repartee_logbrowser_ro_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("messages.db");
        let path_str = path.to_str().unwrap();

        // Seed via the writable opener.
        let rw = open_database_at(path_str, false).unwrap();
        rw.execute(
            "INSERT INTO messages (timestamp, network, buffer, type, nick, text) \
             VALUES (?1, 'libera', '#test', 'message', 'ada', 'hello')",
            params![100],
        )
        .unwrap();
        drop(rw);

        let ro = open_readonly_at(path_str).unwrap();
        let count: i64 = ro
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let write_err = ro.execute(
            "INSERT INTO messages (timestamp, network, buffer, type, nick, text) \
             VALUES (1, 'a', 'b', 'message', 'c', 'd')",
            [],
        );
        assert!(write_err.is_err(), "read-only handle must reject writes");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

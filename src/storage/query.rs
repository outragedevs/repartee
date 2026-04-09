use aes_gcm::{Aes256Gcm, Key};
use rusqlite::{params, types::ToSql, Connection};

use super::crypto;
use super::types::{ReadMarker, StoredMessage};

/// Map a database row to a `StoredMessage`, optionally decrypting the text.
fn map_row(
    row: &rusqlite::Row,
    encrypt: bool,
    crypto_key: Option<&Key<Aes256Gcm>>,
) -> rusqlite::Result<StoredMessage> {
    let id: i64 = row.get("id")?;
    let msg_id: String = row.get("msg_id")?;
    let network: String = row.get("network")?;
    let buffer: String = row.get("buffer")?;
    let timestamp: i64 = row.get("timestamp")?;
    let msg_type: String = row.get("type")?;
    let nick: Option<String> = row.get("nick")?;
    let stored_text: String = row.get("text")?;
    let highlight_int: i32 = row.get("highlight")?;
    let iv: Option<Vec<u8>> = row.get("iv")?;

    let text = if encrypt {
        if let (Some(key), Some(iv_bytes)) = (crypto_key, iv.as_deref()) {
            crypto::decrypt(&stored_text, iv_bytes, key).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    7,
                    rusqlite::types::Type::Text,
                    Box::from(e),
                )
            })?
        } else {
            stored_text
        }
    } else {
        stored_text
    };

    let ref_id: Option<String> = row.get("ref_id")?;
    let tags: Option<String> = row.get("tags")?;
    let event_key: Option<String> = row.get("event_key")?;

    Ok(StoredMessage {
        id,
        msg_id,
        network,
        buffer,
        timestamp,
        msg_type,
        nick,
        text,
        highlight: highlight_int != 0,
        ref_id,
        tags,
        event_key,
    })
}

/// Fetch messages for a buffer with cursor-based pagination.
///
/// Returns messages in chronological (ascending timestamp) order.
/// When `before` is `Some(ts)`, only messages with `timestamp < ts` are returned.
pub fn get_messages(
    db: &Connection,
    network: &str,
    buffer: &str,
    before: Option<i64>,
    limit: usize,
    encrypt: bool,
    crypto_key: Option<&Key<Aes256Gcm>>,
) -> rusqlite::Result<Vec<StoredMessage>> {
    let mut messages = if let Some(before_ts) = before {
        let mut stmt = db.prepare(
            "SELECT * FROM messages
             WHERE network = ?1 AND buffer = ?2 AND timestamp < ?3
             ORDER BY timestamp DESC
             LIMIT ?4",
        )?;
        #[expect(
            clippy::cast_possible_wrap,
            reason = "limit will never exceed i64::MAX in practice"
        )]
        let rows = stmt.query_map(params![network, buffer, before_ts, limit as i64], |row| {
            map_row(row, encrypt, crypto_key)
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let mut stmt = db.prepare(
            "SELECT * FROM messages
             WHERE network = ?1 AND buffer = ?2
             ORDER BY timestamp DESC
             LIMIT ?3",
        )?;
        #[expect(
            clippy::cast_possible_wrap,
            reason = "limit will never exceed i64::MAX in practice"
        )]
        let rows = stmt.query_map(params![network, buffer, limit as i64], |row| {
            map_row(row, encrypt, crypto_key)
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    // Reverse to get chronological order.
    messages.reverse();
    Ok(messages)
}

/// Full-text search across messages (plain mode only, no encryption).
///
/// The query string is wrapped in double quotes for phrase matching.
/// Optional network and buffer filters narrow the results.
pub fn search_messages(
    db: &Connection,
    query: &str,
    network: Option<&str>,
    buffer: Option<&str>,
    limit: usize,
) -> rusqlite::Result<Vec<StoredMessage>> {
    let safe_query = format!("\"{}\"", query.replace('"', "\"\""));
    let mut sql = "SELECT m.* FROM messages m \
                   JOIN messages_fts fts ON m.id = fts.rowid \
                   WHERE messages_fts MATCH ?1"
        .to_string();

    let mut param_idx = 2;
    let mut dyn_params: Vec<Box<dyn ToSql>> = vec![Box::new(safe_query)];

    if let Some(n) = network {
        use std::fmt::Write;
        let _ = write!(sql, " AND m.network = ?{param_idx}");
        dyn_params.push(Box::new(n.to_string()));
        param_idx += 1;
    }
    if let Some(b) = buffer {
        use std::fmt::Write;
        let _ = write!(sql, " AND m.buffer = ?{param_idx}");
        dyn_params.push(Box::new(b.to_string()));
        param_idx += 1;
    }
    {
        use std::fmt::Write;
        let _ = write!(sql, " ORDER BY m.timestamp DESC LIMIT ?{param_idx}");
    }
    #[expect(
        clippy::cast_possible_wrap,
        reason = "limit will never exceed i64::MAX in practice"
    )]
    {
        dyn_params.push(Box::new(limit as i64));
    }

    let param_refs: Vec<&dyn ToSql> = dyn_params.iter().map(Box::as_ref).collect();
    let mut stmt = db.prepare(&sql)?;

    let rows = stmt.query_map(&*param_refs, |row| map_row(row, false, None))?;
    let mut results: Vec<StoredMessage> = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    results.reverse();
    Ok(results)
}

/// List distinct buffer names for a given network.
pub fn get_buffers(db: &Connection, network: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt =
        db.prepare("SELECT DISTINCT buffer FROM messages WHERE network = ?1 ORDER BY buffer")?;
    let rows = stmt.query_map(params![network], |row| row.get(0))?;
    rows.collect()
}

/// Return the total number of messages stored.
pub fn get_message_count(db: &Connection) -> rusqlite::Result<u64> {
    db.query_row("SELECT COUNT(*) FROM messages", [], |row| {
        #[expect(
            clippy::cast_sign_loss,
            reason = "COUNT(*) always returns non-negative"
        )]
        row.get::<_, i64>(0).map(|c| c as u64)
    })
}

/// Insert or update a read marker for the given (network, buffer, client).
pub fn update_read_marker(
    db: &Connection,
    network: &str,
    buffer: &str,
    client: &str,
    timestamp: i64,
) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO read_markers (network, buffer, client, last_read)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (network, buffer, client)
         DO UPDATE SET last_read = excluded.last_read",
        params![network, buffer, client, timestamp],
    )?;
    Ok(())
}

/// Retrieve the last-read timestamp for a specific client.
pub fn get_read_marker(
    db: &Connection,
    network: &str,
    buffer: &str,
    client: &str,
) -> rusqlite::Result<Option<i64>> {
    let mut stmt = db.prepare(
        "SELECT last_read FROM read_markers
         WHERE network = ?1 AND buffer = ?2 AND client = ?3",
    )?;
    let mut rows = stmt.query(params![network, buffer, client])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

/// Retrieve all read markers for a (network, buffer) pair.
pub fn get_read_markers(
    db: &Connection,
    network: &str,
    buffer: &str,
) -> rusqlite::Result<Vec<ReadMarker>> {
    let mut stmt = db.prepare(
        "SELECT network, buffer, client, last_read FROM read_markers
         WHERE network = ?1 AND buffer = ?2",
    )?;
    let rows = stmt.query_map(params![network, buffer], |row| {
        Ok(ReadMarker {
            network: row.get(0)?,
            buffer: row.get(1)?,
            client: row.get(2)?,
            last_read: row.get(3)?,
        })
    })?;
    rows.collect()
}

/// Count unread messages for a client in a buffer.
///
/// If the client has no read marker, all messages in the buffer are unread.
pub fn get_unread_count(
    db: &Connection,
    network: &str,
    buffer: &str,
    client: &str,
) -> rusqlite::Result<u64> {
    let last_read = get_read_marker(db, network, buffer, client)?;
    #[expect(
        clippy::cast_sign_loss,
        reason = "COUNT(*) always returns non-negative"
    )]
    last_read.map_or_else(
        || {
            db.query_row(
                "SELECT COUNT(*) FROM messages
                 WHERE network = ?1 AND buffer = ?2",
                params![network, buffer],
                |row| row.get::<_, i64>(0).map(|c| c as u64),
            )
        },
        |ts| {
            db.query_row(
                "SELECT COUNT(*) FROM messages
                 WHERE network = ?1 AND buffer = ?2 AND timestamp > ?3",
                params![network, buffer, ts],
                |row| row.get::<_, i64>(0).map(|c| c as u64),
            )
        },
    )
}

// === Mentions ===

/// Insert a mention into the mentions table. Returns the row ID.
pub fn insert_mention(
    db: &Connection,
    timestamp: i64,
    network: &str,
    buffer: &str,
    channel: &str,
    nick: &str,
    text: &str,
) -> rusqlite::Result<i64> {
    db.execute(
        "INSERT INTO mentions (timestamp, network, buffer, channel, nick, text)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![timestamp, network, buffer, channel, nick, text],
    )?;
    Ok(db.last_insert_rowid())
}

/// Fetch all unread mentions (where `read_at` is NULL), newest first.
pub fn get_unread_mentions(db: &Connection) -> rusqlite::Result<Vec<super::types::MentionRow>> {
    let mut stmt = db.prepare(
        "SELECT id, timestamp, network, buffer, channel, nick, text
         FROM mentions WHERE read_at IS NULL
         ORDER BY timestamp DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(super::types::MentionRow {
            id: row.get(0)?,
            timestamp: row.get(1)?,
            network: row.get(2)?,
            buffer: row.get(3)?,
            channel: row.get(4)?,
            nick: row.get(5)?,
            text: row.get(6)?,
        })
    })?;
    rows.collect()
}

/// Count unread mentions.
pub fn get_unread_mention_count(db: &Connection) -> rusqlite::Result<u32> {
    db.query_row(
        "SELECT COUNT(*) FROM mentions WHERE read_at IS NULL",
        [],
        |row| {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "COUNT(*) always returns non-negative and will never exceed u32::MAX"
            )]
            row.get::<_, i64>(0).map(|c| c as u32)
        },
    )
}

/// Mark all unread mentions as read. Returns the number of rows updated.
pub fn mark_mentions_read(db: &Connection) -> rusqlite::Result<usize> {
    let now = chrono::Utc::now().timestamp();
    db.execute(
        "UPDATE mentions SET read_at = ?1 WHERE read_at IS NULL",
        params![now],
    )
}

/// Load recent mentions for the mentions buffer.
/// Returns up to `limit` mentions newer than `since_ts` (Unix timestamp), oldest first.
pub fn load_recent_mentions(
    db: &Connection,
    since_ts: i64,
    limit: u32,
) -> rusqlite::Result<Vec<super::types::MentionRow>> {
    let mut stmt = db.prepare(
        "SELECT id, timestamp, network, buffer, channel, nick, text
         FROM mentions
         WHERE timestamp > ?1
         ORDER BY timestamp ASC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![since_ts, limit], |row| {
        Ok(super::types::MentionRow {
            id: row.get(0)?,
            timestamp: row.get(1)?,
            network: row.get(2)?,
            buffer: row.get(3)?,
            channel: row.get(4)?,
            nick: row.get(5)?,
            text: row.get(6)?,
        })
    })?;
    rows.collect()
}

/// Delete mentions older than the given Unix timestamp.
pub fn purge_old_mentions(db: &Connection, before_ts: i64) -> rusqlite::Result<usize> {
    db.execute(
        "DELETE FROM mentions WHERE timestamp < ?1",
        params![before_ts],
    )
}

/// Delete ALL mentions (used by `/clear` on mentions buffer).
pub fn truncate_mentions(db: &Connection) -> rusqlite::Result<usize> {
    db.execute("DELETE FROM mentions", [])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db::open_database;

    fn setup_test_db() -> Connection {
        open_database(false).unwrap()
    }

    /// Insert a test message with the given timestamp and text.
    fn insert_msg(db: &Connection, network: &str, buffer: &str, ts: i64, text: &str) {
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                format!("msg-{ts}"),
                network,
                buffer,
                ts,
                "message",
                "alice",
                text,
                0
            ],
        )
        .unwrap();
    }

    #[test]
    fn get_messages_returns_chronological() {
        let db = open_database(false).unwrap();
        for i in 1..=5 {
            insert_msg(&db, "net", "#chan", i * 100, &format!("msg {i}"));
        }

        let msgs = get_messages(&db, "net", "#chan", None, 10, false, None).unwrap();
        assert_eq!(msgs.len(), 5);

        // Verify ascending timestamps (chronological order).
        for i in 1..msgs.len() {
            assert!(
                msgs[i].timestamp >= msgs[i - 1].timestamp,
                "messages should be in ascending timestamp order"
            );
        }
        assert_eq!(msgs[0].text, "msg 1");
        assert_eq!(msgs[4].text, "msg 5");
    }

    #[test]
    fn get_messages_cursor_pagination() {
        let db = open_database(false).unwrap();
        for i in 1..=10 {
            insert_msg(&db, "net", "#chan", i * 100, &format!("msg {i}"));
        }

        // Page 1: last 5 messages (no cursor).
        let page1 = get_messages(&db, "net", "#chan", None, 5, false, None).unwrap();
        assert_eq!(page1.len(), 5);
        // Should be messages 6-10 in chronological order.
        assert_eq!(page1[0].text, "msg 6");
        assert_eq!(page1[4].text, "msg 10");

        // Page 2: 5 messages before the oldest in page1.
        let cursor = page1[0].timestamp;
        let page2 = get_messages(&db, "net", "#chan", Some(cursor), 5, false, None).unwrap();
        assert_eq!(page2.len(), 5);
        // Should be messages 1-5 in chronological order.
        assert_eq!(page2[0].text, "msg 1");
        assert_eq!(page2[4].text, "msg 5");
    }

    #[test]
    fn search_messages_fts() {
        let db = open_database(false).unwrap();
        insert_msg(&db, "net", "#chan", 100, "hello world");
        insert_msg(&db, "net", "#chan", 200, "goodbye world");
        insert_msg(&db, "net", "#chan", 300, "xyzzy unique needle");

        let results = search_messages(&db, "xyzzy", None, None, 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].text, "xyzzy unique needle");
    }

    #[test]
    fn get_buffers_lists_distinct() {
        let db = open_database(false).unwrap();
        insert_msg(&db, "net", "#alpha", 100, "a");
        insert_msg(&db, "net", "#beta", 200, "b");
        insert_msg(&db, "net", "#alpha", 300, "c"); // duplicate buffer

        let buffers = get_buffers(&db, "net").unwrap();
        assert_eq!(buffers, vec!["#alpha", "#beta"]);
    }

    #[test]
    fn read_marker_crud() {
        let db = open_database(false).unwrap();

        // Initially no marker.
        let marker = get_read_marker(&db, "net", "#chan", "client1").unwrap();
        assert!(marker.is_none());

        // Insert marker.
        update_read_marker(&db, "net", "#chan", "client1", 500).unwrap();
        let marker = get_read_marker(&db, "net", "#chan", "client1").unwrap();
        assert_eq!(marker, Some(500));

        // Update marker.
        update_read_marker(&db, "net", "#chan", "client1", 900).unwrap();
        let marker = get_read_marker(&db, "net", "#chan", "client1").unwrap();
        assert_eq!(marker, Some(900));

        // Different client returns None.
        let marker = get_read_marker(&db, "net", "#chan", "client2").unwrap();
        assert!(marker.is_none());

        // get_read_markers returns all markers for the buffer.
        update_read_marker(&db, "net", "#chan", "client2", 700).unwrap();
        let markers = get_read_markers(&db, "net", "#chan").unwrap();
        assert_eq!(markers.len(), 2);
    }

    #[test]
    fn unread_count() {
        let db = open_database(false).unwrap();
        for i in 1..=10 {
            insert_msg(&db, "net", "#chan", i * 100, &format!("msg {i}"));
        }

        // No read marker: all 10 are unread.
        let count = get_unread_count(&db, "net", "#chan", "client1").unwrap();
        assert_eq!(count, 10);

        // Mark read at message 5 (timestamp 500) — messages 6-10 should be unread but
        // message 5 itself (timestamp == 500) is NOT counted since we use `> last_read`.
        // That means timestamps 600, 700, 800, 900, 1000 are unread = 5.
        update_read_marker(&db, "net", "#chan", "client1", 600).unwrap();
        let count = get_unread_count(&db, "net", "#chan", "client1").unwrap();
        assert_eq!(count, 4);
    }

    #[test]
    fn get_stats_works() {
        let db = open_database(false).unwrap();
        assert_eq!(get_message_count(&db).unwrap(), 0);

        insert_msg(&db, "net", "#a", 100, "one");
        insert_msg(&db, "net", "#b", 200, "two");
        insert_msg(&db, "net", "#a", 300, "three");

        assert_eq!(get_message_count(&db).unwrap(), 3);
    }

    // === Mention tests ===

    #[test]
    fn insert_and_query_mentions() {
        let db = open_database(false).unwrap();
        insert_mention(&db, 1000, "libera", "#rust", "#rust", "bob", "hey kofany!").unwrap();
        insert_mention(
            &db,
            2000,
            "libera",
            "#tokio",
            "#tokio",
            "alice",
            "kofany: look",
        )
        .unwrap();

        let mentions = get_unread_mentions(&db).unwrap();
        assert_eq!(mentions.len(), 2);
        // Newest first.
        assert_eq!(mentions[0].timestamp, 2000);
        assert_eq!(mentions[0].nick, "alice");
        assert_eq!(mentions[1].timestamp, 1000);
        assert_eq!(mentions[1].nick, "bob");
    }

    #[test]
    fn unread_mention_count() {
        let db = open_database(false).unwrap();
        assert_eq!(get_unread_mention_count(&db).unwrap(), 0);

        insert_mention(&db, 1000, "net", "#a", "#a", "x", "hi").unwrap();
        insert_mention(&db, 2000, "net", "#b", "#b", "y", "hey").unwrap();
        assert_eq!(get_unread_mention_count(&db).unwrap(), 2);
    }

    #[test]
    fn mark_mentions_read_clears_unread() {
        let db = open_database(false).unwrap();
        insert_mention(&db, 1000, "net", "#a", "#a", "x", "hi").unwrap();
        insert_mention(&db, 2000, "net", "#b", "#b", "y", "hey").unwrap();

        let updated = mark_mentions_read(&db).unwrap();
        assert_eq!(updated, 2);
        assert_eq!(get_unread_mention_count(&db).unwrap(), 0);
        assert!(get_unread_mentions(&db).unwrap().is_empty());

        // New mention after marking still shows as unread.
        insert_mention(&db, 3000, "net", "#c", "#c", "z", "yo").unwrap();
        assert_eq!(get_unread_mention_count(&db).unwrap(), 1);
    }

    #[test]
    fn load_recent_mentions_returns_within_window_oldest_first() {
        let db = setup_test_db();
        let now = chrono::Utc::now().timestamp();
        insert_mention(&db, now - 100, "net", "buf", "#ch", "nick", "old").unwrap();
        insert_mention(&db, now - 50, "net", "buf", "#ch", "nick", "mid").unwrap();
        insert_mention(&db, now, "net", "buf", "#ch", "nick", "new").unwrap();

        let rows = load_recent_mentions(&db, now - 200, 1000).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].text, "old");
        assert_eq!(rows[2].text, "new");
    }

    #[test]
    fn load_recent_mentions_respects_limit() {
        let db = setup_test_db();
        let now = chrono::Utc::now().timestamp();
        for i in 0..10 {
            insert_mention(
                &db,
                now + i,
                "net",
                "buf",
                "#ch",
                "nick",
                &format!("msg{i}"),
            )
            .unwrap();
        }
        let rows = load_recent_mentions(&db, now - 1, 5).unwrap();
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn purge_old_mentions_deletes_expired() {
        let db = setup_test_db();
        let now = chrono::Utc::now().timestamp();
        insert_mention(&db, now - 1000, "net", "buf", "#ch", "nick", "old").unwrap();
        insert_mention(&db, now, "net", "buf", "#ch", "nick", "new").unwrap();
        let deleted = purge_old_mentions(&db, now - 500).unwrap();
        assert_eq!(deleted, 1);
        let remaining = load_recent_mentions(&db, 0, 1000).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].text, "new");
    }

    #[test]
    fn truncate_mentions_deletes_all() {
        let db = setup_test_db();
        let now = chrono::Utc::now().timestamp();
        insert_mention(&db, now, "net", "buf", "#ch", "nick", "msg").unwrap();
        truncate_mentions(&db).unwrap();
        let remaining = load_recent_mentions(&db, 0, 1000).unwrap();
        assert!(remaining.is_empty());
    }
}

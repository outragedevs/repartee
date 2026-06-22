use aes_gcm::{Aes256Gcm, Key};
use rusqlite::{Connection, params, types::ToSql};

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
    // Read paths usually alias `COALESCE(ts_ms, timestamp*1000) AS ts_ms` (always
    // non-null), but `search_messages` selects raw `m.*` where `ts_ms` may be
    // NULL for rows predating the column — fall back to whole-seconds either way.
    let ts_ms: i64 = row
        .get::<_, Option<i64>>("ts_ms")?
        .unwrap_or(timestamp * 1000);
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
        ts_ms,
        msg_type,
        nick,
        text,
        highlight: highlight_int != 0,
        ref_id,
        tags,
        event_key,
    })
}

/// Columns selected by every chat/log read path.
///
/// Fan-out reference rows (e.g. a single QUIT broadcast across N channels)
/// are stored with `text = ''` and `ref_id = <primary msg_id>` to save
/// space — only the primary row carries the actual message text and IV.
/// Without a JOIN to that primary row, every reference row would render
/// as a blank event line in backlog or in the log browser. The aliases
/// below transparently substitute the primary's `text` + `iv` whenever
/// a reference exists; `map_row` is unchanged.
const SELECT_MESSAGE_COLUMNS: &str = "
    m.id, m.msg_id, m.network, m.buffer, m.timestamp,
    COALESCE(m.ts_ms, m.timestamp * 1000) AS ts_ms,
    m.type, m.nick,
    COALESCE(p.text, m.text) AS text,
    m.highlight,
    COALESCE(p.iv,   m.iv)   AS iv,
    m.ref_id, m.tags, m.event_key
";

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
        let sql = format!(
            "SELECT {SELECT_MESSAGE_COLUMNS}
             FROM messages m
             LEFT JOIN messages p ON p.msg_id = m.ref_id AND p.network = m.network
             WHERE m.network = ?1 AND m.buffer = ?2 AND m.timestamp < ?3
             ORDER BY m.timestamp DESC
             LIMIT ?4"
        );
        let mut stmt = db.prepare(&sql)?;
        #[expect(
            clippy::cast_possible_wrap,
            reason = "limit will never exceed i64::MAX in practice"
        )]
        let rows = stmt.query_map(params![network, buffer, before_ts, limit as i64], |row| {
            map_row(row, encrypt, crypto_key)
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let sql = format!(
            "SELECT {SELECT_MESSAGE_COLUMNS}
             FROM messages m
             LEFT JOIN messages p ON p.msg_id = m.ref_id AND p.network = m.network
             WHERE m.network = ?1 AND m.buffer = ?2
             ORDER BY m.timestamp DESC
             LIMIT ?3"
        );
        let mut stmt = db.prepare(&sql)?;
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

/// Second-resolution cursor-paginated fetch using a `(timestamp, id)` tuple.
///
/// `get_messages` alone uses `WHERE timestamp < ?` which silently drops
/// rows that share a timestamp with the cursor — a real problem when
/// many messages land in the same second on a busy channel. This
/// variant uses the strict ordering `(timestamp DESC, id DESC)` and
/// `WHERE timestamp < ?ts OR (timestamp = ?ts AND id < ?id)`, so paging
/// is lossless even at second-precision timestamps.
///
/// Pass `before = None` for the initial page (latest messages).
///
/// This is the cursor the **web** client uses: its wire timestamps are whole
/// seconds (`message_to_wire`) and it re-sorts by `(seconds, id)`, so the
/// second-resolution keyset matches its world exactly. Native TUI scroll-back
/// instead uses [`get_messages_paginated_subsecond`], which orders by the
/// full-millisecond `ts_ms` so a backfilled older same-second row (which gets a
/// larger autoincrement id) is not skipped.
pub fn get_messages_paginated(
    db: &Connection,
    network: &str,
    buffer: &str,
    before: Option<(i64, i64)>,
    limit: usize,
    encrypt: bool,
    crypto_key: Option<&Key<Aes256Gcm>>,
) -> rusqlite::Result<Vec<StoredMessage>> {
    let mut messages = if let Some((ts, id)) = before {
        let sql = format!(
            "SELECT {SELECT_MESSAGE_COLUMNS}
             FROM messages m
             LEFT JOIN messages p ON p.msg_id = m.ref_id AND p.network = m.network
             WHERE m.network = ?1 AND m.buffer = ?2
               AND (m.timestamp < ?3 OR (m.timestamp = ?3 AND m.id < ?4))
             ORDER BY m.timestamp DESC, m.id DESC
             LIMIT ?5"
        );
        let mut stmt = db.prepare(&sql)?;
        #[expect(
            clippy::cast_possible_wrap,
            reason = "limit will never exceed i64::MAX in practice"
        )]
        let rows = stmt.query_map(params![network, buffer, ts, id, limit as i64], |row| {
            map_row(row, encrypt, crypto_key)
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let sql = format!(
            "SELECT {SELECT_MESSAGE_COLUMNS}
             FROM messages m
             LEFT JOIN messages p ON p.msg_id = m.ref_id AND p.network = m.network
             WHERE m.network = ?1 AND m.buffer = ?2
             ORDER BY m.timestamp DESC, m.id DESC
             LIMIT ?3"
        );
        let mut stmt = db.prepare(&sql)?;
        #[expect(
            clippy::cast_possible_wrap,
            reason = "limit will never exceed i64::MAX in practice"
        )]
        let rows = stmt.query_map(params![network, buffer, limit as i64], |row| {
            map_row(row, encrypt, crypto_key)
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    messages.reverse();
    Ok(messages)
}

/// Subsecond-resolution cursor-paginated fetch using a `(unix_millis, id)` tuple.
///
/// Native scroll-back (live-chat backlog and the log browser) reconstructs each
/// in-memory row's timestamp at full millisecond precision (see
/// `stored_to_message`), so its cursor carries true `@time` millis. Rows are
/// ordered by `COALESCE(ts_ms, timestamp * 1000)` — the persisted millisecond
/// time, falling back to `timestamp * 1000` for rows written before the `ts_ms`
/// column existed. This is what fixes the same-second backfill bug: a
/// `CHATHISTORY BEFORE` row that is older in `@time` than an existing same-second
/// row, yet inserted later (so it has a larger autoincrement id), still sorts —
/// and paginates — as older, instead of being hidden by an `id`-based keyset.
///
/// The cursor predicate keeps the indexed `m.timestamp <= ?secs` bound (so the
/// `(network, buffer, timestamp)` index still prunes the scan) alongside the
/// precise millisecond keyset; `?secs` is the floored-second of the cursor and
/// never excludes a wanted row (any row with `COALESCE(...) <= ?ms` has
/// `timestamp <= ?ms / 1000`).
///
/// Pass `before = None` for the initial page (latest messages).
pub fn get_messages_paginated_subsecond(
    db: &Connection,
    network: &str,
    buffer: &str,
    before: Option<(i64, i64)>,
    limit: usize,
    encrypt: bool,
    crypto_key: Option<&Key<Aes256Gcm>>,
) -> rusqlite::Result<Vec<StoredMessage>> {
    let mut messages = if let Some((ms, id)) = before {
        let secs = ms.div_euclid(1000);
        let sql = format!(
            "SELECT {SELECT_MESSAGE_COLUMNS}
             FROM messages m
             LEFT JOIN messages p ON p.msg_id = m.ref_id AND p.network = m.network
             WHERE m.network = ?1 AND m.buffer = ?2
               AND m.timestamp <= ?3
               AND (COALESCE(m.ts_ms, m.timestamp * 1000) < ?4
                    OR (COALESCE(m.ts_ms, m.timestamp * 1000) = ?4 AND m.id < ?5))
             ORDER BY COALESCE(m.ts_ms, m.timestamp * 1000) DESC, m.id DESC
             LIMIT ?6"
        );
        let mut stmt = db.prepare(&sql)?;
        #[expect(
            clippy::cast_possible_wrap,
            reason = "limit will never exceed i64::MAX in practice"
        )]
        let rows = stmt.query_map(params![network, buffer, secs, ms, id, limit as i64], |row| {
            map_row(row, encrypt, crypto_key)
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let sql = format!(
            "SELECT {SELECT_MESSAGE_COLUMNS}
             FROM messages m
             LEFT JOIN messages p ON p.msg_id = m.ref_id AND p.network = m.network
             WHERE m.network = ?1 AND m.buffer = ?2
             ORDER BY COALESCE(m.ts_ms, m.timestamp * 1000) DESC, m.id DESC
             LIMIT ?3"
        );
        let mut stmt = db.prepare(&sql)?;
        #[expect(
            clippy::cast_possible_wrap,
            reason = "limit will never exceed i64::MAX in practice"
        )]
        let rows = stmt.query_map(params![network, buffer, limit as i64], |row| {
            map_row(row, encrypt, crypto_key)
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    messages.reverse();
    Ok(messages)
}

/// Newest stored row for a buffer, used as the anchor for a
/// `CHATHISTORY AFTER` gap-fill request after (re)connecting.
///
/// Returns `(timestamp, id)` for the row with the greatest `(timestamp, id)`,
/// or `None` if the buffer has no stored messages.
///
/// The stored `msg_id` is intentionally NOT returned: live PRIVMSG/NOTICE rows
/// are logged with a locally-minted UUID (see `state/events::maybe_log`), not
/// the server's IRC `@msgid`, and the column can't distinguish the two. Feeding
/// such a UUID to `CHATHISTORY ... msgid=<uuid>` fails on a `MSGREFTYPES=msgid`
/// server, so the anchor is always a full-precision timestamp reference. Only a
/// `@msgid` verified via a chathistory batch (the per-target watermark) is ever
/// used as a msgid reference.
pub fn newest_anchor(
    db: &Connection,
    network: &str,
    buffer: &str,
) -> rusqlite::Result<Option<(i64, i64)>> {
    let mut stmt = db.prepare(
        "SELECT timestamp, id FROM messages
         WHERE network = ?1 AND buffer = ?2
         ORDER BY timestamp DESC, id DESC
         LIMIT 1",
    )?;
    // Log rows are stored under the lowercased buffer key (make_buffer_id);
    // callers pass the display-case channel/nick, so normalize for the lookup.
    let mut rows = stmt.query(params![network, buffer.to_lowercase()])?;
    match rows.next()? {
        Some(row) => {
            let timestamp: i64 = row.get(0)?;
            let id: i64 = row.get(1)?;
            Ok(Some((timestamp, id)))
        }
        None => Ok(None),
    }
}

/// Oldest stored row for a buffer, used as the anchor for the **first**
/// `CHATHISTORY BEFORE` scroll-back request (before any per-target watermark
/// has been recorded).
///
/// Returns `(unix_millis, msgid?)` for the row with the least `(timestamp, id)`,
/// or `None` if the buffer has no stored messages.
///
/// The anchor is taken from the row's `IRCv3` `tags` (the `@time`/`@msgid` the
/// server sent), NOT from the second-resolution `messages.timestamp` column or
/// the ambiguous `msg_id` column:
/// - `@time` gives the full-millisecond boundary, so `BEFORE` does not floor to
///   `.000Z` and skip older messages from the same second forever.
/// - `@msgid` is a *verified* server reference (it came from the server tag),
///   safe to use as a `msgid=` anchor; the `msg_id` column may be a locally
///   minted UUID and is never used here.
///
/// Falls back to `timestamp × 1000` (and no msgid) for rows stored without tags.
pub fn oldest_anchor(
    db: &Connection,
    network: &str,
    buffer: &str,
) -> rusqlite::Result<Option<(i64, Option<String>)>> {
    let mut stmt = db.prepare(
        "SELECT timestamp, tags FROM messages
         WHERE network = ?1 AND buffer = ?2
         ORDER BY timestamp ASC, id ASC
         LIMIT 1",
    )?;
    // Normalize the buffer key to match how rows are stored (see `newest_anchor`).
    let mut rows = stmt.query(params![network, buffer.to_lowercase()])?;
    match rows.next()? {
        Some(row) => {
            let timestamp: i64 = row.get(0)?;
            let tags: Option<String> = row.get(1)?;
            Ok(Some(anchor_from_tags(tags.as_deref(), timestamp)))
        }
        None => Ok(None),
    }
}

/// Build a full-precision `(unix_millis, msgid?)` CHATHISTORY anchor from a
/// stored row's `IRCv3` `tags` JSON, falling back to `timestamp_secs × 1000` (and
/// no msgid) when `@time`/`@msgid` are absent or unparseable.
fn anchor_from_tags(tags_json: Option<&str>, timestamp_secs: i64) -> (i64, Option<String>) {
    let tags: Option<std::collections::HashMap<String, String>> =
        tags_json.and_then(|j| serde_json::from_str(j).ok());
    let millis = tags
        .as_ref()
        .and_then(|t| t.get("time"))
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map_or(timestamp_secs * 1000, |dt| dt.timestamp_millis());
    let msgid = tags
        .as_ref()
        .and_then(|t| t.get("msgid"))
        .filter(|m| !m.is_empty())
        .cloned();
    (millis, msgid)
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

// === Log-browser catalog queries ===

/// Distinct networks present in the message log, sorted ascending.
/// Used by the log browser to populate sidebar headers.
pub fn list_networks(db: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = db.prepare("SELECT DISTINCT network FROM messages ORDER BY network")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// Distinct buffers logged for a given network, sorted ascending.
pub fn list_buffers_for_network(db: &Connection, network: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt =
        db.prepare("SELECT DISTINCT buffer FROM messages WHERE network = ?1 ORDER BY buffer")?;
    let rows = stmt.query_map(params![network], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// `(line_count, oldest_ts, newest_ts)` for a given network/buffer pair.
/// Returns `None` if no messages exist there. Cached on the `Buffer` at
/// activation so the topic-bar render doesn't requery on every frame.
pub fn buffer_stats(
    db: &Connection,
    network: &str,
    buffer: &str,
) -> rusqlite::Result<Option<(u64, i64, i64)>> {
    let row = db.query_row(
        "SELECT COUNT(*), MIN(timestamp), MAX(timestamp) \
         FROM messages WHERE network = ?1 AND buffer = ?2",
        params![network, buffer],
        |r| {
            let count: i64 = r.get(0)?;
            // MIN/MAX are NULL when the count is 0.
            let oldest: Option<i64> = r.get(1)?;
            let newest: Option<i64> = r.get(2)?;
            #[expect(
                clippy::cast_sign_loss,
                reason = "COUNT(*) is non-negative by SQL semantics"
            )]
            Ok((count as u64, oldest, newest))
        },
    )?;
    Ok(match row {
        (0, _, _) => None,
        (n, Some(o), Some(x)) => Some((n, o, x)),
        _ => None,
    })
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

    // === Log-browser catalog queries ===

    #[test]
    fn list_networks_returns_distinct_sorted() {
        let db = setup_test_db();
        insert_msg(&db, "libera", "#rust", 1, "a");
        insert_msg(&db, "libera", "#polska", 2, "b");
        insert_msg(&db, "oftc", "#debian", 3, "c");
        insert_msg(&db, "libera", "#rust", 4, "d");
        insert_msg(&db, "ircnet", "#pl", 5, "e");

        assert_eq!(
            list_networks(&db).unwrap(),
            vec!["ircnet", "libera", "oftc"]
        );
    }

    #[test]
    fn list_buffers_for_network_filters_correctly() {
        let db = setup_test_db();
        insert_msg(&db, "libera", "#rust", 1, "x");
        insert_msg(&db, "libera", "#polska", 2, "x");
        insert_msg(&db, "oftc", "#debian", 3, "x");
        insert_msg(&db, "libera", "#rust", 4, "y");

        assert_eq!(
            list_buffers_for_network(&db, "libera").unwrap(),
            vec!["#polska", "#rust"]
        );
        assert_eq!(
            list_buffers_for_network(&db, "oftc").unwrap(),
            vec!["#debian"]
        );
        assert!(list_buffers_for_network(&db, "missing").unwrap().is_empty());
    }

    #[test]
    fn get_messages_paginated_does_not_lose_same_timestamp_rows() {
        // Regression: timestamp-only pagination drops rows when many
        // messages share the same second. Composite (timestamp, id)
        // cursor pages through them losslessly.
        let db = setup_test_db();
        // Insert 5 rows with the same timestamp but distinct msg_ids
        // (UNIQUE constraint), distinct text so we can verify identity.
        for i in 0..5 {
            db.execute(
                "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight) \
                 VALUES (?1, 'libera', '#rust', 100, 'message', 'ada', ?2, 0)",
                params![format!("dup-{i}"), format!("text-{i}")],
            )
            .unwrap();
        }
        // Sanity: 5 rows at ts=100, distinct ids assigned by SQLite.
        let all = get_messages_paginated(&db, "libera", "#rust", None, 1000, false, None).unwrap();
        assert_eq!(all.len(), 5);

        // Page 1: limit 2 → 2 newest at ts=100.
        let page1 = get_messages_paginated(&db, "libera", "#rust", None, 2, false, None).unwrap();
        assert_eq!(page1.len(), 2);
        let oldest = page1.first().unwrap();
        // Page 2: cursor on (ts=100, id=oldest.id) → must yield the
        // remaining 3 rows that share timestamp 100, *not* return empty.
        let page2 = get_messages_paginated(
            &db,
            "libera",
            "#rust",
            Some((oldest.timestamp, oldest.id)),
            10,
            false,
            None,
        )
        .unwrap();
        assert_eq!(page2.len(), 3, "all same-timestamp rows must paginate");
    }

    #[test]
    fn paginated_subsecond_orders_same_second_backfill_by_time() {
        // P1 regression: three messages share whole-second 100 but were stored
        // in REVERSED time order (as a live row, then an older CHATHISTORY BEFORE
        // backfill), so insertion id contradicts @time:
        //   id=1  ts_ms=100_900  "a900"  (newest in the second, stored first)
        //   id=2  ts_ms=100_700  "b700"  (oldest, backfilled later -> larger id)
        //   id=3  ts_ms=100_800  "c800"  (middle, backfilled later)
        let db = setup_test_db();
        for (ms, text) in [(100_900_i64, "a900"), (100_700, "b700"), (100_800, "c800")] {
            db.execute(
                "INSERT INTO messages (msg_id, network, buffer, timestamp, ts_ms, type, nick, text, highlight) \
                 VALUES (?1, 'libera', '#rust', 100, ?2, 'message', 'ada', ?3, 0)",
                params![text, ms, text],
            )
            .unwrap();
        }

        // The second-resolution keyset (web path) HIDES the older same-second
        // rows: anchored at the newest row (timestamp=100, id=1), `id < 1` matches
        // nothing even though b700/c800 are older in @time. This is the bug.
        let hidden = get_messages_paginated(&db, "libera", "#rust", Some((100, 1)), 10, false, None)
            .unwrap();
        assert!(
            hidden.is_empty(),
            "seconds keyset skips same-second rows with larger ids (the P1 bug)"
        );

        // The subsecond keyset orders by @time, so a full page is chronological…
        let all =
            get_messages_paginated_subsecond(&db, "libera", "#rust", None, 10, false, None).unwrap();
        let texts: Vec<&str> = all.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, vec!["b700", "c800", "a900"], "ordered by @time, not id");

        // …and anchoring at the newest row (its true millis + id) still yields the
        // two older same-second rows instead of dropping them.
        let newest = all.last().expect("newest row");
        assert_eq!(newest.text, "a900");
        let page = get_messages_paginated_subsecond(
            &db,
            "libera",
            "#rust",
            Some((newest.ts_ms, newest.id)),
            10,
            false,
            None,
        )
        .unwrap();
        let page_texts: Vec<&str> = page.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(page_texts, vec!["b700", "c800"], "older same-second rows paginate");
    }

    #[test]
    fn paginated_subsecond_falls_back_to_seconds_for_null_ts_ms() {
        // Backwards compat: rows written before the ts_ms column exists store
        // NULL. COALESCE(ts_ms, timestamp*1000) keeps them ordering exactly as
        // before — by whole second, tie-broken by id.
        let db = setup_test_db();
        for (ts, text) in [(100_i64, "older"), (100, "same-sec"), (200, "newer")] {
            db.execute(
                "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight) \
                 VALUES (?1, 'libera', '#rust', ?2, 'message', 'ada', ?3, 0)",
                params![text, ts, text],
            )
            .unwrap();
        }
        let all =
            get_messages_paginated_subsecond(&db, "libera", "#rust", None, 10, false, None).unwrap();
        let texts: Vec<&str> = all.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, vec!["older", "same-sec", "newer"]);
        // ts_ms surfaces as timestamp*1000 for these NULL rows.
        assert_eq!(all[0].ts_ms, 100_000);
    }

    #[test]
    fn newest_anchor_empty_is_none() {
        let db = setup_test_db();
        assert_eq!(newest_anchor(&db, "net", "#chan").unwrap(), None);
    }

    #[test]
    fn newest_anchor_returns_latest_row() {
        let db = setup_test_db();
        insert_msg(&db, "net", "#chan", 100, "a");
        insert_msg(&db, "net", "#chan", 300, "c");
        insert_msg(&db, "net", "#chan", 200, "b");

        // Anchor is (timestamp, id) — the stored `msg_id` is deliberately NOT
        // returned (it may be a locally-minted UUID, not a server @msgid).
        let anchor = newest_anchor(&db, "net", "#chan").unwrap().expect("anchor");
        assert_eq!(anchor.0, 300);
    }

    #[test]
    fn newest_anchor_breaks_ties_by_id() {
        let db = setup_test_db();
        for i in 0..3 {
            db.execute(
                "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight) \
                 VALUES (?1, 'net', '#chan', 500, 'message', 'ada', ?2, 0)",
                params![format!("dup-{i}"), format!("t{i}")],
            )
            .unwrap();
        }
        let anchor = newest_anchor(&db, "net", "#chan").unwrap().expect("anchor");
        // Last inserted shares timestamp 500 but has the greatest id.
        assert_eq!(anchor, (500, db.last_insert_rowid()));
    }

    #[test]
    fn newest_anchor_scoped_to_buffer() {
        let db = setup_test_db();
        insert_msg(&db, "net", "#chan", 100, "a");
        insert_msg(&db, "net", "#other", 999, "b");

        let anchor = newest_anchor(&db, "net", "#chan").unwrap().expect("anchor");
        assert_eq!(anchor.0, 100);
    }

    #[test]
    fn newest_anchor_does_not_expose_stored_msgid_column() {
        // Regression: live rows are logged with a generated UUID in
        // `messages.msg_id` (state/events::maybe_log), NOT the server @msgid.
        // newest_anchor surfaces only (timestamp, id) so a caller can never feed
        // that UUID to `CHATHISTORY ... msgid=<uuid>`.
        let db = setup_test_db();
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight) \
             VALUES ('550e8400-e29b-41d4-a716-446655440000', 'net', '#chan', 100, 'message', 'ada', 'hi', 0)",
            [],
        )
        .unwrap();
        let newest = newest_anchor(&db, "net", "#chan").unwrap().expect("anchor");
        assert_eq!(newest.0, 100);
    }

    #[test]
    fn anchors_match_buffer_case_insensitively() {
        // Log rows are stored under the lowercased buffer key (make_buffer_id),
        // but callers pass the display-case channel/nick. Both anchor lookups
        // must normalize, or a mixed-case target misses its stored history and
        // gap-fill falls back to LATEST (replaying) instead of AFTER.
        let db = setup_test_db();
        insert_msg(&db, "net", "#rust", 100, "a");
        insert_msg(&db, "net", "#rust", 200, "b");

        let newest = newest_anchor(&db, "net", "#Rust").unwrap().expect("anchor");
        assert_eq!(newest.0, 200);
        let oldest = oldest_anchor(&db, "net", "#Rust").unwrap().expect("anchor");
        assert_eq!(oldest.0, 100_000);
    }

    #[test]
    fn oldest_anchor_empty_is_none() {
        let db = setup_test_db();
        assert_eq!(oldest_anchor(&db, "net", "#chan").unwrap(), None);
    }

    #[test]
    fn oldest_anchor_uses_full_precision_time_and_msgid_from_tags() {
        // The first BEFORE anchor must not floor to the whole second: the
        // boundary row's @time/@msgid (preserved in the `tags` column) give a
        // full-precision anchor, so older messages within the same second are
        // not skipped forever. msg-100 (whole-second 100) carries @time .800.
        let db = setup_test_db();
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight, tags) \
             VALUES ('msg-100', 'net', '#chan', 100, 'message', 'ada', 'hi', 0, \
             '{\"time\":\"1970-01-01T00:01:40.800Z\",\"msgid\":\"server-M\"}')",
            [],
        )
        .unwrap();

        let anchor = oldest_anchor(&db, "net", "#chan").unwrap().expect("anchor");
        assert_eq!(anchor, (100_800, Some("server-M".to_string())));
    }

    #[test]
    fn oldest_anchor_floors_to_millis_and_no_msgid_without_tags() {
        // Rows stored without server tags fall back to the whole-second
        // timestamp (×1000) and carry no msgid — and never leak the stored
        // `msg_id` column (a UUID for live rows) as a server reference.
        let db = setup_test_db();
        insert_msg(&db, "net", "#chan", 100, "a");
        insert_msg(&db, "net", "#chan", 300, "c");

        let anchor = oldest_anchor(&db, "net", "#chan").unwrap().expect("anchor");
        assert_eq!(anchor, (100_000, None));
    }

    #[test]
    fn buffer_stats_returns_count_and_range() {
        let db = setup_test_db();
        insert_msg(&db, "libera", "#rust", 100, "x");
        insert_msg(&db, "libera", "#rust", 200, "y");
        insert_msg(&db, "libera", "#rust", 50, "z");
        insert_msg(&db, "libera", "#other", 9999, "q");

        let stats = buffer_stats(&db, "libera", "#rust").unwrap();
        assert_eq!(stats, Some((3, 50, 200)));
        assert_eq!(buffer_stats(&db, "libera", "#unknown").unwrap(), None);
    }

    #[test]
    fn fanout_reference_rows_resolve_text_from_primary() {
        // Regression: fan-out QUIT/NICK rows are written with `text=''`
        // and `ref_id=<primary msg_id>` (state/events.rs:308). Both
        // `get_messages` and `get_messages_paginated` must JOIN to
        // the primary so the reference row renders with the actual
        // text instead of producing a blank event line.
        let db = setup_test_db();
        // Primary: full text on #rust.
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight) \
             VALUES ('p1', 'libera', '#rust', 100, 'event', 'alice', 'alice has quit (Bye)', 0)",
            [],
        )
        .unwrap();
        // Reference on #polska: empty text, ref_id pointing at primary.
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight, ref_id) \
             VALUES ('r1', 'libera', '#polska', 100, 'event', 'alice', '', 0, 'p1')",
            [],
        )
        .unwrap();

        let on_polska = get_messages(&db, "libera", "#polska", None, 10, false, None).unwrap();
        assert_eq!(on_polska.len(), 1);
        assert_eq!(on_polska[0].text, "alice has quit (Bye)");
        assert_eq!(on_polska[0].ref_id.as_deref(), Some("p1"));

        let on_polska_paged =
            get_messages_paginated(&db, "libera", "#polska", None, 10, false, None).unwrap();
        assert_eq!(on_polska_paged.len(), 1);
        assert_eq!(on_polska_paged[0].text, "alice has quit (Bye)");
    }

    #[test]
    fn ref_rows_resolve_within_their_own_network() {
        // The composite (network, msg_id) index permits the same primary msg_id
        // on two networks. A reference row must hydrate from ITS OWN network's
        // primary; an unscoped join would match the other network's primary
        // (wrong text) or duplicate the row.
        let db = setup_test_db();
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight) \
             VALUES ('P', 'neta', '#chan', 100, 'event', 'alice', 'quit on A', 0)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight) \
             VALUES ('P', 'netb', '#chan', 100, 'event', 'bob', 'quit on B', 0)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight, ref_id) \
             VALUES ('R', 'netb', '#other', 100, 'event', 'bob', '', 0, 'P')",
            [],
        )
        .unwrap();

        let rows = get_messages_paginated(&db, "netb", "#other", None, 10, false, None).unwrap();
        assert_eq!(rows.len(), 1, "no cross-network duplicate join");
        assert_eq!(rows[0].text, "quit on B", "hydrated from netb's primary");
    }

    #[test]
    fn orphan_reference_row_keeps_empty_text() {
        // Defensive: if the primary row is missing (purged / never
        // written), the reference row falls back to its own (empty)
        // text rather than crashing. Bug surfacing as an empty event
        // line is acceptable in this edge case.
        let db = setup_test_db();
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight, ref_id) \
             VALUES ('orphan', 'libera', '#polska', 100, 'event', 'alice', '', 0, 'gone')",
            [],
        )
        .unwrap();
        let rows = get_messages(&db, "libera", "#polska", None, 10, false, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "");
    }
}

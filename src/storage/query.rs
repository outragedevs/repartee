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
    // Read paths usually alias `<expr> AS ts_ms` (always non-null), but
    // `search_messages` selects raw `m.*`, where `ts_ms` is NULL for rows
    // predating the column — or absent entirely on an unmigrated read-only DB.
    // `get_ref` tolerates a missing column (Err) as well as NULL, both falling
    // back to whole-seconds.
    let ts_ms: i64 = row
        .get_ref("ts_ms")
        .ok()
        .and_then(|v| v.as_i64().ok())
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

/// Whether the `messages` table has the `ts_ms` column.
///
/// A read-only log DB (`repartee l` via `open_readonly_at`) deliberately skips
/// migration, so a database created before `ts_ms` existed will not have the
/// column. Read paths must not reference `m.ts_ms` unconditionally — that would
/// fail at prepare time with `no such column` — so they consult this first and
/// fall back to `timestamp * 1000` when the column is absent.
fn has_ts_ms_column(db: &Connection) -> bool {
    db.prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name = 'ts_ms'")
        .and_then(|mut stmt| stmt.exists([]))
        .unwrap_or(false)
}

/// SQL expression yielding a row's millisecond `@time`, tolerant of databases
/// that lack the `ts_ms` column (see [`has_ts_ms_column`]). `prefix` is the
/// table alias plus dot (`"m."` for the JOIN reads, `""` for single-table reads).
fn ts_ms_expr(db: &Connection, prefix: &str) -> String {
    if has_ts_ms_column(db) {
        format!("COALESCE({prefix}ts_ms, {prefix}timestamp * 1000)")
    } else {
        format!("({prefix}timestamp * 1000)")
    }
}

/// Columns selected by every chat/log read path.
///
/// Fan-out reference rows (e.g. a single QUIT broadcast across N channels)
/// are stored with `text = ''` and `ref_id = <primary msg_id>` to save
/// space — only the primary row carries the actual message text and IV.
/// Without a JOIN to that primary row, every reference row would render
/// as a blank event line in backlog or in the log browser. The aliases
/// below transparently substitute the primary's `text` + `iv` whenever
/// a reference exists; `map_row` is unchanged. `ts_ms` is built via
/// [`ts_ms_expr`] so the read works even on an unmigrated read-only DB.
fn select_message_columns(db: &Connection) -> String {
    format!(
        "m.id, m.msg_id, m.network, m.buffer, m.timestamp,
         {} AS ts_ms,
         m.type, m.nick,
         COALESCE(p.text, m.text) AS text,
         m.highlight,
         COALESCE(p.iv,   m.iv)   AS iv,
         m.ref_id, m.tags, m.event_key",
        ts_ms_expr(db, "m.")
    )
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
    let cols = select_message_columns(db);
    let mut messages = if let Some(before_ts) = before {
        let sql = format!(
            "SELECT {cols}
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
            "SELECT {cols}
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
    let cols = select_message_columns(db);
    let mut messages = if let Some((ts, id)) = before {
        let sql = format!(
            "SELECT {cols}
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
            "SELECT {cols}
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
    let cols = select_message_columns(db);
    // Same millisecond expression drives the SELECT alias, the cursor predicate
    // and the ORDER BY, and degrades to `timestamp * 1000` on an unmigrated DB.
    let order = ts_ms_expr(db, "m.");
    let mut messages = if let Some((ms, id)) = before {
        let secs = ms.div_euclid(1000);
        let sql = format!(
            "SELECT {cols}
             FROM messages m
             LEFT JOIN messages p ON p.msg_id = m.ref_id AND p.network = m.network
             WHERE m.network = ?1 AND m.buffer = ?2
               AND m.timestamp <= ?3
               AND ({order} < ?4
                    OR ({order} = ?4 AND m.id < ?5))
             ORDER BY {order} DESC, m.id DESC
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
            "SELECT {cols}
             FROM messages m
             LEFT JOIN messages p ON p.msg_id = m.ref_id AND p.network = m.network
             WHERE m.network = ?1 AND m.buffer = ?2
             ORDER BY {order} DESC, m.id DESC
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

/// Paginate older web-client history from its scroll-back cursor, picking the
/// keyset that matches the cursor's resolution.
///
/// The current web bundle sends the oldest loaded row's full-millisecond `@time`
/// (`ts_ms`) and wants the subsecond keyset so a CHATHISTORY-backfilled same-second
/// row (larger rowid, smaller `ts_ms`) is returned rather than hidden. A client
/// still running the *previous* bundle sends a whole-**second** `before`; routing
/// that through the subsecond keyset floors it to `.000` and silently drops
/// same-second rows whose `ts_ms` is past `.000` but whose rowid is smaller than
/// the cursor's. A second-scale `before` therefore uses the whole-second keyset
/// (`timestamp < S OR (timestamp = S AND id < before_id)`), which is lossless for
/// a seconds cursor. `before = None` is the initial (newest) page.
///
/// The seconds/millis split is by magnitude. The cutoff (`1e10`) sits in the gap
/// where the two ranges cannot overlap for any real IRC log: `1e10` **seconds** is
/// year 2286 (no plausible seconds timestamp is that large) and `1e10`
/// **milliseconds** is 1970-04-26 (no plausible log row predates IRC's 1988
/// origin, let alone that). A higher cutoff like `1e12` would misread a *current*
/// client's millisecond cursor for a pre-2001 log row (e.g. `946684800000` for
/// 2000-01-01) as seconds and return the cursor/newer rows again.
pub fn paginate_web_history(
    db: &Connection,
    network: &str,
    buffer: &str,
    cursor: Option<(i64, Option<i64>)>,
    limit: usize,
    encrypt: bool,
    crypto_key: Option<&Key<Aes256Gcm>>,
) -> rusqlite::Result<Vec<StoredMessage>> {
    // `cursor` is `(before, before_id?)`. Below this a `before` value must be whole
    // seconds (a previous-bundle client): 1e10 s is year 2286 and 1e10 ms is
    // 1970-04-26, so real seconds timestamps fall below it and real millisecond
    // timestamps fall on or above it, with no overlap. See the doc comment.
    const SECOND_SCALE_MAX: i64 = 10_000_000_000;
    match cursor {
        Some((secs, before_id)) if secs < SECOND_SCALE_MAX => get_messages_paginated(
            db,
            network,
            buffer,
            Some((secs, before_id.unwrap_or(0))),
            limit,
            encrypt,
            crypto_key,
        ),
        Some((ms, before_id)) => get_messages_paginated_subsecond(
            db,
            network,
            buffer,
            Some((ms, before_id.unwrap_or(0))),
            limit,
            encrypt,
            crypto_key,
        ),
        None => get_messages_paginated_subsecond(db, network, buffer, None, limit, encrypt, crypto_key),
    }
}

/// Newest stored row for a buffer, used as the anchor for a
/// `CHATHISTORY AFTER` gap-fill request after (re)connecting.
///
/// Returns `(unix_millis, msgid?)` for the newest row, or `None` if the buffer
/// has no stored messages. The timestamp is the full-millisecond `@time`,
/// ordered by `COALESCE(ts_ms, timestamp * 1000)`, NOT the floored whole second:
/// a busy channel can fit a whole `CHATHISTORY` page inside the boundary second,
/// so an `AFTER` anchor floored to `.000` could keep refetching already-stored
/// rows and never reach the messages missed during the disconnect.
///
/// `before_ms` optionally excludes rows at or after a millisecond cutoff. The
/// reconnect gap-fill passes the moment of (re)connection so the anchor stays on
/// the pre-disconnect tail: by end-of-NAMES the JOIN echo and any traffic during
/// a slow NAMES may already be logged, and anchoring `AFTER` one of those would
/// ask only for post-reconnect messages and miss the disconnected gap entirely.
/// `None` imposes no cutoff (the newest row overall).
///
/// Like [`oldest_anchor`], the `@msgid` comes from the row's `IRCv3` `tags` (a
/// *verified* server reference), NOT the ambiguous `msg_id` column (a
/// locally-minted UUID for live rows). The caller uses it as a `msgid=` anchor
/// when the server advertises `MSGREFTYPES=msgid` — `CHATHISTORY AFTER
/// timestamp=...` starts strictly after the second/millisecond, so a `@msgid`
/// reference avoids skipping rows in the same millisecond as the anchor.
pub fn newest_anchor(
    db: &Connection,
    network: &str,
    buffer: &str,
    before_ms: Option<i64>,
) -> rusqlite::Result<Option<(i64, Option<String>)>> {
    let expr = ts_ms_expr(db, "");
    let cutoff_clause = if before_ms.is_some() {
        format!(" AND {expr} < ?3")
    } else {
        String::new()
    };
    // Select the same millisecond expression the ORDER BY uses, so a row whose
    // tags lack `@time` falls back to its full-precision `ts_ms`, not a floored
    // whole second (see `anchor_from_tags`).
    let sql = format!(
        "SELECT {expr} AS anchor_ms, tags FROM messages
         WHERE network = ?1 AND buffer = ?2{cutoff_clause}
         ORDER BY {expr} DESC, id DESC
         LIMIT 1"
    );
    let mut stmt = db.prepare(&sql)?;
    // Log rows are stored under the lowercased buffer key (make_buffer_id);
    // callers pass the display-case channel/nick, so normalize for the lookup.
    let buffer_lc = buffer.to_lowercase();
    let mut rows = match before_ms {
        Some(cutoff) => stmt.query(params![network, buffer_lc, cutoff])?,
        None => stmt.query(params![network, buffer_lc])?,
    };
    match rows.next()? {
        Some(row) => {
            let anchor_ms: i64 = row.get(0)?;
            let tags: Option<String> = row.get(1)?;
            Ok(Some(anchor_from_tags(tags.as_deref(), anchor_ms)))
        }
        None => Ok(None),
    }
}

/// Oldest stored row for a buffer, used as the anchor for the **first**
/// `CHATHISTORY BEFORE` scroll-back request (before any per-target watermark
/// has been recorded).
///
/// Returns `(unix_millis, msgid?)` for the row with the least
/// `(COALESCE(ts_ms, timestamp * 1000), id)`, or `None` if the buffer has no
/// stored messages.
///
/// Ordered by the full-millisecond time (same key as
/// [`get_messages_paginated_subsecond`]), NOT `(timestamp, id)`: a
/// `CHATHISTORY BEFORE` row older in `@time` than a same-second live row is
/// inserted later, so it carries a larger rowid. A `(timestamp, id)` order would
/// then pick the live row (lower rowid) as "oldest" and anchor the first BEFORE
/// at a NEWER point — ingesting store-only rows newer than the buffer's current
/// oldest, which older-only pagination never surfaces (invisible until reload).
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
    let expr = ts_ms_expr(db, "");
    // Select the ordering expression (not the whole-second `timestamp`) so a row
    // without an `@time` tag anchors at its full-precision `ts_ms`.
    let sql = format!(
        "SELECT {expr} AS anchor_ms, tags FROM messages
         WHERE network = ?1 AND buffer = ?2
         ORDER BY {expr} ASC, id ASC
         LIMIT 1"
    );
    let mut stmt = db.prepare(&sql)?;
    // Normalize the buffer key to match how rows are stored (see `newest_anchor`).
    let mut rows = stmt.query(params![network, buffer.to_lowercase()])?;
    match rows.next()? {
        Some(row) => {
            let anchor_ms: i64 = row.get(0)?;
            let tags: Option<String> = row.get(1)?;
            Ok(Some(anchor_from_tags(tags.as_deref(), anchor_ms)))
        }
        None => Ok(None),
    }
}

/// Build a full-precision `(unix_millis, msgid?)` CHATHISTORY anchor from a
/// stored row's `IRCv3` `tags` JSON.
///
/// When `@time` is absent or unparseable the millis come from `fallback_ms` —
/// which callers pass as the row's `COALESCE(ts_ms, timestamp * 1000)`, NOT a
/// floored `timestamp * 1000`. A row stored without an `@time` tag (e.g. a live
/// message on a server without `server-time`) still has full-millisecond `ts_ms`,
/// and flooring it to the whole second here would anchor `AFTER`/`BEFORE` less
/// precisely than the database actually orders by, re-fetching (or skipping)
/// same-second rows at the boundary. `@msgid` is dropped (no msgid) when absent.
fn anchor_from_tags(tags_json: Option<&str>, fallback_ms: i64) -> (i64, Option<String>) {
    let tags: Option<std::collections::HashMap<String, String>> =
        tags_json.and_then(|j| serde_json::from_str(j).ok());
    let millis = tags
        .as_ref()
        .and_then(|t| t.get("time"))
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map_or(fallback_ms, |dt| dt.timestamp_millis());
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

    /// A `messages` table shaped like a read-only log DB opened before the
    /// `ts_ms` migration: the column simply does not exist.
    fn db_without_ts_ms() -> Connection {
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
             );
             CREATE VIRTUAL TABLE messages_fts USING fts5(nick, text, content=messages, content_rowid=id);
             CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
                 INSERT INTO messages_fts(rowid, nick, text) VALUES (new.id, new.nick, new.text);
             END;",
        )
        .unwrap();
        db
    }

    #[test]
    fn read_paths_work_without_ts_ms_column() {
        // Regression: a read-only log DB (repartee l) skips migration, so the
        // ts_ms column is absent. The read paths must not reference m.ts_ms
        // unconditionally — they fall back to timestamp*1000.
        let db = db_without_ts_ms();
        assert!(!has_ts_ms_column(&db));
        for (ts, text) in [(100_i64, "a"), (100, "b"), (200, "c")] {
            db.execute(
                "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight) \
                 VALUES (?1, 'libera', '#rust', ?2, 'message', 'ada', ?3, 0)",
                params![text, ts, text],
            )
            .unwrap();
        }

        // The log browser's initial + keyset pages (would previously panic with
        // "no such column: m.ts_ms").
        let all =
            get_messages_paginated_subsecond(&db, "libera", "#rust", None, 10, false, None).unwrap();
        let texts: Vec<&str> = all.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, vec!["a", "b", "c"]);
        assert_eq!(all[0].ts_ms, 100_000, "ts_ms falls back to timestamp*1000");

        let newest = all.last().unwrap();
        assert_eq!(newest.text, "c");
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
        assert_eq!(page_texts, vec!["a", "b"], "keyset pages older rows without ts_ms");

        // newest_anchor and FTS search also tolerate the missing column.
        let anchor = newest_anchor(&db, "libera", "#rust", None).unwrap().expect("anchor");
        assert_eq!(anchor.0, 200_000);
        let hits = search_messages(&db, "c", None, None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].ts_ms, 200_000);
    }

    /// Run `EXPLAIN QUERY PLAN` for `sql` and return the joined detail lines.
    fn explain(db: &Connection, sql: &str, params: &[&dyn rusqlite::ToSql]) -> String {
        let mut stmt = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        stmt.query_map(params, |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join("\n")
    }

    #[test]
    fn subsecond_pagination_is_index_backed_not_temp_btree() {
        // Regression: the scroll-back, web FetchMessages and log-browser hot
        // paths order by COALESCE(ts_ms, timestamp*1000). Without a matching
        // index that key forces SQLite to materialize and sort every row in the
        // buffer (USE TEMP B-TREE FOR ORDER BY) before applying LIMIT — O(N log N)
        // on large logs. The expression index lets it walk the index in reverse
        // and stop at LIMIT. Build the exact ORDER BY the production query runs
        // (via ts_ms_expr) so the assertion tracks the real key.
        let db = setup_test_db();
        let order = ts_ms_expr(&db, "m.");

        // Initial page (no cursor).
        let initial = format!(
            "SELECT m.id, p.text FROM messages m
             LEFT JOIN messages p ON p.msg_id = m.ref_id AND p.network = m.network
             WHERE m.network = ?1 AND m.buffer = ?2
             ORDER BY {order} DESC, m.id DESC
             LIMIT 50"
        );
        let plan = explain(&db, &initial, &[&"libera", &"#rust"]);
        assert!(
            !plan.contains("TEMP B-TREE"),
            "initial page must be index-backed, got plan:\n{plan}"
        );
        assert!(
            plan.contains("idx_messages_subsecond"),
            "initial page should use the subsecond index, got plan:\n{plan}"
        );

        // Cursor page (keyset predicate + same ordering).
        let cursor = format!(
            "SELECT m.id, p.text FROM messages m
             LEFT JOIN messages p ON p.msg_id = m.ref_id AND p.network = m.network
             WHERE m.network = ?1 AND m.buffer = ?2
               AND m.timestamp <= ?3
               AND ({order} < ?4 OR ({order} = ?4 AND m.id < ?5))
             ORDER BY {order} DESC, m.id DESC
             LIMIT 50"
        );
        let plan = explain(
            &db,
            &cursor,
            &[&"libera", &"#rust", &1700_i64, &1_700_000_i64, &9999_i64],
        );
        assert!(
            !plan.contains("TEMP B-TREE"),
            "cursor page must be index-backed, got plan:\n{plan}"
        );
        assert!(
            plan.contains("idx_messages_subsecond"),
            "cursor page should use the subsecond index, got plan:\n{plan}"
        );
    }

    #[test]
    fn web_pagination_legacy_seconds_cursor_keeps_same_second_subsecond_rows() {
        // Regression: a web client on the PREVIOUS bundle sends a whole-SECOND
        // `before`. Three rows share second S=1000: a(.200,id1), b(.500,id2),
        // c(.800,id3). The client loaded down to b and sends before=1000 (seconds)
        // + before_id=id_b. The older same-second row `a` must still come back.
        let db = setup_test_db();
        for (text, ts_ms) in [("a", 1_000_200_i64), ("b", 1_000_500), ("c", 1_000_800)] {
            db.execute(
                "INSERT INTO messages (msg_id, network, buffer, timestamp, ts_ms, type, nick, text, highlight) \
                 VALUES (?1, 'net', '#chan', 1000, ?2, 'message', 'ada', ?3, 0)",
                params![format!("m-{text}"), ts_ms, text],
            )
            .unwrap();
        }
        let id_b: i64 = db
            .query_row("SELECT id FROM messages WHERE text = 'b'", [], |r| r.get(0))
            .unwrap();

        // The fix routes a second-scale cursor to the lossless whole-second keyset.
        let page =
            paginate_web_history(&db, "net", "#chan", Some((1000, Some(id_b))), 10, false, None)
                .unwrap();
        let texts: Vec<&str> = page.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["a"],
            "legacy seconds cursor must return the same-second older row, not skip it"
        );

        // Documents the bug: the subsecond keyset with that cursor floored to
        // `.000` (1_000_000) drops `a` (ts_ms .200 is neither < .000 nor == .000).
        let skipped =
            get_messages_paginated_subsecond(&db, "net", "#chan", Some((1_000_000, id_b)), 10, false, None)
                .unwrap();
        assert!(
            skipped.iter().all(|m| m.text != "a"),
            "subsecond keyset floored to .000 would skip a — the regression being fixed"
        );
    }

    #[test]
    fn web_pagination_pre_2001_millis_cursor_is_classified_as_millis() {
        // A CURRENT web client sends `before` in milliseconds. For a log row before
        // ~2001-09-09 the ms value is < 1e12 but >= 1e10; it must still be treated
        // as millis. With the old 1e12 cutoff it was misread as a seconds cursor,
        // and the whole-second query compared a 9.4e11 value against the seconds
        // `timestamp` column (~9.4e8), matching every row and re-returning the
        // cursor/newer rows — duplicate/broken scrollback for old logs.
        let db = setup_test_db();
        for (text, secs, ts_ms) in [
            ("older", 946_684_795_i64, 946_684_795_000_i64),
            ("cursor", 946_684_800, 946_684_800_000),
        ] {
            db.execute(
                "INSERT INTO messages (msg_id, network, buffer, timestamp, ts_ms, type, nick, text, highlight) \
                 VALUES (?1, 'net', '#chan', ?2, ?3, 'message', 'ada', ?4, 0)",
                params![format!("m-{text}"), secs, ts_ms, text],
            )
            .unwrap();
        }
        let cursor_id: i64 = db
            .query_row("SELECT id FROM messages WHERE text = 'cursor'", [], |r| r.get(0))
            .unwrap();

        let page = paginate_web_history(
            &db,
            "net",
            "#chan",
            Some((946_684_800_000, Some(cursor_id))),
            10,
            false,
            None,
        )
        .unwrap();
        let texts: Vec<&str> = page.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["older"],
            "pre-2001 millis cursor must page older rows, not re-return the cursor row"
        );
    }

    #[test]
    fn anchors_keep_ts_ms_precision_when_tags_lack_time() {
        // A row with sub-second ts_ms but no parseable @time in tags (e.g. a live
        // message logged on a server without server-time). The anchor must use the
        // precise ts_ms the query orders by, not floor to timestamp*1000.
        let db = setup_test_db();
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, ts_ms, type, nick, text, highlight, tags) \
             VALUES ('m1', 'net', '#chan', 1000, 1000500, 'message', 'ada', 'hi', 0, NULL)",
            [],
        )
        .unwrap();

        let newest = newest_anchor(&db, "net", "#chan", None).unwrap().expect("anchor");
        assert_eq!(
            newest.0, 1_000_500,
            "newest_anchor must keep ts_ms precision when @time is absent"
        );
        let oldest = oldest_anchor(&db, "net", "#chan").unwrap().expect("anchor");
        assert_eq!(
            oldest.0, 1_000_500,
            "oldest_anchor must keep ts_ms precision when @time is absent"
        );
    }

    #[test]
    fn newest_anchor_empty_is_none() {
        let db = setup_test_db();
        assert_eq!(newest_anchor(&db, "net", "#chan", None).unwrap(), None);
    }

    #[test]
    fn newest_anchor_returns_latest_row() {
        let db = setup_test_db();
        insert_msg(&db, "net", "#chan", 100, "a");
        insert_msg(&db, "net", "#chan", 300, "c");
        insert_msg(&db, "net", "#chan", 200, "b");

        // Anchor is (unix_millis, id) — the stored `msg_id` is deliberately NOT
        // returned (it may be a locally-minted UUID, not a server @msgid).
        let anchor = newest_anchor(&db, "net", "#chan", None).unwrap().expect("anchor");
        assert_eq!(anchor.0, 300_000);
    }

    #[test]
    fn newest_anchor_cutoff_excludes_reconnect_time_rows() {
        // The reconnect gap-fill passes the connect time as `before_ms`: a row at
        // or after it (a JOIN echo or traffic logged before end-of-NAMES) must be
        // excluded so the AFTER anchor stays on the pre-disconnect tail.
        let db = setup_test_db();
        insert_msg(&db, "net", "#chan", 100, "pre-disconnect"); // ts_ms 100_000
        insert_msg(&db, "net", "#chan", 500, "reconnect echo"); // ts_ms 500_000

        // No cutoff → newest overall.
        assert_eq!(
            newest_anchor(&db, "net", "#chan", None)
                .unwrap()
                .map(|(ms, _)| ms),
            Some(500_000)
        );
        // Cutoff above the pre-disconnect row, below the echo → the tail.
        let anchor = newest_anchor(&db, "net", "#chan", Some(200_000))
            .unwrap()
            .expect("anchor");
        assert_eq!(anchor.0, 100_000);
        // Cutoff at/below the only remaining row → nothing qualifies (gap-fill
        // then falls back to LATEST). `< cutoff` is strict.
        assert_eq!(newest_anchor(&db, "net", "#chan", Some(100_000)).unwrap(), None);
    }

    #[test]
    fn newest_anchor_breaks_ties_by_id() {
        let db = setup_test_db();
        for i in 0..3 {
            db.execute(
                "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight, tags) \
                 VALUES (?1, 'net', '#chan', 500, 'message', 'ada', ?2, 0, ?3)",
                params![
                    format!("dup-{i}"),
                    format!("t{i}"),
                    format!("{{\"msgid\":\"m{i}\"}}")
                ],
            )
            .unwrap();
        }
        // All share whole-second 500 (no ts_ms → 500_000); the greatest id wins,
        // and its verified @msgid (m2) comes back as the anchor reference.
        let anchor = newest_anchor(&db, "net", "#chan", None).unwrap().expect("anchor");
        assert_eq!(anchor, (500_000, Some("m2".to_string())));
    }

    #[test]
    fn newest_anchor_scoped_to_buffer() {
        let db = setup_test_db();
        insert_msg(&db, "net", "#chan", 100, "a");
        insert_msg(&db, "net", "#other", 999, "b");

        let anchor = newest_anchor(&db, "net", "#chan", None).unwrap().expect("anchor");
        assert_eq!(anchor.0, 100_000);
    }

    #[test]
    fn newest_anchor_does_not_expose_stored_msgid_column() {
        // Regression: live rows are logged with a generated UUID in
        // `messages.msg_id` (state/events::maybe_log), NOT the server @msgid. The
        // anchor's msgid comes from the verified `tags` column, so a row with no
        // tags yields `None` — the UUID `msg_id` column is never fed to
        // `CHATHISTORY ... msgid=<uuid>`.
        let db = setup_test_db();
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, type, nick, text, highlight) \
             VALUES ('550e8400-e29b-41d4-a716-446655440000', 'net', '#chan', 100, 'message', 'ada', 'hi', 0)",
            [],
        )
        .unwrap();
        let newest = newest_anchor(&db, "net", "#chan", None).unwrap().expect("anchor");
        assert_eq!(newest, (100_000, None));
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

        let newest = newest_anchor(&db, "net", "#Rust", None).unwrap().expect("anchor");
        assert_eq!(newest.0, 200_000);
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
    fn oldest_anchor_picks_true_oldest_by_subsecond_not_rowid() {
        // A live row at .900 (rowid 1) and a CHATHISTORY backfill at .200 inserted
        // LATER (rowid 2), same whole-second. The first BEFORE anchor must be the
        // .200 row — a (timestamp, id) order would pick the .900 live row (lower
        // rowid) and anchor too new, ingesting store-only rows the buffer can't
        // surface.
        let db = setup_test_db();
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, ts_ms, type, nick, text, highlight, tags) \
             VALUES ('live', 'net', '#chan', 100, 100900, 'message', 'ada', 'live', 0, \
             '{\"time\":\"1970-01-01T00:01:40.900Z\",\"msgid\":\"M9\"}')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO messages (msg_id, network, buffer, timestamp, ts_ms, type, nick, text, highlight, tags) \
             VALUES ('backfill', 'net', '#chan', 100, 100200, 'message', 'ada', 'older', 0, \
             '{\"time\":\"1970-01-01T00:01:40.200Z\",\"msgid\":\"M2\"}')",
            [],
        )
        .unwrap();

        let anchor = oldest_anchor(&db, "net", "#chan").unwrap().expect("anchor");
        assert_eq!(
            anchor,
            (100_200, Some("M2".to_string())),
            "anchor is the true-oldest @time, not the lowest rowid"
        );
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

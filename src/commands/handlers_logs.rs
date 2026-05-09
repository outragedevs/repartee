//! Slash command handlers active only when `app.log_browser_mode == true`.
//!
//! These are dispatched directly from `execute_command_with_depth` when the
//! mode flag is set — they never reach `commands::registry`. Keeping them
//! out of the global registry means the chat-mode help/list output stays
//! free of log-only commands and the registry doesn't need a `condition`
//! predicate.
//!
//! V1 surface: `/search`, `/quit`, `/help`. Future: `/jump`, `/grep`.

#![allow(clippy::redundant_pub_crate)]
#![allow(
    clippy::missing_const_for_fn,
    reason = "consistent with other command handlers"
)]

use crate::app::App;
use crate::commands::helpers::add_local_event;

pub(crate) fn cmd_log_quit(app: &mut App, _args: &[String]) {
    app.should_quit = true;
}

pub(crate) fn cmd_log_help(app: &mut App, _args: &[String]) {
    add_local_event(app, "log mode commands:");
    add_local_event(app, "  /search <text>   search the active log");
    add_local_event(app, "  /quit            exit log browser");
    add_local_event(app, "  /help            this list");
    add_local_event(app, "Hotkeys (outside input): Q quit, ↑/↓ scroll, PgUp/PgDn page, g/G start/end");
}

pub(crate) fn cmd_log_search(app: &mut App, args: &[String]) {
    const SEARCH_LIMIT: usize = 1000;
    if args.is_empty() {
        add_local_event(app, "Usage: /search <text>");
        return;
    }
    let query = args.join(" ");
    let Some(active_id) = app.state.active_buffer_id.clone() else {
        add_local_event(app, "No active log buffer");
        return;
    };
    let Some((net, buf)) = app.split_log_buffer_id(&active_id) else {
        add_local_event(app, "Active buffer is not a log");
        return;
    };
    let Some(log_db) = &app.log_db else {
        add_local_event(app, "Log DB unavailable");
        return;
    };

    // FTS5 is gated on the schema being plain-text — encrypted logs
    // store ciphertext, so the FTS index is never built. Two query
    // paths:
    //
    //   plain DB     → `search_messages` (FTS5) — fast, ranks by
    //                  match relevance, supports phrase queries.
    //   encrypted DB → fetch the buffer through `get_messages` so
    //                  rows are decrypted in-process, then filter
    //                  in Rust with `text.contains(query)`. Slower
    //                  (whole buffer scan) but correctness over
    //                  performance — encrypted users still get
    //                  /search instead of a useless error.
    let rows = if log_db.has_fts {
        let Ok(db) = log_db.db.lock() else {
            add_local_event(app, "Log DB lock poisoned");
            return;
        };
        match crate::storage::query::search_messages(
            &db,
            &query,
            Some(&net),
            Some(&buf),
            SEARCH_LIMIT,
        ) {
            Ok(r) => r,
            Err(e) => {
                drop(db);
                add_local_event(app, &format!("Search failed: {e}"));
                return;
            }
        }
    } else {
        // Encrypted path: pull a bounded slice of recent rows
        // (decrypted by `get_messages`) and filter case-insensitively
        // in memory. The cap exists so a 100k-row encrypted log
        // doesn't freeze the TUI on a single `/search` — better to
        // tell the user we scanned the most recent N and offer
        // narrowing the query than to block the runtime.
        const MAX_ENCRYPTED_SCAN: usize = 10_000;
        let Ok(db) = log_db.db.lock() else {
            add_local_event(app, "Log DB lock poisoned");
            return;
        };
        let scanned = match crate::storage::query::get_messages(
            &db,
            &net,
            &buf,
            None,
            MAX_ENCRYPTED_SCAN,
            true,
            log_db.crypto_key.as_ref(),
        ) {
            Ok(r) => r,
            Err(e) => {
                drop(db);
                add_local_event(app, &format!("Search failed: {e}"));
                return;
            }
        };
        drop(db);
        let scan_capped = scanned.len() == MAX_ENCRYPTED_SCAN;
        let needle = query.to_lowercase();
        let hits: Vec<_> = scanned
            .into_iter()
            .filter(|m| m.text.to_lowercase().contains(&needle))
            .take(SEARCH_LIMIT)
            .collect();
        if scan_capped {
            add_local_event(
                app,
                &format!(
                    "[encrypted /search scanned the most recent {MAX_ENCRYPTED_SCAN} rows; \
                     narrow the query or use /jump to anchor older]"
                ),
            );
        }
        hits
    };

    add_local_event(
        app,
        &format!("[{} matches for \"{}\" in {}/{}]", rows.len(), query, net, buf),
    );
    for hit in rows {
        let when = chrono::DateTime::<chrono::Utc>::from_timestamp(hit.timestamp, 0)
            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        let nick = hit.nick.as_deref().unwrap_or("*");
        add_local_event(app, &format!("{when}  <{nick}> {}", hit.text));
    }
}

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
    if !log_db.has_fts {
        // search_messages requires FTS. With an encrypted DB we'd need a
        // LIKE fallback; defer that to V1.1.
        add_local_event(
            app,
            "/search requires plain-text logs; enable [storage] encrypt = false to use it",
        );
        return;
    }

    let hits = {
        let Ok(db) = log_db.db.lock() else {
            add_local_event(app, "Log DB lock poisoned");
            return;
        };
        crate::storage::query::search_messages(&db, &query, Some(&net), Some(&buf), 100)
    };
    match hits {
        Ok(rows) => {
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
        Err(e) => add_local_event(app, &format!("Search failed: {e}")),
    }
}

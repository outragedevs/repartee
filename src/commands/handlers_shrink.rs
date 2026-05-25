//! `/shrink <url>` — manually shorten a single URL via the shrink API.
//!
//! Output lands in the current buffer as a local event line:
//!
//!   • success: `Shortened: <original> → https://shr.al/<slug>`
//!   • cached:  `Shortened: <original> → https://shr.al/<slug> (cached)`
//!   • failure: `Shrink failed: <reason>`
//!
//! The handler is synchronous (returns immediately); the result
//! reaches the buffer via the shared `shrink_deliver_tx` channel,
//! which the main loop drains and routes through
//! `App::apply_shrink_deliver` (`Manual` variant).

use std::sync::Arc;
use std::time::Duration;

use super::helpers::add_local_event;
use super::types::{C_ERR, C_RST};
use crate::app::App;
use crate::app::shrink::ShrinkDeliver;

pub(crate) fn cmd_shrink(app: &mut App, args: &[String]) {
    if args.is_empty() {
        add_local_event(app, "Usage: /shrink <url>");
        return;
    }
    let url = args[0].clone();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        add_local_event(
            app,
            &format!("{C_ERR}URL must start with http:// or https://{C_RST}"),
        );
        return;
    }

    let Some(active_id) = app.state.active_buffer_id.clone() else {
        add_local_event(app, &format!("{C_ERR}No active buffer{C_RST}"));
        return;
    };

    let Some(client) = app.shrink_client.clone() else {
        add_local_event(
            app,
            &format!(
                "{C_ERR}Shrink is disabled — set `shrink.enabled = true` and \
                 `SHRINK_API_KEY=…` in `.env`{C_RST}"
            ),
        );
        return;
    };

    let timeout = Duration::from_millis(app.config.shrink.outgoing_timeout_ms);
    let cache = Arc::clone(&app.shrink_cache);
    let tx = app.shrink_deliver_tx.clone();

    add_local_event(app, &format!("Shortening {url}…"));

    tokio::spawn(async move {
        // Cache hit returns instantly without an API round-trip;
        // marker `(cached)` so the user can tell at a glance.
        let cached = cache.lock().get(&url);
        let display = match cached {
            Some(sh) => format!("Shortened: {} → {} (cached)", sh.original, sh.shortened),
            None => match client.shorten(&url, timeout).await {
                Ok(sh) => {
                    cache.lock().insert(sh.original.clone(), sh.clone());
                    format!("Shortened: {} → {}", sh.original, sh.shortened)
                }
                Err(e) => format!("Shrink failed: {e}"),
            },
        };
        let _ = tx
            .send(ShrinkDeliver::Manual {
                buffer_id: active_id,
                display,
            })
            .await;
    });
}

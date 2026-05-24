//! `/shrink <url>` — manually shorten a single URL via the shrink API.
//!
//! Output lands in the current buffer as a local event line:
//!
//!   • success: `Shortened: <original> → https://shr.al/<slug>`
//!   • failure: `Shrink failed: <reason>`
//!
//! Asynchronous: the slash-command handler kicks off a tokio task and
//! returns immediately; the result is delivered back through the
//! shared `shrink_tx` channel and handled by `App::apply_shrink_result`
//! (manual variant). Same pipeline incoming/outgoing flows use, so
//! shutdown ordering / channel sizing only have to be thought about
//! in one place.

use std::sync::Arc;
use std::time::Duration;

use super::helpers::add_local_event;
use super::types::{C_ERR, C_RST};
use crate::app::{App, ShrinkResult};
use crate::app::shrink::ManualShrinkOutput;

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

    // Use the outgoing timeout — `/shrink` is a manual outgoing
    // action, same UX expectation as typing a URL in a message.
    let timeout = Duration::from_millis(app.config.shrink.outgoing_timeout_ms);
    let cache = Arc::clone(&app.shrink_cache);
    let tx = app.shrink_tx.clone();

    add_local_event(app, &format!("Shortening {url}…"));

    tokio::spawn(async move {
        // Check cache first — `/shrink` of a recently-seen URL should
        // return instantly without an API round-trip.
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
            .send(ShrinkResult {
                buffer_id: active_id,
                message_id: 0,
                shortenings: Vec::new(),
                outgoing_send: None,
                manual: Some(ManualShrinkOutput { display }),
            })
            .await;
    });
}

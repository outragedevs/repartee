//! Glue between the shrink module (`src/shrink/`) and the `App` event
//! loop. Two responsibilities:
//!
//! 1. `App::dispatch_shrink_for_incoming` — kick off background shrink
//!    tasks for newly-arrived chat messages whose text contains URLs
//!    above the threshold. Cache hits short-circuit; misses spawn an
//!    HTTP call.
//! 2. `App::dispatch_shrink_for_outgoing` — same idea for outgoing
//!    messages, but the task also writes the substituted text to the
//!    IRC sender so other clients see the short URL.
//! 3. `App::apply_shrink_result` — main-loop arm handler that merges
//!    the result into `Message.shortenings` and broadcasts a
//!    `WebEvent::MessageShortened` to web clients.
//!
//! Out-of-order send risk: outgoing tasks complete in shrink-latency
//! order, not user-submission order. Back-to-back outgoing messages
//! containing URLs of wildly different shrink latencies CAN arrive
//! out of order on the IRC wire. In practice this is rare (most
//! messages have ≤1 URL, most URLs hit cache after the first), so
//! we trade strict ordering for the simplicity of spawn-per-send.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::mpsc;

use super::App;
use crate::shrink::{ShrinkCache, ShrinkClient, UrlShortening, find_long_urls};
use crate::web::protocol::WebEvent;

/// Posted by a background shrink task; consumed by the main event loop.
/// Two flavours share the type so the loop only has one arm to drain:
///
/// - `shortenings` non-empty + `message_id != 0` → merge into the
///   already-buffered Message (incoming live / outgoing). For outgoing
///   the `outgoing_send` field carries the IRC sender info needed to
///   push the substituted text on the wire after shrinking completes.
/// - `manual` set → add a `Shortened: …` / `Shrink failed: …` event
///   line to `buffer_id`. The `/shrink` slash command takes this path
///   because it just wants user-visible output, not a substitution
///   into someone else's chat message.
#[derive(Debug)]
pub struct ShrinkResult {
    pub buffer_id: String,
    pub message_id: u64,
    pub shortenings: Vec<UrlShortening>,
    pub outgoing_send: Option<OutgoingSend>,
    pub manual: Option<ManualShrinkOutput>,
}

#[derive(Debug)]
pub struct OutgoingSend {
    pub conn_id: String,
    pub target: String,
    pub substituted_text: String,
}

#[derive(Debug)]
pub struct ManualShrinkOutput {
    /// Already-formatted line to add as a local event in `buffer_id`.
    pub display: String,
}

impl App {
    /// Drain `state.pending_shrink_dispatch`, kicking off background
    /// shortenings for each enqueued live chat message. Called from
    /// the main event loop right after the IRC dispatcher runs (same
    /// rhythm as `drain_pending_web_events`).
    pub(crate) fn drain_pending_shrink_dispatch(&mut self) {
        let pending = std::mem::take(&mut self.state.pending_shrink_dispatch);
        for d in pending {
            self.dispatch_shrink_for_incoming(d.buffer_id, d.message_id, &d.text);
        }
    }

    /// Kick off background shrinking for an incoming live chat message.
    /// No-op if the feature is disabled, no client is configured, or
    /// the text has no URLs above the threshold.
    pub(crate) fn dispatch_shrink_for_incoming(
        &self,
        buffer_id: String,
        message_id: u64,
        text: &str,
    ) {
        let cfg = &self.config.shrink;
        if !cfg.enabled || !cfg.incoming_enabled {
            return;
        }
        let Some(ref client) = self.shrink_client else { return };
        self.spawn_shrink(
            buffer_id,
            message_id,
            text,
            cfg.min_url_length as usize,
            Duration::from_millis(cfg.incoming_timeout_ms),
            client.clone(),
            None,
        );
    }

    /// Kick off background shrinking for an outgoing message; the
    /// spawned task additionally calls `irc::Sender::send_privmsg`
    /// with the substituted text once shrinking completes (or with
    /// the original text on timeout).
    pub(crate) fn dispatch_shrink_for_outgoing(
        &self,
        buffer_id: String,
        message_id: u64,
        conn_id: String,
        target: String,
        text: &str,
    ) -> bool {
        let cfg = &self.config.shrink;
        if !cfg.enabled || !cfg.outgoing_enabled {
            return false;
        }
        let Some(ref client) = self.shrink_client else { return false };
        let urls = find_long_urls(text, cfg.min_url_length as usize);
        if urls.is_empty() {
            return false;
        }
        self.spawn_shrink(
            buffer_id,
            message_id,
            text,
            cfg.min_url_length as usize,
            Duration::from_millis(cfg.outgoing_timeout_ms),
            client.clone(),
            Some((conn_id, target, text.to_string())),
        );
        true
    }

    fn spawn_shrink(
        &self,
        buffer_id: String,
        message_id: u64,
        text: &str,
        min_length: usize,
        timeout: Duration,
        client: ShrinkClient,
        outgoing: Option<(String, String, String)>,
    ) {
        let urls = find_long_urls(text, min_length);
        if urls.is_empty() {
            // Outgoing callers already filtered; this path only
            // matters for incoming, where a no-URL message just
            // skips the spawn entirely.
            return;
        }
        let tx = self.shrink_tx.clone();
        let cache = Arc::clone(&self.shrink_cache);
        tokio::spawn(async move {
            let shortenings = resolve_shortenings(&client, &cache, urls, timeout).await;
            let outgoing_send = outgoing.map(|(conn_id, target, original_text)| {
                let substituted_text = apply_substitutions(&original_text, &shortenings);
                OutgoingSend {
                    conn_id,
                    target,
                    substituted_text,
                }
            });
            if shortenings.is_empty() && outgoing_send.is_none() {
                return;
            }
            let _ = tx
                .send(ShrinkResult {
                    buffer_id,
                    message_id,
                    shortenings,
                    outgoing_send,
                    manual: None,
                })
                .await;
        });
    }

    /// Drain one shrink result from the main-loop arm: update the
    /// in-memory message, broadcast to web clients, and (for the
    /// outgoing variant) hand the substituted text to the IRC sender.
    pub(crate) fn apply_shrink_result(&mut self, result: ShrinkResult) {
        // Manual `/shrink` output path: bypass everything below and
        // just print the result as a local event in the target buffer.
        if let Some(manual) = result.manual {
            // The buffer might have been closed while the request was
            // in flight; silently drop the output rather than crash.
            if !self.state.buffers.contains_key(&result.buffer_id) {
                return;
            }
            let prior = self.state.active_buffer_id.clone();
            self.state.active_buffer_id = Some(result.buffer_id.clone());
            crate::commands::helpers::add_local_event(self, &manual.display);
            self.state.active_buffer_id = prior;
            return;
        }

        // Send to IRC FIRST so other clients see the short URL with
        // the smallest possible additional delay; only on send failure
        // do we leave the local copy untouched (the user already sees
        // their original message in the buffer).
        if let Some(ref out) = result.outgoing_send
            && let Some(handle) = self.irc_handles.get(&out.conn_id)
            && handle
                .sender
                .send_privmsg(&out.target, &out.substituted_text)
                .is_err()
        {
            tracing::warn!(
                conn_id = %out.conn_id,
                target = %out.target,
                "shrink: IRC send failed for substituted text"
            );
        }

        // Merge into the in-memory message. Order-preserving so the
        // renderer applies substitutions in the same sequence as the
        // URLs appeared in `text`.
        let Some(buf) = self.state.buffers.get_mut(&result.buffer_id) else {
            return;
        };
        let Some(msg) = buf
            .messages
            .iter_mut()
            .find(|m| m.id == result.message_id)
        else {
            return;
        };
        for sh in &result.shortenings {
            if let Some(existing) = msg
                .shortenings
                .iter_mut()
                .find(|s| s.original == sh.original)
            {
                existing.shortened = sh.shortened.clone();
            } else {
                msg.shortenings.push(sh.clone());
            }
        }

        // Broadcast to web clients. Wire form mirrors the in-memory
        // shortening 1:1; the renderer recomputes host hints client-
        // side from `original`.
        let wire = msg
            .shortenings
            .iter()
            .map(crate::web::snapshot::shortening_to_wire)
            .collect();
        self.broadcast_web(WebEvent::MessageShortened {
            buffer_id: result.buffer_id,
            message_id: result.message_id,
            shortenings: wire,
        });
    }
}

/// Resolve every URL: cache hit returns the stored shortening; miss
/// kicks off a parallel `client.shorten` call. Cache hits + misses
/// are returned in input order so callers can rebuild substituted
/// text deterministically.
async fn resolve_shortenings(
    client: &ShrinkClient,
    cache: &Mutex<ShrinkCache>,
    urls: Vec<String>,
    timeout: Duration,
) -> Vec<UrlShortening> {
    // Partition into hits (resolved immediately) and misses (need HTTP).
    let mut hits: Vec<(usize, UrlShortening)> = Vec::new();
    let mut misses: Vec<(usize, String)> = Vec::new();
    {
        let mut c = cache.lock();
        for (i, url) in urls.iter().enumerate() {
            if let Some(sh) = c.get(url) {
                hits.push((i, sh));
            } else {
                misses.push((i, url.clone()));
            }
        }
    }

    // Fire all misses in parallel — `join_all` runs concurrently
    // because each `shorten` future is independent.
    let miss_futures = misses
        .iter()
        .map(|(_, url)| client.shorten(url, timeout));
    let miss_results = futures::future::join_all(miss_futures).await;

    let mut resolved: Vec<(usize, UrlShortening)> = hits;
    {
        let mut c = cache.lock();
        for ((idx, _), res) in misses.iter().zip(miss_results) {
            if let Ok(sh) = res {
                c.insert(sh.original.clone(), sh.clone());
                resolved.push((*idx, sh));
            }
        }
    }
    resolved.sort_by_key(|(i, _)| *i);
    resolved.into_iter().map(|(_, sh)| sh).collect()
}

/// Replace every shortening's `original` with `shortened` in `text`.
/// Single-pass per URL via `str::replace` — fine for the typical
/// 1–5-URL message; if a hot path ever appears, switch to a single
/// regex pass.
fn apply_substitutions(text: &str, shortenings: &[UrlShortening]) -> String {
    let mut out = text.to_string();
    for sh in shortenings {
        out = out.replace(&sh.original, &sh.shortened);
    }
    out
}

/// Build a `(ShrinkClient option, cache Arc, channel pair)` for
/// `App::new`. Splitting this out keeps the constructor concise and
/// makes the disabled-feature path trivial to spot.
pub(crate) fn build_runtime(
    cfg: &crate::config::ShrinkConfig,
) -> (
    Option<ShrinkClient>,
    Arc<Mutex<ShrinkCache>>,
    (mpsc::Sender<ShrinkResult>, mpsc::Receiver<ShrinkResult>),
) {
    let client = if cfg.enabled && !cfg.api_key.is_empty() {
        Some(ShrinkClient::new(cfg.api_url.clone(), cfg.api_key.clone()))
    } else {
        None
    };
    let cache = Arc::new(Mutex::new(ShrinkCache::new(cfg.cache_max_entries as usize)));
    let channel = mpsc::channel(256);
    (client, cache, channel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_substitutions_replaces_each() {
        let text = "see https://a.com/long and https://b.com/long";
        let shorts = vec![
            UrlShortening {
                original: "https://a.com/long".into(),
                shortened: "https://shr.al/1".into(),
            },
            UrlShortening {
                original: "https://b.com/long".into(),
                shortened: "https://shr.al/2".into(),
            },
        ];
        let out = apply_substitutions(text, &shorts);
        assert_eq!(out, "see https://shr.al/1 and https://shr.al/2");
    }

    #[test]
    fn apply_substitutions_handles_empty() {
        let out = apply_substitutions("nothing to do", &[]);
        assert_eq!(out, "nothing to do");
    }

    #[test]
    fn build_runtime_disabled_when_no_key() {
        let mut cfg = crate::config::ShrinkConfig::default();
        cfg.enabled = true;
        cfg.api_key = String::new();
        let (client, _cache, _ch) = build_runtime(&cfg);
        assert!(client.is_none());
    }

    #[test]
    fn build_runtime_enabled_with_key() {
        let mut cfg = crate::config::ShrinkConfig::default();
        cfg.enabled = true;
        cfg.api_key = "secret".into();
        let (client, _cache, _ch) = build_runtime(&cfg);
        assert!(client.is_some());
    }
}

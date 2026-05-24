//! Glue between the shrink module (`src/shrink/`) and the `App` event
//! loop.
//!
//! Design contract (matches the user-facing model in
//! `docs/commands/shrink.md`):
//!
//! - **Outgoing**: when the user presses Enter on a message that
//!   contains URLs over the threshold, the entire downstream pipeline
//!   (E2E encrypt, IRC send, local echo, log) waits for shrink to
//!   complete or time out — **shrink is the FIRST step**, not an
//!   after-the-fact decoration. By the time anyone (us, server, peers)
//!   sees the bytes, they're already the shortened form (or the
//!   original if shrink errored / timed out).
//!
//! - **Incoming**: shrink intercepts AFTER everything else in the
//!   PRIVMSG handler (decrypt, ignore checks, mention detection,
//!   highlight flagging) and BEFORE the message is added to the
//!   buffer. Display, web broadcast, and SQLite log all see the
//!   shortened text. This makes echo-message and E2E free of any
//!   special-casing: their wire/cipher already carries shortened
//!   plaintext for outgoing, and incoming shrinks the plaintext we
//!   already extracted via the normal pipeline.
//!
//! The user-perceived latency budget is `shrink.{outgoing,incoming}_timeout_ms`
//! (default 2 s each). Cache hits short-circuit synchronously inside
//! the worker; misses are HTTP round-trips.
//!
//! Two channels drive this:
//!
//! - `shrink_outgoing_tx` — `handle_plain_message` enqueues
//!   `PendingOutgoing`. A dedicated tokio task pulls, awaits shrink,
//!   then posts a `ShrinkDeliver::Outgoing` back into
//!   `shrink_deliver_rx`. Sequential per worker (one outgoing
//!   message at a time) so user-perceived order is preserved.
//! - `shrink_incoming_tx` — same shape for `PendingIncoming` from
//!   `handle_privmsg` / `handle_notice` / etc. Separate worker so a
//!   busy channel can't starve our outgoing.
//!
//! `apply_shrink_deliver` runs in the main loop and is the only path
//! that mutates `App` state (state.add_message_with_activity, IRC
//! sender, e2e encrypt). Workers stay pure-async.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::mpsc;

use super::App;
use crate::shrink::{ShrinkCache, ShrinkClient, UrlShortening, find_long_urls, host_of};
use crate::state::buffer::{ActivityLevel, BufferType, Message};

/// Posted by either shrink worker (outgoing / incoming) and consumed
/// by the main event loop. Carries the final substituted text plus
/// whatever the loop needs to deliver it to the user / server / etc.
#[derive(Debug)]
pub enum ShrinkDeliver {
    /// User-pressed-Enter outgoing message that has finished its
    /// shrink pass. The loop now runs the full
    /// "e2e encrypt → IRC send → local echo" pipeline with
    /// `substituted_text`.
    Outgoing(OutgoingDeliver),
    /// Live incoming PRIVMSG / ACTION / NOTICE that has finished its
    /// shrink pass. The loop calls
    /// `state.add_message_with_activity(buffer_id, message,
    /// activity_level)` (with `message.text` already substituted) and
    /// runs the secondary side-effects (mention buffer push).
    Incoming(IncomingDeliver),
    /// `/shrink <url>` manual output line. The loop just appends a
    /// local event with `display` to the buffer.
    Manual {
        buffer_id: String,
        display: String,
    },
}

#[derive(Debug)]
pub struct OutgoingDeliver {
    pub conn_id: String,
    pub buffer_id: String,
    pub buffer_name: String,
    pub buffer_type: BufferType,
    /// Post-substitution text (or the original on timeout / error).
    pub substituted_text: String,
}

#[derive(Debug)]
pub struct IncomingDeliver {
    pub buffer_id: String,
    /// Message with `text` already substituted (or original on
    /// timeout / error).
    pub message: Message,
    pub activity_level: ActivityLevel,
    /// `true` when the original `handle_privmsg` would have pushed
    /// the message into the `_mentions` buffer. We carry the flag so
    /// the deferred deliver can replicate that side-effect with the
    /// shortened text intact.
    pub push_to_mentions: bool,
}

/// `handle_plain_message` posts this when the message qualifies for
/// outgoing shrink. The worker takes ownership, awaits shrink,
/// substitutes, then posts an `OutgoingDeliver` to the main loop.
#[derive(Debug)]
pub struct PendingOutgoing {
    pub conn_id: String,
    pub buffer_id: String,
    pub buffer_name: String,
    pub buffer_type: BufferType,
    pub original_text: String,
}

/// `handle_privmsg` / `handle_action` / `handle_notice` posts this
/// when the message qualifies for incoming shrink. The worker takes
/// ownership, awaits shrink, substitutes (with `[host]` hint), then
/// posts an `IncomingDeliver` to the main loop.
#[derive(Debug)]
pub struct PendingIncoming {
    pub buffer_id: String,
    pub message: Message,
    pub activity_level: ActivityLevel,
    pub push_to_mentions: bool,
}

/// All the channels + shared state the shrink workers need. Built
/// once in `App::new` and kept alive for the App lifetime.
pub(crate) struct ShrinkRuntime {
    pub client: Option<ShrinkClient>,
    pub cache: Arc<Mutex<ShrinkCache>>,
    pub outgoing_tx: mpsc::Sender<PendingOutgoing>,
    pub incoming_tx: mpsc::Sender<PendingIncoming>,
    pub deliver_tx: mpsc::Sender<ShrinkDeliver>,
    pub deliver_rx: mpsc::Receiver<ShrinkDeliver>,
}

impl ShrinkRuntime {
    /// Build the runtime and spawn the two worker tasks. Returns the
    /// pieces App needs to store, plus the deliver receiver to wire
    /// into the main `tokio::select!`.
    pub fn build(cfg: &crate::config::ShrinkConfig) -> Self {
        let client = if cfg.enabled && !cfg.api_key.is_empty() {
            Some(ShrinkClient::new(cfg.api_url.clone(), cfg.api_key.clone()))
        } else {
            None
        };
        let cache = Arc::new(Mutex::new(ShrinkCache::new(cfg.cache_max_entries as usize)));

        let (outgoing_tx, outgoing_rx) = mpsc::channel::<PendingOutgoing>(256);
        let (incoming_tx, incoming_rx) = mpsc::channel::<PendingIncoming>(1024);
        let (deliver_tx, deliver_rx) = mpsc::channel::<ShrinkDeliver>(1024);

        // Spawn workers. Each owns its own clone of the client (Arc
        // internally) + cache (Arc<Mutex<>>) + deliver sender.
        if let Some(ref c) = client {
            spawn_outgoing_worker(
                outgoing_rx,
                c.clone(),
                Arc::clone(&cache),
                deliver_tx.clone(),
                Duration::from_millis(cfg.outgoing_timeout_ms),
                cfg.min_url_length as usize,
            );
            spawn_incoming_worker(
                incoming_rx,
                c.clone(),
                Arc::clone(&cache),
                deliver_tx.clone(),
                Duration::from_millis(cfg.incoming_timeout_ms),
                cfg.min_url_length as usize,
            );
        } else {
            // Shrink disabled: drain the queues to /dev/null so
            // `try_send` from the IRC / input paths doesn't backpressure.
            spawn_drain(outgoing_rx);
            spawn_drain(incoming_rx);
        }

        Self {
            client,
            cache,
            outgoing_tx,
            incoming_tx,
            deliver_tx,
            deliver_rx,
        }
    }
}

fn spawn_drain<T: Send + 'static>(mut rx: mpsc::Receiver<T>) {
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
}

fn spawn_outgoing_worker(
    mut rx: mpsc::Receiver<PendingOutgoing>,
    client: ShrinkClient,
    cache: Arc<Mutex<ShrinkCache>>,
    deliver: mpsc::Sender<ShrinkDeliver>,
    timeout: Duration,
    min_length: usize,
) {
    tokio::spawn(async move {
        // Process one at a time so outgoing IRC sends keep their
        // user-submitted order. Cache hits keep this from being slow.
        while let Some(pending) = rx.recv().await {
            let substituted_text = shrink_and_substitute(
                &client,
                &cache,
                &pending.original_text,
                timeout,
                min_length,
                false, // outgoing: no [host] hint
            )
            .await;
            let _ = deliver
                .send(ShrinkDeliver::Outgoing(OutgoingDeliver {
                    conn_id: pending.conn_id,
                    buffer_id: pending.buffer_id,
                    buffer_name: pending.buffer_name,
                    buffer_type: pending.buffer_type,
                    substituted_text,
                }))
                .await;
        }
    });
}

fn spawn_incoming_worker(
    mut rx: mpsc::Receiver<PendingIncoming>,
    client: ShrinkClient,
    cache: Arc<Mutex<ShrinkCache>>,
    deliver: mpsc::Sender<ShrinkDeliver>,
    timeout: Duration,
    min_length: usize,
) {
    tokio::spawn(async move {
        // Sequential within the worker — keeps per-channel order
        // intact for the typical case. A pathological busy channel
        // with many unique long URLs and a slow API would queue up
        // behind the slowest shortenings; that's an acceptable
        // trade-off (and `min_url_length` keeps the candidate set
        // small).
        while let Some(mut pending) = rx.recv().await {
            let substituted_text = shrink_and_substitute(
                &client,
                &cache,
                &pending.message.text,
                timeout,
                min_length,
                true, // incoming: add [host] hint
            )
            .await;
            pending.message.text = substituted_text;
            let _ = deliver
                .send(ShrinkDeliver::Incoming(IncomingDeliver {
                    buffer_id: pending.buffer_id,
                    message: pending.message,
                    activity_level: pending.activity_level,
                    push_to_mentions: pending.push_to_mentions,
                }))
                .await;
        }
    });
}

async fn shrink_and_substitute(
    client: &ShrinkClient,
    cache: &Mutex<ShrinkCache>,
    text: &str,
    timeout: Duration,
    min_length: usize,
    add_host_hint: bool,
) -> String {
    let urls = find_long_urls(text, min_length);
    if urls.is_empty() {
        return text.to_string();
    }
    let shortenings = resolve_shortenings(client, cache, urls, timeout).await;
    if shortenings.is_empty() {
        return text.to_string();
    }
    apply_substitutions(text, &shortenings, add_host_hint)
}

/// Resolve every URL: cache hit returns the stored shortening; miss
/// fires a parallel HTTP call. Returns shortenings in input order.
async fn resolve_shortenings(
    client: &ShrinkClient,
    cache: &Mutex<ShrinkCache>,
    urls: Vec<String>,
    timeout: Duration,
) -> Vec<UrlShortening> {
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

    let miss_futures = misses.iter().map(|(_, url)| client.shorten(url, timeout));
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

/// Substitute every `original` with `shortened` in `text`. Incoming
/// messages (`add_host_hint = true`) append `[host]` after each
/// shortened URL so the reader still sees the destination. Outgoing
/// messages skip the hint per spec (the sender already knew it).
fn apply_substitutions(text: &str, shortenings: &[UrlShortening], add_host_hint: bool) -> String {
    let mut out = text.to_string();
    for sh in shortenings {
        let replacement = if add_host_hint {
            let host = host_of(&sh.original).unwrap_or_default();
            if host.is_empty() {
                sh.shortened.clone()
            } else {
                format!("{} [{}]", sh.shortened, host)
            }
        } else {
            sh.shortened.clone()
        };
        out = out.replace(&sh.original, &replacement);
    }
    out
}

impl App {
    /// Drain one shrink-deliver action from the main-loop arm.
    pub(crate) fn apply_shrink_deliver(&mut self, deliver: ShrinkDeliver) {
        match deliver {
            ShrinkDeliver::Manual { buffer_id, display } => {
                if !self.state.buffers.contains_key(&buffer_id) {
                    return;
                }
                let prior = self.state.active_buffer_id.clone();
                self.state.active_buffer_id = Some(buffer_id);
                crate::commands::helpers::add_local_event(self, &display);
                self.state.active_buffer_id = prior;
            }
            ShrinkDeliver::Outgoing(out) => {
                self.send_outgoing_substituted(&out);
            }
            ShrinkDeliver::Incoming(inc) => {
                // Use the `_unshrunk` variant — text is already
                // substituted, taking the shrink path again would
                // loop forever (worker would push back to the
                // worker queue).
                self.state.add_message_with_activity_unshrunk(
                    &inc.buffer_id,
                    inc.message,
                    inc.activity_level,
                );
                // `push_to_mentions` intentionally unused on the
                // deferred path — the caller (handle_privmsg) ran
                // the inline mention-buffer push with the original
                // text. Documented trade-off: mentions buffer shows
                // the original URL; chat buffer shows shortened.
            }
        }
    }

    /// Final stage of the outgoing pipeline once shrink has returned.
    /// E2E re-encrypts the (possibly substituted) plaintext so peers
    /// receive the shortened form inside the ciphertext, and the
    /// local echo / IRC send mirror `handle_plain_message`'s
    /// non-shrink path with the now-shrunken text.
    fn send_outgoing_substituted(&mut self, out: &OutgoingDeliver) {
        if !self.irc_handles.contains_key(&out.conn_id) {
            return;
        }
        let Some((wire_lines, plain_echo)) = self.e2e_encrypt_or_passthrough(
            &out.buffer_name,
            &out.buffer_type,
            &out.substituted_text,
        ) else {
            return;
        };
        let echo_message_enabled = self
            .state
            .connections
            .get(&out.conn_id)
            .is_some_and(|c| c.enabled_caps.contains("echo-message"));
        let is_e2e_encrypted = wire_lines
            .first()
            .is_some_and(|w| w.starts_with("+RPE2E01"));
        // Re-borrow the sender for each wire — `send_privmsg` takes
        // `&self`, so this avoids any clone/ownership friction on
        // `IrcHandle`.
        for wire in wire_lines {
            let Some(handle) = self.irc_handles.get(&out.conn_id) else {
                return;
            };
            if handle.sender.send_privmsg(&out.buffer_name, &wire).is_err() {
                tracing::warn!(
                    conn_id = %out.conn_id,
                    target = %out.buffer_name,
                    "shrink: deferred outgoing send failed"
                );
                return;
            }
        }
        if !self.state.pending_e2e_sends.is_empty() {
            self.drain_pending_e2e_sends();
        }
        if !echo_message_enabled || is_e2e_encrypted {
            let nick = self
                .state
                .connections
                .get(&out.conn_id)
                .map(|c| c.nick.clone())
                .unwrap_or_default();
            let own_mode = self.state.nick_prefix(&out.buffer_id, &nick);
            let local_chunks = if plain_echo.len() <= crate::irc::MESSAGE_MAX_BYTES {
                vec![plain_echo]
            } else {
                crate::irc::split_irc_message(&plain_echo, crate::irc::MESSAGE_MAX_BYTES)
            };
            for chunk in local_chunks {
                let id = self.state.next_message_id();
                self.state.add_message(
                    &out.buffer_id,
                    Message {
                        id,
                        timestamp: chrono::Utc::now(),
                        message_type: crate::state::buffer::MessageType::Message,
                        nick: Some(nick.clone()),
                        nick_mode: own_mode.map(|c| c.to_string()),
                        text: chunk,
                        highlight: false,
                        event_key: None,
                        event_params: None,
                        log_msg_id: None,
                        log_ref_id: None,
                        tags: None,
                    },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shrink::UrlShortening;

    #[test]
    fn apply_substitutions_outgoing_no_host() {
        let text = "see https://a.com/long-url";
        let shorts = vec![UrlShortening {
            original: "https://a.com/long-url".into(),
            shortened: "https://shr.al/1".into(),
        }];
        assert_eq!(
            apply_substitutions(text, &shorts, false),
            "see https://shr.al/1"
        );
    }

    #[test]
    fn apply_substitutions_incoming_with_host() {
        let text = "see https://sklepinsekt.pl/p/foo for prusaki";
        let shorts = vec![UrlShortening {
            original: "https://sklepinsekt.pl/p/foo".into(),
            shortened: "https://shr.al/1".into(),
        }];
        assert_eq!(
            apply_substitutions(text, &shorts, true),
            "see https://shr.al/1 [sklepinsekt.pl] for prusaki"
        );
    }

    #[test]
    fn apply_substitutions_handles_multiple() {
        let text = "https://a.com/long and https://b.com/long";
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
        assert_eq!(
            apply_substitutions(text, &shorts, false),
            "https://shr.al/1 and https://shr.al/2"
        );
    }
}

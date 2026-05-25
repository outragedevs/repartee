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

use futures::FutureExt;
use parking_lot::Mutex;
use tokio::sync::mpsc;

use super::App;
use crate::shrink::{ShrinkCache, ShrinkClient, UrlShortening, host_of};
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
    /// Captured at dispatch — used by `send_outgoing_substituted`
    /// for the local echo so the deferred delivery shows the same
    /// nick / mode the wire was sent under.
    pub nick: String,
    pub own_mode: Option<char>,
    /// Captured Query peer_handle (None for channels / no-handle
    /// queries) — passed into e2e_encrypt_or_passthrough so a
    /// closed-buffer wait window cannot fall back to plaintext.
    pub peer_handle: Option<String>,
}

#[derive(Debug)]
pub struct IncomingDeliver {
    pub buffer_id: String,
    /// Message with `text` already substituted (or original on
    /// timeout / error).
    pub message: Message,
    pub activity_level: ActivityLevel,
    /// Reserved for the v2 fix of the mentions-buffer divergence
    /// (`docs/commands/shrink.md` "Known limitations"): when set, the
    /// deferred deliver would re-format and push the substituted text
    /// into `_mentions`. Always `false` in v1 — the inline push in
    /// `handle_privmsg` runs synchronously with the original URL.
    #[expect(
        dead_code,
        reason = "v2 hook for shrink-aware mentions buffer push"
    )]
    pub push_to_mentions: bool,
}

/// `handle_plain_message` posts this when the message qualifies for
/// outgoing shrink. The worker takes ownership, awaits shrink,
/// substitutes, then posts an `OutgoingDeliver` to the main loop.
///
/// Every field that the deliver path would otherwise re-resolve from
/// `state` is **captured at dispatch time** so a buffer close, /nick,
/// or +o/+v during the shrink wait can't break the eventual encrypt
/// + send + echo. This addresses three review findings at once:
///   * E2E peer_handle leak when the Query buffer is closed
///   * Local-echo nick/mode split-brain after `/nick` mid-wait
///   * Worker re-extracting URLs against a stale `min_url_length`
#[derive(Debug)]
pub struct PendingOutgoing {
    pub conn_id: String,
    pub buffer_id: String,
    pub buffer_name: String,
    pub buffer_type: BufferType,
    pub original_text: String,
    /// URLs pre-extracted at dispatch time using the CURRENT state's
    /// `shrink_min_url_length`. The worker uses these directly; it
    /// no longer calls `find_long_urls` itself, so a runtime change
    /// to `min_url_length` via `/set` can never be out of sync.
    pub urls: Vec<String>,
    /// Local-echo nick captured from the connection at dispatch
    /// time; used by `send_outgoing_substituted` instead of
    /// re-reading current state at deliver.
    pub nick: String,
    /// Local-echo mode prefix (e.g. `+`, `@`) captured from the
    /// channel's user list at dispatch time.
    pub own_mode: Option<char>,
    /// E2E peer handle for Query buffers, resolved at dispatch from
    /// `state.buffers[buffer_id].peer_handle`. `None` for channels
    /// (which key on the channel name) and for queries with no
    /// captured handle (E2E will fall through to its existing
    /// legacy-config refusal).
    pub peer_handle: Option<String>,
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
    /// URLs pre-extracted at dispatch (same fix as `PendingOutgoing`).
    pub urls: Vec<String>,
    /// Copied verbatim into `IncomingDeliver.push_to_mentions` (see
    /// its `#[expect(dead_code)]` for the v2 hook). The field IS
    /// read here; only the destination field is dead.
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
            );
            spawn_incoming_worker(
                incoming_rx,
                c.clone(),
                Arc::clone(&cache),
                deliver_tx.clone(),
                Duration::from_millis(cfg.incoming_timeout_ms),
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
) {
    tokio::spawn(async move {
        // Process one at a time so outgoing IRC sends keep their
        // user-submitted order. Cache hits keep this from being slow.
        // Each per-message body is wrapped in catch_unwind so a panic
        // in shorten / substitution / json parsing kills only the
        // current message — the worker keeps draining subsequent
        // ones rather than going Closed and forcing a restart for
        // the feature to recover.
        while let Some(pending) = rx.recv().await {
            let client_ref = &client;
            let cache_ref = &cache;
            let outcome = std::panic::AssertUnwindSafe(async move {
                let substituted_text = shrink_and_substitute(
                    client_ref,
                    cache_ref,
                    &pending.original_text,
                    &pending.urls,
                    timeout,
                    false,
                )
                .await;
                ShrinkDeliver::Outgoing(OutgoingDeliver {
                    conn_id: pending.conn_id,
                    buffer_id: pending.buffer_id,
                    buffer_name: pending.buffer_name,
                    buffer_type: pending.buffer_type,
                    substituted_text,
                    nick: pending.nick,
                    own_mode: pending.own_mode,
                    peer_handle: pending.peer_handle,
                })
            });
            // futures::FutureExt::catch_unwind wraps the future
            // so a panic becomes Err(payload) instead of unwinding
            // the worker task.
            match outcome.catch_unwind().await {
                Ok(deliver_msg) => {
                    let _ = deliver.send(deliver_msg).await;
                }
                Err(_) => {
                    tracing::error!(
                        "shrink: outgoing worker caught a panic on one \
                         message; dropping and continuing"
                    );
                }
            }
        }
    });
}

fn spawn_incoming_worker(
    mut rx: mpsc::Receiver<PendingIncoming>,
    client: ShrinkClient,
    cache: Arc<Mutex<ShrinkCache>>,
    deliver: mpsc::Sender<ShrinkDeliver>,
    timeout: Duration,
) {
    tokio::spawn(async move {
        // See spawn_outgoing_worker for the panic-isolation rationale.
        while let Some(mut pending) = rx.recv().await {
            let client_ref = &client;
            let cache_ref = &cache;
            let outcome = std::panic::AssertUnwindSafe(async move {
                let substituted_text = shrink_and_substitute(
                    client_ref,
                    cache_ref,
                    &pending.message.text,
                    &pending.urls,
                    timeout,
                    true,
                )
                .await;
                pending.message.text = substituted_text;
                ShrinkDeliver::Incoming(IncomingDeliver {
                    buffer_id: pending.buffer_id,
                    message: pending.message,
                    activity_level: pending.activity_level,
                    push_to_mentions: pending.push_to_mentions,
                })
            });
            match outcome.catch_unwind().await {
                Ok(deliver_msg) => {
                    let _ = deliver.send(deliver_msg).await;
                }
                Err(_) => {
                    tracing::error!(
                        "shrink: incoming worker caught a panic on one \
                         message; dropping and continuing"
                    );
                }
            }
        }
    });
}

/// Substitute URLs in `text`. URLs were pre-extracted at dispatch
/// time using the CURRENT state's `shrink_min_url_length` and passed
/// in via `urls`; the worker does NOT re-extract here. This is the
/// fix for the half-sync issue where lowering `min_url_length` at
/// runtime caused the gate to dispatch messages the worker would
/// then re-filter against the boot-time threshold and return
/// unmodified.
async fn shrink_and_substitute(
    client: &ShrinkClient,
    cache: &Mutex<ShrinkCache>,
    text: &str,
    urls: &[String],
    timeout: Duration,
    add_host_hint: bool,
) -> String {
    if urls.is_empty() {
        return text.to_string();
    }
    let shortenings = resolve_shortenings(client, cache, urls.to_vec(), timeout).await;
    if shortenings.is_empty() {
        return text.to_string();
    }
    apply_substitutions(text, &shortenings, add_host_hint)
}

/// Resolve every URL: cache hit returns the stored shortening; miss
/// fires an HTTP call. Misses are processed with a small concurrency
/// cap so a single chat message containing N URLs cannot burst N
/// parallel POSTs at the API — a markdown link list (20 URLs) used
/// to trip rate limits and burn quota for everyone. Per-message cap
/// of 4 in-flight matches the user-facing latency budget (most
/// shortenings complete inside the per-call timeout) while keeping
/// the API happy.
async fn resolve_shortenings(
    client: &ShrinkClient,
    cache: &Mutex<ShrinkCache>,
    urls: Vec<String>,
    timeout: Duration,
) -> Vec<UrlShortening> {
    /// Max concurrent shorten calls per message. Tuned for shr.al's
    /// observed throughput; safe upper bound for typical chat.
    const PER_MESSAGE_CONCURRENCY: usize = 4;

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

    // Process misses in chunks of PER_MESSAGE_CONCURRENCY. Each
    // chunk is run with `join_all` (Send-friendly future shape) and
    // chunks themselves are sequential, giving us a hard cap on
    // in-flight requests without the lifetime headaches that
    // `buffer_unordered` brings when borrowing `&Mutex` across await
    // points on `tokio::spawn`.
    let mut miss_results: Vec<(usize, Result<UrlShortening, crate::shrink::ShrinkError>)> =
        Vec::with_capacity(misses.len());
    for chunk in misses.chunks(PER_MESSAGE_CONCURRENCY) {
        let chunk_futures = chunk
            .iter()
            .map(|(idx, url)| async move { (*idx, client.shorten(url, timeout).await) });
        miss_results.extend(futures::future::join_all(chunk_futures).await);
    }

    let mut resolved: Vec<(usize, UrlShortening)> = hits;
    {
        let mut c = cache.lock();
        for (idx, res) in miss_results {
            if let Ok(sh) = res {
                c.insert(sh.original.clone(), sh.clone());
                resolved.push((idx, sh));
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
///
/// Substitutions are applied **longest-original-first** to defeat the
/// prefix-overlap trap: a plain `str::replace` would otherwise rewrite
/// the shorter URL inside the longer one and corrupt both. Example:
/// `https://x.com/abc` and `https://x.com/abc/def` in the same
/// message — without sorting, replacing `…/abc` first also rewrites
/// the inside of `…/abc/def`, leaving the longer URL mangled.
fn apply_substitutions(text: &str, shortenings: &[UrlShortening], add_host_hint: bool) -> String {
    let mut sorted: Vec<&UrlShortening> = shortenings.iter().collect();
    sorted.sort_by(|a, b| b.original.len().cmp(&a.original.len()));
    let mut out = text.to_string();
    for sh in sorted {
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
    ///
    /// Uses captured `out.nick` / `out.own_mode` / `out.peer_handle`
    /// instead of re-reading state — the user may have /nick'd,
    /// /closed the buffer, or gained/lost a channel mode during the
    /// shrink wait. Reading current state would produce a local echo
    /// inconsistent with what hit the wire (sent under the prior
    /// nick/mode) and could even leak plaintext for an E2E PM if
    /// the Query buffer is gone.
    fn send_outgoing_substituted(&mut self, out: &OutgoingDeliver) {
        // Surface IRC-down as an in-buffer error — old code silently
        // returned, losing the user's message with no UI feedback.
        if !self.irc_handles.contains_key(&out.conn_id) {
            self.deliver_outgoing_error(
                out,
                "Failed to send message — connection unavailable",
            );
            return;
        }
        let Some((wire_lines, plain_echo)) = self.e2e_encrypt_or_passthrough(
            &out.buffer_id,
            &out.buffer_name,
            &out.buffer_type,
            &out.substituted_text,
            out.peer_handle.as_deref(),
        ) else {
            // Mirror handle_plain_message's themed error event so
            // the user knows the PM didn't go out (no peer handle
            // yet). Without this the message vanished silently.
            self.deliver_outgoing_error(
                out,
                &format!(
                    "{err}[E2E] cannot encrypt PM without peer handle \
                     — wait for a message from them first{rst}",
                    err = crate::commands::types::C_ERR,
                    rst = crate::commands::types::C_RST,
                ),
            );
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
        let mut send_ok = true;
        for wire in wire_lines {
            let Some(handle) = self.irc_handles.get(&out.conn_id) else {
                self.deliver_outgoing_error(
                    out,
                    "Failed to send message — connection dropped",
                );
                send_ok = false;
                break;
            };
            if handle.sender.send_privmsg(&out.buffer_name, &wire).is_err() {
                tracing::warn!(
                    conn_id = %out.conn_id,
                    target = %out.buffer_name,
                    "shrink: deferred outgoing send failed"
                );
                self.deliver_outgoing_error(out, "Failed to send message");
                send_ok = false;
                break;
            }
        }
        // Only drain pending_e2e_sends on success. Inline
        // handle_plain_message returns early on send failure WITHOUT
        // draining, leaving REKEY NOTICEs queued for the next
        // successful send — match that policy here. Draining on
        // failure would flush REKEYs to peers whose session
        // assumes the triggering ciphertext arrived, producing
        // silent decrypt breakage on subsequent messages.
        if send_ok && !self.state.pending_e2e_sends.is_empty() {
            self.drain_pending_e2e_sends();
        }
        if !send_ok {
            return;
        }
        if !echo_message_enabled || is_e2e_encrypted {
            self.write_outgoing_local_echo(out, &plain_echo);
        }
    }

    /// Emit the local-echo chunks for a successfully-sent outgoing
    /// message. Uses the captured nick/own_mode from dispatch time
    /// so the displayed sender matches what peers saw. Skips the
    /// echo entirely (with a status-line diagnostic) when the
    /// destination buffer was closed during the shrink wait — the
    /// wire send already succeeded, the peer has the message, but
    /// our buffer is gone.
    fn write_outgoing_local_echo(&mut self, out: &OutgoingDeliver, plain_echo: &str) {
        if !self.state.buffers.contains_key(&out.buffer_id) {
            // Peers received the message but our buffer is gone.
            // Surface this in the active buffer so the user knows
            // the send went through (and won't retype).
            crate::commands::helpers::add_local_event(
                self,
                &format!(
                    "{dim}shrink: message to {target} sent (buffer was \
                     closed during shrink wait){rst}",
                    target = out.buffer_name,
                    dim = crate::commands::types::C_DIM,
                    rst = crate::commands::types::C_RST,
                ),
            );
            return;
        }
        let local_chunks = if plain_echo.len() <= crate::irc::MESSAGE_MAX_BYTES {
            vec![plain_echo.to_string()]
        } else {
            crate::irc::split_irc_message(plain_echo, crate::irc::MESSAGE_MAX_BYTES)
        };
        let nick_mode_str = out.own_mode.map(|c| c.to_string());
        for chunk in local_chunks {
            let id = self.state.next_message_id();
            self.state.add_message(
                &out.buffer_id,
                Message {
                    id,
                    timestamp: chrono::Utc::now(),
                    message_type: crate::state::buffer::MessageType::Message,
                    nick: Some(out.nick.clone()),
                    nick_mode: nick_mode_str.clone(),
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

    /// Route an outgoing-pipeline error event to the right buffer.
    /// Prefers the original destination buffer (out.buffer_id) when
    /// it still exists; falls back to the current active buffer
    /// (so the user always sees the error somewhere).
    fn deliver_outgoing_error(&mut self, out: &OutgoingDeliver, message: &str) {
        let target_buf = if self.state.buffers.contains_key(&out.buffer_id) {
            Some(out.buffer_id.clone())
        } else {
            self.state.active_buffer_id.clone()
        };
        let Some(buf_id) = target_buf else {
            tracing::warn!("shrink: outgoing error with no target buffer: {message}");
            return;
        };
        let prior = self.state.active_buffer_id.clone();
        self.state.active_buffer_id = Some(buf_id);
        crate::commands::helpers::add_local_event(self, message);
        self.state.active_buffer_id = prior;
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
    fn apply_substitutions_longest_first_defeats_prefix_overlap() {
        // Regression: naive str::replace in order would rewrite
        // /abc inside /abc/def, corrupting both URLs. Sorting by
        // original.len() descending replaces /abc/def first.
        let text = "see https://x.com/abc and https://x.com/abc/def";
        let shorts = vec![
            UrlShortening {
                original: "https://x.com/abc".into(),
                shortened: "https://shr.al/1".into(),
            },
            UrlShortening {
                original: "https://x.com/abc/def".into(),
                shortened: "https://shr.al/2".into(),
            },
        ];
        let out = apply_substitutions(text, &shorts, false);
        assert_eq!(out, "see https://shr.al/1 and https://shr.al/2");
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

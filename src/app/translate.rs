//! Glue between `src/translate/` and the `App` event loop.
//!
//! Shaped after `src/app/shrink.rs`, which solves the same problem for URL
//! shortening: a dispatch gate inside the `add_message` path, background
//! workers, and a deliver channel drained by the main `select!`. The
//! idioms carried over from there — capturing every state-dependent value
//! at dispatch time, per-message panic isolation, draining the queues when
//! the feature is off so `try_send` never backpressures — were each the fix
//! for a real review finding and are load-bearing.
//!
//! Two deliberate divergences from shrink:
//!
//! * **The incoming worker is concurrent, not serial.** Shrink processes
//!   one message at a time, which is fine because URLs are rare. Every line
//!   goes through translation, so a serial pipeline would make latency
//!   cumulative and build an unbounded backlog on a live channel. Display
//!   order is restored by the reorder queue in `src/translate/queue.rs`,
//!   not by serialising here.
//! * **A panicking backend still produces an outcome.** Shrink drops the
//!   message and moves on. Here a dropped outcome would leave its queue
//!   entry pending until the timeout fires, stalling everything behind it,
//!   so the panic is converted into `Untranslated`.
//!
//! The outgoing worker IS serial, exactly as shrink's is, and for the same
//! reason: it is what keeps two outgoing messages in submission order on
//! the wire.

use std::sync::Arc;

use futures::FutureExt;
use tokio::sync::{Semaphore, mpsc};

use crate::state::buffer::BufferType;
use crate::translate::backend::{SharedBackend, StubBackend};
use crate::translate::{TranslateOutcome, TranslateRequest, UntranslatedReason};

/// Everything needed to build one outgoing translation request.
///
/// A struct rather than seven positional parameters: the four `&str`s next
/// to each other were an easy place to transpose a buffer id and a buffer
/// name, and the compiler would not have noticed.
#[derive(Debug, Clone, Copy)]
pub struct OutgoingRequest<'a> {
    pub conn_id: &'a str,
    pub buffer_id: &'a str,
    pub buffer_name: &'a str,
    pub buffer_type: &'a BufferType,
    pub nick: &'a str,
    pub text: &'a str,
    /// `true` for `/me`; `text` is then the ACTION's inner prose.
    pub is_action: bool,
}

/// Outcome of the outgoing translation gate for one submitted line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutgoingTranslatePolicy {
    /// Not a translated buffer — carry on with the ordinary send.
    NotApplicable,
    /// Translate first. Nothing may reach the wire until the outcome is back.
    Translate,
    /// Cannot be translated and must not be sent as-is. The text goes back
    /// to the user with this reason.
    Refuse(&'static str),
}

/// An incoming line handed to the translation worker.
#[derive(Debug)]
pub struct PendingTranslate {
    pub buffer_id: String,
    pub req: TranslateRequest,
}

/// An outgoing message handed to the translation worker.
///
/// Every field the deliver path would otherwise re-resolve from `state` is
/// captured here at dispatch time, so a `/nick`, a `/close`, or a mode
/// change during the translation wait cannot produce a local echo
/// inconsistent with what reached the wire. See `PendingOutgoing` in
/// `shrink.rs` for the review findings that established this.
#[derive(Debug)]
pub struct PendingOutgoingTranslate {
    pub conn_id: String,
    pub buffer_id: String,
    pub buffer_name: String,
    pub buffer_type: BufferType,
    pub original_text: String,
    pub req: TranslateRequest,
    pub nick: String,
    pub own_mode: Option<char>,
    pub peer_handle: Option<String>,
    pub show_original: bool,
    /// `true` for `/me`. The request carries the ACTION's inner text —
    /// translating the `\\x01ACTION …\\x01` framing would corrupt the CTCP —
    /// and the deliver path re-wraps it.
    pub is_action: bool,
}

/// Posted by either worker, consumed by the main event loop.
#[derive(Debug)]
pub enum TranslateDeliver {
    /// An incoming line's outcome. The loop folds it into that buffer's
    /// reorder queue and releases whatever that unblocks.
    Incoming {
        buffer_id: String,
        outcome: TranslateOutcome,
    },
    /// An outgoing message's outcome. The loop runs the rest of the send
    /// pipeline — E2E encrypt, IRC send, local echo — or refuses.
    Outgoing(Box<OutgoingTranslateDeliver>),
}

#[derive(Debug)]
pub struct OutgoingTranslateDeliver {
    pub conn_id: String,
    pub buffer_id: String,
    pub buffer_name: String,
    pub buffer_type: BufferType,
    /// Kept for the local echo and for restoring the input line when the
    /// send is refused.
    pub original_text: String,
    pub outcome: TranslateOutcome,
    pub nick: String,
    pub own_mode: Option<char>,
    pub peer_handle: Option<String>,
    pub show_original: bool,
    /// `true` for `/me`. The request carries the ACTION's inner text —
    /// translating the `\\x01ACTION …\\x01` framing would corrupt the CTCP —
    /// and the deliver path re-wraps it.
    pub is_action: bool,
}

/// Channels and shared state the translation workers need. Built once in
/// `App::new` and kept alive for the App's lifetime.
pub struct TranslateRuntime {
    /// `None` when translation is disabled — the gates check this to decide
    /// between dispatching and delivering straight through.
    pub backend: Option<SharedBackend>,
    /// The incoming worker's concurrency limiter, shared so
    /// `/set translate.max_in_flight` can retune it without a restart.
    /// Exposed as a plain `/set` option, so it has to actually do something.
    pub in_flight: Arc<Semaphore>,
    pub incoming_tx: mpsc::Sender<PendingTranslate>,
    pub outgoing_tx: mpsc::Sender<PendingOutgoingTranslate>,
    pub deliver_tx: mpsc::Sender<TranslateDeliver>,
    pub deliver_rx: mpsc::Receiver<TranslateDeliver>,
}

impl TranslateRuntime {
    /// Build the runtime and spawn the workers.
    ///
    /// This branch ships the stub backend: the mechanism is what is being
    /// built, and the stub proves it end to end with no API key. Swapping
    /// in a real broker is a one-line change here.
    pub fn build(cfg: &crate::config::TranslateConfig) -> Self {
        let backend: Option<SharedBackend> = if cfg.enabled {
            Some(Arc::new(StubBackend::new(0, 0)))
        } else {
            None
        };
        Self::with_backend(backend, cfg)
    }

    /// Build with an explicit backend. Used by tests to inject a stub with
    /// jitter or injected failures.
    pub fn with_backend(backend: Option<SharedBackend>, cfg: &crate::config::TranslateConfig) -> Self {
        let (incoming_tx, incoming_rx) = mpsc::channel::<PendingTranslate>(1024);
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<PendingOutgoingTranslate>(256);
        let (deliver_tx, deliver_rx) = mpsc::channel::<TranslateDeliver>(1024);
        let in_flight = Arc::new(Semaphore::new(cfg.max_in_flight.max(1) as usize));

        if let Some(ref b) = backend {
            spawn_incoming_worker(
                incoming_rx,
                Arc::clone(b),
                deliver_tx.clone(),
                Arc::clone(&in_flight),
            );
            spawn_outgoing_worker(outgoing_rx, Arc::clone(b), deliver_tx.clone());
        } else {
            // Disabled: drain to nowhere so a `try_send` from the IRC or
            // input paths never backpressures.
            spawn_drain(incoming_rx);
            spawn_drain(outgoing_rx);
        }

        Self {
            backend,
            in_flight,
            incoming_tx,
            outgoing_tx,
            deliver_tx,
            deliver_rx,
        }
    }
}

fn spawn_drain<T: Send + 'static>(mut rx: mpsc::Receiver<T>) {
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
}

/// Translate one request, converting a panic into an `Untranslated`
/// outcome.
///
/// Every dispatched line MUST produce exactly one outcome. Dropping it —
/// which is what shrink does on panic — would leave the queue entry pending
/// until the timeout fires, stalling every line behind it for the full
/// budget.
async fn translate_isolated(backend: &SharedBackend, req: TranslateRequest) -> TranslateOutcome {
    let id = req.id;
    // The boundary has to cover BUILDING the future, not just polling it. A
    // backend that panics synchronously inside `translate()` — before it ever
    // returns its `BoxFuture` — would otherwise unwind past this: the
    // incoming task would produce no outcome at all (its queue entry sitting
    // pending until the timeout), and an outgoing lane would lose the
    // message outright.
    let fut = std::panic::AssertUnwindSafe(async { backend.translate(req).await });
    fut.catch_unwind().await.unwrap_or_else(|_| {
        tracing::error!(id, "translate: backend panicked on one line");
        TranslateOutcome::Untranslated {
            id,
            reason: UntranslatedReason::Error("backend panic".to_string()),
        }
    })
}

/// Concurrent up to `max_in_flight`. See the module docs for why this one
/// is not serial.
fn spawn_incoming_worker(
    mut rx: mpsc::Receiver<PendingTranslate>,
    backend: SharedBackend,
    deliver: mpsc::Sender<TranslateDeliver>,
    permits: Arc<Semaphore>,
) {
    tokio::spawn(async move {
        while let Some(pending) = rx.recv().await {
            let Ok(permit) = Arc::clone(&permits).acquire_owned().await else {
                break;
            };
            let backend = Arc::clone(&backend);
            let deliver = deliver.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let outcome = translate_isolated(&backend, pending.req).await;
                let _ = deliver
                    .send(TranslateDeliver::Incoming {
                        buffer_id: pending.buffer_id,
                        outcome,
                    })
                    .await;
            });
        }
    });
}

/// Fan out to one serial worker PER CONNECTION.
///
/// Serial within a connection is what keeps two outgoing messages in
/// submission order on the wire. Serial ACROSS connections would be
/// head-of-line blocking: one hung request on network A would hold up
/// translated sends on network B, which have nothing to do with it. The
/// design calls for a per-connection FIFO for exactly that reason.
///
/// The dispatcher never awaits a per-connection send. A connection whose
/// queue is full resolves as untranslated, so the deliver path refuses that
/// one message (fail-closed) instead of stalling every other connection
/// behind it.
fn spawn_outgoing_worker(
    mut rx: mpsc::Receiver<PendingOutgoingTranslate>,
    backend: SharedBackend,
    deliver: mpsc::Sender<TranslateDeliver>,
) {
    /// Per-connection queue depth. Deep enough that normal typing never
    /// trips it; shallow enough that a wedged provider surfaces quickly.
    const PER_CONNECTION_QUEUE: usize = 64;

    tokio::spawn(async move {
        let mut lanes: std::collections::HashMap<
            String,
            mpsc::Sender<PendingOutgoingTranslate>,
        > = std::collections::HashMap::new();

        while let Some(pending) = rx.recv().await {
            let conn_id = pending.conn_id.clone();
            let lane = lanes.entry(conn_id.clone()).or_insert_with(|| {
                let (tx, lane_rx) = mpsc::channel(PER_CONNECTION_QUEUE);
                spawn_connection_lane(lane_rx, Arc::clone(&backend), deliver.clone());
                tx
            });
            match lane.try_send(pending) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(p)) => {
                    tracing::warn!(
                        conn_id = %conn_id,
                        "translate: outgoing lane full, refusing this message"
                    );
                    refuse_outgoing(&deliver, p, "the translation queue for this connection is full")
                        .await;
                }
                Err(mpsc::error::TrySendError::Closed(p)) => {
                    // The lane died; drop it so the next message respawns one.
                    lanes.remove(&conn_id);
                    tracing::error!(conn_id = %conn_id, "translate: outgoing lane died");
                    refuse_outgoing(&deliver, p, "the translation worker died").await;
                }
            }
        }
    });
}

/// One connection's serial worker: submission order in, submission order out.
fn spawn_connection_lane(
    mut rx: mpsc::Receiver<PendingOutgoingTranslate>,
    backend: SharedBackend,
    deliver: mpsc::Sender<TranslateDeliver>,
) {
    tokio::spawn(async move {
        while let Some(pending) = rx.recv().await {
            let outcome = translate_isolated(&backend, pending.req).await;
            let _ = deliver
                .send(TranslateDeliver::Outgoing(Box::new(
                    OutgoingTranslateDeliver {
                        conn_id: pending.conn_id,
                        buffer_id: pending.buffer_id,
                        buffer_name: pending.buffer_name,
                        buffer_type: pending.buffer_type,
                        original_text: pending.original_text,
                        outcome,
                        nick: pending.nick,
                        own_mode: pending.own_mode,
                        peer_handle: pending.peer_handle,
                        show_original: pending.show_original,
                        is_action: pending.is_action,
                    },
                )))
                .await;
        }
    });
}

/// Report a dispatch-side failure as a normal untranslated outcome, so the
/// deliver path refuses the send and hands the text back rather than the
/// message vanishing between the queues.
async fn refuse_outgoing(
    deliver: &mpsc::Sender<TranslateDeliver>,
    pending: PendingOutgoingTranslate,
    reason: &str,
) {
    let _ = deliver
        .send(TranslateDeliver::Outgoing(Box::new(
            OutgoingTranslateDeliver {
                conn_id: pending.conn_id,
                buffer_id: pending.buffer_id,
                buffer_name: pending.buffer_name,
                buffer_type: pending.buffer_type,
                original_text: pending.original_text,
                outcome: TranslateOutcome::Untranslated {
                    id: pending.req.id,
                    reason: UntranslatedReason::Error(reason.to_string()),
                },
                nick: pending.nick,
                own_mode: pending.own_mode,
                peer_handle: pending.peer_handle,
                show_original: pending.show_original,
                is_action: pending.is_action,
            },
        )))
        .await;
}

impl crate::app::App {
    /// Drain one translation deliver from the main-loop arm.
    pub(crate) fn apply_translate_deliver(&mut self, deliver: TranslateDeliver) {
        match deliver {
            TranslateDeliver::Incoming { buffer_id, outcome } => {
                self.resolve_incoming_translation(&buffer_id, outcome);
            }
            TranslateDeliver::Outgoing(out) => {
                self.send_outgoing_translated(&out);
            }
        }
    }

    /// Fold an incoming outcome into its buffer's queue and release whatever
    /// that unblocks.
    fn resolve_incoming_translation(&mut self, buffer_id: &str, outcome: TranslateOutcome) {
        let ready = {
            let Some(queue) = self.state.translate_queues.get_mut(buffer_id) else {
                // The queue is gone — the buffer was closed, or a flush
                // already released this line. Expected, not an error.
                tracing::debug!(
                    buffer_id,
                    id = outcome.id(),
                    "translate: outcome for a buffer with no queue, dropped"
                );
                return;
            };
            let resolution = match outcome {
                TranslateOutcome::Translated { id, text } => (id, Ok(text)),
                TranslateOutcome::Untranslated { id, reason } => (id, Err(reason)),
            };
            if !queue.resolve(resolution.0, resolution.1) {
                tracing::debug!(
                    buffer_id,
                    id = resolution.0,
                    "translate: late outcome for an already-released line, dropped"
                );
            }
            queue.drain_ready()
        };
        self.release_translated(buffer_id, ready);
        self.prune_empty_translate_queues();
    }

    /// Expire timed-out entries and enforce the ceiling on every queue, then
    /// release whatever that unblocked.
    pub(crate) fn tick_translate_queues(&mut self) {
        if self.state.translate_queues.is_empty() {
            return;
        }
        let timeout = std::time::Duration::from_millis(self.config.translate.timeout_ms);
        let max_queue = self.config.translate.max_queue.max(1) as usize;
        let now = std::time::Instant::now();
        let buffer_ids: Vec<String> = self.state.translate_queues.keys().cloned().collect();
        for buffer_id in buffer_ids {
            let ready = {
                let Some(queue) = self.state.translate_queues.get_mut(&buffer_id) else {
                    continue;
                };
                let expired = queue.expire(now, timeout);
                let forced = queue.enforce_ceiling(max_queue);
                if expired > 0 || forced > 0 {
                    tracing::debug!(
                        buffer_id = %buffer_id,
                        expired,
                        forced,
                        "translate: released lines untranslated"
                    );
                }
                queue.drain_ready()
            };
            self.release_translated(&buffer_id, ready);
        }
        self.prune_empty_translate_queues();
    }

    /// Deliver cleared lines to the buffer.
    ///
    /// Calls the `_unshrunk` variant deliberately: the text is final, and
    /// re-entering `add_message` would hand a translated line straight back
    /// to the translation gate. Translation and shrink are mutually
    /// exclusive per line by design — two external round-trips on one line
    /// is worse than losing shrink on translated buffers.
    pub(crate) fn release_translated(
        &mut self,
        buffer_id: &str,
        ready: Vec<crate::translate::queue::ReadyEntry>,
    ) {
        for entry in ready {
            if let Some(reason) = entry.reason.as_ref().filter(|r| r.is_gap()) {
                tracing::debug!(
                    buffer_id,
                    id = entry.id,
                    reason = %reason.label(),
                    "translate: line delivered untranslated"
                );
            }
            self.state.add_message_with_activity_unshrunk(
                buffer_id,
                entry.message,
                entry.activity,
            );
        }
    }

    /// Final stage of the outgoing pipeline once translation has returned.
    ///
    /// Uses the captured `nick` / `own_mode` / `peer_handle` rather than
    /// re-reading state: the user may have `/nick`ed, closed the buffer, or
    /// gained a channel mode during the wait, and reading current state
    /// would produce a local echo inconsistent with what hit the wire — or
    /// leak plaintext for an E2E PM whose Query buffer is gone. Same
    /// reasoning as `send_outgoing_substituted` in `shrink.rs`.
    fn send_outgoing_translated(&mut self, out: &OutgoingTranslateDeliver) {
        // Re-wrap here rather than translating the framing: the request
        // carried the ACTION's inner text, because `\x01ACTION …\x01` handed
        // to a translator comes back as anything but a valid CTCP.
        let wrap = |body: &str| {
            if out.is_action {
                format!("\x01ACTION {body}\x01")
            } else {
                body.to_string()
            }
        };
        let wire_text = match &out.outcome {
            TranslateOutcome::Translated { text, .. } => wrap(text),
            // The broker decided this line needed no translation, so the
            // original IS the correct thing to send.
            TranslateOutcome::Untranslated { reason, .. } if !reason.is_gap() => {
                wrap(&out.original_text)
            }
            // Everything else is a genuine gap. Do NOT fall back to sending
            // the original: the user asked for this channel to be written in
            // another language, and shipping their untranslated text is
            // sending something other than what they intended. Fail closed,
            // hand the text back, and let them decide.
            TranslateOutcome::Untranslated { reason, .. } => {
                let reason_label = reason.label();
                self.restore_outgoing_input(out, &reason_label);
                return;
            }
        };

        if !self.irc_handles.contains_key(&out.conn_id) {
            self.deliver_translate_error(
                &out.buffer_id,
                "Failed to send message — connection unavailable",
            );
            return;
        }

        let (wire_lines, plain_echo) = match self.state.e2e_encrypt_or_passthrough(
            &out.buffer_id,
            &out.buffer_name,
            &out.buffer_type,
            &wire_text,
            out.peer_handle.as_deref(),
        ) {
            Ok(v) => v,
            Err(reason) => {
                self.deliver_translate_error(&out.buffer_id, &reason.user_message());
                return;
            }
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
        let mut sent_any = false;
        for wire in wire_lines {
            let Some(handle) = self.irc_handles.get(&out.conn_id) else {
                self.deliver_translate_error(
                    &out.buffer_id,
                    "Failed to send message — connection dropped",
                );
                send_ok = false;
                break;
            };
            if handle.sender().send_privmsg(&out.buffer_name, &wire).is_err() {
                tracing::warn!(
                    conn_id = %out.conn_id,
                    target = %out.buffer_name,
                    "translate: deferred outgoing send failed"
                );
                self.deliver_translate_error(&out.buffer_id, "Failed to send message");
                send_ok = false;
                break;
            }
            sent_any = true;
        }
        if sent_any {
            self.note_message_sent(&out.buffer_id);
        }
        // Only drain on success — matching handle_plain_message, which
        // leaves REKEY NOTICEs queued for the next successful send rather
        // than flushing them to peers whose session assumes the triggering
        // ciphertext arrived.
        if send_ok && !self.state.pending_e2e_sends.is_empty() {
            self.drain_pending_e2e_sends();
        }
        if !send_ok {
            return;
        }
        if !echo_message_enabled || is_e2e_encrypted {
            self.write_translated_local_echo(out, &plain_echo);
        }
    }

    /// Assemble the outgoing translation request, capturing every
    /// state-dependent value now.
    ///
    /// The user may `/nick`, close the buffer, or gain a channel mode during
    /// the wait; reading current state at deliver time would produce a local
    /// echo inconsistent with what hit the wire, and could strand the E2E
    /// peer handle for a closed Query. Returns `None` when the buffer is
    /// gone, in which case the caller falls through to the synchronous send.
    pub(crate) fn build_outgoing_translate(
        &mut self,
        req: &OutgoingRequest<'_>,
    ) -> Option<PendingOutgoingTranslate> {
        let OutgoingRequest {
            conn_id,
            buffer_id,
            buffer_name,
            buffer_type,
            nick,
            text,
            is_action,
        } = *req;
        // Belt and braces. `handle_plain_message` already refuses to reach
        // this function when E2E cannot be ruled out, but this is the point
        // where cleartext becomes a payload bound for a third party, and
        // this function is reachable from anywhere in the crate. A caller
        // that forgets the gate must not be able to leak; re-checking here
        // makes the refusal a property of the function rather than of its
        // call sites. Fail-closed predicate, never the advisory one.
        if self.state.e2e_possible_for_target(conn_id, buffer_name) {
            tracing::warn!(
                target = %buffer_name,
                "translate: refused an outgoing request for a possibly-E2E target"
            );
            return None;
        }
        let buffer = self.state.buffers.get(buffer_id)?;
        let known_nicks: Vec<String> = buffer.users.keys().cloned().collect();
        let network = self
            .state
            .connections
            .get(conn_id)
            .map(|c| c.label.clone())
            .unwrap_or_default();
        let captured_nick = self
            .state
            .connections
            .get(conn_id)
            .map_or_else(|| nick.to_string(), |c| c.nick.clone());
        let captured_own_mode = self.state.nick_prefix(buffer_id, &captured_nick);
        // Resolve the FULL peer handle now, while the buffer still exists —
        // a `/close` during the wait would otherwise leave
        // `e2e_encrypt_or_passthrough` unable to recover the network and
        // fall through to plaintext for an E2E-enabled DM.
        let captured_peer_handle = if *buffer_type == BufferType::Query {
            self.state
                .resolve_query_peer_handle(buffer_id, buffer_name)
                .unwrap_or_else(|e| {
                    tracing::warn!("e2e: failed to resolve DM peer handle for {buffer_name}: {e}");
                    None
                })
        } else {
            None
        };
        // Same resolver as the incoming path, asked for the other
        // direction — the pair is never read per-direction off the config.
        // `outgoing()` returns None when the buffer has no language, because
        // a TARGET cannot be autodetected: there is nothing to detect which
        // language to write in from. Refusing lets the caller fall through
        // to sending untranslated rather than guessing.
        let Some((source_lang, target_lang)) = crate::translate::resolve_langs(
            self.config.translate.buffers.get(buffer_id),
            &self.config.translate.my_lang,
        )
        .outgoing() else {
            tracing::warn!(
                buffer_id,
                "translate: outgoing needs the buffer's language; \
                 set it with /translate addout <target> <lang>"
            );
            return None;
        };

        let id = self.state.next_message_id();
        Some(PendingOutgoingTranslate {
            conn_id: conn_id.to_string(),
            buffer_id: buffer_id.to_string(),
            buffer_name: buffer_name.to_string(),
            buffer_type: buffer_type.clone(),
            original_text: text.to_string(),
            req: TranslateRequest {
                id,
                direction: crate::translate::Direction::Outgoing,
                network,
                target: buffer_name.to_string(),
                nick: captured_nick.clone(),
                text: text.to_string(),
                source_lang,
                target_lang,
                known_nicks,
            },
            nick: captured_nick,
            own_mode: captured_own_mode,
            peer_handle: captured_peer_handle,
            show_original: self.config.translate.show_original_out,
            is_action,
        })
    }

    /// What the outgoing gate has decided about one submitted line.
    ///
    /// Extracted so the whole outgoing policy is one readable, testable
    /// statement rather than a chain of conditions inside the submit path.
    /// Every way of NOT completing a required translation has to refuse, and
    /// that is much easier to check when the options are enumerated.
    pub(crate) fn outgoing_translate_policy(
        &self,
        buffer_id: &str,
        text: &str,
        e2e_possible: bool,
    ) -> OutgoingTranslatePolicy {
        // E2E wins: the conversation is simply not translated, and the
        // ordinary (encrypted) send is exactly right.
        if e2e_possible || !self.state.translate_active {
            return OutgoingTranslatePolicy::NotApplicable;
        }
        let Some(cfg) = self.config.translate.buffers.get(buffer_id) else {
            return OutgoingTranslatePolicy::NotApplicable;
        };
        if !cfg.outgoing {
            return OutgoingTranslatePolicy::NotApplicable;
        }
        // From here translation is REQUIRED, so everything below refuses
        // rather than falling through to a plaintext send. Putting text on a
        // channel in a language the user did not choose is the failure the
        // strict outgoing rule exists to prevent, and one they cannot see
        // happen.
        // These two are exactly `multiline::needs_multiline`, split apart so
        // the refusal names the real reason: a long single-line paste is not
        // a "multi-line message", and telling the user it is would send them
        // looking for a newline that is not there.
        if text.contains('\n') {
            return OutgoingTranslatePolicy::Refuse("multi-line messages cannot be translated");
        }
        if text.len() > crate::irc::MESSAGE_MAX_BYTES {
            return OutgoingTranslatePolicy::Refuse(
                "message is too long to translate in one piece",
            );
        }
        if cfg.lang.is_none() {
            return OutgoingTranslatePolicy::Refuse(
                "no target language set for this buffer — \
                 /translate addout <target> <lang>",
            );
        }
        OutgoingTranslatePolicy::Translate
    }

    /// Apply the outgoing translation policy to a send addressed by TARGET
    /// NAME — `/msg`, `/query <nick> <text>`, `/me`, and the script senders.
    ///
    /// Returns `Some(result)` when translation took the send over (dispatched
    /// or refused) and `None` when the caller should carry on with the
    /// ordinary send.
    ///
    /// Gating here rather than at each call site is what stops the per-buffer
    /// `addout` setting depending on HOW the message was submitted — typing
    /// it in the buffer would translate, `/msg`ing the same text would not,
    /// and nothing would say so.
    pub(crate) fn gate_by_target_translation(
        &mut self,
        conn_id: &str,
        target: &str,
        wire_text: &str,
    ) -> Option<bool> {
        let body = crate::app::e2e_gate::translatable_outgoing_body(wire_text)?;
        let buffer_id = crate::state::buffer::make_buffer_id(conn_id, target);
        let e2e_possible = self.state.e2e_possible_for_target(conn_id, target);
        match self.outgoing_translate_policy(&buffer_id, body, e2e_possible) {
            OutgoingTranslatePolicy::NotApplicable => None,
            OutgoingTranslatePolicy::Refuse(reason) => {
                Some(self.refuse_untranslatable_send(body, reason))
            }
            OutgoingTranslatePolicy::Translate => {
                // A shorter body than the wire means the framing was
                // stripped, i.e. this is an ACTION.
                let is_action = body.len() != wire_text.len();
                Some(self.dispatch_by_target_translation(
                    conn_id, &buffer_id, target, body, is_action,
                ))
            }
        }
    }

    /// Dispatch a by-target send (`/msg`, `/query`, `/me`, a script) for
    /// translation. Returns `false` — nothing is on the wire yet.
    ///
    /// Failure here refuses, exactly as the buffer-input path does. A
    /// by-target send is not a lesser send: publishing it untranslated puts
    /// the same wrong-language text on the same channel.
    pub(crate) fn dispatch_by_target_translation(
        &mut self,
        conn_id: &str,
        buffer_id: &str,
        target: &str,
        body: &str,
        is_action: bool,
    ) -> bool {
        let buffer_type = if crate::e2e::is_channel_target(target) {
            BufferType::Channel
        } else {
            BufferType::Query
        };
        let nick = self
            .state
            .connections
            .get(conn_id)
            .map(|c| c.nick.clone())
            .unwrap_or_default();
        let Some(pending) = self.build_outgoing_translate(&OutgoingRequest {
            conn_id,
            buffer_id,
            buffer_name: target,
            buffer_type: &buffer_type,
            nick: &nick,
            text: body,
            is_action,
        }) else {
            return self.refuse_untranslatable_send(
                body,
                "this conversation can no longer be translated",
            );
        };
        match self.translate_outgoing_tx.try_send(pending) {
            Ok(()) => false,
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                self.refuse_untranslatable_send(body, "the translation queue is full")
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => self
                .refuse_untranslatable_send(
                    body,
                    "the translation worker has died — restart to restore it",
                ),
        }
    }

    /// Refuse an outgoing send that cannot be translated, before anything
    /// reaches the wire.
    ///
    /// Always returns `false` (nothing was sent) so submit paths can
    /// `return` it directly.
    pub(crate) fn refuse_untranslatable_send(&mut self, text: &str, reason: &str) -> bool {
        crate::commands::helpers::add_local_event(
            self,
            &format!(
                "{err}Not sent — {reason}.{rst} {dim}Your text is back in the \
                 input line; /translate delout this buffer to send it as-is.{rst}",
                err = crate::commands::types::C_ERR,
                dim = crate::commands::types::C_DIM,
                rst = crate::commands::types::C_RST,
            ),
        );
        self.restore_input_text(text);
        false
    }

    /// Put a refused message back in the input line.
    ///
    /// Only into an EMPTY input: the user may have typed something else
    /// while a deferred send was in flight, and clobbering that would be a
    /// second, worse surprise. The text is still visible in the error row
    /// above either way, so it is never truly lost.
    pub(crate) fn restore_input_text(&mut self, text: &str) {
        if self.input.value.is_empty() {
            self.input.value = text.to_string();
            self.input.cursor_pos = self.input.value.chars().count();
        }
    }

    /// Hand a refused message back to the user instead of losing it.
    fn restore_outgoing_input(&mut self, out: &OutgoingTranslateDeliver, reason: &str) {
        self.deliver_translate_error(
            &out.buffer_id,
            &format!(
                "Not sent — translation failed ({reason}). Your text is back in the input line."
            ),
        );
        let original = out.original_text.clone();
        self.restore_input_text(&original);
    }

    /// Emit the local echo for a successfully sent translated message.
    fn write_translated_local_echo(&mut self, out: &OutgoingTranslateDeliver, plain_echo: &str) {
        if !self.state.buffers.contains_key(&out.buffer_id) {
            crate::commands::helpers::add_local_event(
                self,
                &format!(
                    "{dim}translate: message to {target} sent (buffer was \
                     closed during the translation wait){rst}",
                    target = out.buffer_name,
                    dim = crate::commands::types::C_DIM,
                    rst = crate::commands::types::C_RST,
                ),
            );
            return;
        }
        // An action echoes as its inner text under `MessageType::Action`;
        // showing the raw `\x01ACTION …\x01` would render the framing.
        let echo_body = if out.is_action {
            plain_echo
                .strip_prefix('\x01')
                .and_then(|t| t.strip_suffix('\x01'))
                .and_then(|t| t.strip_prefix("ACTION "))
                .unwrap_or(plain_echo)
        } else {
            plain_echo
        };
        let (echo_text, orig_offset) = if matches!(out.outcome, TranslateOutcome::Translated { .. })
        {
            crate::translate::compose_display(echo_body, &out.original_text, out.show_original)
        } else {
            (echo_body.to_string(), None)
        };
        let local_chunks = if echo_text.len() <= crate::irc::MESSAGE_MAX_BYTES {
            vec![echo_text]
        } else {
            crate::irc::split_irc_message(&echo_text, crate::irc::MESSAGE_MAX_BYTES)
        };
        let nick_mode_str = out.own_mode.map(|c| c.to_string());
        // Only a single-chunk echo can carry the offset: splitting moves the
        // suffix into the last chunk and the byte offset no longer maps.
        let single_chunk = local_chunks.len() == 1;
        for chunk in local_chunks {
            let id = self.state.next_message_id();
            // `add_own_message`, not `add_message`: this echo carries the
            // nick captured at dispatch, so a `/nick` during the wait would
            // make the dispatch gate mistake it for someone else's line and
            // translate our own message a second time.
            self.state.add_own_message(
                &out.buffer_id,
                crate::state::buffer::Message {
                    id,
                    timestamp: chrono::Utc::now(),
                    message_type: if out.is_action {
                        crate::state::buffer::MessageType::Action
                    } else {
                        crate::state::buffer::MessageType::Message
                    },
                    nick: Some(out.nick.clone()),
                    nick_mode: nick_mode_str.clone(),
                    text: chunk,
                    highlight: false,
                    event_key: None,
                    event_params: None,
                    log_msg_id: None,
                    log_ref_id: None,
                    tags: None,
                    orig_offset: if single_chunk { orig_offset } else { None },
                },
            );
        }
    }

    /// Route a translation-pipeline error to the right buffer, falling back
    /// to the active one when the destination is gone.
    fn deliver_translate_error(&mut self, buffer_id: &str, message: &str) {
        let target = if self.state.buffers.contains_key(buffer_id) {
            Some(buffer_id.to_string())
        } else {
            self.state.active_buffer_id.clone()
        };
        let Some(buf_id) = target else {
            tracing::warn!("translate: outgoing error with no target buffer: {message}");
            return;
        };
        let prior = self.state.active_buffer_id.clone();
        self.state.active_buffer_id = Some(buf_id);
        crate::commands::helpers::add_local_event(self, message);
        self.state.active_buffer_id = prior;
    }

    /// Re-derive every translate mirror on `AppState` from the config.
    ///
    /// Idempotent by design — it re-derives rather than undoing a specific
    /// switch — so `/set`, `/translate addin|delin`, and `/reload` can all
    /// call the same function and never drift apart. A per-key arm is
    /// exactly what let `/reload` fall out of step with `/set` for typing.
    ///
    /// `translate_active` stays false when no backend was built at startup:
    /// the worker queues are bound in `App::new`, so flipping the switch at
    /// runtime cannot materialise one.
    pub(crate) fn sync_translate_from_config(&mut self) {
        let has_backend = self.translate_backend.is_some();
        self.state.translate_active = self.config.translate.enabled && has_backend;
        self.state
            .translate_buffers
            .clone_from(&self.config.translate.buffers);
        self.state
            .translate_my_lang
            .clone_from(&self.config.translate.my_lang);
        self.state.translate_show_original_in = self.config.translate.show_original_in;
        self.retune_translate_concurrency();
    }

    /// Apply `translate.max_in_flight` to the running incoming worker.
    ///
    /// The limiter is built once at startup, so without this the setting is
    /// accepted, persisted, and silently ignored — worse than not offering
    /// it. `Semaphore` can only be nudged by a delta, so the last applied
    /// value is tracked alongside it.
    fn retune_translate_concurrency(&mut self) {
        let Some(limiter) = self.translate_in_flight.as_ref() else {
            return;
        };
        let want = self.config.translate.max_in_flight.max(1) as usize;
        let have = self.translate_in_flight_applied;
        if want == have {
            return;
        }
        if want > have {
            limiter.add_permits(want - have);
            self.translate_in_flight_applied = want;
            self.translate_in_flight_debt = 0;
        } else {
            // `forget_permits` can only take permits that are AVAILABLE, and
            // returns how many it actually took — possibly zero when every
            // permit is checked out. Recording the requested value regardless
            // would be wrong twice over: the old concurrency would come back
            // as in-flight work returns its permits, and the next retune
            // would compute its delta from a baseline that never existed.
            let forgotten = limiter.forget_permits(have - want);
            self.translate_in_flight_applied = have - forgotten;
            self.translate_in_flight_debt = (have - want) - forgotten;
        }
        tracing::info!(
            from = have,
            to = want,
            applied = self.translate_in_flight_applied,
            "translate: concurrency retuned"
        );
    }

    /// Collect a concurrency reduction that could not be applied at once.
    ///
    /// Permits held by in-flight requests cannot be forgotten until they come
    /// back, so the outstanding remainder is retried from the tick. Without
    /// this a lowered `max_in_flight` silently reverts as soon as the current
    /// batch finishes.
    pub(crate) fn settle_translate_concurrency_debt(&mut self) {
        if self.translate_in_flight_debt == 0 {
            return;
        }
        let Some(limiter) = self.translate_in_flight.as_ref() else {
            return;
        };
        let forgotten = limiter.forget_permits(self.translate_in_flight_debt);
        if forgotten == 0 {
            return;
        }
        self.translate_in_flight_debt -= forgotten;
        self.translate_in_flight_applied -= forgotten;
        tracing::debug!(
            forgotten,
            remaining = self.translate_in_flight_debt,
            "translate: settled part of a concurrency reduction"
        );
    }

    /// Drop queues that have fully drained, so the tick has nothing to walk
    /// on an idle client.
    fn prune_empty_translate_queues(&mut self) {
        self.state
            .translate_queues
            .retain(|_, queue| !queue.is_empty());
    }

    /// Release every queued line for one buffer, untranslated where still
    /// pending, and drop its queue.
    #[allow(dead_code, reason = "called once the flush points are wired")]
    ///
    /// Called on buffer close, `/part`, disconnect, quit, detach, and
    /// `/translate delin`. Pending lines are released rather than dropped —
    /// the user already saw them arrive on the network, and losing them
    /// silently would be worse than showing them untranslated.
    pub(crate) fn flush_translate_queue(&mut self, buffer_id: &str) {
        let Some(mut queue) = self.state.translate_queues.remove(buffer_id) else {
            return;
        };
        let ready = queue.flush_all();
        if !ready.is_empty() {
            tracing::debug!(
                buffer_id,
                count = ready.len(),
                "translate: flushed queued lines"
            );
        }
        self.release_translated(buffer_id, ready);
    }

    /// Flush every buffer's queue. Used on quit and detach.
    #[allow(dead_code, reason = "called once the flush points are wired")]
    pub(crate) fn flush_all_translate_queues(&mut self) {
        let buffer_ids: Vec<String> = self.state.translate_queues.keys().cloned().collect();
        for buffer_id in buffer_ids {
            self.flush_translate_queue(&buffer_id);
        }
    }
}

#[cfg(test)]
mod app_tests {
    use super::*;
    use crate::state::buffer::{ActivityLevel, Buffer, BufferType, Message, MessageType};
    use crate::translate::TranslateOutcome;
    use crate::app::input::submit_typing_tests::test_app;
    use crate::translate::queue::{PendingPayload, TranslateQueue};

    const BUF: &str = "test/#dupa";

    fn message(id: u64, text: &str) -> Message {
        Message {
            id,
            timestamp: chrono::Utc::now(),
            message_type: MessageType::Message,
            nick: Some("alice".to_string()),
            nick_mode: None,
            text: text.to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
            orig_offset: None,
        }
    }

    fn payload(id: u64, text: &str) -> PendingPayload {
        PendingPayload {
            message: message(id, text),
            activity: ActivityLevel::Activity,
            show_original: false,
        }
    }

    /// App with one channel buffer and `count` lines queued for translation.
    fn app_with_queue(count: u64) -> crate::app::App {
        let mut app = test_app();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Channel, "#dupa"));
        let mut queue = TranslateQueue::new();
        for id in 1..=count {
            queue.push_pending(id, format!("line {id}"), payload(id, &format!("line {id}")));
        }
        app.state.translate_queues.insert(BUF.to_string(), queue);
        app
    }

    fn shown(app: &crate::app::App) -> Vec<String> {
        app.state.buffers[BUF]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect()
    }

    #[test]
    fn an_outcome_releases_only_up_to_the_first_pending_line() {
        let mut app = app_with_queue(3);
        app.apply_translate_deliver(TranslateDeliver::Incoming {
            buffer_id: BUF.to_string(),
            outcome: TranslateOutcome::Translated {
                id: 2,
                text: "second".to_string(),
            },
        });
        assert!(
            shown(&app).is_empty(),
            "line 2 must wait for line 1 even though it finished first"
        );
        app.apply_translate_deliver(TranslateDeliver::Incoming {
            buffer_id: BUF.to_string(),
            outcome: TranslateOutcome::Translated {
                id: 1,
                text: "first".to_string(),
            },
        });
        assert_eq!(
            shown(&app),
            vec!["first".to_string(), "second".to_string()],
            "both release, in arrival order"
        );
    }

    #[test]
    fn a_drained_queue_is_pruned() {
        let mut app = app_with_queue(1);
        app.apply_translate_deliver(TranslateDeliver::Incoming {
            buffer_id: BUF.to_string(),
            outcome: TranslateOutcome::Translated {
                id: 1,
                text: "x".to_string(),
            },
        });
        assert!(
            !app.state.translate_queues.contains_key(BUF),
            "an empty queue must not linger for the tick to walk"
        );
    }

    #[test]
    fn an_outcome_for_a_vanished_queue_is_dropped_without_panicking() {
        let mut app = test_app();
        app.apply_translate_deliver(TranslateDeliver::Incoming {
            buffer_id: "test/#gone".to_string(),
            outcome: TranslateOutcome::Translated {
                id: 1,
                text: "x".to_string(),
            },
        });
    }

    #[test]
    fn the_tick_releases_a_stuck_head_after_the_timeout() {
        let mut app = app_with_queue(2);
        app.config.translate.timeout_ms = 0;
        app.tick_translate_queues();
        assert_eq!(
            shown(&app),
            vec![
                "line 1 [untranslated: timeout]".to_string(),
                "line 2 [untranslated: timeout]".to_string(),
            ],
            "a timed-out line shows its original, marked, in order"
        );
        assert!(!app.state.translate_queues.contains_key(BUF));
    }

    #[test]
    fn the_tick_enforces_the_ceiling() {
        let mut app = app_with_queue(10);
        // Long timeout, so only the ceiling can release anything.
        app.config.translate.timeout_ms = 600_000;
        app.config.translate.max_queue = 4;
        app.tick_translate_queues();
        assert_eq!(shown(&app).len(), 6, "the six oldest are forced out");
        assert_eq!(
            app.state.translate_queues[BUF].len(),
            4,
            "the queue is bounded afterwards"
        );
    }

    #[test]
    fn the_tick_is_a_no_op_with_no_queues() {
        let mut app = test_app();
        app.tick_translate_queues();
        assert!(app.state.translate_queues.is_empty());
    }

    fn outgoing(text: &str, outcome: TranslateOutcome, show_original: bool) -> OutgoingTranslateDeliver {
        OutgoingTranslateDeliver {
            conn_id: "test".to_string(),
            buffer_id: BUF.to_string(),
            buffer_name: "#dupa".to_string(),
            buffer_type: BufferType::Channel,
            original_text: text.to_string(),
            outcome,
            nick: "me".to_string(),
            own_mode: None,
            peer_handle: None,
            show_original,
            is_action: false,
        }
    }

    fn app_with_buffer() -> crate::app::App {
        let mut app = test_app();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Channel, "#dupa"));
        app
    }

    #[test]
    fn a_failed_outgoing_translation_sends_nothing_and_returns_the_text() {
        // Sending the original would transmit something other than what the
        // user intended for this channel. Fail closed and hand it back.
        let mut app = app_with_buffer();
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(outgoing(
            "moje zdanie",
            TranslateOutcome::Untranslated {
                id: 1,
                reason: crate::translate::UntranslatedReason::NoProvider,
            },
            false,
        ))));

        assert_eq!(
            app.input.value, "moje zdanie",
            "the text is back in the input line"
        );
        let last = app.state.buffers[BUF]
            .messages
            .back()
            .expect("an error row explains why");
        assert!(
            last.text.contains("no provider"),
            "the reason is named: {}",
            last.text
        );
    }

    #[test]
    fn a_failed_outgoing_translation_does_not_clobber_new_typing() {
        // The user may have typed something else during the wait; replacing
        // it would be a second, worse surprise. The text is still visible in
        // the error row, so it is never truly lost.
        let mut app = app_with_buffer();
        app.input.value = "something else".to_string();
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(outgoing(
            "moje zdanie",
            TranslateOutcome::Untranslated {
                id: 1,
                reason: crate::translate::UntranslatedReason::Timeout,
            },
            false,
        ))));
        assert_eq!(app.input.value, "something else");
    }

    #[test]
    fn a_filtered_outgoing_line_is_sent_as_the_original() {
        // Filtered means the broker correctly decided no translation was
        // needed, so the original IS the right thing to send. With no IRC
        // handle the send fails, which is exactly what proves we got as far
        // as attempting it rather than refusing up front.
        let mut app = app_with_buffer();
        app.apply_translate_deliver(TranslateDeliver::Outgoing(Box::new(outgoing(
            "moin",
            TranslateOutcome::Untranslated {
                id: 1,
                reason: crate::translate::UntranslatedReason::Filtered,
            },
            false,
        ))));
        assert!(
            app.input.value.is_empty(),
            "a filtered line is not handed back to the user"
        );
        let last = app.state.buffers[BUF].messages.back().unwrap();
        assert!(
            last.text.contains("connection unavailable"),
            "it reached the send attempt: {}",
            last.text
        );
    }

    fn set_buffer_langs(app: &mut crate::app::App, lang: Option<&str>, my_lang: Option<&str>) {
        app.config.translate.buffers.insert(
            BUF.to_string(),
            crate::config::TranslateBufferConfig {
                incoming: true,
                outgoing: true,
                lang: lang.map(str::to_string),
                my_lang: my_lang.map(str::to_string),
            },
        );
    }

    fn build(app: &mut crate::app::App) -> Option<PendingOutgoingTranslate> {
        app.build_outgoing_translate(&OutgoingRequest {
            conn_id: "test",
            buffer_id: BUF,
            buffer_name: "#dupa",
            buffer_type: &BufferType::Channel,
            nick: "me",
            text: "moje zdanie",
            is_action: false,
        })
    }

    #[test]
    fn outgoing_translates_out_of_our_language_into_the_buffers() {
        // THE regression. These were inverted: a message typed in our own
        // language went to the broker labelled as the CHANNEL's language and
        // asked to become ours — a no-op at best, garbage at worst. It also
        // made writing to two channels in two languages impossible, since
        // the target came from a single global setting.
        let mut app = app_with_buffer();
        app.config.translate.my_lang = "pl".to_string();
        set_buffer_langs(&mut app, Some("de"), None);

        let pending = build(&mut app).expect("a request is built");
        assert_eq!(
            pending.req.source_lang.as_deref(),
            Some("pl"),
            "outgoing starts in OUR language"
        );
        assert_eq!(
            pending.req.target_lang, "de",
            "and lands in the language this buffer speaks"
        );
    }

    #[test]
    fn two_buffers_can_have_two_different_outgoing_targets() {
        // The user's actual objection: one global target_lang made this
        // impossible.
        let mut app = app_with_buffer();
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Channel, "#otro"));
        app.config.translate.my_lang = "pl".to_string();
        set_buffer_langs(&mut app, Some("de"), None);
        app.config.translate.buffers.insert(
            "test/#otro".to_string(),
            crate::config::TranslateBufferConfig {
                incoming: true,
                outgoing: true,
                lang: Some("es".to_string()),
                my_lang: None,
            },
        );

        let de = build(&mut app).expect("german buffer");
        let es = app
            .build_outgoing_translate(&OutgoingRequest {
                conn_id: "test",
                buffer_id: "test/#otro",
                buffer_name: "#otro",
                buffer_type: &BufferType::Channel,
                nick: "me",
                text: "moje zdanie",
                is_action: false,
            })
            .expect("spanish buffer");
        assert_eq!(de.req.target_lang, "de");
        assert_eq!(es.req.target_lang, "es");
        assert_eq!(de.req.source_lang.as_deref(), Some("pl"));
        assert_eq!(es.req.source_lang.as_deref(), Some("pl"));
    }

    #[test]
    fn a_per_buffer_override_wins_for_outgoing_too() {
        let mut app = app_with_buffer();
        app.config.translate.my_lang = "pl".to_string();
        set_buffer_langs(&mut app, Some("zh"), Some("en"));
        let pending = build(&mut app).expect("a request is built");
        assert_eq!(
            pending.req.source_lang.as_deref(),
            Some("en"),
            "we write this buffer in English, not the global Polish"
        );
        assert_eq!(pending.req.target_lang, "zh");
    }

    #[test]
    fn outgoing_is_refused_when_the_buffer_has_no_language() {
        // There is nothing to detect a WRITE language from, so building a
        // request could only ever produce a wrong target.
        let mut app = app_with_buffer();
        set_buffer_langs(&mut app, None, None);
        assert!(build(&mut app).is_none());
    }

    use crate::app::translate::OutgoingTranslatePolicy as P;

    /// App with `#dupa` configured for outgoing translation.
    fn app_with_outgoing(lang: Option<&str>) -> crate::app::App {
        let mut app = app_with_buffer();
        app.state.set_active_buffer(BUF);
        app.state.translate_active = true;
        app.config.translate.enabled = true;
        app.config.translate.my_lang = "pl".to_string();
        set_buffer_langs(&mut app, lang, None);
        app
    }

    #[test]
    fn policy_translates_a_normal_line_on_an_enabled_buffer() {
        // The happy path must hold, or the refusals below could all pass by
        // refusing everything.
        let app = app_with_outgoing(Some("de"));
        assert_eq!(
            app.outgoing_translate_policy(BUF, "moje zdanie", false),
            P::Translate
        );
    }

    #[test]
    fn policy_refuses_multiline_rather_than_sending_it_untranslated() {
        let app = app_with_outgoing(Some("de"));
        let P::Refuse(reason) = app.outgoing_translate_policy(BUF, "pierwsza\ndruga", false)
        else {
            panic!("multi-line must be refused, not sent");
        };
        assert!(reason.contains("multi-line"), "got: {reason}");
    }

    #[test]
    fn policy_refuses_a_line_too_long_to_translate() {
        let app = app_with_outgoing(Some("de"));
        let long = "a".repeat(crate::irc::MESSAGE_MAX_BYTES + 1);
        let P::Refuse(reason) = app.outgoing_translate_policy(BUF, &long, false) else {
            panic!("an oversized line must be refused, not sent");
        };
        assert!(reason.contains("too long"), "got: {reason}");
    }

    #[test]
    fn policy_refuses_a_buffer_with_no_target_language() {
        // `outgoing = true` with no language is reachable by hand-editing
        // config.toml, and /translate list already reports it. A target
        // cannot be guessed, so it is a refusal, not permission to send.
        let app = app_with_outgoing(None);
        let P::Refuse(reason) = app.outgoing_translate_policy(BUF, "moje zdanie", false) else {
            panic!("a missing target language must be refused, not sent");
        };
        assert!(reason.contains("no target language"), "got: {reason}");
    }

    #[test]
    fn policy_steps_aside_for_e2e_so_the_encrypted_send_proceeds() {
        // Not a refusal: the conversation is simply never translated, and
        // the ordinary encrypted send is exactly right.
        let app = app_with_outgoing(Some("de"));
        assert_eq!(
            app.outgoing_translate_policy(BUF, "moje zdanie", true),
            P::NotApplicable
        );
    }

    #[test]
    fn policy_steps_aside_when_the_buffer_has_no_outgoing_flag() {
        let mut app = app_with_outgoing(Some("de"));
        app.config
            .translate
            .buffers
            .get_mut(BUF)
            .expect("configured")
            .outgoing = false;
        assert_eq!(
            app.outgoing_translate_policy(BUF, "moje zdanie", false),
            P::NotApplicable
        );
    }

    #[test]
    fn policy_steps_aside_when_the_master_switch_is_off() {
        let mut app = app_with_outgoing(Some("de"));
        app.state.translate_active = false;
        assert_eq!(
            app.outgoing_translate_policy(BUF, "moje zdanie", false),
            P::NotApplicable
        );
    }

    #[test]
    fn a_refusal_restores_the_text_and_names_the_reason() {
        let mut app = app_with_outgoing(Some("de"));
        let sent = app.refuse_untranslatable_send("moje zdanie", "the translation queue is full");
        assert!(!sent, "a refusal never reports a send");
        assert_eq!(app.input.value, "moje zdanie", "the text comes back");
        let last = app.state.buffers[BUF]
            .messages
            .back()
            .expect("an error row")
            .text
            .clone();
        assert!(last.contains("queue is full"), "got: {last}");
    }

    #[test]
    fn a_refusal_does_not_clobber_something_typed_since() {
        let mut app = app_with_outgoing(Some("de"));
        app.input.value = "something else".to_string();
        app.refuse_untranslatable_send("moje zdanie", "the translation queue is full");
        assert_eq!(app.input.value, "something else");
    }

    #[test]
    fn raising_max_in_flight_takes_effect_without_a_restart() {
        // It is offered as an ordinary /set option, so accepting it and
        // silently ignoring it is worse than not offering it at all.
        let mut app = app_with_buffer();
        app.translate_in_flight = Some(Arc::new(Semaphore::new(4)));
        app.translate_in_flight_applied = 4;
        app.config.translate.max_in_flight = 9;
        app.sync_translate_from_config();
        assert_eq!(
            app.translate_in_flight.as_ref().unwrap().available_permits(),
            9
        );
        assert_eq!(app.translate_in_flight_applied, 9);
    }

    #[test]
    fn lowering_max_in_flight_takes_effect_without_a_restart() {
        let mut app = app_with_buffer();
        app.translate_in_flight = Some(Arc::new(Semaphore::new(8)));
        app.translate_in_flight_applied = 8;
        app.config.translate.max_in_flight = 2;
        app.sync_translate_from_config();
        assert_eq!(
            app.translate_in_flight.as_ref().unwrap().available_permits(),
            2
        );
    }

    #[test]
    fn a_reduction_blocked_by_in_flight_work_is_carried_and_settled_later() {
        // `forget_permits` can only take what is available. Recording the
        // requested value anyway would let the old concurrency creep back as
        // in-flight work returns its permits.
        let mut app = app_with_buffer();
        let sem = Arc::new(Semaphore::new(8));
        // Six permits are checked out, so only two can be forgotten now.
        let held = sem.clone().try_acquire_many_owned(6).expect("6 available");
        app.translate_in_flight = Some(Arc::clone(&sem));
        app.translate_in_flight_applied = 8;
        app.config.translate.max_in_flight = 2;
        app.sync_translate_from_config();

        assert_eq!(sem.available_permits(), 0, "both spare permits were taken");
        assert_eq!(app.translate_in_flight_applied, 6, "only what really happened");
        assert_eq!(app.translate_in_flight_debt, 4, "the rest is owed");

        // The in-flight work finishes and its permits come back.
        drop(held);
        app.settle_translate_concurrency_debt();
        assert_eq!(app.translate_in_flight_applied, 2, "the target is reached");
        assert_eq!(app.translate_in_flight_debt, 0);
        assert_eq!(sem.available_permits(), 2);
    }

    #[test]
    fn settling_is_a_no_op_with_no_debt() {
        let mut app = app_with_buffer();
        let sem = Arc::new(Semaphore::new(4));
        app.translate_in_flight = Some(Arc::clone(&sem));
        app.translate_in_flight_applied = 4;
        app.settle_translate_concurrency_debt();
        assert_eq!(sem.available_permits(), 4);
    }

    #[test]
    fn retuning_to_the_same_value_is_a_no_op() {
        let mut app = app_with_buffer();
        app.translate_in_flight = Some(Arc::new(Semaphore::new(4)));
        app.translate_in_flight_applied = 4;
        app.config.translate.max_in_flight = 4;
        app.sync_translate_from_config();
        assert_eq!(
            app.translate_in_flight.as_ref().unwrap().available_permits(),
            4
        );
    }

    #[test]
    fn the_by_target_gate_takes_over_a_translatable_send() {
        let mut app = app_with_outgoing(Some("de"));
        let (tx, mut rx) = mpsc::channel(4);
        app.translate_outgoing_tx = tx;
        let handled = app.gate_by_target_translation("test", "#dupa", "moje zdanie");
        assert_eq!(handled, Some(false), "taken over, nothing on the wire yet");
        assert_eq!(rx.try_recv().expect("dispatched").req.text, "moje zdanie");
    }

    #[test]
    fn the_by_target_gate_unwraps_an_action() {
        let mut app = app_with_outgoing(Some("de"));
        let (tx, mut rx) = mpsc::channel(4);
        app.translate_outgoing_tx = tx;
        let handled =
            app.gate_by_target_translation("test", "#dupa", "\x01ACTION waves hello\x01");
        assert_eq!(handled, Some(false));
        let pending = rx.try_recv().expect("dispatched");
        assert_eq!(pending.req.text, "waves hello");
        assert!(pending.is_action);
    }

    #[test]
    fn the_by_target_gate_steps_aside_for_an_untranslated_buffer() {
        let mut app = app_with_buffer();
        assert_eq!(
            app.gate_by_target_translation("test", "#dupa", "hello there"),
            None,
            "the ordinary send must proceed"
        );
    }

    #[test]
    fn the_by_target_gate_ignores_non_action_ctcp() {
        // Protocol, not prose — a translated VERSION reply is nonsense.
        let mut app = app_with_outgoing(Some("de"));
        assert_eq!(
            app.gate_by_target_translation("test", "#dupa", "\x01VERSION\x01"),
            None
        );
    }

    #[test]
    fn an_action_is_dispatched_as_its_inner_text() {
        // `/me` must reach the broker as prose, not as CTCP framing.
        let mut app = app_with_outgoing(Some("de"));
        let (tx, mut rx) = mpsc::channel(4);
        app.translate_outgoing_tx = tx;

        let sent = app.dispatch_by_target_translation(
            "test",
            BUF,
            "#dupa",
            "waves hello",
            true,
        );
        assert!(!sent, "nothing is on the wire yet");
        let pending = rx.try_recv().expect("dispatched");
        assert_eq!(pending.req.text, "waves hello", "no CTCP framing in the request");
        assert!(pending.is_action, "the shape is carried for re-wrapping");
        assert_eq!(pending.req.target_lang, "de");
    }

    #[test]
    fn a_by_target_send_refuses_when_it_cannot_translate() {
        // A by-target send is not a lesser send: publishing it untranslated
        // puts the same wrong-language text on the same channel.
        let mut app = app_with_outgoing(Some("de"));
        let (tx, rx) = mpsc::channel(1);
        app.translate_outgoing_tx = tx;
        drop(rx);

        let sent =
            app.dispatch_by_target_translation("test", BUF, "#dupa", "moje zdanie", false);
        assert!(!sent);
        assert_eq!(app.input.value, "moje zdanie", "the text comes back");
    }

    #[test]
    fn build_outgoing_translate_refuses_a_possibly_e2e_target() {
        // The caller already gates, but this is where cleartext becomes a
        // payload bound for a third party, so the refusal must be a property
        // of the function rather than of its call sites.
        let mut app = app_with_buffer();
        let db = crate::storage::db::open_database(false).unwrap();
        let keyring =
            crate::e2e::keyring::Keyring::new(std::sync::Arc::new(std::sync::Mutex::new(db)));
        let mgr = crate::e2e::manager::E2eManager::load_or_init(keyring).unwrap();
        // Derive the network exactly as the production path does, so the
        // config we install is the one the gate will look up.
        let network = app
            .state
            .connections
            .get("test")
            .map_or_else(String::new, |c| c.label.clone());
        mgr.keyring()
            .set_channel_config(&crate::e2e::keyring::ChannelConfig {
                channel: crate::e2e::scoped_context(&network, "#dupa"),
                enabled: true,
                mode: crate::e2e::keyring::ChannelMode::Normal,
            })
            .unwrap();
        app.state.e2e_manager = Some(std::sync::Arc::new(mgr));

        let pending = app.build_outgoing_translate(&OutgoingRequest {
            conn_id: "test",
            buffer_id: BUF,
            buffer_name: "#dupa",
            buffer_type: &BufferType::Channel,
            nick: "me",
            text: "moje zdanie",
            is_action: false,
        });
        assert!(
            pending.is_none(),
            "no payload may be built for a possibly-E2E target"
        );
    }

    #[test]
    fn flushing_a_buffer_releases_its_pending_lines_in_order() {
        let mut app = app_with_queue(3);
        app.flush_translate_queue(BUF);
        assert_eq!(
            shown(&app),
            vec![
                "line 1 [untranslated: timeout]".to_string(),
                "line 2 [untranslated: timeout]".to_string(),
                "line 3 [untranslated: timeout]".to_string(),
            ],
            "pending lines are released, never dropped"
        );
        assert!(!app.state.translate_queues.contains_key(BUF));
    }

    #[test]
    fn closing_a_buffer_drops_its_queue() {
        // The buffer is gone, so releasing into it would be refused anyway
        // and logging under it would orphan the rows. What must not happen
        // is the queue outliving the buffer.
        let mut app = app_with_queue(3);
        app.state.remove_buffer(BUF);
        assert!(
            !app.state.translate_queues.contains_key(BUF),
            "the queue must not outlive its buffer"
        );
    }

    #[test]
    fn flushing_all_queues_covers_every_buffer() {
        let mut app = app_with_queue(1);
        let other = "test/#other";
        app.state
            .add_buffer(Buffer::for_test("test", BufferType::Channel, "#other"));
        let mut queue = TranslateQueue::new();
        queue.push_pending(9, "other line".to_string(), payload(9, "other line"));
        app.state.translate_queues.insert(other.to_string(), queue);

        app.flush_all_translate_queues();
        assert!(app.state.translate_queues.is_empty());
        assert_eq!(app.state.buffers[other].messages.len(), 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translate::Direction;

    fn cfg(max_in_flight: u32) -> crate::config::TranslateConfig {
        crate::config::TranslateConfig {
            enabled: true,
            max_in_flight,
            ..Default::default()
        }
    }

    fn req(id: u64, text: &str) -> TranslateRequest {
        TranslateRequest {
            id,
            direction: Direction::Incoming,
            network: "libera".to_string(),
            target: "#dupa".to_string(),
            nick: "alice".to_string(),
            text: text.to_string(),
            source_lang: None,
            target_lang: "pl".to_string(),
            known_nicks: Vec::new(),
        }
    }

    #[tokio::test]
    async fn incoming_worker_returns_one_outcome_per_line() {
        let mut rt = TranslateRuntime::with_backend(
            Some(Arc::new(StubBackend::new(0, 0))),
            &cfg(4),
        );
        for id in 1..=3u64 {
            rt.incoming_tx
                .send(PendingTranslate {
                    buffer_id: "libera/#dupa".to_string(),
                    req: req(id, "hello world"),
                })
                .await
                .expect("worker alive");
        }
        let mut seen = Vec::new();
        for _ in 0..3 {
            match rt.deliver_rx.recv().await {
                Some(TranslateDeliver::Incoming { outcome, buffer_id }) => {
                    assert_eq!(buffer_id, "libera/#dupa");
                    seen.push(outcome.id());
                }
                other => panic!("expected an incoming outcome, got {other:?}"),
            }
        }
        seen.sort_unstable();
        assert_eq!(seen, vec![1, 2, 3], "every line gets exactly one outcome");
    }

    #[tokio::test]
    async fn incoming_worker_runs_concurrently() {
        // Four lines at 60 ms each must not take 240 ms — if this worker
        // were serial like shrink's, latency would be cumulative and a busy
        // channel would build an unbounded backlog.
        let mut rt = TranslateRuntime::with_backend(
            Some(Arc::new(StubBackend::with_jitter(&[60]))),
            &cfg(4),
        );
        let started = tokio::time::Instant::now();
        for id in 1..=4u64 {
            rt.incoming_tx
                .send(PendingTranslate {
                    buffer_id: "b".to_string(),
                    req: req(id, "hello world"),
                })
                .await
                .expect("worker alive");
        }
        for _ in 0..4 {
            rt.deliver_rx.recv().await.expect("outcome");
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "four concurrent 60 ms lines took {elapsed:?} — worker looks serial"
        );
    }

    fn outgoing_for(conn: &str, id: u64, text: &str) -> PendingOutgoingTranslate {
        PendingOutgoingTranslate {
            conn_id: conn.to_string(),
            buffer_id: format!("{conn}/#chan"),
            buffer_name: "#chan".to_string(),
            buffer_type: BufferType::Channel,
            original_text: text.to_string(),
            req: req(id, text),
            nick: "me".to_string(),
            own_mode: None,
            peer_handle: None,
            show_original: false,
            is_action: false,
        }
    }

    #[tokio::test]
    async fn a_slow_connection_does_not_block_another_connections_send() {
        // The point of a per-connection FIFO: one hung request on network A
        // must not hold up translated sends on network B, which have
        // nothing to do with it.
        let mut rt = TranslateRuntime::with_backend(
            Some(Arc::new(StubBackend::with_jitter(&[400, 0]))),
            &cfg(4),
        );
        // First into lane "slow" takes the 400 ms slot; second into lane
        // "fast" takes the 0 ms one.
        rt.outgoing_tx
            .send(outgoing_for("slow", 1, "hello world"))
            .await
            .expect("worker alive");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        rt.outgoing_tx
            .send(outgoing_for("fast", 2, "hello world"))
            .await
            .expect("worker alive");

        let first = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            rt.deliver_rx.recv(),
        )
        .await
        .expect("the fast connection must not wait on the slow one")
        .expect("an outcome");
        let TranslateDeliver::Outgoing(d) = first else {
            panic!("expected an outgoing deliver");
        };
        assert_eq!(d.conn_id, "fast", "the unrelated connection finished first");
    }

    #[tokio::test]
    async fn outgoing_worker_preserves_submission_order() {
        // The first message is slow, the second fast. Serial dispatch is
        // what keeps them in order on the wire.
        let mut rt = TranslateRuntime::with_backend(
            Some(Arc::new(StubBackend::with_jitter(&[40, 0]))),
            &cfg(4),
        );
        for id in 1..=2u64 {
            rt.outgoing_tx
                .send(PendingOutgoingTranslate {
                    conn_id: "c".to_string(),
                    buffer_id: "b".to_string(),
                    buffer_name: "#dupa".to_string(),
                    buffer_type: BufferType::Channel,
                    original_text: "hello world".to_string(),
                    req: req(id, "hello world"),
                    nick: "me".to_string(),
                    own_mode: None,
                    peer_handle: None,
                    show_original: false,
                    is_action: false,
                })
                .await
                .expect("worker alive");
        }
        let mut order = Vec::new();
        for _ in 0..2 {
            match rt.deliver_rx.recv().await {
                Some(TranslateDeliver::Outgoing(d)) => order.push(d.outcome.id()),
                other => panic!("expected an outgoing deliver, got {other:?}"),
            }
        }
        assert_eq!(order, vec![1, 2], "submission order is preserved");
    }

    #[tokio::test]
    async fn disabled_runtime_drains_instead_of_backpressuring() {
        // With no backend the gates never dispatch, but a stale `try_send`
        // must not fill the channel and block the IRC path.
        let cfg = crate::config::TranslateConfig::default();
        let rt = TranslateRuntime::with_backend(None, &cfg);
        assert!(rt.backend.is_none());
        for id in 0..2000u64 {
            rt.incoming_tx
                .send(PendingTranslate {
                    buffer_id: "b".to_string(),
                    req: req(id, "hello world"),
                })
                .await
                .expect("drain keeps the channel open");
        }
    }
}

#[cfg(test)]
mod ordering_integration {
    use super::*;
    use crate::config::{TranslateBufferConfig, TranslateConfig};
    use crate::state::buffer::{ActivityLevel, Buffer, BufferType, Message, MessageType};
    use crate::translate::backend::StubBackend;

    const BUF: &str = "test/#dupa";

    fn message(id: u64, text: &str) -> Message {
        Message {
            id,
            timestamp: chrono::Utc::now(),
            message_type: MessageType::Message,
            nick: Some("alice".to_string()),
            nick_mode: None,
            text: text.to_string(),
            highlight: false,
            event_key: None,
            event_params: None,
            log_msg_id: None,
            log_ref_id: None,
            tags: None,
            orig_offset: None,
        }
    }

    /// The headline guarantee, end to end: a burst whose translations come
    /// back deliberately out of order must still render in arrival order.
    ///
    /// The jitter sequence is what gives the test its teeth — with a uniform
    /// delay the burst would finish in order by accident and prove nothing.
    #[tokio::test]
    async fn a_burst_with_out_of_order_returns_preserves_arrival_order() {
        const LINES: u64 = 20;

        let cfg = TranslateConfig {
            enabled: true,
            max_in_flight: 8,
            show_original_in: false,
            ..Default::default()
        };
        // Descending delays: the LAST line dispatched finishes first.
        let delays: Vec<u64> = (0..LINES).map(|i| (LINES - i) * 4).collect();
        let mut rt = TranslateRuntime::with_backend(
            Some(Arc::new(StubBackend::with_jitter(&delays))),
            &cfg,
        );

        let mut state = crate::state::AppState::new();
        state.add_buffer(Buffer::for_test("test", BufferType::Channel, "#dupa"));
        state.translate_incoming_tx = Some(rt.incoming_tx.clone());
        state.translate_active = true;
        state.translate_show_original_in = false;
        state.translate_buffers.insert(
            BUF.to_string(),
            TranslateBufferConfig {
                incoming: true,
                outgoing: false,
                lang: None,
                my_lang: None,
            },
        );

        // Each line's words are reversed by the stub, so "line N of burst"
        // comes back as "burst of N line" — position N is still identifiable
        // and we can prove the translation actually ran.
        for i in 1..=LINES {
            let id = state.next_message_id();
            let msg = message(id, &format!("line {i} of burst"));
            state.add_message_with_activity(BUF, msg, ActivityLevel::Activity);
        }
        assert_eq!(
            state.buffers[BUF].messages.len(),
            0,
            "every line is queued, nothing shown yet"
        );

        // Drain outcomes exactly as the select! arm does.
        let mut arrival_order = Vec::new();
        for _ in 0..LINES {
            let deliver = rt.deliver_rx.recv().await.expect("an outcome per line");
            let TranslateDeliver::Incoming { buffer_id, outcome } = deliver else {
                panic!("expected an incoming outcome");
            };
            arrival_order.push(outcome.id());
            let resolution = match outcome {
                TranslateOutcome::Translated { id, text } => (id, Ok(text)),
                TranslateOutcome::Untranslated { id, reason } => (id, Err(reason)),
            };
            let ready = {
                let queue = state
                    .translate_queues
                    .get_mut(&buffer_id)
                    .expect("the queue exists");
                queue.resolve(resolution.0, resolution.1);
                queue.drain_ready()
            };
            for entry in ready {
                state.add_message_with_activity_unshrunk(&buffer_id, entry.message, entry.activity);
            }
        }

        // The test only means something if the outcomes really did come
        // back out of order. Assert that, so a future change to the stub's
        // timing cannot quietly turn this into a test of nothing.
        assert!(
            arrival_order.windows(2).any(|w| w[0] > w[1]),
            "outcomes arrived in id order ({arrival_order:?}) — the jitter is \
             not producing reordering, so this test proves nothing"
        );

        let shown: Vec<String> = state.buffers[BUF]
            .messages
            .iter()
            .map(|m| m.text.clone())
            .collect();
        assert_eq!(shown.len() as u64, LINES, "every line surfaced");
        for (idx, line) in shown.iter().enumerate() {
            let expected = format!("burst of {} line", idx + 1);
            assert_eq!(
                line, &expected,
                "position {idx} holds the wrong line — ordering broke"
            );
        }
        assert!(
            state.translate_queues[BUF].is_empty(),
            "the queue fully drained"
        );
    }
}

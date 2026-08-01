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
}

/// Channels and shared state the translation workers need. Built once in
/// `App::new` and kept alive for the App's lifetime.
pub struct TranslateRuntime {
    /// `None` when translation is disabled — the gates check this to decide
    /// between dispatching and delivering straight through.
    pub backend: Option<SharedBackend>,
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

        if let Some(ref b) = backend {
            spawn_incoming_worker(
                incoming_rx,
                Arc::clone(b),
                deliver_tx.clone(),
                cfg.max_in_flight.max(1) as usize,
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
    let fut = std::panic::AssertUnwindSafe(backend.translate(req));
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
    max_in_flight: usize,
) {
    tokio::spawn(async move {
        let permits = Arc::new(Semaphore::new(max_in_flight));
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

/// Serial, so two outgoing messages reach IRC in submission order. Nothing
/// incoming participates: an incoming line stuck behind a slow provider
/// must not delay the user's own message.
fn spawn_outgoing_worker(
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
                    },
                )))
                .await;
        }
    });
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
        let wire_text = match &out.outcome {
            TranslateOutcome::Translated { text, .. } => text.clone(),
            // The broker decided this line needed no translation, so the
            // original IS the correct thing to send.
            TranslateOutcome::Untranslated { reason, .. } if !reason.is_gap() => {
                out.original_text.clone()
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
        conn_id: &str,
        buffer_id: &str,
        buffer_name: &str,
        buffer_type: &BufferType,
        nick: &str,
        text: &str,
    ) -> Option<PendingOutgoingTranslate> {
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
        let source_lang = self
            .config
            .translate
            .buffers
            .get(buffer_id)
            .and_then(|c| c.source_lang.clone());

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
                target_lang: self.config.translate.target_lang.clone(),
                known_nicks,
            },
            nick: captured_nick,
            own_mode: captured_own_mode,
            peer_handle: captured_peer_handle,
            show_original: self.config.translate.show_original_out,
        })
    }

    /// Hand a refused message back to the user instead of losing it.
    fn restore_outgoing_input(&mut self, out: &OutgoingTranslateDeliver, reason: &str) {
        self.deliver_translate_error(
            &out.buffer_id,
            &format!(
                "Not sent — translation failed ({reason}). Your text is back in the input line."
            ),
        );
        // Only restore into an empty input: the user may have typed
        // something else during the wait, and clobbering that would be a
        // second, worse surprise. Otherwise the text is still visible in the
        // error row above, so it is never truly lost.
        if self.input.value.is_empty() {
            self.input.value.clone_from(&out.original_text);
            self.input.cursor_pos = self.input.value.chars().count();
        }
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
        let (echo_text, orig_offset) = if matches!(out.outcome, TranslateOutcome::Translated { .. })
        {
            crate::translate::compose_display(plain_echo, &out.original_text, out.show_original)
        } else {
            (plain_echo.to_string(), None)
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
            self.state.add_message(
                &out.buffer_id,
                crate::state::buffer::Message {
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
            .translate_target_lang
            .clone_from(&self.config.translate.target_lang);
        self.state.translate_show_original_in = self.config.translate.show_original_in;
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
            vec!["line 1".to_string(), "line 2".to_string()],
            "a timed-out line shows its original, in order"
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

    #[test]
    fn flushing_a_buffer_releases_its_pending_lines_in_order() {
        let mut app = app_with_queue(3);
        app.flush_translate_queue(BUF);
        assert_eq!(
            shown(&app),
            vec![
                "line 1".to_string(),
                "line 2".to_string(),
                "line 3".to_string()
            ],
            "pending lines are released, never dropped"
        );
        assert!(!app.state.translate_queues.contains_key(BUF));
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
